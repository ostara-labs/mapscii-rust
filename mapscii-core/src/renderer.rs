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
use std::time::Instant;

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

/// Maximum number of non-fill, non-label features drawn per frame.
/// Acts as a safety valve to prevent UI freeze when a viewport contains
/// an extreme number of road / admin line segments.
const MAX_LINE_FEATURES_PER_FRAME: usize = 4000;

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
pub(crate) struct TileLayerFeatures {
    pub(crate) scale: f64,
    pub(crate) features: Vec<TileFeature>,
}

/// A tile with fetched data and extracted features.
pub(crate) struct FetchedTile {
    pub(crate) visible: VisibleTile,
    pub(crate) layer_features: std::collections::HashMap<String, TileLayerFeatures>,
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

/// Max bridge fill gap (d1+d2), scaled by world pixel width at the given
/// **tile zoom** (integer). Using tile_zoom instead of display zoom ensures
/// a stable gap_max within each zoom band — fractional zoom changes only
/// affect tilesize, not the bridge-fill threshold.
/// Cap at 32 px to avoid bridging across narrow land features (peninsulas,
/// isthmuses) that can be as small as ~50 px at mid-zoom levels.
#[inline]
fn gap_max_for_zoom(tile_zoom: u8, project_size: u32) -> u16 {
    let world_px = project_size as f64 * 2.0_f64.powi(tile_zoom as i32);
    (world_px / 128.0).round().clamp(0.0, 32.0) as u16
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

    /// Prepare the canvas for a new frame: set background color from style + clear.
    pub(crate) fn prepare_frame(&mut self) {
        self.label_buffer.clear();
        if let Some(bg) = self.styler.style_by_id.get("background") {
            if let Some(color_str) = bg.paint.get("background-color").and_then(|v| v.as_str()) {
                let [r, g, b] = hex2rgb(color_str);
                self.canvas.set_background(rgb_to_xterm(r, g, b));
            }
        }
        self.canvas.clear();
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
        let frame_start = Instant::now();

        self.prepare_frame();

        let visible = self.visible_tiles(center_lat, center_lon, zoom);
        if visible.is_empty() {
            return false;
        }
        log::debug!("[renderer] zoom={zoom:.2} visible_tiles={}", visible.len());

        // Fetch all tiles concurrently
        let fetch_start = Instant::now();
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
        let fetch_ms = fetch_start.elapsed().as_millis();

        let mut fetched_tiles: Vec<FetchedTile> = Vec::new();
        let features_start = Instant::now();
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
        let features_ms = features_start.elapsed().as_millis();

        // Render tiles
        let render_start = Instant::now();
        self.render_tiles(&fetched_tiles, zoom);
        let render_ms = render_start.elapsed().as_millis();

        let total_ms = frame_start.elapsed().as_millis();
        log::debug!(
            "[renderer] frame={total_ms}ms fetch={fetch_ms}ms features={features_ms}ms render={render_ms}ms tiles={}",
            fetched_tiles.len()
        );

        true
    }

    /// Determine which tiles are visible in the current viewport.
    ///
    /// `zoom` is the display zoom (fractional). Tile coordinates are computed
    /// from the *tile zoom* (`base_zoom`, integer).
    pub fn visible_tiles(&self, center_lat: f64, center_lon: f64, zoom: f64) -> Vec<VisibleTile> {
        let tile_z = utils::base_zoom(zoom, &self.config) as f64;
        let center = utils::ll2tile(center_lon, center_lat, tile_z);
        let tile_size = utils::tilesize_at_zoom(zoom, &self.config);

        let mut tiles = Vec::new();
        let grid_size = 2.0_f64.powf(tile_z);

        // Dynamic tile range: cover viewport + TILE_PADDING on each side.
        // With floor()-based base_zoom, tile_size >= project_size (tiles
        // only grow within a zoom band), but we keep the dynamic range to
        // handle all viewport sizes cleanly.
        let half_w = (self.width as f64 / 2.0 + TILE_PADDING) / tile_size;
        let half_h = (self.height as f64 / 2.0 + TILE_PADDING) / tile_size;

        let tx_min = (center.x - half_w).floor() as i32 - 1;
        let tx_max = (center.x + half_w).floor() as i32 + 1;
        let ty_min = (center.y - half_h).floor() as i32 - 1;
        let ty_max = (center.y + half_h).floor() as i32 + 1;

        for ty in ty_min..=ty_max {
            for tx in tx_min..=tx_max {
                let pos_x =
                    self.width as f64 / 2.0 - (center.x - tx as f64) * tile_size;
                let pos_y =
                    self.height as f64 / 2.0 - (center.y - ty as f64) * tile_size;

                let mut tile_x = tx % (grid_size as i32);
                if tile_x < 0 {
                    tile_x = if tile_z == 0.0 { 0 } else { tile_x + grid_size as i32 };
                }

                if ty < 0
                    || ty >= grid_size as i32
                    || pos_x + tile_size < -TILE_PADDING
                    || pos_y + tile_size < -TILE_PADDING
                    || pos_x > self.width as f64 + TILE_PADDING
                    || pos_y > self.height as f64 + TILE_PADDING
                {
                    continue;
                }

                tiles.push(VisibleTile {
                    key: TileKey::new(tile_z as u8, tile_x as u32, ty as u32),
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
    pub(crate) fn get_tile_features(
        &self,
        vt: &VisibleTile,
        tile: &crate::tile::Tile,
        zoom: f64,
    ) -> std::collections::HashMap<String, TileLayerFeatures> {
        let mut result = std::collections::HashMap::new();
        let tile_zoom = utils::base_zoom(zoom, &self.config);
        let draw_order = Self::generate_draw_order(tile_zoom);
        let mut total_features = 0usize;

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
                .filter(|f| {
                    // Pre-filter by zoom range so we never pass invisible
                    // features to scale_and_reduce / draw_feature.
                    if let Some(minzoom) = f.minzoom {
                        if zoom < minzoom {
                            return false;
                        }
                    }
                    if let Some(maxzoom) = f.maxzoom {
                        if zoom > maxzoom {
                            return false;
                        }
                    }
                    // Skip line features whose bbox is sub-pixel (< 2px in
                    // both axes). At low zoom, thousands of road segments
                    // collapse to single-pixel dots.
                    if f.style_type == "line" {
                        let inv = 1.0 / scale;
                        let w = (f.max_x - f.min_x) as f64 * inv;
                        let h = (f.max_y - f.min_y) as f64 * inv;
                        if w < 2.0 && h < 2.0 {
                            return false;
                        }
                    }
                    true
                })
                .cloned()
                .collect();

            total_features += features.len();

            if *layer_id == "water" {
                let raw_count = layer.tree.locate_in_envelope_intersecting(&search_envelope).count();
                log::trace!(
                    "[renderer] water layer: raw_rtree={} after_filter={} extent={} scale={:.4} tile={:?}",
                    raw_count, features.len(), layer.extent, scale, vt.key
                );
            }

            result.insert(
                layer_id.to_string(),
                TileLayerFeatures { scale, features },
            );
        }

        log::trace!(
            "[renderer] tile {:?} features={total_features} layers={}",
            vt.key,
            result.len()
        );
        result
    }

    /// Render all fetched tiles in draw order.
    pub(crate) fn render_tiles(&mut self, tiles: &[FetchedTile], zoom: f64) {
        if tiles.is_empty() {
            return;
        }

        let tile_zoom = utils::base_zoom(zoom, &self.config);
        let draw_order = Self::generate_draw_order(tile_zoom);


        let mut labels: Vec<(&FetchedTile, &TileFeature, f64)> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut drawn_features = 0usize;
        let mut fill_features = 0usize;
        let mut line_budget = MAX_LINE_FEATURES_PER_FRAME;

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
                        if feature.style_type == "fill" {
                            fill_features += 1;
                        } else {
                            if line_budget == 0 {
                                continue;
                            }
                            line_budget -= 1;
                        }
                        drawn_features += 1;
                        self.draw_feature(&tile.visible, feature, layer.scale, &mut seen);
                    }
                }
            }
        }

        log::debug!(
            "[renderer] render_tiles drawn={drawn_features} fills={fill_features} labels={} line_budget_left={}",
            labels.len(),
            line_budget,
        );

        // Bridge fill: close narrow gaps between distinct water components.
        // 1. Build mask per tile (0=empty, 1=water, 2=barrier).
        // 2. Label water connected components (4-connectivity).
        // 3. Multi-source BFS from water boundary pixels through EMPTY only.
        //    For each empty pixel, record the two nearest distinct components.
        // 4. Fill pixels where d1+d2 <= gap_max and sources are roughly
        //    opposite (angle >= 90°). Single pass — no re-labeling.
        let gap_max = gap_max_for_zoom(tile_zoom, self.config.project_size);

        for tile in tiles {
            if let Some(water_layer) = tile.layer_features.get("water") {
                if water_layer.features.is_empty() {
                    continue;
                }
                let vt = &tile.visible;
                let water_color = water_layer.features[0].color;

                let tw = vt.size.ceil() as usize;
                let th = tw;
                if tw == 0 || th == 0 { continue; }

                let x_off = vt.position_x.round() as i32;
                let y_off = vt.position_y.round() as i32;

                let n = tw * th;
                let mut mask = vec![0u8; n];
                for ly in 0..th {
                    for lx in 0..tw {
                        let px = x_off + lx as i32;
                        let py = y_off + ly as i32;
                        let c = self.canvas.buffer.get_pixel_color(px, py);
                        if c == water_color {
                            mask[ly * tw + lx] = 1;
                        } else if c != 0 {
                            mask[ly * tw + lx] = 2;
                        }
                    }
                }

                // Step 1: label water components (4-connected)
                let mut comp = vec![0u16; n];
                let mut next_id: u16 = 1;
                for ly in 0..th {
                    for lx in 0..tw {
                        let i = ly * tw + lx;
                        if mask[i] == 1 && comp[i] == 0 {
                            let cid = next_id;
                            next_id = next_id.saturating_add(1);
                            let mut stack = vec![(lx, ly)];
                            comp[i] = cid;
                            while let Some((sx, sy)) = stack.pop() {
                                for (nx, ny) in [(sx.wrapping_sub(1), sy), (sx + 1, sy), (sx, sy.wrapping_sub(1)), (sx, sy + 1)] {
                                    if nx < tw && ny < th {
                                        let ni = ny * tw + nx;
                                        if mask[ni] == 1 && comp[ni] == 0 {
                                            comp[ni] = cid;
                                            stack.push((nx, ny));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                if next_id <= 2 { continue; }

                // Identify large edge-connected water components (likely ocean).
                let vp_w = self.width as i32;
                let vp_h = self.height as i32;
                let vis_x0 = 0i32.max(-x_off) as usize;
                let vis_y0 = 0i32.max(-y_off) as usize;
                let vis_x1 = tw.min((vp_w - x_off) as usize);
                let vis_y1 = th.min((vp_h - y_off) as usize);

                let mut comp_sizes: std::collections::HashMap<u16, u32> = std::collections::HashMap::new();
                let mut edge_comps: std::collections::HashSet<u16> = std::collections::HashSet::new();
                for ly in 0..th {
                    for lx in 0..tw {
                        let c = comp[ly * tw + lx];
                        if c == 0 { continue; }
                        *comp_sizes.entry(c).or_default() += 1;
                        if vis_x0 <= vis_x1 && vis_y0 <= vis_y1 {
                            if lx == vis_x0 || lx + 1 == vis_x1 || ly == vis_y0 || ly + 1 == vis_y1 {
                                edge_comps.insert(c);
                            }
                        }
                    }
                }
                let min_ocean_size = (n / 20) as u32;
                let ocean_comps: std::collections::HashSet<u16> = edge_comps
                    .into_iter()
                    .filter(|c| comp_sizes.get(c).copied().unwrap_or(0) >= min_ocean_size)
                    .collect();

                // Step 2: multi-source BFS from water boundary pixels through
                // EMPTY pixels. For each empty pixel track the two nearest
                // distinct components: (comp_id, distance, source_x, source_y).
                #[derive(Clone, Copy, Default)]
                struct Hit { comp: u16, dist: u16, sx: u16, sy: u16 }
                let mut hit1 = vec![Hit::default(); n];
                let mut hit2 = vec![Hit::default(); n];

                let mut queue: std::collections::VecDeque<(usize, usize, u16, u16, u16, u16)> =
                    std::collections::VecDeque::new();

                for ly in 0..th {
                    for lx in 0..tw {
                        let i = ly * tw + lx;
                        if mask[i] != 1 { continue; }
                        let has_empty_neighbor = [(lx.wrapping_sub(1), ly), (lx + 1, ly), (lx, ly.wrapping_sub(1)), (lx, ly + 1)]
                            .iter()
                            .any(|&(nx, ny)| nx < tw && ny < th && mask[ny * tw + nx] == 0);
                        if has_empty_neighbor {
                            queue.push_back((lx, ly, 0, comp[i], lx as u16, ly as u16));
                        }
                    }
                }

                while let Some((x, y, dist, cid, sx, sy)) = queue.pop_front() {
                    if dist >= gap_max { continue; }
                    for (nx, ny) in [(x.wrapping_sub(1), y), (x + 1, y), (x, y.wrapping_sub(1)), (x, y + 1)] {
                        if nx >= tw || ny >= th { continue; }
                        let ni = ny * tw + nx;
                        if mask[ni] != 0 { continue; }

                        let nd = dist + 1;
                        let h = &mut hit1[ni];
                        if h.comp == 0 {
                            *h = Hit { comp: cid, dist: nd, sx, sy };
                            queue.push_back((nx, ny, nd, cid, sx, sy));
                        } else if h.comp == cid {
                            if nd < h.dist {
                                *h = Hit { comp: cid, dist: nd, sx, sy };
                                queue.push_back((nx, ny, nd, cid, sx, sy));
                            }
                        } else {
                            let h2 = &mut hit2[ni];
                            if h2.comp == 0 {
                                *h2 = Hit { comp: cid, dist: nd, sx, sy };
                            } else if h2.comp == cid && nd < h2.dist {
                                *h2 = Hit { comp: cid, dist: nd, sx, sy };
                            }
                        }
                    }
                }

                // Step 3: fill empty pixels between two distinct water
                // components from opposite directions (bridge fill for
                // polygon seams).

                let mut filled_count = 0u32;
                for ly in 0..th {
                    for lx in 0..tw {
                        let i = ly * tw + lx;
                        if mask[i] != 0 { continue; }
                        let h1 = hit1[i];
                        if h1.comp == 0 { continue; }

                        let h2 = hit2[i];
                        if h2.comp != 0 && h1.comp != h2.comp && h1.dist + h2.dist <= gap_max
                            && (ocean_comps.contains(&h1.comp) || ocean_comps.contains(&h2.comp))
                        {
                            let dx1 = h1.sx as f32 - lx as f32;
                            let dy1 = h1.sy as f32 - ly as f32;
                            let dx2 = h2.sx as f32 - lx as f32;
                            let dy2 = h2.sy as f32 - ly as f32;
                            let dot = dx1 * dx2 + dy1 * dy2;
                            let len_sq1 = dx1 * dx1 + dy1 * dy1;
                            let len_sq2 = dx2 * dx2 + dy2 * dy2;
                            if len_sq1 == 0.0 || len_sq2 == 0.0 {
                                let px = x_off + lx as i32;
                                let py = y_off + ly as i32;
                                if self.canvas.buffer.get_pixel_color(px, py) == 0 {
                                    self.canvas.buffer.set_pixel(px, py, water_color);
                                    filled_count += 1;
                                }
                            } else {
                                let cos_angle = dot / (len_sq1.sqrt() * len_sq2.sqrt());
                                if cos_angle <= 0.0 {
                                    let px = x_off + lx as i32;
                                    let py = y_off + ly as i32;
                                    if self.canvas.buffer.get_pixel_color(px, py) == 0 {
                                        self.canvas.buffer.set_pixel(px, py, water_color);
                                        filled_count += 1;
                                    }
                                }
                            }
                        }
                    }
                }

                if filled_count > 0 {
                    log::debug!(
                        "[renderer] bridge-fill tile {:?}: filled {} pixels",
                        vt.key, filled_count,
                    );
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
    ///
    /// Zoom-range filtering is already done in `get_tile_features()`.
    fn draw_feature(
        &mut self,
        vt: &VisibleTile,
        feature: &TileFeature,
        scale: f64,
        seen: &mut HashSet<String>,
    ) {
        match feature.style_type.as_str() {
            "line" => {
                if let Some(ring) = feature.points.first() {
                    let points = self.scale_and_reduce(vt, ring, scale, true);
                    if !points.is_empty() {
                        self.canvas.polyline_thin(&points, feature.color, feature.line_width);
                    }
                }
            }
            "fill" => {
                let scaled_rings: Vec<Vec<Point>> = feature
                    .points
                    .iter()
                    .map(|ring| self.scale_and_reduce(vt, ring, scale, false))
                    .collect();

                if let Some(outer) = scaled_rings.first() {
                    let unique = Self::unique_point_count(outer);
                    if feature.layer == "water" {
                        let (mut min_x, mut min_y, mut max_x, mut max_y) =
                            (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
                        for p in outer {
                            min_x = min_x.min(p.x);
                            min_y = min_y.min(p.y);
                            max_x = max_x.max(p.x);
                            max_y = max_y.max(p.y);
                        }
                        log::trace!(
                            "[renderer] water fill: rings={} pts={} unique={} bbox=({:.0},{:.0})-({:.0},{:.0}) color={}",
                            scaled_rings.len(), outer.len(), unique, min_x, min_y, max_x, max_y, feature.color
                        );
                    }
                    if unique < 3 {
                        for p in outer {
                            self.canvas.buffer.set_pixel(
                                p.x as i32,
                                p.y as i32,
                                feature.color,
                            );
                        }
                    } else {
                        self.canvas.scanline_polygon_fill(&scaled_rings, feature.color);
                    }
                }
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

    /// Count distinct (x,y) positions in a scaled ring, ignoring the
    /// closing duplicate (first == last).
    fn unique_point_count(ring: &[Point]) -> usize {
        if ring.is_empty() {
            return 0;
        }
        let mut count = 1usize;
        for i in 1..ring.len() {
            if ring[i].x != ring[i - 1].x || ring[i].y != ring[i - 1].y {
                count += 1;
            }
        }
        // If the ring is closed (last == first), don't count the duplicate.
        if ring.len() >= 2 {
            let first = &ring[0];
            let last = &ring[ring.len() - 1];
            if first.x == last.x && first.y == last.y {
                count -= 1;
            }
        }
        count
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

            if filter {
                if last_x == Some(x) && last_y == Some(y) {
                    continue;
                }
                last_x = Some(x);
                last_y = Some(y);
            }

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
    /// more detail layers are included. Building and house-number layers are
    /// suppressed below zoom 11 / 14 because the features are sub-pixel noise
    /// that only adds CPU cost.
    fn generate_draw_order(tile_zoom: u8) -> Vec<&'static str> {
        if tile_zoom < 2 {
            vec!["landuse", "admin", "water", "country_label", "marine_label"]
        } else {
            let mut layers = vec![
                "landuse",
                "water",
                "marine_label",
                "road",
                "admin",
                "country_label",
                "state_label",
                "water_label",
                "place_label",
                "rail_station_label",
                "poi_label",
                "road_label",
            ];
            if tile_zoom >= 11 {
                layers.insert(3, "building");
            }
            if tile_zoom >= 14 {
                layers.push("housenum_label");
            }
            layers
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
        assert!(tiles.len() <= 16, "too many tiles: {}", tiles.len());
        assert!(tiles.len() >= 1);
    }

    #[test]
    fn test_visible_tiles_zoom_0() {
        let config = Arc::new(MapConfig::default());
        let styler = test_styler();
        let mut r = Renderer::new(styler, config);
        r.set_size(160, 80);

        let tiles = r.visible_tiles(0.0, 0.0, 0.0);
        assert!(!tiles.is_empty());
        assert!(tiles.len() <= 16, "too many tiles: {}", tiles.len());
    }

    #[test]
    fn test_draw_order_low_zoom() {
        let order = Renderer::generate_draw_order(1);
        assert_eq!(order.len(), 5);
        assert!(order.contains(&"water"));
        assert!(order.contains(&"landuse"));
        let landuse_pos = order.iter().position(|&l| l == "landuse").unwrap();
        let water_pos = order.iter().position(|&l| l == "water").unwrap();
        assert!(landuse_pos < water_pos, "landuse must be drawn before water");
    }

    #[test]
    fn test_draw_order_high_zoom() {
        let order = Renderer::generate_draw_order(5);
        assert!(order.len() > 4);
        assert!(!order.contains(&"building"));
        assert!(order.contains(&"road"));

        let order11 = Renderer::generate_draw_order(11);
        assert!(order11.contains(&"building"));
        assert!(order11.contains(&"road"));

        let order14 = Renderer::generate_draw_order(14);
        assert!(order14.contains(&"housenum_label"));
    }

    #[test]
    fn test_gap_max_for_zoom() {
        assert_eq!(gap_max_for_zoom(0, 256), 2);
        assert_eq!(gap_max_for_zoom(2, 256), 8);
        assert_eq!(gap_max_for_zoom(3, 256), 16);
        assert_eq!(gap_max_for_zoom(4, 256), 32);
        assert_eq!(gap_max_for_zoom(5, 256), 32);
        assert_eq!(gap_max_for_zoom(8, 256), 32);
    }
}
