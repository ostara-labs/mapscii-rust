//! The map renderer — orchestrates tile fetching, styling, and drawing.
//!
//! Given a center position, zoom level, and viewport size, the renderer:
//! 1. Determines which tiles are visible
//! 2. Fetches them from the tile source
//! 3. Queries each tile's spatial index for visible features
//! 4. Draws features onto the canvas in proper draw order
//! 5. Handles label placement with collision detection
//!
//! Translated from the original mapscii `Renderer.js`.

use std::collections::HashSet;
use std::sync::Arc;

use futures::future::join_all;
use rstar::AABB;

use crate::braille_buffer::rgb_to_xterm;
use crate::canvas::{Canvas, Point};
use crate::config::MapConfig;
use crate::label_buffer::LabelBuffer;
use crate::styler::Styler;
use crate::tile::TileFeature;
use crate::tile_source::{TileKey, TileSource};
use crate::utils::{self, hex2rgb};

/// Padding (in pixels) around the viewport for rendering off-screen features
/// that might extend into view.
const TILE_PADDING: f64 = 64.0;

// ---------------------------------------------------------------------------
// Visible tile descriptor
// ---------------------------------------------------------------------------

/// A tile that is visible in the current viewport.
#[derive(Debug, Clone)]
pub struct VisibleTile {
    /// Tile coordinates (z/x/y).
    pub key: TileKey,
    /// Fractional zoom (for minzoom/maxzoom checks).
    pub zoom: f64,
    /// Pixel position of this tile's top-left corner in the viewport.
    pub position_x: f64,
    pub position_y: f64,
    /// Pixel size of this tile at the current zoom.
    pub size: f64,
}

// ---------------------------------------------------------------------------
// Per-tile extracted features (ready for drawing)
// ---------------------------------------------------------------------------

/// Features extracted from a tile, grouped by layer.
struct TileLayerFeatures {
    scale: f64,
    features: Vec<TileFeature>,
}

/// A tile with fetched data and extracted features.
struct FetchedTile {
    visible: VisibleTile,
    layer_features: std::collections::HashMap<String, TileLayerFeatures>,
}

// ---------------------------------------------------------------------------
// Renderer
// ---------------------------------------------------------------------------

/// The map renderer. Not `Send`/`Sync` — it owns the canvas and label buffer.
pub struct Renderer {
    pub width: usize,
    pub height: usize,
    pub canvas: Canvas,
    pub label_buffer: LabelBuffer,
    pub styler: Arc<Styler>,
    pub config: Arc<MapConfig>,
}

impl Renderer {
    /// Create a new renderer.
    pub fn new(styler: Arc<Styler>, config: Arc<MapConfig>) -> Self {
        Self {
            width: 0,
            height: 0,
            canvas: Canvas::new(4, 4), // placeholder, will be resized
            label_buffer: LabelBuffer::new(),
            styler,
            config,
        }
    }

    /// Set the viewport size in pixels (Braille pixel space: width = cols*2, height = rows*4).
    pub fn set_size(&mut self, width: usize, height: usize) {
        self.width = width;
        self.height = height;
        self.canvas = Canvas::new(width, height);
    }

    /// Draw the map at the given center and zoom.
    ///
    /// `tile_source` is used to fetch tiles asynchronously. Returns `true` if
    /// the draw completed, `false` if there was nothing to draw.
    pub async fn draw(
        &mut self,
        center_lat: f64,
        center_lon: f64,
        zoom: f64,
        tile_source: &TileSource,
    ) -> bool {
        self.label_buffer.clear();

        // Set background color
        if let Some(bg) = self.styler.style_by_id.get("background") {
            if let Some(color_str) = bg.paint.get("background-color").and_then(|v| v.as_str()) {
                let [r, g, b] = hex2rgb(color_str);
                self.canvas.set_background(rgb_to_xterm(r, g, b));
            }
        }

        self.canvas.clear();

        let visible = self.visible_tiles(center_lat, center_lon, zoom);
        if visible.is_empty() {
            return false;
        }

        // Fetch all tiles concurrently
        let fetch_tasks: Vec<_> = visible
            .iter()
            .map(|vt| {
                let key = vt.key;
                async move {
                    let tile_result = tile_source.get_tile(key).await;
                    (vt.clone(), tile_result)
                }
            })
            .collect();

        let results = join_all(fetch_tasks).await;

        let mut fetched_tiles: Vec<FetchedTile> = Vec::new();
        for (vt, tile_result) in results {
            match tile_result {
                Ok(tile) => {
                    let layer_features = self.get_tile_features(&vt, &tile, zoom);
                    fetched_tiles.push(FetchedTile {
                        visible: vt,
                        layer_features,
                    });
                }
                Err(e) => {
                    log::warn!("Failed to fetch tile {:?}: {}", vt.key, e);
                }
            }
        }

        // Render tiles
        self.render_tiles(&fetched_tiles, zoom);

        true
    }

