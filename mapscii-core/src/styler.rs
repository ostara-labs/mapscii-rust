//! Mapbox GL style parser and filter engine.
//!
//! Parses a JSON style file (like `dark.json`), replaces `@constant`
//! references, and compiles layer filters into an enum-based filter tree
//! for fast feature matching at render time.
//!
//! Translated from the original mapscii `Styler.js`.

use serde_json::Value;
use std::collections::HashMap;

use crate::braille_buffer::{rgb_to_xterm, ColorIdx};
use crate::utils::hex2rgb;

/// A compiled Mapbox GL filter expression.
#[derive(Debug, Clone)]
pub enum Filter {
    /// Always matches.
    All,
    /// All sub-filters must match (logical AND).
    AllOf(Vec<Filter>),
    /// At least one sub-filter must match (logical OR).
    AnyOf(Vec<Filter>),
    /// No sub-filter may match (logical NOR).
    NoneOf(Vec<Filter>),
    /// `property == value`
    Eq(String, Value),
    /// `property != value`
    Ne(String, Value),
    /// `property in [values...]`
    In(String, Vec<Value>),
    /// `property not in [values...]`
    NotIn(String, Vec<Value>),
    /// `property exists`
    Has(String),
    /// `property does not exist`
    NotHas(String),
    /// `property > value`
    Gt(String, f64),
    /// `property >= value`
    Gte(String, f64),
    /// `property < value`
    Lt(String, f64),
    /// `property <= value`
    Lte(String, f64),
}

impl Filter {
    /// Test whether a feature's properties match this filter.
    ///
    /// `feature_type` is the MVT geometry type string: "Point", "LineString", or "Polygon".
    pub fn matches(&self, properties: &HashMap<String, Value>, feature_type: &str) -> bool {
        match self {
            Filter::All => true,

            Filter::AllOf(subs) => subs.iter().all(|f| f.matches(properties, feature_type)),

            Filter::AnyOf(subs) => subs.iter().any(|f| f.matches(properties, feature_type)),

            Filter::NoneOf(subs) => !subs.iter().any(|f| f.matches(properties, feature_type)),

            Filter::Eq(key, val) => {
                if key == "$type" {
                    if let Some(s) = val.as_str() {
                        return feature_type == s;
                    }
                }
                properties.get(key).is_some_and(|v| values_equal(v, val))
            }

            Filter::Ne(key, val) => {
                if key == "$type" {
                    if let Some(s) = val.as_str() {
                        return feature_type != s;
                    }
                }
                properties.get(key).is_none_or(|v| !values_equal(v, val))
            }

            Filter::In(key, vals) => {
                if key == "$type" {
                    return vals.iter().any(|v| v.as_str() == Some(feature_type));
                }
                properties
                    .get(key)
                    .is_some_and(|prop| vals.iter().any(|v| values_equal(prop, v)))
            }

            Filter::NotIn(key, vals) => {
                if key == "$type" {
                    return !vals.iter().any(|v| v.as_str() == Some(feature_type));
                }
                properties
                    .get(key)
                    .is_none_or(|prop| !vals.iter().any(|v| values_equal(prop, v)))
            }

            Filter::Has(key) => properties.contains_key(key),

            Filter::NotHas(key) => !properties.contains_key(key),

            Filter::Gt(key, val) => properties
                .get(key)
                .and_then(as_f64)
                .is_some_and(|v| v > *val),
            Filter::Gte(key, val) => properties
                .get(key)
                .and_then(as_f64)
                .is_some_and(|v| v >= *val),
            Filter::Lt(key, val) => properties
                .get(key)
                .and_then(as_f64)
                .is_some_and(|v| v < *val),
            Filter::Lte(key, val) => properties
                .get(key)
                .and_then(as_f64)
                .is_some_and(|v| v <= *val),
        }
    }
}

