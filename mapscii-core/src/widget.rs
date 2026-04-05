//! Ratatui `StatefulWidget` implementation for the map.
//!
//! `MapWidget` is a lightweight configuration reference that implements
//! `StatefulWidget`. `MapState` holds all mutable state: center position,
//! zoom, tile cache, overlay collection, and the renderer.
//!
//! Usage:
//! ```ignore
//! let config = Arc::new(MapConfig::default());
//! let mut state = MapState::new(config);
//! // In your async runtime:
//! state.load_visible_tiles().await;
//! // In your ratatui render loop:
//! let widget = MapWidget::new();
//! frame.render_stateful_widget(widget, area, &mut state);
//! ```

use std::sync::Arc;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::StatefulWidget;

use crate::braille_buffer::ColorIdx;
use crate::config::MapConfig;
use crate::overlay::{self, LatLon, OverlayCollection, OverlayId};
use crate::renderer::Renderer;
use crate::styler::Styler;
use crate::tile_source::TileSource;
use crate::utils;

// ---------------------------------------------------------------------------
// MapState
// ---------------------------------------------------------------------------

/// Mutable map state. Holds the center position, zoom level, overlays,
/// tile source, renderer, and a "needs redraw" flag.
pub struct MapState {
    /// Current center latitude.
    pub center_lat: f64,
    /// Current center longitude.
    pub center_lon: f64,
    /// Current fractional zoom level.
    pub zoom: f64,
    /// Minimum zoom (auto-calculated from viewport size).
    pub min_zoom: f64,
    /// Whether to use Braille rendering.
    pub use_braille: bool,

    /// The map configuration.
    pub config: Arc<MapConfig>,
    /// The tile source (HTTP / MBTiles).
    pub tile_source: TileSource,
    /// The compiled style.
    pub styler: Arc<Styler>,
    /// The renderer (canvas + label buffer).
    pub renderer: Renderer,
    /// Custom overlays.
    pub overlays: OverlayCollection,

    /// Whether a redraw is needed (set by navigation methods, cleared by render).
    pub needs_redraw: bool,
    /// The last rendered viewport size (cols, rows) — detect resize.
    last_area: Option<(u16, u16)>,
    /// Active drag anchor: (start_col, start_row, start_lat, start_lon).
    drag_anchor: Option<(f64, f64, f64, f64)>,
}

impl MapState {
    /// Create a new map state with the given configuration.
    pub fn new(config: Arc<MapConfig>) -> Self {
        let styler = Arc::new(
            Styler::new(config.style_json).expect("failed to parse style JSON"),
        );

        let mut tile_source = TileSource::new(Arc::clone(&config));
        tile_source.set_styler(Arc::clone(&styler));

        let renderer = Renderer::new(Arc::clone(&styler), Arc::clone(&config));

        Self {
            center_lat: config.initial_lat,
            center_lon: config.initial_lon,
            zoom: config.initial_zoom.unwrap_or(0.0),
            min_zoom: 0.0,
            use_braille: config.use_braille,
            config,
            tile_source,
            styler,
            renderer,
            overlays: OverlayCollection::new(),
            needs_redraw: true,
            last_area: None,
            drag_anchor: None,
        }
    }

    /// Builder: set the initial center.
    pub fn with_center(mut self, lat: f64, lon: f64) -> Self {
        self.center_lat = lat;
        self.center_lon = lon;
        self
    }

    /// Builder: set the initial zoom.
    pub fn with_zoom(mut self, zoom: f64) -> Self {
        self.zoom = zoom;
        self
    }

    // -- Navigation --------------------------------------------------------

    /// Set the center position.
    pub fn set_center(&mut self, lat: f64, lon: f64) {
        let mut lat = lat;
        let mut lon = lon;
        utils::normalize(&mut lon, &mut lat);
        self.center_lat = lat;
        self.center_lon = lon;
        self.needs_redraw = true;
    }

    /// Move the center by a lat/lon delta.
    pub fn move_by(&mut self, dlat: f64, dlon: f64) {
        self.set_center(self.center_lat + dlat, self.center_lon + dlon);
    }

    /// Zoom by a step amount (+ve = zoom in, -ve = zoom out).
    pub fn zoom_by(&mut self, step: f64) {
        let new_zoom = self.zoom + step;
        if new_zoom < self.min_zoom {
            self.zoom = self.min_zoom;
        } else if new_zoom > self.config.max_zoom as f64 {
            self.zoom = self.config.max_zoom as f64;
        } else {
            self.zoom = new_zoom;
        }
        self.needs_redraw = true;
    }