    /// Determine which tiles are visible in the current viewport.
    pub fn visible_tiles(&self, center_lat: f64, center_lon: f64, zoom: f64) -> Vec<VisibleTile> {
        let z = utils::base_zoom(zoom, &self.config) as f64;
        let center = utils::ll2tile(center_lon, center_lat, z);
        let tile_size = utils::tilesize_at_zoom(zoom, &self.config);

        let mut tiles = Vec::new();
        let grid_size = 2.0_f64.powf(z);

        let center_tile_x = center.x.floor() as i32;
        let center_tile_y = center.y.floor() as i32;

        for ty in (center_tile_y - 1)..=(center_tile_y + 1) {
            for tx in (center_tile_x - 1)..=(center_tile_x + 1) {
                let pos_x =
                    self.width as f64 / 2.0 - (center.x - tx as f64) * tile_size;
                let pos_y =
                    self.height as f64 / 2.0 - (center.y - ty as f64) * tile_size;

                let mut tile_x = tx % (grid_size as i32);
                if tile_x < 0 {
                    tile_x = if z == 0.0 { 0 } else { tile_x + grid_size as i32 };
                }

                // Skip tiles outside the valid range
                if ty < 0
                    || ty >= grid_size as i32
                    || pos_x + tile_size < 0.0
                    || pos_y + tile_size < 0.0
                    || pos_x > self.width as f64
                    || pos_y > self.height as f64
                {
                    continue;
                }

                tiles.push(VisibleTile {
                    key: TileKey::new(z as u8, tile_x as u32, ty as u32),
                    zoom,
                    position_x: pos_x,
                    position_y: pos_y,
                    size: tile_size,
                });
            }
        }

        tiles
    }

    /// Extract visible features from a tile, grouped by layer.
    fn get_tile_features(
        &self,
        vt: &VisibleTile,
        tile: &crate::tile::Tile,
        zoom: f64,
    ) -> std::collections::HashMap<String, TileLayerFeatures> {
        let mut result = std::collections::HashMap::new();
        let draw_order = Self::generate_draw_order(zoom);

        for layer_id in &draw_order {
            let layer = match tile.layers.get(*layer_id) {
                Some(l) => l,
                None => continue,
            };

            let tile_size = utils::tilesize_at_zoom(zoom, &self.config);
            let scale = layer.extent as f64 / tile_size;

            // Search the RTree for features in the visible area
            let search_envelope = AABB::from_corners(
                [
                    (-vt.position_x * scale) as i32,
                    (-vt.position_y * scale) as i32,
                ],
                [
                    ((self.width as f64 - vt.position_x) * scale) as i32,
                    ((self.height as f64 - vt.position_y) * scale) as i32,
                ],
            );

            let features: Vec<TileFeature> = layer
                .tree
                .locate_in_envelope_intersecting(&search_envelope)
                .cloned()
                .collect();

            result.insert(
                layer_id.to_string(),
                TileLayerFeatures { scale, features },
            );
        }

        result
    }

