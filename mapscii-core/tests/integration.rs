//! Integration test: fetch a real tile, parse it, render, verify output.
//!
//! This test requires network access to `http://mapscii.me/`.
//! Run with: cargo test --test integration

use std::sync::Arc;

use mapscii_core::{MapConfig, MapState};

/// Fetch tiles for Berlin at zoom 4, render, and check that the canvas
/// contains some non-empty pixels (i.e. the pipeline actually works).
#[tokio::test]
async fn test_end_to_end_tile_fetch_and_render() {
    let config = Arc::new(MapConfig::default());
    let mut state = MapState::new(config);

    // Simulate a viewport: 80 cols × 24 rows (a common terminal size).
    // The renderer needs pixel dimensions: cols*2 × rows*4.
    state.renderer.set_size(160, 96);

    // Set zoom to 4 (moderate detail, just a few tiles)
    state.zoom = 4.0;

    // Load and render tiles
    state.load_visible_tiles().await;

    // Check that some pixels were drawn
    let buf = &state.renderer.canvas.buffer;
    let total_cells = buf.cell_count();
    assert!(total_cells > 0, "buffer should have cells");

    let pixel_count = buf.pixel_count();
    let fg_count = buf.foreground_count();
    let char_count = buf.char_count();

    // We should have at least *some* drawn content — water, land borders, etc.
    println!(
        "Rendered: {} total cells, {} with pixels, {} with fg color, {} with chars",
        total_cells, pixel_count, fg_count, char_count
    );

    // At zoom 4 showing Berlin, we definitely expect drawn content.
    assert!(
        pixel_count > 0 || fg_count > 0 || char_count > 0,
        "Expected some rendered content, got none. \
         pixel_count={pixel_count}, fg_count={fg_count}, char_count={char_count}"
    );
}

/// Verify that the tile source URL actually returns valid data.
#[tokio::test]
async fn test_tile_fetch_raw() {
    let config = Arc::new(MapConfig::default());
    let mut source = mapscii_core::TileSource::new(config.clone());

    let styler = Arc::new(
        mapscii_core::Styler::new(config.style_json).unwrap()
    );
    source.set_styler(styler);

    // Fetch the world overview tile (0/0/0)
    let tile = source.get_tile(mapscii_core::TileKey::new(0, 0, 0)).await;
    match &tile {
        Ok(t) => {
            println!("Tile 0/0/0: {} layers", t.layers.len());
            for (name, layer) in &t.layers {
                println!("  layer '{}': extent={}, features={}", name, layer.extent, layer.tree.size());
            }
            assert!(!t.layers.is_empty(), "Tile should have at least one layer");
        }
        Err(e) => {
            panic!("Failed to fetch tile 0/0/0: {}", e);
        }
    }
}