/// A parsed style layer.
#[derive(Debug, Clone)]
pub struct StyleLayer {
    pub id: String,
    pub layer_type: String,
    pub source_layer: Option<String>,
    pub filter: Filter,
    /// Raw JSON filter (kept for `ref` inheritance).
    raw_filter: Option<Value>,
    pub paint: HashMap<String, Value>,
    pub minzoom: Option<f64>,
    pub maxzoom: Option<f64>,
}

impl StyleLayer {
    /// Get the resolved color for a paint property (e.g. "line-color", "fill-color").
    pub fn color(&self, property: &str) -> Option<ColorIdx> {
        let val = self.paint.get(property)?;
        let hex = val.as_str()?;
        let [r, g, b] = hex2rgb(hex);
        Some(rgb_to_xterm(r, g, b))
    }

    /// Get the text color.
    pub fn text_color(&self) -> Option<ColorIdx> {
        self.color("text-color")
    }

    /// Get the line color (tries "line-color", falls back to "fill-color").
    pub fn line_color(&self) -> Option<ColorIdx> {
        self.color("line-color")
            .or_else(|| self.color("fill-color"))
    }

    /// Get the fill color.
    pub fn fill_color(&self) -> Option<ColorIdx> {
        self.color("fill-color")
    }

    /// Get the background color.
    pub fn background_color(&self) -> Option<ColorIdx> {
        self.color("background-color")
    }
}

/// The compiled style: a set of layers indexed by source-layer name and by id.
pub struct Styler {
    pub name: String,
    /// Layers grouped by source-layer name, in draw order.
    pub style_by_layer: HashMap<String, Vec<StyleLayer>>,
    /// Layers indexed by id.
    pub style_by_id: HashMap<String, StyleLayer>,
    /// All layers in original draw order.
    pub layers: Vec<StyleLayer>,
}