    /// Set the zoom level to an absolute value (clamped to `[min_zoom, max_zoom]`).
    pub fn set_zoom(&mut self, zoom: f64) {
        self.zoom = zoom.clamp(self.min_zoom, self.config.max_zoom as f64);
        self.needs_redraw = true;
    }

    /// Zoom by `step` anchored at a terminal cell position (`col`, `row`).
    ///
    /// The map point under `(col, row)` stays fixed while the zoom changes.
    /// This is the behavior callers expect for scroll-wheel zoom: the cursor
    /// position stays put.
    pub fn zoom_at(&mut self, step: f64, col: f64, row: f64) {
        let (anchor_lat, anchor_lon) = self.colrow_to_ll(col, row);

        let old_zoom = self.zoom;
        self.zoom_by(step);

        if (self.zoom - old_zoom).abs() < f64::EPSILON {
            return;
        }

        let px = (col - 0.5) * 2.0;
        let py = (row - 0.5) * 4.0;
        let dx = px - self.renderer.width as f64 / 2.0;
        let dy = py - self.renderer.height as f64 / 2.0;

        let size = utils::tilesize_at_zoom(self.zoom, &self.config);
        let z = utils::base_zoom(self.zoom, &self.config) as f64;
        let anchor_tile = utils::ll2tile(anchor_lon, anchor_lat, z);
        let (new_center_lon, new_center_lat) =
            utils::tile2ll(anchor_tile.x - dx / size, anchor_tile.y - dy / size, z);

        self.set_center(new_center_lat, new_center_lon);
    }

    // -- Drag ---------------------------------------------------------------

    /// Begin a drag gesture at the given terminal cell position.
    pub fn drag_start(&mut self, col: f64, row: f64) {
        self.drag_anchor = Some((col, row, self.center_lat, self.center_lon));
    }

    /// Move the map so the point under `drag_start` stays under `(col, row)`.
    ///
    /// Returns `false` if no drag is active.
    pub fn drag_to(&mut self, col: f64, row: f64) -> bool {
        let Some((start_col, start_row, start_lat, start_lon)) = self.drag_anchor else {
            return false;
        };

        let dx = (start_col - col) * 2.0;
        let dy = (start_row - row) * 4.0;

        let tile_size = utils::tilesize_at_zoom(self.zoom, &self.config);
        let z = utils::base_zoom(self.zoom, &self.config) as f64;
        let center = utils::ll2tile(start_lon, start_lat, z);
        let (new_lon, new_lat) =
            utils::tile2ll(center.x + dx / tile_size, center.y + dy / tile_size, z);

        self.set_center(new_lat, new_lon);
        true
    }

    /// End the current drag gesture. Returns `true` if a drag was active.
    pub fn drag_end(&mut self) -> bool {
        self.drag_anchor.take().is_some()
    }

    // -- Focus --------------------------------------------------------------

    /// Set the center and zoom in a single call.
    pub fn focus_position(&mut self, lat: f64, lon: f64, zoom: f64) {
        self.set_center(lat, lon);
        self.set_zoom(zoom);
    }

    /// Fit the map to a geographic bounding box.
    ///
    /// Computes the zoom level that makes the box fill the viewport, then
    /// centers on the box's midpoint. The viewport must have been sized
    /// (via `set_render_size` or at least one render pass) for this to
    /// produce meaningful results.
    pub fn focus_area(&mut self, min_lat: f64, min_lon: f64, max_lat: f64, max_lon: f64) {
        let center_lat = (min_lat + max_lat) / 2.0;
        let center_lon = (min_lon + max_lon) / 2.0;

        let w = self.renderer.width as f64;
        let h = self.renderer.height as f64;

        if w < 1.0 || h < 1.0 {
            self.set_center(center_lat, center_lon);
            return;
        }

        let lon_span = (max_lon - min_lon).abs().max(1e-6);

        // zoom_lon: 2^z <= viewport_width * 360 / (lon_span * project_size)
        let ps = self.config.project_size as f64;
        let zoom_lon = (w * 360.0 / (lon_span * ps)).log2();

        // zoom_lat: use Mercator tile coordinates (higher lat = smaller y)
        let min_y = utils::ll2tile(0.0, max_lat, 0.0).y;
        let max_y = utils::ll2tile(0.0, min_lat, 0.0).y;
        let y_span = (max_y - min_y).abs().max(1e-12);
        let zoom_lat = (h / (y_span * ps)).log2();

        let zoom = zoom_lon
            .min(zoom_lat)
            .clamp(self.min_zoom, self.config.max_zoom as f64);

        self.set_center(center_lat, center_lon);
        self.set_zoom(zoom);
    }

