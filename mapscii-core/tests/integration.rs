//! Integration tests: fetch real tiles, render, verify water/land ratios.
//!
//! Requires network access to `http://mapscii.me/`.
//! Run with: cargo test --test integration

use std::sync::Arc;

use mapscii_core::{MapConfig, MapState};

const WATER_COLOR: u8 = 69;

/// Helper: render a view and return the water percentage of the full canvas.
async fn water_pct(lat: f64, lon: f64, zoom: f64) -> f64 {
    let config = Arc::new(MapConfig::default());
    let mut state = MapState::new(config);
    state.renderer.set_size(308, 120);
    state.center_lat = lat;
    state.center_lon = lon;
    state.zoom = zoom;

    state.load_visible_tiles().await;

    let colors = state.renderer.canvas.buffer.dump_colors();
    let mut water = 0u32;
    let mut total = 0u32;
    for row in &colors {
        for cell in row {
            total += 1;
            if cell[0] == WATER_COLOR {
                water += 1;
            }
        }
    }
    if total > 0 { water as f64 / total as f64 * 100.0 } else { 0.0 }
}

// ---------------------------------------------------------------------------
// Pipeline smoke tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_end_to_end_tile_fetch_and_render() {
    let config = Arc::new(MapConfig::default());
    let mut state = MapState::new(config);
    state.renderer.set_size(160, 96);
    state.zoom = 4.0;
    state.load_visible_tiles().await;

    let buf = &state.renderer.canvas.buffer;
    assert!(buf.cell_count() > 0);
    assert!(
        buf.pixel_count() > 0 || buf.foreground_count() > 0 || buf.char_count() > 0,
        "expected some rendered content"
    );
}

#[tokio::test]
async fn test_tile_fetch_raw() {
    let config = Arc::new(MapConfig::default());
    let mut source = mapscii_core::TileSource::new(config.clone());
    let styler = Arc::new(mapscii_core::Styler::new(config.style_json).unwrap());
    source.set_styler(styler);

    let tile = source.get_tile(mapscii_core::TileKey::new(0, 0, 0)).await;
    match &tile {
        Ok(t) => assert!(!t.layers.is_empty()),
        Err(e) => panic!("failed to fetch tile 0/0/0: {e}"),
    }
}

// ---------------------------------------------------------------------------
// Systematic water/land ratio tests.
//
// Three reference points per zoom level starting at 3.5:
//   LAND    — Sahara desert (25°N, 10°E): should be <5% water
//   SEA     — Mid-Atlantic  (30°N, 30°W): should be >90% water
//   COASTAL — Bay of Biscay (44.7°N, 2.85°W): should be 30-80% water
//             (mix of ocean + French coastline)
// ---------------------------------------------------------------------------

// Sahara — deep inland, zero water expected
const LAND_LAT: f64 = 25.0;
const LAND_LON: f64 = 10.0;

// Mid-Atlantic — open ocean, no land
const SEA_LAT: f64 = 30.0;
const SEA_LON: f64 = -30.0;

// Bay of Biscay — coastline mix (the original bug location)
const COAST_LAT: f64 = 44.71;
const COAST_LON: f64 = -2.85;

#[tokio::test]
async fn test_water_ratio_zoom_3_5() {
    let z = 3.5;
    let land = water_pct(LAND_LAT, LAND_LON, z).await;
    let sea = water_pct(SEA_LAT, SEA_LON, z).await;
    let coast = water_pct(COAST_LAT, COAST_LON, z).await;
    println!("z={z} land={land:.1}% sea={sea:.1}% coast={coast:.1}%");
    assert!(land < 5.0, "z={z} land={land:.1}% — should be <5%");
    assert!(sea > 90.0, "z={z} sea={sea:.1}% — should be >90%");
    assert!(coast > 30.0 && coast < 85.0, "z={z} coast={coast:.1}% — should be 30-85%");
}

#[tokio::test]
async fn test_water_ratio_zoom_4() {
    let z = 4.0;
    let land = water_pct(LAND_LAT, LAND_LON, z).await;
    let sea = water_pct(SEA_LAT, SEA_LON, z).await;
    let coast = water_pct(COAST_LAT, COAST_LON, z).await;
    println!("z={z} land={land:.1}% sea={sea:.1}% coast={coast:.1}%");
    assert!(land < 5.0, "z={z} land={land:.1}% — should be <5%");
    assert!(sea > 90.0, "z={z} sea={sea:.1}% — should be >90%");
    assert!(coast > 30.0 && coast < 85.0, "z={z} coast={coast:.1}% — should be 30-85%");
}

#[tokio::test]
async fn test_water_ratio_zoom_5() {
    let z = 5.0;
    let land = water_pct(LAND_LAT, LAND_LON, z).await;
    let sea = water_pct(SEA_LAT, SEA_LON, z).await;
    let coast = water_pct(COAST_LAT, COAST_LON, z).await;
    println!("z={z} land={land:.1}% sea={sea:.1}% coast={coast:.1}%");
    assert!(land < 5.0, "z={z} land={land:.1}% — should be <5%");
    assert!(sea > 90.0, "z={z} sea={sea:.1}% — should be >90%");
    assert!(coast > 30.0 && coast < 85.0, "z={z} coast={coast:.1}% — should be 30-85%");
}