impl Styler {
    /// Parse and compile a Mapbox GL style JSON string.
    pub fn new(json: &str) -> Result<Self, serde_json::Error> {
        let mut root: Value = serde_json::from_str(json)?;

        let name = root
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        // Replace @constant references
        if let Some(constants) = root.get("constants").cloned() {
            if let Some(layers) = root.get_mut("layers") {
                Self::replace_constants(&constants, layers);
            }
        }

        let raw_layers = root
            .get("layers")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let mut style_by_layer: HashMap<String, Vec<StyleLayer>> = HashMap::new();
        let mut style_by_id: HashMap<String, StyleLayer> = HashMap::new();
        let mut layers: Vec<StyleLayer> = Vec::new();

        for raw in &raw_layers {
            let id = raw
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let layer_type = raw
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            // Resolve `ref` fields
            let mut source_layer = raw
                .get("source-layer")
                .and_then(|v| v.as_str())
                .map(String::from);
            let mut filter_val = raw.get("filter").cloned();
            let mut minzoom = raw.get("minzoom").and_then(as_f64);
            let mut maxzoom = raw.get("maxzoom").and_then(as_f64);

            if let Some(ref_id) = raw.get("ref").and_then(|v| v.as_str()) {
                if let Some(ref_layer) = style_by_id.get(ref_id) {
                    if source_layer.is_none() {
                        source_layer = ref_layer.source_layer.clone();
                    }
                    if filter_val.is_none() {
                        filter_val = ref_layer.raw_filter.clone();
                    }
                    if minzoom.is_none() {
                        minzoom = ref_layer.minzoom;
                    }
                    if maxzoom.is_none() {
                        maxzoom = ref_layer.maxzoom;
                    }
                }
            }

            let filter = Self::compile_filter(filter_val.as_ref());

            let paint = raw
                .get("paint")
                .and_then(|v| v.as_object())
                .map(|obj| obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                .unwrap_or_default();

            let layer = StyleLayer {
                id: id.clone(),
                layer_type,
                source_layer: source_layer.clone(),
                filter,
                raw_filter: filter_val.clone(),
                paint,
                minzoom,
                maxzoom,
            };

            if let Some(ref sl) = source_layer {
                style_by_layer
                    .entry(sl.clone())
                    .or_default()
                    .push(layer.clone());
            }
            style_by_id.insert(id, layer.clone());
            layers.push(layer);
        }

        Ok(Self {
            name,
            style_by_layer,
            style_by_id,
            layers,
        })
    }

    /// Get the matching style for a given source layer and feature.
    pub fn get_style_for(
        &self,
        layer: &str,
        properties: &HashMap<String, Value>,
        feature_type: &str,
    ) -> Option<&StyleLayer> {
        let styles = self.style_by_layer.get(layer)?;
        styles
            .iter()
            .find(|s| s.filter.matches(properties, feature_type))
    }

    /// Get all matching styles for a given source layer and feature.
    pub fn get_all_styles_for(
        &self,
        layer: &str,
        properties: &HashMap<String, Value>,
        feature_type: &str,
    ) -> Vec<&StyleLayer> {
        self.style_by_layer
            .get(layer)
            .map(|styles| {
                styles
                    .iter()
                    .filter(|s| s.filter.matches(properties, feature_type))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Recursively replace `@constant` references in the style JSON tree.
    fn replace_constants(constants: &Value, tree: &mut Value) {
        match tree {
            Value::Array(arr) => {
                for item in arr.iter_mut() {
                    Self::replace_constants(constants, item);
                }
            }
            Value::Object(obj) => {
                for (_, val) in obj.iter_mut() {
                    Self::replace_constants(constants, val);
                }
            }
            Value::String(s) if s.starts_with('@') => {
                if let Some(replacement) = constants.get(s.as_str()) {
                    *tree = replacement.clone();
                }
            }
            _ => {}
        }
    }

    /// Compile a JSON filter expression into a `Filter` enum tree.
    fn compile_filter(filter: Option<&Value>) -> Filter {
        let filter = match filter {
            Some(Value::Array(arr)) if !arr.is_empty() => arr,
            _ => return Filter::All,
        };

        let op = match filter[0].as_str() {
            Some(s) => s,
            None => return Filter::All,
        };

        match op {
            "all" => {
                let subs: Vec<Filter> = filter[1..]
                    .iter()
                    .map(|v| Self::compile_filter(Some(v)))
                    .collect();
                Filter::AllOf(subs)
            }
            "any" => {
                let subs: Vec<Filter> = filter[1..]
                    .iter()
                    .map(|v| Self::compile_filter(Some(v)))
                    .collect();
                Filter::AnyOf(subs)
            }
            "none" => {
                let subs: Vec<Filter> = filter[1..]
                    .iter()
                    .map(|v| Self::compile_filter(Some(v)))
                    .collect();
                Filter::NoneOf(subs)
            }
            "==" => {
                let key = filter
                    .get(1)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let val = filter.get(2).cloned().unwrap_or(Value::Null);
                Filter::Eq(key, val)
            }
            "!=" => {
                let key = filter
                    .get(1)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let val = filter.get(2).cloned().unwrap_or(Value::Null);
                Filter::Ne(key, val)
            }
            "in" => {
                let key = filter
                    .get(1)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let vals: Vec<Value> = filter[2..].to_vec();
                Filter::In(key, vals)
            }
            "!in" => {
                let key = filter
                    .get(1)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let vals: Vec<Value> = filter[2..].to_vec();
                Filter::NotIn(key, vals)
            }
            "has" => {
                let key = filter
                    .get(1)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                Filter::Has(key)
            }
            "!has" => {
                let key = filter
                    .get(1)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                Filter::NotHas(key)
            }
            ">" => {
                let key = filter
                    .get(1)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let val = filter.get(2).and_then(as_f64).unwrap_or(0.0);
                Filter::Gt(key, val)
            }
            ">=" => {
                let key = filter
                    .get(1)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let val = filter.get(2).and_then(as_f64).unwrap_or(0.0);
                Filter::Gte(key, val)
            }
            "<" => {
                let key = filter
                    .get(1)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let val = filter.get(2).and_then(as_f64).unwrap_or(0.0);
                Filter::Lt(key, val)
            }
            "<=" => {
                let key = filter
                    .get(1)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let val = filter.get(2).and_then(as_f64).unwrap_or(0.0);
                Filter::Lte(key, val)
            }
            _ => Filter::All,
        }
    }
}

/// Compare two `serde_json::Value`s for equality, handling number type mismatches.
fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(na), Value::Number(nb)) => {
            let fa = na.as_f64().unwrap_or(f64::NAN);
            let fb = nb.as_f64().unwrap_or(f64::NAN);
            (fa - fb).abs() < f64::EPSILON
        }
        _ => a == b,
    }
}

/// Extract a number from a JSON value.
fn as_f64(v: &Value) -> Option<f64> {
    v.as_f64()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_style_json() -> &'static str {
        include_str!("../../styles/dark.json")
    }

