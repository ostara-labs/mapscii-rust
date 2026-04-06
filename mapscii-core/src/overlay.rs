//! Custom overlay layers: markers, areas, lines, and GeoJSON.
//!
//! This module provides a simple API for adding custom visual elements on
//! top of the rendered map. Overlays are drawn after the base map tiles.
//!
//! Supports:
//! - **Markers**: point labels at a lat/lon position
//! - **Lines**: polylines connecting lat/lon positions
//! - **Areas**: filled polygons (outer ring + optional holes)
//! - **GeoJSON layers**: full GeoJSON FeatureCollection/Feature/Geometry support

use std::sync::atomic::{AtomicU64, Ordering};

use crate::braille_buffer::ColorIdx;
use crate::canvas::{Canvas, Point};
use crate::config::MapConfig;
use crate::utils;

// ---------------------------------------------------------------------------
// ID generation
// ---------------------------------------------------------------------------

static NEXT_OVERLAY_ID: AtomicU64 = AtomicU64::new(1);

/// Opaque handle for removing overlays.
pub type OverlayId = u64;

fn next_id() -> OverlayId {
    NEXT_OVERLAY_ID.fetch_add(1, Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// LatLon
// ---------------------------------------------------------------------------

/// A geographic position in WGS-84 coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LatLon {
    pub lat: f64,
    pub lon: f64,
}

impl LatLon {
    pub fn new(lat: f64, lon: f64) -> Self {
        Self { lat, lon }
    }
}

// ---------------------------------------------------------------------------
// Overlay types
// ---------------------------------------------------------------------------

/// A custom overlay element.
#[derive(Debug, Clone)]
pub struct Overlay {
    /// Unique identifier for removal.
    pub id: OverlayId,
    /// The overlay kind/geometry.
    pub kind: OverlayKind,
    /// Display color (xterm-256 index). Used for markers and lines.
    pub color: ColorIdx,
    /// Fill color for areas/polygons (xterm-256 index). `None` = no fill.
    pub fill_color: Option<ColorIdx>,
    /// Stroke/outline color for areas/polygons (xterm-256 index). `None` = no outline.
    pub stroke_color: Option<ColorIdx>,
    /// Fill opacity (0.0 = fully transparent, 1.0 = fully opaque). Default: 1.0.
    pub fill_opacity: f64,
    /// Optional text label (for markers).
    pub label: Option<String>,
    /// Line width (for lines and area outlines).
    pub line_width: f64,
}

/// The geometry kind of an overlay.
#[derive(Debug, Clone)]
pub enum OverlayKind {
    /// A single point marker.
    Marker(LatLon),
    /// A polyline connecting multiple positions.
    Line(Vec<LatLon>),
    /// A filled polygon (outer ring; holes not yet supported via simple API).
    Area(Vec<LatLon>),
    /// A full GeoJSON layer (parsed geometries).
    GeoJson(Vec<GeoJsonGeometry>),
}

/// A parsed GeoJSON geometry ready for rendering.
#[derive(Debug, Clone)]
pub enum GeoJsonGeometry {
    Point(LatLon),
    LineString(Vec<LatLon>),
    Polygon(Vec<Vec<LatLon>>), // outer ring + holes
    MultiPoint(Vec<LatLon>),
    MultiLineString(Vec<Vec<LatLon>>),
    MultiPolygon(Vec<Vec<Vec<LatLon>>>),
}

// ---------------------------------------------------------------------------
// Overlay collection
// ---------------------------------------------------------------------------

/// Manages a set of overlays.
#[derive(Debug, Clone, Default)]
pub struct OverlayCollection {
    overlays: Vec<Overlay>,
}

impl OverlayCollection {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_marker(&mut self, pos: LatLon, label: Option<String>, color: ColorIdx) -> OverlayId {
        let id = next_id();
        self.overlays.push(Overlay {
            id,
            kind: OverlayKind::Marker(pos),
            color,
            fill_color: None,
            stroke_color: None,
            fill_opacity: 1.0,
            label,
            line_width: 1.0,
        });
        id
    }

    pub fn add_line(&mut self, points: Vec<LatLon>, color: ColorIdx, line_width: f64) -> OverlayId {
        let id = next_id();
        self.overlays.push(Overlay {
            id,
            kind: OverlayKind::Line(points),
            color,
            fill_color: None,
            stroke_color: None,
            fill_opacity: 1.0,
            label: None,
            line_width,
        });
        id
    }

    /// Add a filled area with separate fill and stroke colors.
    ///
    /// - `fill_color`: interior color (`None` = no fill)
    /// - `stroke_color`: outline color (`None` = no outline)
    /// - `fill_opacity`: 0.0 (transparent) to 1.0 (opaque)
    pub fn add_area(
        &mut self,
        points: Vec<LatLon>,
        fill_color: Option<ColorIdx>,
        stroke_color: Option<ColorIdx>,
        fill_opacity: f64,
    ) -> OverlayId {
        let color = fill_color.or(stroke_color).unwrap_or(0);
        let id = next_id();
        self.overlays.push(Overlay {
            id,
            kind: OverlayKind::Area(points),
            color,
            fill_color,
            stroke_color,
            fill_opacity: fill_opacity.clamp(0.0, 1.0),
            label: None,
            line_width: 1.0,
        });
        id
    }

    pub fn add_geojson_layer(
        &mut self,
        geojson_str: &str,
        fill_color: Option<ColorIdx>,
        stroke_color: Option<ColorIdx>,
        fill_opacity: f64,
    ) -> Result<OverlayId, String> {
        let geojson: geojson::GeoJson = geojson_str
            .parse()
            .map_err(|e: geojson::Error| e.to_string())?;
        let geometries = parse_geojson(&geojson);
        if geometries.is_empty() {
            return Err("no geometries found in GeoJSON".to_string());
        }
        let color = fill_color.or(stroke_color).unwrap_or(0);
        let id = next_id();
        self.overlays.push(Overlay {
            id,
            kind: OverlayKind::GeoJson(geometries),
            color,
            fill_color,
            stroke_color,
            fill_opacity: fill_opacity.clamp(0.0, 1.0),
            label: None,
            line_width: 1.0,
        });
        Ok(id)
    }

    /// Remove an overlay by its ID. Returns `true` if found.
    pub fn remove(&mut self, id: OverlayId) -> bool {
        let len_before = self.overlays.len();
        self.overlays.retain(|o| o.id != id);
        self.overlays.len() < len_before
    }

    /// Remove all overlays.
    pub fn clear(&mut self) {
        self.overlays.clear();
    }

    /// Get all overlays (for rendering).
    pub fn iter(&self) -> impl Iterator<Item = &Overlay> {
        self.overlays.iter()
    }

    /// Number of overlays.
    pub fn len(&self) -> usize {
        self.overlays.len()
    }

    /// Whether the collection is empty.
    pub fn is_empty(&self) -> bool {
        self.overlays.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Overlay rendering
// ---------------------------------------------------------------------------

/// Render all overlays onto the canvas.
///
/// `center_lat`, `center_lon`, `zoom` define the current viewport.
/// `width` and `height` are the canvas pixel dimensions.
pub fn render_overlays(
    overlays: &OverlayCollection,
    canvas: &mut Canvas,
    center_lat: f64,
    center_lon: f64,
    zoom: f64,
    width: usize,
    height: usize,
    config: &MapConfig,
) {
    for overlay in overlays.iter() {
        match &overlay.kind {
            OverlayKind::Marker(pos) => {
                let p = ll_to_pixel(pos, center_lat, center_lon, zoom, width, height, config);
                let default_marker = config.poi_marker.to_string();
                let label = overlay.label.as_deref().unwrap_or(&default_marker);
                canvas.text(label, p.x as i32, p.y as i32, overlay.color, true);
            }
            OverlayKind::Line(positions) => {
                let points: Vec<Point> = positions
                    .iter()
                    .map(|ll| ll_to_pixel(ll, center_lat, center_lon, zoom, width, height, config))
                    .collect();
                if points.len() >= 2 {
                    canvas.polyline(&points, overlay.color, overlay.line_width);
                }
            }
            OverlayKind::Area(positions) => {
                let ring: Vec<Point> = positions
                    .iter()
                    .map(|ll| ll_to_pixel(ll, center_lat, center_lon, zoom, width, height, config))
                    .collect();
                if ring.len() >= 3 {
                    render_area_rings(canvas, &[ring], overlay);
                }
            }
            OverlayKind::GeoJson(geometries) => {
                for geom in geometries {
                    render_geojson_geometry(
                        geom, canvas, overlay, center_lat, center_lon, zoom, width, height, config,
                    );
                }
            }
        }
    }
}

fn render_area_rings(canvas: &mut Canvas, px_rings: &[Vec<Point>], overlay: &Overlay) {
    if let Some(fill_color) = overlay.fill_color {
        canvas.polygon_fill(px_rings, fill_color, overlay.fill_opacity);
    }
    if let Some(stroke_color) = overlay.stroke_color {
        canvas.polygon_stroke(px_rings, stroke_color, overlay.line_width);
    }
}

fn render_geojson_geometry(
    geom: &GeoJsonGeometry,
    canvas: &mut Canvas,
    overlay: &Overlay,
    center_lat: f64,
    center_lon: f64,
    zoom: f64,
    width: usize,
    height: usize,
    config: &MapConfig,
) {
    let to_px = |ll: &LatLon| ll_to_pixel(ll, center_lat, center_lon, zoom, width, height, config);

    match geom {
        GeoJsonGeometry::Point(pos) => {
            let p = to_px(pos);
            let marker = config.poi_marker.to_string();
            canvas.text(&marker, p.x as i32, p.y as i32, overlay.color, true);
        }
        GeoJsonGeometry::LineString(positions) => {
            let points: Vec<Point> = positions.iter().map(|ll| to_px(ll)).collect();
            if points.len() >= 2 {
                canvas.polyline(&points, overlay.color, overlay.line_width);
            }
        }
        GeoJsonGeometry::Polygon(rings) => {
            let px_rings: Vec<Vec<Point>> = rings
                .iter()
                .map(|ring| ring.iter().map(|ll| to_px(ll)).collect())
                .collect();
            if !px_rings.is_empty() && px_rings[0].len() >= 3 {
                render_area_rings(canvas, &px_rings, overlay);
            }
        }
        GeoJsonGeometry::MultiPoint(positions) => {
            for pos in positions {
                let p = to_px(pos);
                let marker = config.poi_marker.to_string();
                canvas.text(&marker, p.x as i32, p.y as i32, overlay.color, true);
            }
        }
        GeoJsonGeometry::MultiLineString(lines) => {
            for line in lines {
                let points: Vec<Point> = line.iter().map(|ll| to_px(ll)).collect();
                if points.len() >= 2 {
                    canvas.polyline(&points, overlay.color, overlay.line_width);
                }
            }
        }
        GeoJsonGeometry::MultiPolygon(polygons) => {
            for polygon_rings in polygons {
                let px_rings: Vec<Vec<Point>> = polygon_rings
                    .iter()
                    .map(|ring| ring.iter().map(|ll| to_px(ll)).collect())
                    .collect();
                if !px_rings.is_empty() && px_rings[0].len() >= 3 {
                    render_area_rings(canvas, &px_rings, overlay);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Coordinate conversion
// ---------------------------------------------------------------------------

/// Convert a LatLon to pixel coordinates in the current viewport.
fn ll_to_pixel(
    pos: &LatLon,
    center_lat: f64,
    center_lon: f64,
    zoom: f64,
    width: usize,
    height: usize,
    config: &MapConfig,
) -> Point {
    let z = utils::base_zoom(zoom, config) as f64;
    let center_tile = utils::ll2tile(center_lon, center_lat, z);
    let pos_tile = utils::ll2tile(pos.lon, pos.lat, z);
    let tile_size = utils::tilesize_at_zoom(zoom, config);

    let px = width as f64 / 2.0 + (pos_tile.x - center_tile.x) * tile_size;
    let py = height as f64 / 2.0 + (pos_tile.y - center_tile.y) * tile_size;

    Point::new(px, py)
}

// ---------------------------------------------------------------------------
// GeoJSON parsing
// ---------------------------------------------------------------------------

/// Parse a GeoJson value into a flat list of geometries.
fn parse_geojson(geojson: &geojson::GeoJson) -> Vec<GeoJsonGeometry> {
    let mut result = Vec::new();

    match geojson {
        geojson::GeoJson::FeatureCollection(fc) => {
            for feature in &fc.features {
                if let Some(ref geom) = feature.geometry {
                    result.extend(convert_geometry(&geom.value));
                }
            }
        }
        geojson::GeoJson::Feature(feature) => {
            if let Some(ref geom) = feature.geometry {
                result.extend(convert_geometry(&geom.value));
            }
        }
        geojson::GeoJson::Geometry(geom) => {
            result.extend(convert_geometry(&geom.value));
        }
    }

    result
}

/// Convert a GeoJSON geometry value into our internal types.
fn convert_geometry(value: &geojson::Value) -> Vec<GeoJsonGeometry> {
    match value {
        geojson::Value::Point(coords) => {
            if coords.len() >= 2 {
                vec![GeoJsonGeometry::Point(LatLon::new(coords[1], coords[0]))]
            } else {
                vec![]
            }
        }
        geojson::Value::MultiPoint(positions) => {
            let points: Vec<LatLon> = positions
                .iter()
                .filter(|c| c.len() >= 2)
                .map(|c| LatLon::new(c[1], c[0]))
                .collect();
            if points.is_empty() {
                vec![]
            } else {
                vec![GeoJsonGeometry::MultiPoint(points)]
            }
        }
        geojson::Value::LineString(coords) => {
            let line: Vec<LatLon> = coords
                .iter()
                .filter(|c| c.len() >= 2)
                .map(|c| LatLon::new(c[1], c[0]))
                .collect();
            if line.len() >= 2 {
                vec![GeoJsonGeometry::LineString(line)]
            } else {
                vec![]
            }
        }
        geojson::Value::MultiLineString(lines) => {
            let converted: Vec<Vec<LatLon>> = lines
                .iter()
                .map(|line| {
                    line.iter()
                        .filter(|c| c.len() >= 2)
                        .map(|c| LatLon::new(c[1], c[0]))
                        .collect()
                })
                .filter(|line: &Vec<LatLon>| line.len() >= 2)
                .collect();
            if converted.is_empty() {
                vec![]
            } else {
                vec![GeoJsonGeometry::MultiLineString(converted)]
            }
        }
        geojson::Value::Polygon(rings) => {
            let converted: Vec<Vec<LatLon>> = rings
                .iter()
                .map(|ring| {
                    ring.iter()
                        .filter(|c| c.len() >= 2)
                        .map(|c| LatLon::new(c[1], c[0]))
                        .collect()
                })
                .collect();
            if converted.is_empty() || converted[0].len() < 3 {
                vec![]
            } else {
                vec![GeoJsonGeometry::Polygon(converted)]
            }
        }
        geojson::Value::MultiPolygon(polygons) => {
            let converted: Vec<Vec<Vec<LatLon>>> = polygons
                .iter()
                .map(|polygon| {
                    polygon
                        .iter()
                        .map(|ring| {
                            ring.iter()
                                .filter(|c| c.len() >= 2)
                                .map(|c| LatLon::new(c[1], c[0]))
                                .collect()
                        })
                        .collect()
                })
                .collect();
            if converted.is_empty() {
                vec![]
            } else {
                vec![GeoJsonGeometry::MultiPolygon(converted)]
            }
        }
        geojson::Value::GeometryCollection(geoms) => {
            let mut result = Vec::new();
            for geom in geoms {
                result.extend(convert_geometry(&geom.value));
            }
            result
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_latlon() {
        let pos = LatLon::new(52.5, 13.4);
        assert_eq!(pos.lat, 52.5);
        assert_eq!(pos.lon, 13.4);
    }

    #[test]
    fn test_overlay_collection_add_marker() {
        let mut col = OverlayCollection::new();
        let id = col.add_marker(LatLon::new(52.5, 13.4), Some("Berlin".to_string()), 1);
        assert_eq!(col.len(), 1);
        assert!(id > 0);
    }

    #[test]
    fn test_overlay_collection_add_line() {
        let mut col = OverlayCollection::new();
        let points = vec![LatLon::new(52.5, 13.4), LatLon::new(48.8, 2.3)];
        let id = col.add_line(points, 2, 1.5);
        assert_eq!(col.len(), 1);
        assert!(id > 0);
    }

    #[test]
    fn test_overlay_collection_add_area() {
        let mut col = OverlayCollection::new();
        let points = vec![
            LatLon::new(52.0, 13.0),
            LatLon::new(52.5, 13.0),
            LatLon::new(52.5, 13.5),
            LatLon::new(52.0, 13.5),
        ];
        let id = col.add_area(points, Some(3), Some(1), 0.5);
        assert_eq!(col.len(), 1);
        assert!(id > 0);
    }

    #[test]
    fn test_overlay_collection_remove() {
        let mut col = OverlayCollection::new();
        let id1 = col.add_marker(LatLon::new(52.5, 13.4), None, 1);
        let _id2 = col.add_marker(LatLon::new(48.8, 2.3), None, 2);
        assert_eq!(col.len(), 2);

        assert!(col.remove(id1));
        assert_eq!(col.len(), 1);

        assert!(!col.remove(id1)); // already removed
    }

    #[test]
    fn test_overlay_collection_clear() {
        let mut col = OverlayCollection::new();
        col.add_marker(LatLon::new(52.5, 13.4), None, 1);
        col.add_marker(LatLon::new(48.8, 2.3), None, 2);
        col.clear();
        assert!(col.is_empty());
    }

    #[test]
    fn test_geojson_point() {
        let geojson_str = r#"{"type":"Point","coordinates":[13.4, 52.5]}"#;
        let mut col = OverlayCollection::new();
        let result = col.add_geojson_layer(geojson_str, Some(1), None, 1.0);
        assert!(result.is_ok());
        assert_eq!(col.len(), 1);
    }

    #[test]
    fn test_geojson_feature_collection() {
        let geojson_str = r#"{
            "type": "FeatureCollection",
            "features": [
                {
                    "type": "Feature",
                    "geometry": {
                        "type": "Point",
                        "coordinates": [13.4, 52.5]
                    },
                    "properties": {}
                },
                {
                    "type": "Feature",
                    "geometry": {
                        "type": "LineString",
                        "coordinates": [[13.4, 52.5], [2.3, 48.8]]
                    },
                    "properties": {}
                }
            ]
        }"#;
        let mut col = OverlayCollection::new();
        let result = col.add_geojson_layer(geojson_str, Some(5), None, 1.0);
        assert!(result.is_ok());
        assert_eq!(col.len(), 1);
    }

    #[test]
    fn test_geojson_invalid() {
        let mut col = OverlayCollection::new();
        let result = col.add_geojson_layer("not valid json", Some(1), None, 1.0);
        assert!(result.is_err());
    }

    #[test]
    fn test_ll_to_pixel_center() {
        // When pos == center, pixel should be at viewport center
        let config = MapConfig::default();
        let p = ll_to_pixel(&LatLon::new(52.5, 13.4), 52.5, 13.4, 5.0, 160, 80, &config);
        assert!((p.x - 80.0).abs() < 1.0);
        assert!((p.y - 40.0).abs() < 1.0);
    }

    #[test]
    fn test_unique_ids() {
        let mut col = OverlayCollection::new();
        let id1 = col.add_marker(LatLon::new(0.0, 0.0), None, 1);
        let id2 = col.add_marker(LatLon::new(0.0, 0.0), None, 1);
        assert_ne!(id1, id2);
    }
}