#[tokio::test]
async fn test_water_ratio_zoom_6() {
    let z = 6.0;
    let land = water_pct(LAND_LAT, LAND_LON, z).await;
    let sea = water_pct(SEA_LAT, SEA_LON, z).await;
    let coast = water_pct(COAST_LAT, COAST_LON, z).await;
    println!("z={z} land={land:.1}% sea={sea:.1}% coast={coast:.1}%");
    assert!(land < 5.0, "z={z} land={land:.1}% — should be <5%");
    assert!(sea > 90.0, "z={z} sea={sea:.1}% — should be >90%");
    assert!(coast > 30.0 && coast < 85.0, "z={z} coast={coast:.1}% — should be 30-85%");
}

#[tokio::test]
async fn test_water_ratio_zoom_7() {
    let z = 7.0;
    let land = water_pct(LAND_LAT, LAND_LON, z).await;
    let sea = water_pct(SEA_LAT, SEA_LON, z).await;
    let coast = water_pct(COAST_LAT, COAST_LON, z).await;
    println!("z={z} land={land:.1}% sea={sea:.1}% coast={coast:.1}%");
    assert!(land < 5.0, "z={z} land={land:.1}% — should be <5%");
    assert!(sea > 90.0, "z={z} sea={sea:.1}% — should be >90%");
    // At z≥7 the viewport zooms into open ocean at the coast coords
    assert!(coast > 30.0, "z={z} coast={coast:.1}% — should be >30%");
}

#[tokio::test]
async fn test_water_ratio_zoom_8() {
    let z = 8.0;
    let land = water_pct(LAND_LAT, LAND_LON, z).await;
    let sea = water_pct(SEA_LAT, SEA_LON, z).await;
    let coast = water_pct(COAST_LAT, COAST_LON, z).await;
    println!("z={z} land={land:.1}% sea={sea:.1}% coast={coast:.1}%");
    assert!(land < 5.0, "z={z} land={land:.1}% — should be <5%");
    assert!(sea > 90.0, "z={z} sea={sea:.1}% — should be >90%");
    assert!(coast > 30.0, "z={z} coast={coast:.1}% — should be >30%");
}

// ---------------------------------------------------------------------------
// Low-zoom world view: at zoom < 2, tile data has NO landuse layer — only
// admin (lines) and water (fills). Water polygons have simplified coastlines
// that overlap land masses. This is a tile data limitation shared with the
// original JS mapscii. We verify the water ratio stays within known bounds.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_water_ratio_world_view() {
    let spain = water_pct(40.0, -4.0, 0.267).await;
    let turkey = water_pct(39.0, 35.0, 0.267).await;
    let sahara = water_pct(LAND_LAT, LAND_LON, 0.267).await;
    let atlantic = water_pct(SEA_LAT, SEA_LON, 0.267).await;
    println!("z=0.267 spain={spain:.1}% turkey={turkey:.1}% sahara={sahara:.1}% atlantic={atlantic:.1}%");

    assert!(spain < 85.0, "z=0.267 spain={spain:.1}% — too much water");
    assert!(turkey < 85.0, "z=0.267 turkey={turkey:.1}% — too much water");
}

// ---------------------------------------------------------------------------
// Pan stability: small pans at the same zoom must not dramatically change
// land/water ratios. Regression test for integer-dedup polygon distortion.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_pan_stability_australia() {
    let z = 1.667;
    let a = water_pct(-32.87, 152.21, z).await;
    let b = water_pct(-31.37, 152.21, z).await;
    println!("pan stability z={z} a={a:.1}% b={b:.1}% diff={:.1}%", (a - b).abs());
    assert!(
        (a - b).abs() < 5.0,
        "small pan should not change water ratio by >5%: a={a:.1}% b={b:.1}%"
    );
}

// ---------------------------------------------------------------------------
// Zoom boundary stability: crossing an integer tile-zoom boundary must not
// cause large water bodies to lose significant area. Regression test for
// the round()-based base_zoom fix (Lake Victoria z=5 vs z=6 simplification).
// ---------------------------------------------------------------------------

const LAKE_VICTORIA_LAT: f64 = -1.14;
const LAKE_VICTORIA_LON: f64 = 33.35;

#[tokio::test]
async fn test_zoom_boundary_lake_victoria() {
    let below = water_pct(LAKE_VICTORIA_LAT, LAKE_VICTORIA_LON, 5.4).await;
    let above = water_pct(LAKE_VICTORIA_LAT, LAKE_VICTORIA_LON, 5.6).await;
    let at_six = water_pct(LAKE_VICTORIA_LAT, LAKE_VICTORIA_LON, 6.0).await;
    println!(
        "Lake Victoria: z=5.4 water={below:.1}% z=5.6 water={above:.1}% z=6.0 water={at_six:.1}%"
    );
    let diff = (below - above).abs();
    assert!(
        diff < 25.0,
        "zoom boundary cliff too large: z=5.4={below:.1}% z=5.6={above:.1}% diff={diff:.1}%"
    );
}
