//! Mapbox Vector Tile (MVT) parsing, feature extraction, and spatial indexing.
//!
//! Loads a protobuf-encoded vector tile, optionally gzip-decompressed,
//! parses layers and features, applies styling, builds an RTree for fast
//! spatial queries, and produces a set of `TileLayer`s ready for rendering.
//!
//! Translated from the original mapscii `Tile.js`.

use std::collections::HashMap;
use std::io::Read;

use flate2::read::GzDecoder;
use prost::Message;
use rstar::{RTree, RTreeObject, AABB};
use serde_json::Value;

use crate::braille_buffer::{rgb_to_xterm, ColorIdx};
use crate::config::MapConfig;
use crate::styler::Styler;
use crate::utils::hex2rgb;

// ---------------------------------------------------------------------------
// Protobuf MVT structures (manually defined — the spec is tiny and stable)
// ---------------------------------------------------------------------------

/// A single MVT tile.
#[derive(Clone, PartialEq, Message)]
pub struct MvtTile {
    #[prost(message, repeated, tag = "3")]
    pub layers: Vec<MvtLayer>,
}

/// A layer inside an MVT tile.
#[derive(Clone, PartialEq, Message)]
pub struct MvtLayer {
    #[prost(string, required, tag = "1")]
    pub name: String,
    #[prost(message, repeated, tag = "2")]
    pub features: Vec<MvtFeature>,
    #[prost(string, repeated, tag = "3")]
    pub keys: Vec<String>,
    #[prost(message, repeated, tag = "4")]
    pub values: Vec<MvtValue>,
    #[prost(uint32, optional, tag = "5", default = 4096)]
    pub extent: Option<u32>,
    #[prost(uint32, optional, tag = "15")]
    pub version: Option<u32>,
}

/// A feature inside an MVT layer.
#[derive(Clone, PartialEq, Message)]
pub struct MvtFeature {
    #[prost(uint64, optional, tag = "1")]
    pub id: Option<u64>,
    #[prost(uint32, repeated, packed = "true", tag = "2")]
    pub tags: Vec<u32>,
    #[prost(enumeration = "MvtGeomType", optional, tag = "3")]
    pub r#type: Option<i32>,
    #[prost(uint32, repeated, packed = "true", tag = "4")]
    pub geometry: Vec<u32>,
}

/// Protobuf value union — only one field is set.
#[derive(Clone, PartialEq, Message)]
pub struct MvtValue {
    #[prost(string, optional, tag = "1")]
    pub string_value: Option<String>,
    #[prost(float, optional, tag = "2")]
    pub float_value: Option<f32>,
    #[prost(double, optional, tag = "3")]
    pub double_value: Option<f64>,
    #[prost(int64, optional, tag = "4")]
    pub int_value: Option<i64>,
    #[prost(uint64, optional, tag = "5")]
    pub uint_value: Option<u64>,
    #[prost(sint64, optional, tag = "6")]
    pub sint_value: Option<i64>,
    #[prost(bool, optional, tag = "7")]
    pub bool_value: Option<bool>,
}

/// Geometry type enum from the MVT spec.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, prost::Enumeration)]
#[repr(i32)]
pub enum MvtGeomType {
    Unknown = 0,
    Point = 1,
    LineString = 2,
    Polygon = 3,
}

impl MvtValue {
    /// Convert to a `serde_json::Value` for uniform handling.
    pub fn to_json_value(&self) -> Value {
        if let Some(ref s) = self.string_value {
            Value::String(s.clone())
        } else if let Some(f) = self.float_value {
            serde_json::Number::from_f64(f as f64)
                .map(Value::Number)
                .unwrap_or(Value::Null)
        } else if let Some(d) = self.double_value {
            serde_json::Number::from_f64(d)
                .map(Value::Number)
                .unwrap_or(Value::Null)
        } else if let Some(i) = self.int_value {
            Value::Number(i.into())
        } else if let Some(u) = self.uint_value {
            Value::Number(u.into())
        } else if let Some(s) = self.sint_value {
            Value::Number(s.into())
        } else if let Some(b) = self.bool_value {
            Value::Bool(b)
        } else {
            Value::Null
        }
    }
}

// ---------------------------------------------------------------------------
// Geometry decoding
// ---------------------------------------------------------------------------

/// A point in tile-local coordinates.
#[derive(Debug, Clone, Copy)]
pub struct TilePoint {
    pub x: i32,
    pub y: i32,
}