    /// Render all fetched tiles in draw order.
    fn render_tiles(&mut self, tiles: &[FetchedTile], zoom: f64) {
        if tiles.is_empty() {
            return;
        }

        let draw_order = Self::generate_draw_order(zoom);
        let mut labels: Vec<(&FetchedTile, &TileFeature, f64)> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();

        // First pass: draw non-label features
        for layer_id in &draw_order {
            for tile in tiles {
                let layer = match tile.layer_features.get(*layer_id) {
                    Some(l) => l,
                    None => continue,
                };

                for feature in &layer.features {
                    if layer_id.contains("label") {
                        labels.push((tile, feature, layer.scale));
                    } else {
                        self.draw_feature(&tile.visible, feature, layer.scale, &mut seen);
                    }
                }
            }
        }

        // Sort labels by sort order
        labels.sort_by(|a, b| {
            let sa = a.1.sort.unwrap_or(0);
            let sb = b.1.sort.unwrap_or(0);
            sa.cmp(&sb)
        });

        // Second pass: draw labels
        for (tile, feature, scale) in labels {
            self.draw_feature(&tile.visible, feature, scale, &mut seen);
        }
    }

    /// Draw a single feature onto the canvas.
    fn draw_feature(
        &mut self,
        vt: &VisibleTile,
        feature: &TileFeature,
        scale: f64,
        seen: &mut HashSet<String>,
    ) {
        // Check zoom range
        if let Some(minzoom) = feature.minzoom {
            if vt.zoom < minzoom {
                return;
            }
        }
        if let Some(maxzoom) = feature.maxzoom {
            if vt.zoom > maxzoom {
                return;
            }
        }

        match feature.style_type.as_str() {
            "line" => {
                if let Some(ring) = feature.points.first() {
                    let points = self.scale_and_reduce(vt, ring, scale, true);
                    if !points.is_empty() {
                        self.canvas.polyline(&points, feature.color, feature.line_width);
                    }
                }
            }
            "fill" => {
                let scaled_rings: Vec<Vec<Point>> = feature
                    .points
                    .iter()
                    .map(|ring| self.scale_and_reduce(vt, ring, scale, false))
                    .collect();
                self.canvas.polygon(&scaled_rings, feature.color);
            }
            "symbol" => {
                let poi_marker = self.config.poi_marker.to_string();
                let text = feature
                    .label
                    .as_deref()
                    .unwrap_or(&poi_marker);

                if seen.contains(text) && text != poi_marker {
                    return;
                }

                if let Some(ring) = feature.points.first() {
                    let points = self.scale_and_reduce(vt, ring, scale, true);
                    let mut placed = false;

                    for p in &points {
                        let x = p.x - text.len() as f64;
                        let layer_margin = self
                            .config
                            .layers
                            .get(&feature.layer)
                            .and_then(|lc| lc.margin)
                            .map(|m| m as f64);
                        let margin = layer_margin.or(Some(self.config.label_margin as f64));

                        if self.label_buffer.write_if_possible(
                            text,
                            x,
                            p.y,
                            None,
                            margin,
                        ) {
                            self.canvas.text(text, x as i32, p.y as i32, feature.color, false);
                            placed = true;
                            break;
                        } else {
                            // Try cluster marker
                            let cluster = self
                                .config
                                .layers
                                .get(&feature.layer)
                                .map(|lc| lc.cluster)
                                .unwrap_or(false);
                            if cluster {
                                if self.label_buffer.write_if_possible(
                                    &poi_marker,
                                    p.x,
                                    p.y,
                                    None,
                                    Some(3.0),
                                ) {
                                    self.canvas.text(
                                        &poi_marker,
                                        p.x as i32,
                                        p.y as i32,
                                        feature.color,
                                        false,
                                    );
                                    placed = true;
                                    break;
                                }
                            }
                        }
                    }

                    if placed {
                        seen.insert(text.to_string());
                    }
                }
            }
            _ => {}
        }
    }