    // -- Render size --------------------------------------------------------

    /// Set the render area size in terminal cells (for embedded use).
    ///
    /// Call before `load_visible_tiles` to size the viewport correctly
    /// when the map occupies a sub-region of the terminal.
    pub fn set_render_size(&mut self, cols: u16, rows: u16) {
        let width = (cols as usize) * 2;
        let height = (rows as usize) * 4;

        if width < 4 || height < 4 {
            return;
        }

        let width = width & !1;
        let height = height & !3;

        self.renderer.set_size(width, height);

        self.min_zoom = 4.0 - (4096.0 / width as f64).ln() / std::f64::consts::LN_2;
        if self.zoom < self.min_zoom {
            self.zoom = self.min_zoom;
        }

        self.last_area = Some((cols, rows));
        self.needs_redraw = true;
    }

    /// Toggle between Braille and ASCII rendering.
    pub fn toggle_braille(&mut self) {
        self.use_braille = !self.use_braille;
        self.needs_redraw = true;
    }

    // -- Overlays ----------------------------------------------------------

    /// Add a marker at a position.
    pub fn add_marker(
        &mut self,
        pos: LatLon,
        label: Option<String>,
        color: ColorIdx,
    ) -> OverlayId {
        self.needs_redraw = true;
        self.overlays.add_marker(pos, label, color)
    }

    /// Add a polyline.
    pub fn add_line(
        &mut self,
        points: Vec<LatLon>,
        color: ColorIdx,
        line_width: f64,
    ) -> OverlayId {
        self.needs_redraw = true;
        self.overlays.add_line(points, color, line_width)
    }

    /// Add a filled area.
    pub fn add_area(&mut self, points: Vec<LatLon>, color: ColorIdx) -> OverlayId {
        self.needs_redraw = true;
        self.overlays.add_area(points, color)
    }

    /// Add a GeoJSON layer.
    pub fn add_geojson_layer(
        &mut self,
        geojson_str: &str,
        color: ColorIdx,
    ) -> Result<OverlayId, String> {
        self.needs_redraw = true;
        self.overlays.add_geojson_layer(geojson_str, color)
    }

    /// Remove an overlay by ID.
    pub fn remove_overlay(&mut self, id: OverlayId) -> bool {
        let removed = self.overlays.remove(id);
        if removed {
            self.needs_redraw = true;
        }
        removed
    }

    /// Remove all overlays.
    pub fn clear_overlays(&mut self) {
        self.overlays.clear();
        self.needs_redraw = true;
    }

    // -- Tile loading & rendering -------------------------------------------

    /// Load (fetch) all visible tiles and render them onto the canvas.
    ///
    /// This is async and should be called from your tokio runtime before
    /// the ratatui render loop. The widget's `render()` just blits the
    /// already-drawn canvas to the ratatui buffer.
    pub async fn load_visible_tiles(&mut self) {
        self.renderer.draw(
            self.center_lat,
            self.center_lon,
            self.zoom,
            &self.tile_source,
        ).await;

        // Draw overlays on top of the base map
        overlay::render_overlays(
            &self.overlays,
            &mut self.renderer.canvas,
            self.center_lat,
            self.center_lon,
            self.zoom,
            self.renderer.width,
            self.renderer.height,
            &self.config,
        );

        self.needs_redraw = true;
    }

    // -- Viewport ----------------------------------------------------------

    /// Update the renderer size from a ratatui `Rect`.
    /// Returns `true` if the size changed.
    fn update_size(&mut self, area: Rect) -> bool {
        let new_area = (area.width, area.height);
        if self.last_area == Some(new_area) {
            return false;
        }

        // Pixel dimensions: each terminal cell = 2 px wide, 4 px tall
        let width = (area.width as usize) * 2;
        let height = (area.height as usize) * 4;

        if width < 4 || height < 4 {
            return false;
        }

        // Ensure even width and height divisible by 4
        let width = width & !1; // round down to even
        let height = height & !3; // round down to multiple of 4

        self.renderer.set_size(width, height);

        // Calculate minimum zoom based on viewport
        self.min_zoom = 4.0 - (4096.0 / width as f64).ln() / std::f64::consts::LN_2;

        // If zoom was 0 (auto), set to min_zoom
        if self.zoom < self.min_zoom {
            self.zoom = self.min_zoom;
        }

        self.last_area = Some(new_area);
        self.needs_redraw = true;
        true
    }

