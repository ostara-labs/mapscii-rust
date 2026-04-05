//! MapSCII — The Whole World In Your Console
//!
//! A Braille & ASCII map renderer for the terminal. Navigate with arrow keys,
//! zoom with `a`/`z`, toggle Braille/ASCII with `c`, quit with `q`.

use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, MouseEventKind};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Style};
use ratatui::text::Span;
use ratatui::widgets::Paragraph;
use ratatui::Terminal;

use mapscii_core::{MapConfig, MapState, MapWidget};

// ---------------------------------------------------------------------------
// CLI arguments
// ---------------------------------------------------------------------------

/// MapSCII — The Whole World In Your Console
#[derive(Parser, Debug)]
#[command(name = "mapscii", version, about)]
struct Args {
    /// Initial latitude
    #[arg(long, default_value_t = 52.51298)]
    lat: f64,

    /// Initial longitude
    #[arg(long, default_value_t = 13.42012)]
    lon: f64,

    /// Initial zoom level
    #[arg(long, short = 'z')]
    zoom: Option<f64>,

    /// Tile server URL (must end with `/`)
    #[arg(long)]
    source: Option<String>,

    /// Label language (e.g. en, de, fr, ja)
    #[arg(long, default_value = "en")]
    language: String,

    /// Use ASCII blocks instead of Braille
    #[arg(long)]
    ascii: bool,

    /// Disable tile persistence to disk
    #[arg(long)]
    no_cache: bool,

    /// Maximum zoom level
    #[arg(long, default_value_t = 18)]
    max_zoom: u8,
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Build config from CLI args
    let mut config = MapConfig::default();
    config.initial_lat = args.lat;
    config.initial_lon = args.lon;
    config.language = args.language;
    config.use_braille = !args.ascii;
    config.persist_downloaded_tiles = !args.no_cache;
    config.max_zoom = args.max_zoom;

    if let Some(zoom) = args.zoom {
        config.initial_zoom = Some(zoom);
    }
    if let Some(source) = args.source {
        config.source = source;
    }

    let config = Arc::new(config);

    // Initialize terminal
    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    crossterm::execute!(io::stdout(), crossterm::event::EnableMouseCapture)?;

    let backend = ratatui::backend::CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;

    // Create map state
    let mut state = MapState::new(config.clone());

    let result = run_app(&mut terminal, &mut state).await;

    // Restore terminal
    disable_raw_mode()?;
    crossterm::execute!(io::stdout(), crossterm::event::DisableMouseCapture)?;
    io::stdout().execute(LeaveAlternateScreen)?;

    if let Err(err) = result {
        eprintln!("Error: {err}");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// App loop
// ---------------------------------------------------------------------------

async fn run_app(
    terminal: &mut Terminal<ratatui::backend::CrosstermBackend<io::Stdout>>,
    state: &mut MapState,
) -> Result<()> {
    const DEBOUNCE_MS: u64 = 50;
    let mut pending_reload = true;
    let mut last_navigation = Instant::now();

    loop {
        if pending_reload && last_navigation.elapsed() >= Duration::from_millis(DEBOUNCE_MS) {
            state.load_visible_tiles().await;
            pending_reload = false;
        }
        
        terminal.draw(|frame| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(1), Constraint::Length(1)])
                .split(frame.area());

            let map_area = chunks[0];
            frame.render_stateful_widget(MapWidget::new(), map_area, state);

            let status = state.status_text();
            let status_bar = Paragraph::new(Span::styled(
                format!(" {status}  [arrows: move | a/z: zoom | c: braille | q: quit]"),
                Style::default().fg(Color::DarkGray),
            ));
            frame.render_widget(status_bar, chunks[1]);
        })?;

        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    let zoom_step = state.config.zoom_step;
                    let zoom = state.zoom;

                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => {
                            return Ok(());
                        }
                        KeyCode::Char('a') => {
                            state.zoom_by(zoom_step);
                            pending_reload = true;
                            last_navigation = Instant::now();
                        }
                        KeyCode::Char('z') | KeyCode::Char('y') => {
                            state.zoom_by(-zoom_step);
                            pending_reload = true;
                            last_navigation = Instant::now();
                        }
                        KeyCode::Left | KeyCode::Char('h') => {
                            state.move_by(0.0, -8.0 / 2.0_f64.powf(zoom));
                            pending_reload = true;
                            last_navigation = Instant::now();
                        }
                        KeyCode::Right | KeyCode::Char('l') => {
                            state.move_by(0.0, 8.0 / 2.0_f64.powf(zoom));
                            pending_reload = true;
                            last_navigation = Instant::now();
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            state.move_by(6.0 / 2.0_f64.powf(zoom), 0.0);
                            pending_reload = true;
                            last_navigation = Instant::now();
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            state.move_by(-6.0 / 2.0_f64.powf(zoom), 0.0);
                            pending_reload = true;
                            last_navigation = Instant::now();
                        }
                        KeyCode::Char('c') => {
                            state.toggle_braille();
                        }
                        _ => {}
                    }
                }
                Event::Mouse(mouse) => {
                    match mouse.kind {
                        MouseEventKind::ScrollUp => {
                            state.zoom_by(state.config.zoom_step);
                            pending_reload = true;
                            last_navigation = Instant::now();
                        }
                        MouseEventKind::ScrollDown => {
                            state.zoom_by(-state.config.zoom_step);
                            pending_reload = true;
                            last_navigation = Instant::now();
                        }
                        MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
                            state.drag_start(mouse.column as f64, mouse.row as f64);
                        }
                        MouseEventKind::Up(crossterm::event::MouseButton::Left) => {
                            if state.drag_end() {
                                pending_reload = true;
                                last_navigation = Instant::now();
                            }
                        }
                        MouseEventKind::Drag(crossterm::event::MouseButton::Left) => {
                            if state.drag_to(mouse.column as f64, mouse.row as f64) {
                                pending_reload = true;
                                last_navigation = Instant::now();
                            }
                        }
                        _ => {}
                    }
                }
                Event::Resize(_, _) => {
                    state.needs_redraw = true;
                    pending_reload = true;
                    last_navigation = Instant::now();
                }
                _ => {}
            }
        }
    }
}