    /// Scale tile coordinates to viewport pixel coordinates and remove duplicates.
    ///
    /// If `filter` is true, points outside the padded viewport are culled
    /// (but boundary points are kept to avoid gaps).
    fn scale_and_reduce(
        &self,
        vt: &VisibleTile,
        points: &[crate::tile::TilePoint],
        scale: f64,
        filter: bool,
    ) -> Vec<Point> {
        let mut scaled: Vec<Point> = Vec::with_capacity(points.len());
        let mut last_x: Option<i32> = None;
        let mut last_y: Option<i32> = None;
        let mut outside = false;

        let min_x = -TILE_PADDING;
        let min_y = -TILE_PADDING;
        let max_x = self.width as f64 + TILE_PADDING;
        let max_y = self.height as f64 + TILE_PADDING;

        for p in points {
            let x = (vt.position_x + (p.x as f64 / scale)).floor() as i32;
            let y = (vt.position_y + (p.y as f64 / scale)).floor() as i32;

            // Skip duplicate points
            if last_x == Some(x) && last_y == Some(y) {
                continue;
            }
            last_x = Some(x);
            last_y = Some(y);

            if filter {
                let xf = x as f64;
                let yf = y as f64;
                if xf < min_x || xf > max_x || yf < min_y || yf > max_y {
                    if outside {
                        continue;
                    }
                    outside = true;
                } else {
                    if outside {
                        outside = false;
                        // Include the boundary point to avoid gaps
                        if let (Some(lx), Some(ly)) = (last_x, last_y) {
                            scaled.push(Point::new(lx as f64, ly as f64));
                        }
                    }
                }
            }

            scaled.push(Point::new(x as f64, y as f64));
        }

        // For non-symbol features, require at least 2 points
        if scaled.len() < 2 && filter {
            return Vec::new();
        }

        // Optionally simplify
        if self.config.simplify_polylines && filter && scaled.len() >= 3 {
            let tuples: Vec<(f64, f64)> = scaled.iter().map(|p| (p.x, p.y)).collect();
            let simplified = utils::simplify(&tuples, 0.5);
            return simplified
                .into_iter()
                .map(|(x, y)| Point::new(x, y))
                .collect();
        }

        scaled
    }

    /// Generate the layer draw order based on zoom level.
    ///
    /// At low zoom levels, only broad features are drawn. At higher zooms,
    /// more detail layers are included.
    fn generate_draw_order(zoom: f64) -> Vec<&'static str> {
        if zoom < 2.0 {
            vec!["admin", "water", "country_label", "marine_label"]
        } else {
            vec![
                "landuse",
                "water",
                "marine_label",
                "building",
                "road",
                "admin",
                "country_label",
                "state_label",
                "water_label",
                "place_label",
                "rail_station_label",
                "poi_label",
                "road_label",
                "housenum_label",
            ]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_styler() -> Arc<Styler> {
        Arc::new(Styler::new(include_str!("../../styles/dark.json")).unwrap())
    }

    #[test]
    fn test_renderer_creation() {
        let config = Arc::new(MapConfig::default());
        let styler = test_styler();
        let r = Renderer::new(styler, config);
        assert_eq!(r.width, 0);
        assert_eq!(r.height, 0);
    }

    #[test]
    fn test_set_size() {
        let config = Arc::new(MapConfig::default());
        let styler = test_styler();
        let mut r = Renderer::new(styler, config);
        r.set_size(160, 80);
        assert_eq!(r.width, 160);
        assert_eq!(r.height, 80);
        assert_eq!(r.canvas.width, 160);
        assert_eq!(r.canvas.height, 80);
    }

    #[test]
    fn test_visible_tiles_berlin() {
        let config = Arc::new(MapConfig::default());
        let styler = test_styler();
        let mut r = Renderer::new(styler, config);
        r.set_size(160, 80);

        let tiles = r.visible_tiles(52.51298, 13.42012, 4.0);
        assert!(!tiles.is_empty());
        // At zoom 4, we should have a 3x3 grid minus any that fall outside
        assert!(tiles.len() <= 9);
        assert!(tiles.len() >= 1);
    }

    #[test]
    fn test_visible_tiles_zoom_0() {
        let config = Arc::new(MapConfig::default());
        let styler = test_styler();
        let mut r = Renderer::new(styler, config);
        r.set_size(160, 80);

        let tiles = r.visible_tiles(0.0, 0.0, 0.0);
        // At zoom 0 there's only 1 tile in the world
        assert!(!tiles.is_empty());
        assert!(tiles.len() <= 9);
    }

    #[test]
    fn test_draw_order_low_zoom() {
        let order = Renderer::generate_draw_order(1.5);
        assert_eq!(order.len(), 4);
        assert!(order.contains(&"water"));
    }

    #[test]
    fn test_draw_order_high_zoom() {
        let order = Renderer::generate_draw_order(5.0);
        assert!(order.len() > 4);
        assert!(order.contains(&"building"));
        assert!(order.contains(&"road"));
    }
}
