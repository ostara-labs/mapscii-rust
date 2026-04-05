//! Compile-time verification of README code examples.
//! Run with: cargo test --test readme_examples

use std::sync::Arc;
use mapscii_core::{MapConfig, MapState, MapWidget, LatLon};

/// Verify the minimal example compiles.
#[tokio::test]
async fn readme_minimal_example() {
    let config = Arc::new(MapConfig::default());
    let mut state = MapState::new(config);
    state.renderer.set_size(160, 96);
    state.load_visible_tiles().await;
    let _ = MapWidget::new();
}

/// Verify config builder chain compiles.
#[test]
fn readme_config_builder() {
    let _config = Arc::new(
        MapConfig::default()
            .with_center(35.6762, 139.6503)
            .with_zoom(6.0)
            .with_max_zoom(18)
            .with_zoom_step(0.5)
            .with_language("ja")
            .with_braille(true)
            .with_tile_persistence(true)
            .with_source("http://mapscii.me/")
    );
}

/// Verify overlay API compiles.
#[test]
fn readme_overlays() {
    let config = Arc::new(MapConfig::default());
    let mut state = MapState::new(config);

    // Markers
    let id = state.add_marker(LatLon::new(48.8566, 2.3522), Some("Paris".into()), 1);
    state.add_marker(LatLon::new(51.5074, -0.1278), None, 9);

    // Lines
    let route = vec![
        LatLon::new(52.5200, 13.4050),
        LatLon::new(50.1109, 8.6821),
        LatLon::new(48.1351, 11.5820),
    ];
    let _line_id = state.add_line(route, 4, 1.0);

    // Areas
    let polygon = vec![
        LatLon::new(52.0, 13.0),
        LatLon::new(52.5, 13.0),
        LatLon::new(52.5, 13.5),
        LatLon::new(52.0, 13.5),
    ];
    let _area_id = state.add_area(polygon, 3);

    // Remove
    state.remove_overlay(id);
    state.clear_overlays();
}

/// Verify GeoJSON API compiles.
#[test]
fn readme_geojson() {
    let config = Arc::new(MapConfig::default());
    let mut state = MapState::new(config);

    let geojson = r#"{
        "type": "FeatureCollection",
        "features": [
            {
                "type": "Feature",
                "geometry": {
                    "type": "LineString",
                    "coordinates": [[13.4, 52.5], [2.3, 48.8], [-3.7, 40.4]]
                },
                "properties": {}
            }
        ]
    }"#;

    let result = state.add_geojson_layer(geojson, 5);
    assert!(result.is_ok());
}

/// Verify navigation API compiles.
#[test]
fn readme_navigation() {
    let config = Arc::new(MapConfig::default());
    let mut state = MapState::new(config);

    state.set_center(40.7128, -74.0060);
    state.move_by(1.0, 0.0);
    state.zoom_by(0.2);
    state.zoom_by(-0.2);
    state.toggle_braille();
    let _ = state.status_text();
}

/// Verify MapState builder compiles.
#[test]
fn readme_state_builder() {
    let config = Arc::new(MapConfig::default());
    let _state = MapState::new(config)
        .with_center(40.7128, -74.0060)
        .with_zoom(5.0);
}
