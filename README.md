# mapscii-rust

A Braille & ASCII world map renderer for the terminal, written in Rust. A feature-complete rewrite of [rastapasta/mapscii](https://github.com/rastapasta/mapscii).

Renders Mapbox vector tiles as Unicode Braille characters with xterm-256 colors directly in your terminal. Ships as both a **standalone CLI application** and an **embeddable ratatui widget** you can drop into any TUI app.

```
   ⡀⠀⢀⣀⣤⣤⣤⣤⣤⣤⣤⣀⡀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀
  ⢀⣤⣶⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣷⣶⣄⡀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀
 ⣴⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣦⡀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀
  The Whole World In Your Console
```

## Features

- Renders OpenStreetMap vector tiles as Braille or ASCII art
- Keyboard and mouse navigation (pan, zoom, drag)
- xterm-256 color support with Mapbox GL dark style
- Custom overlay layers: markers, polylines, filled areas, and GeoJSON
- Embeddable as a ratatui `StatefulWidget` for TUI applications
- Async tile fetching with LRU caching and disk persistence
- Optional MBTiles support for offline use

## Table of Contents

- [CLI Installation](#cli-installation)
- [CLI Usage](#cli-usage)
- [Library Installation](#library-installation)
- [Library Usage](#library-usage)
- [Overlays](#overlays)
- [GeoJSON Layers](#geojson-layers)
- [Configuration](#configuration)
- [Architecture](#architecture)
- [License](#license)

---

## CLI Installation

### From source

```sh
git clone https://github.com/your-username/mapscii-rust
cd mapscii-rust
cargo install --path mapscii
```

### Run directly (without installing)

```sh
cargo run
```

## CLI Usage

```sh
mapscii [OPTIONS]
```

### Controls

| Key | Action |
|---|---|
| Arrow keys / `h` `j` `k` `l` | Pan the map |
| `a` | Zoom in |
| `z` / `y` | Zoom out |
| Scroll wheel | Zoom in/out |
| Mouse drag | Pan the map |
| `c` | Toggle Braille / ASCII mode |
| `q` / `Esc` | Quit |

### Options

| Flag | Description | Default |
|---|---|---|
| `--lat <LAT>` | Initial latitude | `52.51298` (Berlin) |
| `--lon <LON>` | Initial longitude | `13.42012` (Berlin) |
| `-z`, `--zoom <ZOOM>` | Initial zoom level | Auto-detect |
| `--source <URL>` | Tile server URL (must end with `/`) | `http://mapscii.me/` |
| `--language <LANG>` | Label language (`en`, `de`, `fr`, `ja`, ...) | `en` |
| `--ascii` | Use ASCII blocks instead of Braille | Off |
| `--no-cache` | Disable tile persistence to disk | Off |
| `--max-zoom <N>` | Maximum zoom level | `18` |

### Examples

```sh
# Start centered on Tokyo
mapscii --lat 35.6762 --lon 139.6503 --zoom 5

# Use ASCII rendering with German labels
mapscii --ascii --language de

# Use a custom tile server
mapscii --source "https://your-tile-server.com/"
```

---

## Library Installation

Add `mapscii-core` to your `Cargo.toml`:

```toml
[dependencies]
mapscii-core = { git = "https://github.com/your-username/mapscii-rust" }

# You'll also need ratatui and tokio
ratatui = { version = "0.30", features = ["crossterm"] }
crossterm = "0.29"
tokio = { version = "1", features = ["full"] }
```

For offline MBTiles support:

```toml
[dependencies]
mapscii-core = { git = "https://github.com/your-username/mapscii-rust", features = ["mbtiles"] }
```

## Library Usage

`mapscii-core` provides a ratatui `StatefulWidget`. The two main types are:

- **`MapState`** -- holds all mutable state (center, zoom, tile cache, overlays, renderer)
- **`MapWidget`** -- lightweight widget that implements `StatefulWidget`

### Minimal example

```rust
use std::sync::Arc;
use mapscii_core::{MapConfig, MapState, MapWidget};

#[tokio::main]
async fn main() {
    let config = Arc::new(MapConfig::default());
    let mut state = MapState::new(config);

    // Load tiles (async -- call before rendering)
    state.load_visible_tiles().await;

    // In your ratatui render loop:
    // frame.render_stateful_widget(MapWidget::new(), area, &mut state);
}
```

### Full ratatui integration

```rust
use std::io;
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::Terminal;
use ratatui::layout::Rect;

use mapscii_core::{MapConfig, MapState, MapWidget, LatLon};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Configure
    let config = Arc::new(
        MapConfig::default()
            .with_center(40.7128, -74.0060)  // New York
            .with_zoom(5.0)
            .with_language("en")
    );

    let mut state = MapState::new(config);

    // Add a marker
    state.add_marker(LatLon::new(40.7128, -74.0060), Some("NYC".into()), 1);

    // Set up terminal
    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    let mut terminal = Terminal::new(
        ratatui::backend::CrosstermBackend::new(io::stdout())
    )?;

    // Initial tile load
    state.load_visible_tiles().await;

    loop {
        terminal.draw(|frame| {
            frame.render_stateful_widget(MapWidget::new(), frame.area(), &mut state);
        })?;

        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('q') => break,
                        KeyCode::Char('a') => {
                            state.zoom_by(0.2);
                            state.load_visible_tiles().await;
                        }
                        KeyCode::Char('z') => {
                            state.zoom_by(-0.2);
                            state.load_visible_tiles().await;
                        }
                        KeyCode::Up => {
                            state.move_by(1.0, 0.0);
                            state.load_visible_tiles().await;
                        }
                        KeyCode::Down => {
                            state.move_by(-1.0, 0.0);
                            state.load_visible_tiles().await;
                        }
                        KeyCode::Left => {
                            state.move_by(0.0, -1.0);
                            state.load_visible_tiles().await;
                        }
                        KeyCode::Right => {
                            state.move_by(0.0, 1.0);
                            state.load_visible_tiles().await;
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    // Restore terminal
    disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;
    Ok(())
}
```

### Rendering lifecycle

The widget uses a two-phase approach:

1. **Async phase** (`load_visible_tiles().await`) -- fetches tiles over HTTP, parses vector tile protobufs, applies Mapbox GL styles, draws features and overlays onto an internal pixel canvas. Call this whenever the view changes (pan, zoom, resize).

2. **Sync phase** (`frame.render_stateful_widget(...)`) -- blits the pre-rendered canvas to the ratatui buffer. This is fast and non-blocking, safe to call from a synchronous render closure.

---

## Overlays

Add custom visual elements on top of the base map.

### Markers

```rust
use mapscii_core::LatLon;

// Labeled marker (xterm-256 color index)
let id = state.add_marker(LatLon::new(48.8566, 2.3522), Some("Paris".into()), 1);

// Icon-only marker (uses default POI marker character)
state.add_marker(LatLon::new(51.5074, -0.1278), None, 9);
```

### Lines

```rust
use mapscii_core::LatLon;

let route = vec![
    LatLon::new(52.5200, 13.4050),  // Berlin
    LatLon::new(50.1109, 8.6821),   // Frankfurt
    LatLon::new(48.1351, 11.5820),  // Munich
];
let id = state.add_line(route, 4, 1.0);  // color index 4, line width 1.0
```

### Areas

```rust
use mapscii_core::LatLon;

let polygon = vec![
    LatLon::new(52.0, 13.0),
    LatLon::new(52.5, 13.0),
    LatLon::new(52.5, 13.5),
    LatLon::new(52.0, 13.5),
];
let id = state.add_area(polygon, 3);
```

### Managing overlays

```rust
// Remove a specific overlay
state.remove_overlay(id);

// Remove all overlays
state.clear_overlays();
```

---

## GeoJSON Layers

Load GeoJSON data directly. Supports `FeatureCollection`, `Feature`, and bare geometry types including `Point`, `MultiPoint`, `LineString`, `MultiLineString`, `Polygon`, `MultiPolygon`, and `GeometryCollection`.

```rust
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

let id = state.add_geojson_layer(geojson, 5)?;
```

---

## Configuration

`MapConfig` controls all rendering behavior. Use the builder methods or set fields directly before wrapping in `Arc`.

```rust
use std::sync::Arc;
use mapscii_core::MapConfig;

let config = Arc::new(
    MapConfig::default()
        .with_center(35.6762, 139.6503)       // Tokyo
        .with_zoom(6.0)                         // Initial zoom
        .with_max_zoom(18)                      // Max zoom level
        .with_zoom_step(0.5)                    // Zoom increment per step
        .with_language("ja")                    // Japanese labels
        .with_braille(true)                     // Braille rendering (default)
        .with_tile_persistence(true)            // Cache tiles to disk
        .with_source("http://mapscii.me/")      // Tile server URL
);
```

### All config fields

| Field | Type | Default | Description |
|---|---|---|---|
| `language` | `String` | `"en"` | Label language code |
| `source` | `String` | `"http://mapscii.me/"` | Tile server URL |
| `mbtiles_path` | `Option<String>` | `None` | Path to `.mbtiles` file |
| `initial_zoom` | `Option<f64>` | `None` | Initial zoom (auto if `None`) |
| `max_zoom` | `u8` | `18` | Maximum zoom level |
| `zoom_step` | `f64` | `0.2` | Zoom increment per step |
| `initial_lat` | `f64` | `52.51298` | Initial latitude |
| `initial_lon` | `f64` | `13.42012` | Initial longitude |
| `use_braille` | `bool` | `true` | Braille vs ASCII rendering |
| `persist_downloaded_tiles` | `bool` | `true` | Cache tiles to `~/.cache/mapscii/` |
| `simplify_polylines` | `bool` | `false` | Apply RDP simplification |
| `label_margin` | `u32` | `5` | Label collision margin (chars) |
| `poi_marker` | `char` | `◉` | Default POI marker character |
| `style_json` | `&'static str` | Built-in dark style | Mapbox GL style JSON |

---

## MapState API Reference

| Method | Description |
|---|---|
| `MapState::new(config)` | Create state from an `Arc<MapConfig>` |
| `.with_center(lat, lon)` | Builder: set initial center |
| `.with_zoom(zoom)` | Builder: set initial zoom |
| `.set_center(lat, lon)` | Set the center position |
| `.move_by(dlat, dlon)` | Pan by a lat/lon delta |
| `.zoom_by(step)` | Zoom in (+) or out (-) |
| `.toggle_braille()` | Toggle Braille/ASCII mode |
| `.load_visible_tiles().await` | Fetch tiles and render to canvas |
| `.add_marker(pos, label, color)` | Add a point marker overlay |
| `.add_line(points, color, width)` | Add a polyline overlay |
| `.add_area(points, color)` | Add a filled polygon overlay |
| `.add_geojson_layer(json, color)` | Add a GeoJSON layer |
| `.remove_overlay(id)` | Remove an overlay by ID |
| `.clear_overlays()` | Remove all overlays |
| `.status_text()` | Get `"center: lat, lon zoom: z"` string |
| `.colrow_to_ll(col, row)` | Convert terminal position to lat/lon |

---

## Architecture

```
mapscii-rust/
├── Cargo.toml                  # Workspace root
├── styles/
│   └── dark.json               # Mapbox GL style (bundled at compile time)
├── mapscii/                    # Binary crate (standalone CLI app)
│   ├── Cargo.toml
│   └── src/main.rs
└── mapscii-core/               # Library crate (embeddable widget)
    ├── Cargo.toml
    └── src/
        ├── lib.rs              # Public API re-exports
        ├── config.rs           # MapConfig with defaults
        ├── utils.rs            # Geo math, color conversion
        ├── braille_buffer.rs   # Pixel buffer -> Braille/ASCII + xterm-256
        ├── canvas.rs           # Drawing primitives (line, polygon, text)
        ├── label_buffer.rs     # RTree-based label collision avoidance
        ├── styler.rs           # Mapbox GL style parser + filter engine
        ├── tile.rs             # Vector tile protobuf parsing
        ├── tile_source.rs      # HTTP fetcher + LRU cache + disk cache
        ├── renderer.rs         # Tile rendering orchestration
        ├── overlay.rs          # Custom layers (markers, areas, GeoJSON)
        └── widget.rs           # ratatui StatefulWidget implementation
```

### Rendering pipeline

```
Tile Server (HTTP)
    │
    ▼
TileSource (fetch + LRU cache + disk cache)
    │
    ▼
Tile (protobuf decode + gzip decompress + spatial index)
    │
    ▼
Styler (Mapbox GL filter matching + color/width resolution)
    │
    ▼
Renderer (draw order + feature drawing + label collision)
    │
    ▼
Canvas → BrailleBuffer (pixel buffer → Braille characters + xterm-256 colors)
    │
    ▼
MapWidget (blit to ratatui Buffer)
```

---

## Credits

- Original [mapscii](https://github.com/rastapasta/mapscii) by [rastapasta](https://github.com/rastapasta)
- Tile data from [OpenStreetMap](https://www.openstreetmap.org/) contributors

## License

MIT
