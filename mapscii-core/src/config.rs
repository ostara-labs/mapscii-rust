//! Map configuration with sensible defaults matching the original mapscii.
//!
//! `MapConfig` is an immutable configuration object. Use the builder-style
//! methods (`with_*`) to customize before passing to `MapState::new()`.

use std::collections::HashMap;

/// Configuration for per-layer behavior (label margins, clustering).
#[derive(Debug, Clone, Default)]
pub struct LayerConfig {
    /// Margin (in characters) for label collision.
    pub margin: Option<u32>,
    /// Whether to cluster overlapping labels.
    pub cluster: bool,
}

/// Immutable map configuration. Construct with `MapConfig::default()` and
/// customize with builder methods.
#[derive(Debug, Clone)]
pub struct MapConfig {
    /// Language code for labels (e.g. "en", "de", "ja").
    pub language: String,

    /// Tile source URL. Must end with `/`.
    /// Default: `"http://mapscii.me/"`
    pub source: String,

    /// Path to an `.mbtiles` file (alternative to HTTP source).
    pub mbtiles_path: Option<String>,

    /// Initial zoom level. `None` means auto-detect based on terminal size.
    pub initial_zoom: Option<f64>,

    /// Maximum allowed zoom level.
    pub max_zoom: u8,

    /// Zoom increment per scroll step.
    pub zoom_step: f64,

    /// Initial latitude (WGS-84).
    pub initial_lat: f64,

    /// Initial longitude (WGS-84).
    pub initial_lon: f64,

    /// Apply Ramer-Douglas-Peucker simplification to polylines.
    pub simplify_polylines: bool,

    /// Render using Braille characters (true) or ASCII blocks (false).
    pub use_braille: bool,

    /// Persist downloaded tiles to `~/.mapscii/`.
    pub persist_downloaded_tiles: bool,

    /// The base tile range (number of zoom levels for tile indexing).
    pub tile_range: u8,

    /// Internal projection size (pixels per tile at base zoom).
    pub project_size: u32,

    /// Default label margin in characters.
    pub label_margin: u32,

    /// Per-layer configuration overrides.
    pub layers: HashMap<String, LayerConfig>,

    /// Marker character for POIs.
    pub poi_marker: char,

    /// The bundled style JSON (defaults to dark.json via `include_str!`).
    pub style_json: &'static str,

    /// Number of parsed tiles to keep in memory cache.
    /// Each tile is approximately 100-200KB parsed.
    /// Default: 256 tiles (~25-50MB RAM).
    pub tile_cache_size: usize,
}

impl Default for MapConfig {
    fn default() -> Self {
        let mut layers = HashMap::new();
        layers.insert(
            "housenum_label".to_string(),
            LayerConfig {
                margin: Some(4),
                cluster: false,
            },
        );
        layers.insert(
            "poi_label".to_string(),
            LayerConfig {
                margin: Some(5),
                cluster: true,
            },
        );
        layers.insert(
            "place_label".to_string(),
            LayerConfig {
                margin: None,
                cluster: true,
            },
        );
        layers.insert(
            "state_label".to_string(),
            LayerConfig {
                margin: None,
                cluster: true,
            },
        );

        Self {
            language: "en".to_string(),
            source: "http://mapscii.me/".to_string(),
            mbtiles_path: None,
            initial_zoom: None,
            max_zoom: 18,
            zoom_step: 0.1,
            initial_lat: 52.51298,
            initial_lon: 13.42012,
            simplify_polylines: false,
            use_braille: true,
            persist_downloaded_tiles: true,
            tile_range: 14,
            project_size: 256,
            label_margin: 5,
            layers,
            poi_marker: '◉',
            style_json: include_str!("../../styles/dark.json"),
            tile_cache_size: 256,
        }
    }
}

impl MapConfig {
    /// Create a new config with default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the tile source URL.
    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = source.into();
        self
    }

    /// Set the MBTiles file path (overrides HTTP source).
    pub fn with_mbtiles(mut self, path: impl Into<String>) -> Self {
        self.mbtiles_path = Some(path.into());
        self
    }

    /// Set the initial center position.
    pub fn with_center(mut self, lat: f64, lon: f64) -> Self {
        self.initial_lat = lat;
        self.initial_lon = lon;
        self
    }

    /// Set the initial zoom level.
    pub fn with_zoom(mut self, zoom: f64) -> Self {
        self.initial_zoom = Some(zoom);
        self
    }

    /// Set the maximum zoom level.
    pub fn with_max_zoom(mut self, max_zoom: u8) -> Self {
        self.max_zoom = max_zoom;
        self
    }

    /// Set zoom step (increment per scroll).
    pub fn with_zoom_step(mut self, step: f64) -> Self {
        self.zoom_step = step;
        self
    }

    /// Set the display language for labels.
    pub fn with_language(mut self, lang: impl Into<String>) -> Self {
        self.language = lang.into();
        self
    }

    /// Enable or disable Braille rendering.
    pub fn with_braille(mut self, enabled: bool) -> Self {
        self.use_braille = enabled;
        self
    }

    /// Enable or disable tile persistence to disk.
    pub fn with_tile_persistence(mut self, enabled: bool) -> Self {
        self.persist_downloaded_tiles = enabled;
        self
    }

    /// Set the tile cache size (number of parsed tiles in memory).
    pub fn with_tile_cache_size(mut self, size: usize) -> Self {
        self.tile_cache_size = size;
        self
    }
}