/// Decode MVT geometry commands into rings of points.
///
/// Each ring is a `Vec<TilePoint>`. For Point features there is one ring per
/// point; for LineString and Polygon features the rings follow the usual
/// winding rules.
fn decode_geometry(geometry: &[u32]) -> Vec<Vec<TilePoint>> {
    let mut rings: Vec<Vec<TilePoint>> = Vec::new();
    let mut current_ring: Vec<TilePoint> = Vec::new();
    let mut cx: i32 = 0;
    let mut cy: i32 = 0;
    let mut i = 0;

    while i < geometry.len() {
        let cmd_int = geometry[i];
        let cmd_id = cmd_int & 0x7;
        let count = (cmd_int >> 3) as usize;
        i += 1;

        match cmd_id {
            // MoveTo
            1 => {
                for _ in 0..count {
                    if i + 1 >= geometry.len() {
                        break;
                    }
                    let dx = zigzag_decode(geometry[i]);
                    let dy = zigzag_decode(geometry[i + 1]);
                    i += 2;
                    cx += dx;
                    cy += dy;

                    // Start a new ring
                    if !current_ring.is_empty() {
                        rings.push(std::mem::take(&mut current_ring));
                    }
                    current_ring.push(TilePoint { x: cx, y: cy });
                }
            }
            // LineTo
            2 => {
                for _ in 0..count {
                    if i + 1 >= geometry.len() {
                        break;
                    }
                    let dx = zigzag_decode(geometry[i]);
                    let dy = zigzag_decode(geometry[i + 1]);
                    i += 2;
                    cx += dx;
                    cy += dy;
                    current_ring.push(TilePoint { x: cx, y: cy });
                }
            }
            // ClosePath
            7 => {
                if let Some(first) = current_ring.first().copied() {
                    current_ring.push(first);
                }
                rings.push(std::mem::take(&mut current_ring));
            }
            _ => {
                // Unknown command — skip
            }
        }
    }

    if !current_ring.is_empty() {
        rings.push(current_ring);
    }
    rings
}

/// Zigzag-decode a protobuf uint32 to a signed i32.
#[inline]
fn zigzag_decode(n: u32) -> i32 {
    ((n >> 1) as i32) ^ (-((n & 1) as i32))
}

// ---------------------------------------------------------------------------
// Feature type strings
// ---------------------------------------------------------------------------

fn geom_type_str(t: i32) -> &'static str {
    match t {
        1 => "Point",
        2 => "LineString",
        3 => "Polygon",
        _ => "Unknown",
    }
}

// ---------------------------------------------------------------------------
// Tile feature (styled, with bounding box for RTree)
// ---------------------------------------------------------------------------

/// A styled feature ready for rendering, stored in a per-layer RTree.
#[derive(Debug, Clone)]
pub struct TileFeature {
    /// Source layer name.
    pub layer: String,
    /// Style layer type: "line", "fill", "symbol", "background".
    pub style_type: String,
    /// Style id (for minzoom/maxzoom checks during rendering).
    pub style_id: String,
    /// Minimum zoom for this feature's style.
    pub minzoom: Option<f64>,
    /// Maximum zoom for this feature's style.
    pub maxzoom: Option<f64>,
    /// xterm-256 color index.
    pub color: ColorIdx,
    /// Line width (for line features).
    pub line_width: f64,
    /// Text label (for symbol features).
    pub label: Option<String>,
    /// Sort order (localrank or scalerank).
    pub sort: Option<i64>,
    /// The geometry points (for line/symbol: single ring; for fill: multiple rings).
    pub points: Vec<Vec<TilePoint>>,
    /// Bounding box for spatial indexing.
    pub min_x: i32,
    pub max_x: i32,
    pub min_y: i32,
    pub max_y: i32,
}

impl RTreeObject for TileFeature {
    type Envelope = AABB<[i32; 2]>;

    fn envelope(&self) -> Self::Envelope {
        AABB::from_corners([self.min_x, self.min_y], [self.max_x, self.max_y])
    }
}

/// A parsed layer from a tile: extent + spatial index of features.
pub struct TileLayer {
    /// Tile extent (usually 4096).
    pub extent: u32,
    /// Spatial index of styled features.
    pub tree: RTree<TileFeature>,
}

// ---------------------------------------------------------------------------
// Tile: top-level parsed tile container
// ---------------------------------------------------------------------------

/// A fully parsed, styled, and spatially indexed vector tile.
pub struct Tile {
    /// Layers keyed by source-layer name.
    pub layers: HashMap<String, TileLayer>,
}

