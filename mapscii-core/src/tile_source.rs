//! Tile source: HTTP fetching with LRU cache + optional disk persistence.
//!
//! Supports:
//! - Remote tile servers (HTTP/HTTPS)
//! - Local MBTiles files (behind the `mbtiles` cargo feature)
//! - In-memory LRU cache
//! - Disk cache for downloaded tiles
//!
//! Translated from the original mapscii `TileSource.js`.

use std::path::PathBuf;
use std::sync::Arc;

use lru::LruCache;
use std::num::NonZeroUsize;
use tokio::sync::Mutex;

use crate::config::MapConfig;
use crate::styler::Styler;
use crate::tile::Tile;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A tile key: (z, x, y).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileKey {
    pub z: u8,
    pub x: u32,
    pub y: u32,
}

impl TileKey {
    pub fn new(z: u8, x: u32, y: u32) -> Self {
        Self { z, x, y }
    }

    fn cache_name(&self) -> String {
        format!("{}-{}-{}", self.z, self.x, self.y)
    }

    fn disk_path(&self, base: &PathBuf) -> PathBuf {
        base.join(self.z.to_string())
            .join(format!("{}-{}.pbf", self.x, self.y))
    }
}

/// The tile source mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceMode {
    Http,
    #[cfg(feature = "mbtiles")]
    MBTiles,
}

// ---------------------------------------------------------------------------
// TileSource
// ---------------------------------------------------------------------------

/// Manages fetching, caching, and parsing of vector tiles.
pub struct TileSource {
    /// The source URL or file path.
    source: String,
    /// The operating mode (HTTP, MBTiles).
    mode: SourceMode,
    /// Parsed + styled tile cache.
    cache: Mutex<LruCache<String, Arc<Tile>>>,
    /// HTTP client (reused for connection pooling).
    http_client: reqwest::Client,
    /// Disk cache directory (if persistence is enabled).
    cache_dir: Option<PathBuf>,
    /// Whether to persist tiles to disk.
    persist: bool,
    /// Reference to the styler.
    styler: Option<Arc<Styler>>,
    /// Reference to config.
    config: Arc<MapConfig>,
    /// MBTiles connection (behind feature flag).
    #[cfg(feature = "mbtiles")]
    mbtiles: Option<rusqlite::Connection>,
}

impl TileSource {
    /// Create a new tile source.
    pub fn new(config: Arc<MapConfig>) -> Self {
        let source = config
            .mbtiles_path
            .clone()
            .unwrap_or_else(|| config.source.clone());

        let mode = {
            #[cfg(feature = "mbtiles")]
            {
                if source.ends_with(".mbtiles") {
                    SourceMode::MBTiles
                } else {
                    SourceMode::Http
                }
            }
            #[cfg(not(feature = "mbtiles"))]
            {
                SourceMode::Http
            }
        };

        let cache_dir = if config.persist_downloaded_tiles {
            Self::init_cache_dir()
        } else {
            None
        };

        Self {
            source,
            mode,
            cache: Mutex::new(LruCache::new(
                NonZeroUsize::new(config.tile_cache_size).unwrap(),
            )),
            http_client: reqwest::Client::new(),
            cache_dir,
            persist: config.persist_downloaded_tiles,
            styler: None,
            config,
            #[cfg(feature = "mbtiles")]
            mbtiles: None,
        }
    }

    /// Set the styler for this tile source.
    pub fn set_styler(&mut self, styler: Arc<Styler>) {
        self.styler = Some(styler);
    }

    /// Initialize the MBTiles connection (only with `mbtiles` feature).
    #[cfg(feature = "mbtiles")]
    pub fn init_mbtiles(&mut self) -> Result<(), TileSourceError> {
        if self.mode == SourceMode::MBTiles {
            let conn = rusqlite::Connection::open(&self.source)
                .map_err(|e| TileSourceError::MBTiles(e.to_string()))?;
            self.mbtiles = Some(conn);
        }
        Ok(())
    }

    /// Fetch a tile at the given coordinates.
    ///
    /// Returns a cached tile if available, otherwise fetches/loads from source.
    pub async fn get_tile(&self, key: TileKey) -> Result<Arc<Tile>, TileSourceError> {
        let cache_name = key.cache_name();

        // Check in-memory cache
        {
            let mut cache = self.cache.lock().await;
            if let Some(tile) = cache.get(&cache_name) {
                return Ok(Arc::clone(tile));
            }
        }

        // Fetch the raw buffer
        let buffer = self.fetch_raw(key).await?;

        // Parse the tile
        let styler = self.styler.as_deref();
        let tile = Tile::load(&buffer, styler, &self.config)
            .map_err(|e| TileSourceError::TileParse(e.to_string()))?;
        let tile = Arc::new(tile);

        // Store in cache
        {
            let mut cache = self.cache.lock().await;
            cache.put(cache_name, Arc::clone(&tile));
        }

        Ok(tile)
    }