    /// Convert terminal column/row position to lat/lon.
    pub fn colrow_to_ll(&self, col: f64, row: f64) -> (f64, f64) {
        let px = (col - 0.5) * 2.0;
        let py = (row - 0.5) * 4.0;

        let w = self.renderer.width as f64;
        let h = self.renderer.height as f64;

        let dx = px - w / 2.0;
        let dy = py - h / 2.0;

        let size = utils::tilesize_at_zoom(self.zoom, &self.config);
        let z = utils::base_zoom(self.zoom, &self.config) as f64;
        let center = utils::ll2tile(self.center_lon, self.center_lat, z);
        let (lon, lat) = utils::tile2ll(center.x + dx / size, center.y + dy / size, z);

        let mut lon = lon;
        let mut lat = lat;
        utils::normalize(&mut lon, &mut lat);
        (lat, lon)
    }

    /// Get the status text (center coords + zoom).
    pub fn status_text(&self) -> String {
        format!(
            "center: {:.3}, {:.3}  zoom: {:.1}",
            self.center_lat, self.center_lon, self.zoom,
        )
    }
}

// ---------------------------------------------------------------------------
// MapWidget
// ---------------------------------------------------------------------------

/// A lightweight ratatui widget that renders the map.
///
/// Implements `StatefulWidget` with `State = MapState`.
pub struct MapWidget;

impl MapWidget {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MapWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl StatefulWidget for MapWidget {
    type State = MapState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        if area.width < 2 || area.height < 1 {
            return;
        }

        // Update size if needed (triggers needs_redraw)
        state.update_size(area);

        // Blit the pre-rendered canvas to the ratatui buffer.
        // The actual drawing is done in load_visible_tiles() (async).
        state
            .renderer
            .canvas
            .buffer
            .render_to_ratatui_buffer(area, buf, state.use_braille);

        state.needs_redraw = false;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> Arc<MapConfig> {
        Arc::new(MapConfig::default())
    }

    #[test]
    fn test_map_state_creation() {
        let config = test_config();
        let state = MapState::new(config.clone());
        assert_eq!(state.center_lat, config.initial_lat);
        assert_eq!(state.center_lon, config.initial_lon);
        assert!(state.needs_redraw);
    }

    #[test]
    fn test_map_state_set_center() {
        let state_config = test_config();
        let mut state = MapState::new(state_config);
        state.set_center(48.8566, 2.3522);
        assert!((state.center_lat - 48.8566).abs() < 0.001);
        assert!((state.center_lon - 2.3522).abs() < 0.001);
    }

    #[test]
    fn test_map_state_move_by() {
        let config = test_config();
        let mut state = MapState::new(config);
        let lat0 = state.center_lat;
        let lon0 = state.center_lon;
        state.move_by(1.0, 2.0);
        assert!((state.center_lat - (lat0 + 1.0)).abs() < 0.01);
        assert!((state.center_lon - (lon0 + 2.0)).abs() < 0.01);
    }

    #[test]
    fn test_map_state_zoom_by() {
        let config = test_config();
        let mut state = MapState::new(config);
        state.zoom = 5.0;
        state.zoom_by(1.0);
        assert!((state.zoom - 6.0).abs() < 0.01);
    }

    #[test]
    fn test_map_state_zoom_clamp_max() {
        let config = test_config();
        let mut state = MapState::new(config.clone());
        state.zoom = config.max_zoom as f64;
        state.zoom_by(10.0);
        assert_eq!(state.zoom, config.max_zoom as f64);
    }

    #[test]
    fn test_map_state_toggle_braille() {
        let config = test_config();
        let mut state = MapState::new(config);
        let initial = state.use_braille;
        state.toggle_braille();
        assert_ne!(state.use_braille, initial);
        state.toggle_braille();
        assert_eq!(state.use_braille, initial);
    }

    #[test]
    fn test_map_state_overlays() {
        let config = test_config();
        let mut state = MapState::new(config);
        let id = state.add_marker(LatLon::new(52.5, 13.4), Some("Test".to_string()), 1);
        assert_eq!(state.overlays.len(), 1);
        assert!(state.remove_overlay(id));
        assert!(state.overlays.is_empty());
    }

    #[test]
    fn test_map_state_status_text() {
        let config = test_config();
        let mut state = MapState::new(config);
        state.zoom = 5.0;
        let text = state.status_text();
        assert!(text.contains("center:"));
        assert!(text.contains("zoom:"));
    }

    #[test]
    fn test_map_widget_default() {
        let _ = MapWidget::default();
    }