impl Tile {
    /// Parse a (potentially gzipped) MVT protobuf buffer and produce a styled,
    /// spatially indexed `Tile`.
    ///
    /// If `styler` is `Some`, features that don't match any style rule are
    /// discarded. If `styler` is `None`, all features are kept with default
    /// styling.
    pub fn load(
        buffer: &[u8],
        styler: Option<&Styler>,
        config: &MapConfig,
    ) -> Result<Self, TileError> {
        let data = Self::unzip_if_needed(buffer)?;
        let mvt = MvtTile::decode(data.as_slice())
            .map_err(|e| TileError::ProtobufDecode(e.to_string()))?;

        let layers = Self::load_layers(&mvt, styler, config);
        Ok(Self { layers })
    }

    /// Decompress if gzipped, otherwise return as-is.
    fn unzip_if_needed(buffer: &[u8]) -> Result<Vec<u8>, TileError> {
        if buffer.len() >= 2 && buffer[0] == 0x1f && buffer[1] == 0x8b {
            let mut decoder = GzDecoder::new(buffer);
            let mut decompressed = Vec::new();
            decoder
                .read_to_end(&mut decompressed)
                .map_err(|e| TileError::Decompress(e.to_string()))?;
            Ok(decompressed)
        } else {
            Ok(buffer.to_vec())
        }
    }