    #[test]
    fn test_parse_dark_style() {
        let styler = Styler::new(test_style_json()).unwrap();
        assert_eq!(styler.name, "dark");
        assert!(!styler.layers.is_empty());
    }

    #[test]
    fn test_constant_replacement() {
        let styler = Styler::new(test_style_json()).unwrap();
        // The "water" layer should have fill-color resolved from "@water" to "#5f87ff"
        let water = styler.style_by_id.get("water").unwrap();
        let fill = water.paint.get("fill-color").unwrap();
        assert_eq!(fill.as_str().unwrap(), "#5f87ff");
    }

    #[test]
    fn test_background_layer() {
        let styler = Styler::new(test_style_json()).unwrap();
        let bg = styler.style_by_id.get("background").unwrap();
        assert_eq!(bg.layer_type, "background");
        let color = bg.background_color();
        assert!(color.is_some());
    }

    #[test]
    fn test_filter_eq() {
        let styler = Styler::new(test_style_json()).unwrap();
        let park = styler.style_by_id.get("landuse_park").unwrap();

        let mut props = HashMap::new();
        props.insert("class".to_string(), Value::String("park".to_string()));

        assert!(park.filter.matches(&props, "Polygon"));

        let mut props2 = HashMap::new();
        props2.insert("class".to_string(), Value::String("wood".to_string()));
        assert!(!park.filter.matches(&props2, "Polygon"));
    }

    #[test]
    fn test_filter_all_of() {
        let styler = Styler::new(test_style_json()).unwrap();
        let admin = styler.style_by_id.get("admin_level_2").unwrap();

        let mut props = HashMap::new();
        props.insert("admin_level".to_string(), Value::from(2));
        props.insert("disputed".to_string(), Value::from(0));
        props.insert("maritime".to_string(), Value::from(0));

        assert!(admin.filter.matches(&props, "LineString"));
    }

    #[test]
    fn test_filter_in() {
        let styler = Styler::new(test_style_json()).unwrap();
        let stream = styler.style_by_id.get("waterway_stream_canal").unwrap();

        let mut props = HashMap::new();
        props.insert("class".to_string(), Value::String("stream".to_string()));
        assert!(stream.filter.matches(&props, "LineString"));

        let mut props2 = HashMap::new();
        props2.insert("class".to_string(), Value::String("river".to_string()));
        assert!(!stream.filter.matches(&props2, "LineString"));
    }

    #[test]
    fn test_get_style_for() {
        let styler = Styler::new(test_style_json()).unwrap();

        let mut props = HashMap::new();
        props.insert("class".to_string(), Value::String("park".to_string()));

        let style = styler.get_style_for("landuse", &props, "Polygon");
        assert!(style.is_some());
        assert_eq!(style.unwrap().id, "landuse_park");
    }

    #[test]
    fn test_layer_color() {
        let styler = Styler::new(test_style_json()).unwrap();
        let water = styler.style_by_id.get("water").unwrap();
        let color = water.fill_color();
        assert!(color.is_some());
        // #5f87ff -> some xterm-256 index
        assert!(color.unwrap() > 0);
    }
}