    /// Fetch raw tile bytes from source.
    async fn fetch_raw(&self, key: TileKey) -> Result<Vec<u8>, TileSourceError> {
        match self.mode {
            SourceMode::Http => self.fetch_http(key).await,
            #[cfg(feature = "mbtiles")]
            SourceMode::MBTiles => self.fetch_mbtiles(key),
        }
    }

    /// Fetch a tile over HTTP.
    async fn fetch_http(&self, key: TileKey) -> Result<Vec<u8>, TileSourceError> {
        // Try disk cache first
        if self.persist {
            if let Some(data) = self.read_from_disk(key) {
                return Ok(data);
            }
        }

        let url = format!("{}{}/{}/{}.pbf", self.source, key.z, key.x, key.y);

        let response = self
            .http_client
            .get(&url)
            .send()
            .await
            .map_err(|e| TileSourceError::Http(e.to_string()))?;

        if !response.status().is_success() {
            return Err(TileSourceError::Http(format!(
                "HTTP {} for {}",
                response.status(),
                url
            )));
        }

        let buffer = response
            .bytes()
            .await
            .map_err(|e| TileSourceError::Http(e.to_string()))?
            .to_vec();

        // Persist to disk
        if self.persist {
            self.write_to_disk(key, &buffer);
        }

        Ok(buffer)
    }

    /// Fetch a tile from MBTiles.
    #[cfg(feature = "mbtiles")]
    fn fetch_mbtiles(&self, key: TileKey) -> Result<Vec<u8>, TileSourceError> {
        let conn = self
            .mbtiles
            .as_ref()
            .ok_or_else(|| TileSourceError::MBTiles("MBTiles not initialized".to_string()))?;

        // MBTiles uses TMS y-axis (flipped)
        let tms_y = (1u32 << key.z) - 1 - key.y;

        let mut stmt = conn
            .prepare("SELECT tile_data FROM tiles WHERE zoom_level = ?1 AND tile_column = ?2 AND tile_row = ?3")
            .map_err(|e| TileSourceError::MBTiles(e.to_string()))?;

        let data: Vec<u8> = stmt
            .query_row(rusqlite::params![key.z as u32, key.x, tms_y], |row| {
                row.get(0)
            })
            .map_err(|e| TileSourceError::MBTiles(e.to_string()))?;

        Ok(data)
    }

    // -- Disk cache --------------------------------------------------------

    /// Initialize the disk cache directory.
    fn init_cache_dir() -> Option<PathBuf> {
        let cache_dir = dirs::cache_dir()?.join("mapscii");
        std::fs::create_dir_all(&cache_dir).ok()?;
        Some(cache_dir)
    }

    /// Read a tile from disk cache.
    fn read_from_disk(&self, key: TileKey) -> Option<Vec<u8>> {
        let dir = self.cache_dir.as_ref()?;
        let path = key.disk_path(dir);
        std::fs::read(&path).ok()
    }

    /// Write a tile to disk cache.
    fn write_to_disk(&self, key: TileKey, data: &[u8]) {
        if let Some(ref dir) = self.cache_dir {
            let path = key.disk_path(dir);
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&path, data);
        }
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors from tile source operations.
#[derive(Debug, thiserror::Error)]
pub enum TileSourceError {
    #[error("HTTP error: {0}")]
    Http(String),
    #[error("tile parse error: {0}")]
    TileParse(String),
    #[cfg(feature = "mbtiles")]
    #[error("MBTiles error: {0}")]
    MBTiles(String),
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tile_key() {
        let key = TileKey::new(5, 16, 10);
        assert_eq!(key.cache_name(), "5-16-10");
    }

    #[test]
    fn test_tile_key_disk_path() {
        let key = TileKey::new(5, 16, 10);
        let base = PathBuf::from("/tmp/cache");
        let path = key.disk_path(&base);
        assert_eq!(path, PathBuf::from("/tmp/cache/5/16-10.pbf"));
    }

    #[test]
    fn test_source_mode_default_http() {
        let config = Arc::new(MapConfig::default());
        let ts = TileSource::new(config);
        assert_eq!(ts.mode, SourceMode::Http);
    }

    #[test]
    fn test_tile_source_new() {
        let config = Arc::new(MapConfig::default());
        let ts = TileSource::new(config.clone());
        assert_eq!(ts.source, config.source);
    }
}