    /// Parse all layers, apply styling, build spatial indices.
    fn load_layers(
        mvt: &MvtTile,
        styler: Option<&Styler>,
        config: &MapConfig,
    ) -> HashMap<String, TileLayer> {
        let mut result: HashMap<String, TileLayer> = HashMap::new();
        let mut color_cache: HashMap<String, ColorIdx> = HashMap::new();

        for mvt_layer in &mvt.layers {
            let name = &mvt_layer.name;
            let extent = mvt_layer.extent.unwrap_or(4096);
            let mut features: Vec<TileFeature> = Vec::new();

            for mvt_feature in &mvt_layer.features {
                let geom_type = mvt_feature.r#type.unwrap_or(0);
                let type_str = geom_type_str(geom_type);

                // Decode properties from tags
                let properties = decode_properties(mvt_feature, mvt_layer);

                // Get style
                let style = if let Some(s) = styler {
                    match s.get_style_for(name, &properties, type_str) {
                        Some(st) => st,
                        None => continue, // Skip features that don't match any style
                    }
                } else {
                    continue; // No styler means no rendering
                };

                // Resolve color
                let color_hex = style
                    .paint
                    .get("line-color")
                    .or_else(|| style.paint.get("fill-color"))
                    .or_else(|| style.paint.get("text-color"));

                let color = if let Some(color_val) = color_hex {
                    let color_str = resolve_color_string(color_val);
                    *color_cache.entry(color_str.clone()).or_insert_with(|| {
                        let [r, g, b] = hex2rgb(&color_str);
                        rgb_to_xterm(r, g, b)
                    })
                } else {
                    0
                };

                // Resolve line width
                let line_width = resolve_line_width(style.paint.get("line-width"));

                // Decode geometry
                let geometries = decode_geometry(&mvt_feature.geometry);

                // Resolve label for symbol layers
                let label = if style.layer_type == "symbol" {
                    let lang_key = format!("name_{}", config.language);
                    properties
                        .get(&lang_key)
                        .and_then(|v| v.as_str().map(String::from))
                        .or_else(|| {
                            properties
                                .get("name_en")
                                .and_then(|v| v.as_str().map(String::from))
                        })
                        .or_else(|| {
                            properties
                                .get("name")
                                .and_then(|v| v.as_str().map(String::from))
                        })
                        .or_else(|| {
                            properties
                                .get("house_num")
                                .and_then(|v| v.as_str().map(String::from))
                        })
                } else {
                    None
                };

                let sort = properties
                    .get("localrank")
                    .or_else(|| properties.get("scalerank"))
                    .and_then(|v| v.as_i64());

                // For fill features, all rings go into one feature (like the JS original).
                // For line/symbol features, each ring is a separate feature.
                if style.layer_type == "fill" {
                    if let Some(feature) = make_feature_fill(
                        name,
                        style,
                        color,
                        line_width,
                        label.clone(),
                        sort,
                        &geometries,
                    ) {
                        features.push(feature);
                    }
                } else {
                    for ring in &geometries {
                        if let Some(feature) = make_feature_single(
                            name,
                            style,
                            color,
                            line_width,
                            label.clone(),
                            sort,
                            ring,
                        ) {
                            features.push(feature);
                        }
                    }
                }
            }

            let tree = RTree::bulk_load(features);
            result.insert(name.clone(), TileLayer { extent, tree });
        }

        result
    }
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// Decode feature properties from interleaved tag indices.
fn decode_properties(feature: &MvtFeature, layer: &MvtLayer) -> HashMap<String, Value> {
    let mut props = HashMap::new();
    let tags = &feature.tags;

    let mut i = 0;
    while i + 1 < tags.len() {
        let key_idx = tags[i] as usize;
        let val_idx = tags[i + 1] as usize;

        if key_idx < layer.keys.len() && val_idx < layer.values.len() {
            let key = layer.keys[key_idx].clone();
            let val = layer.values[val_idx].to_json_value();
            props.insert(key, val);
        }
        i += 2;
    }
    props
}

/// Resolve a paint value to a color string, handling zoom-stop objects.
fn resolve_color_string(val: &Value) -> String {
    match val {
        Value::String(s) => s.clone(),
        Value::Object(obj) => {
            // Handle zoom stops: { "stops": [[z, color], ...] }
            if let Some(stops) = obj.get("stops").and_then(|v| v.as_array()) {
                if let Some(first) = stops.first() {
                    if let Some(arr) = first.as_array() {
                        if let Some(color) = arr.get(1).and_then(|v| v.as_str()) {
                            return color.to_string();
                        }
                    }
                }
            }
            "#ff0000".to_string() // fallback: red
        }
        _ => "#ff0000".to_string(),
    }
}

/// Resolve line-width, handling zoom-stop objects.
fn resolve_line_width(val: Option<&Value>) -> f64 {
    match val {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(1.0),
        Some(Value::Object(obj)) => {
            if let Some(stops) = obj.get("stops").and_then(|v| v.as_array()) {
                if let Some(first) = stops.first() {
                    if let Some(arr) = first.as_array() {
                        if let Some(w) = arr.get(1).and_then(|v| v.as_f64()) {
                            return w;
                        }
                    }
                }
            }
            1.0
        }
        _ => 1.0,
    }
}

/// Compute bounding box for a set of points (used for a single ring).
fn compute_bbox(points: &[TilePoint]) -> (i32, i32, i32, i32) {
    let mut min_x = i32::MAX;
    let mut max_x = i32::MIN;
    let mut min_y = i32::MAX;
    let mut max_y = i32::MIN;

    for p in points {
        if p.x < min_x {
            min_x = p.x;
        }
        if p.x > max_x {
            max_x = p.x;
        }
        if p.y < min_y {
            min_y = p.y;
        }
        if p.y > max_y {
            max_y = p.y;
        }
    }
    (min_x, max_x, min_y, max_y)
}

/// Create a fill feature (multiple rings, bbox from outer ring).
fn make_feature_fill(
    layer_name: &str,
    style: &crate::styler::StyleLayer,
    color: ColorIdx,
    line_width: f64,
    label: Option<String>,
    sort: Option<i64>,
    geometries: &[Vec<TilePoint>],
) -> Option<TileFeature> {
    if geometries.is_empty() {
        return None;
    }
    // Bbox from outer ring (first ring)
    let (min_x, max_x, min_y, max_y) = compute_bbox(&geometries[0]);

    Some(TileFeature {
        layer: layer_name.to_string(),
        style_type: style.layer_type.clone(),
        style_id: style.id.clone(),
        minzoom: style.minzoom,
        maxzoom: style.maxzoom,
        color,
        line_width,
        label,
        sort,
        points: geometries.to_vec(),
        min_x,
        max_x,
        min_y,
        max_y,
    })
}

/// Create a line/symbol feature (single ring).
fn make_feature_single(
    layer_name: &str,
    style: &crate::styler::StyleLayer,
    color: ColorIdx,
    line_width: f64,
    label: Option<String>,
    sort: Option<i64>,
    ring: &[TilePoint],
) -> Option<TileFeature> {
    if ring.is_empty() {
        return None;
    }
    let (min_x, max_x, min_y, max_y) = compute_bbox(ring);

    Some(TileFeature {
        layer: layer_name.to_string(),
        style_type: style.layer_type.clone(),
        style_id: style.id.clone(),
        minzoom: style.minzoom,
        maxzoom: style.maxzoom,
        color,
        line_width,
        label,
        sort,
        points: vec![ring.to_vec()],
        min_x,
        max_x,
        min_y,
        max_y,
    })
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors that can occur when loading a tile.
#[derive(Debug, thiserror::Error)]
pub enum TileError {
    #[error("failed to decompress tile: {0}")]
    Decompress(String),
    #[error("failed to decode protobuf: {0}")]
    ProtobufDecode(String),
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zigzag_decode() {
        assert_eq!(zigzag_decode(0), 0);
        assert_eq!(zigzag_decode(1), -1);
        assert_eq!(zigzag_decode(2), 1);
        assert_eq!(zigzag_decode(3), -2);
        assert_eq!(zigzag_decode(4), 2);
    }

    #[test]
    fn test_decode_geometry_point() {
        // MoveTo(1, 1) — command: (1 << 3) | 1 = 9, params: zigzag(25)=2, zigzag(17)=2
        //                         actually let's use simpler values
        // MoveTo count=1: cmd = (1 << 3) | 1 = 9
        // dx=2 -> zigzag=4, dy=3 -> zigzag=6
        let geometry = vec![9, 4, 6];
        let rings = decode_geometry(&geometry);
        assert_eq!(rings.len(), 1);
        assert_eq!(rings[0].len(), 1);
        assert_eq!(rings[0][0].x, 2);
        assert_eq!(rings[0][0].y, 3);
    }

    #[test]
    fn test_decode_geometry_linestring() {
        // MoveTo(1): cmd = 9, dx=0->0, dy=0->0
        // LineTo(2): cmd = (2 << 3) | 2 = 18
        //   dx=10->20, dy=0->0
        //   dx=0->0,   dy=10->20
        let geometry = vec![9, 0, 0, 18, 20, 0, 0, 20];
        let rings = decode_geometry(&geometry);
        assert_eq!(rings.len(), 1);
        assert_eq!(rings[0].len(), 3);
        assert_eq!(rings[0][0].x, 0);
        assert_eq!(rings[0][0].y, 0);
        assert_eq!(rings[0][1].x, 10);
        assert_eq!(rings[0][1].y, 0);
        assert_eq!(rings[0][2].x, 10);
        assert_eq!(rings[0][2].y, 10);
    }

    #[test]
    fn test_decode_geometry_polygon() {
        // MoveTo(1): cmd = 9, dx=0->0, dy=0->0
        // LineTo(3): cmd = (3 << 3) | 2 = 26
        //   dx=10->20, dy=0->0
        //   dx=0->0,   dy=10->20
        //   dx=-10->19, dy=0->0
        // ClosePath: cmd = (1 << 3) | 7 = 15
        let geometry = vec![9, 0, 0, 26, 20, 0, 0, 20, 19, 0, 15];
        let rings = decode_geometry(&geometry);
        assert_eq!(rings.len(), 1);
        // 4 points: start, 3 lineto, close (adds start again) = 5
        assert_eq!(rings[0].len(), 5);
        // First and last should be the same (closed)
        assert_eq!(rings[0][0].x, rings[0][4].x);
        assert_eq!(rings[0][0].y, rings[0][4].y);
    }

    #[test]
    fn test_mvt_value_to_json() {
        let v = MvtValue {
            string_value: Some("hello".to_string()),
            ..Default::default()
        };
        assert_eq!(v.to_json_value(), Value::String("hello".to_string()));

        let v2 = MvtValue {
            int_value: Some(42),
            ..Default::default()
        };
        assert_eq!(v2.to_json_value(), Value::Number(42.into()));

        let v3 = MvtValue {
            bool_value: Some(true),
            ..Default::default()
        };
        assert_eq!(v3.to_json_value(), Value::Bool(true));
    }

    #[test]
    fn test_resolve_color_string_direct() {
        let val = Value::String("#ff0000".to_string());
        assert_eq!(resolve_color_string(&val), "#ff0000");
    }

    #[test]
    fn test_resolve_color_string_stops() {
        let val = serde_json::json!({
            "stops": [[5, "#00ff00"], [10, "#ff0000"]]
        });
        assert_eq!(resolve_color_string(&val), "#00ff00");
    }

    #[test]
    fn test_resolve_line_width() {
        assert_eq!(resolve_line_width(None), 1.0);
        assert_eq!(resolve_line_width(Some(&serde_json::json!(2.5))), 2.5);
        assert_eq!(
            resolve_line_width(Some(&serde_json::json!({"stops": [[5, 1.0], [10, 3.0]]}))),
            1.0
        );
    }

    #[test]
    fn test_compute_bbox() {
        let points = vec![
            TilePoint { x: 5, y: 10 },
            TilePoint { x: 20, y: 3 },
            TilePoint { x: -1, y: 15 },
        ];
        let (min_x, max_x, min_y, max_y) = compute_bbox(&points);
        assert_eq!(min_x, -1);
        assert_eq!(max_x, 20);
        assert_eq!(min_y, 3);
        assert_eq!(max_y, 15);
    }

    #[test]
    fn test_geom_type_str() {
        assert_eq!(geom_type_str(1), "Point");
        assert_eq!(geom_type_str(2), "LineString");
        assert_eq!(geom_type_str(3), "Polygon");
        assert_eq!(geom_type_str(0), "Unknown");
    }
}
