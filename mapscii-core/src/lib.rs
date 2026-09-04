//! # mapscii-core
//!
//! A Braille & ASCII map renderer for terminals, usable as an embeddable
//! ratatui `StatefulWidget` with support for custom overlay layers.
//!
//! ## Quick Start
//!
//! ```ignore
//! use std::sync::Arc;
//! use mapscii_core::{MapConfig, MapState, MapWidget, LatLon};
//!
//! // 1. Create config
//! let config = Arc::new(MapConfig::default());
//!
//! // 2. Create state (holds center, zoom, tile cache, overlays)
//! let mut state = MapState::new(config);
//!
//! // 3. Optionally add overlays
//! state.add_marker(LatLon::new(52.52, 13.405), Some("Berlin".into()), 1);
//!
//! // 4. In your async runtime, load tiles
//! // state.load_visible_tiles().await;
//!
//! // 5. In your ratatui render loop
//! // frame.render_stateful_widget(MapWidget::new(), area, &mut state);
//! ```

// Public modules
pub mod braille_buffer;
pub mod canvas;
pub mod config;
pub mod label_buffer;
pub mod overlay;
pub mod renderer;
pub mod styler;
pub mod tile;
pub mod tile_source;
pub mod utils;
pub mod widget;

// Re-export primary public API types
pub use config::MapConfig;
pub use overlay::{LatLon, Overlay, OverlayCollection, OverlayId, OverlayKind};
pub use widget::{MapState, MapWidget};

// Re-export commonly used types from submodules
pub use braille_buffer::ColorIdx;
pub use renderer::Renderer;
pub use styler::Styler;
pub use tile_source::{TileKey, TileSource};