    #[test]
    fn test_map_state_with_builders() {
        let config = test_config();
        let state = MapState::new(config)
            .with_center(48.8, 2.3)
            .with_zoom(10.0);
        assert!((state.center_lat - 48.8).abs() < 0.01);
        assert!((state.center_lon - 2.3).abs() < 0.01);
        assert!((state.zoom - 10.0).abs() < 0.01);
    }

    #[test]
    fn test_colrow_to_ll() {
        let config = test_config();
        let mut state = MapState::new(config);
        state.renderer.set_size(160, 80);
        state.zoom = 5.0;
        state.last_area = Some((80, 20));

        let (lat, lon) = state.colrow_to_ll(40.0, 10.0);
        assert!((lat - state.center_lat).abs() < 1.0);
        assert!((lon - state.center_lon).abs() < 1.0);
    }

    #[test]
    fn test_set_zoom() {
        let config = test_config();
        let mut state = MapState::new(config.clone());
        state.set_zoom(10.0);
        assert!((state.zoom - 10.0).abs() < 0.01);

        state.set_zoom(100.0);
        assert_eq!(state.zoom, config.max_zoom as f64);

        state.set_zoom(-5.0);
        assert_eq!(state.zoom, state.min_zoom);
    }

    #[test]
    fn test_zoom_at() {
        let config = test_config();
        let mut state = MapState::new(config);
        state.renderer.set_size(160, 80);
        state.zoom = 5.0;
        state.last_area = Some((80, 20));

        let (lat_before, lon_before) = state.colrow_to_ll(20.0, 5.0);
        state.zoom_at(1.0, 20.0, 5.0);
        let (lat_after, lon_after) = state.colrow_to_ll(20.0, 5.0);

        assert!((lat_after - lat_before).abs() < 0.5);
        assert!((lon_after - lon_before).abs() < 0.5);
    }

    #[test]
    fn test_drag_lifecycle() {
        let config = test_config();
        let mut state = MapState::new(config);
        state.renderer.set_size(160, 80);
        state.zoom = 5.0;
        state.last_area = Some((80, 20));

        assert!(!state.drag_to(50.0, 15.0));
        assert!(!state.drag_end());

        let lat_before = state.center_lat;
        let lon_before = state.center_lon;

        state.drag_start(40.0, 10.0);
        assert!(state.drag_to(50.0, 10.0));

        assert!(state.center_lon != lon_before || state.center_lat != lat_before);

        assert!(state.drag_end());
        assert!(!state.drag_end());
    }

    #[test]
    fn test_focus_position() {
        let config = test_config();
        let mut state = MapState::new(config);
        state.focus_position(35.6762, 139.6503, 8.0);
        assert!((state.center_lat - 35.6762).abs() < 0.001);
        assert!((state.center_lon - 139.6503).abs() < 0.001);
        assert!((state.zoom - 8.0).abs() < 0.01);
    }

    #[test]
    fn test_focus_area() {
        let config = test_config();
        let mut state = MapState::new(config);
        state.renderer.set_size(160, 80);
        state.last_area = Some((80, 20));
        state.min_zoom = 0.0;

        state.focus_area(48.0, 2.0, 53.0, 14.0);

        assert!((state.center_lat - 50.5).abs() < 0.01);
        assert!((state.center_lon - 8.0).abs() < 0.01);
        assert!(state.zoom > 0.0);
        assert!(state.zoom <= state.config.max_zoom as f64);
    }

    #[test]
    fn test_focus_area_no_viewport() {
        let config = test_config();
        let mut state = MapState::new(config);
        let original_zoom = state.zoom;

        state.focus_area(48.0, 2.0, 53.0, 14.0);

        assert!((state.center_lat - 50.5).abs() < 0.01);
        assert_eq!(state.zoom, original_zoom);
    }

    #[test]
    fn test_set_render_size() {
        let config = test_config();
        let mut state = MapState::new(config);
        let original_width = state.renderer.width;

        state.set_render_size(80, 24);
        assert_eq!(state.renderer.width, 160);
        assert_eq!(state.renderer.height, 96);
        assert_eq!(state.last_area, Some((80, 24)));
        assert!(state.renderer.width != original_width || original_width == 160);
    }

    #[test]
    fn test_set_render_size_too_small() {
        let config = test_config();
        let mut state = MapState::new(config);
        state.set_render_size(80, 24);
        let w = state.renderer.width;

        state.set_render_size(1, 0);
        assert_eq!(state.renderer.width, w);
    }
}
