mod data;
mod pane;

use std::ffi::OsString;
use std::io;

use anyhow::Result;
use clap::{Parser, ValueEnum};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use grim_telemetry_common::StreamFilter;
use pane::{render_dual, render_single, DualApp, SingleApp};
use ratatui::{backend::CrosstermBackend, Terminal};

use crate::trace_tui::data::load_entries;

#[derive(Parser, Debug)]
#[command(author, version, about = "Minimal TUI for grim telemetry logs")]
struct Args {
    /// Path to one or two telemetry log files (use - for stdin)
    #[arg(num_args = 1..=2)]
    paths: Vec<String>,
    /// Which telemetry stream to show (semantic/raw/all). See grim_telemetry_common/README.md for stream details.
    #[arg(long, value_enum, default_value_t = CliStreamFilter::Semantic)]
    stream: CliStreamFilter,
    /// Optional label for the first path (defaults to file stem)
    #[arg(long)]
    left_label: Option<String>,
    /// Optional label for the second path (defaults to file stem)
    #[arg(long)]
    right_label: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum CliStreamFilter {
    Semantic,
    Raw,
    All,
}

impl From<CliStreamFilter> for StreamFilter {
    fn from(value: CliStreamFilter) -> Self {
        match value {
            CliStreamFilter::Semantic => StreamFilter::Semantic,
            CliStreamFilter::Raw => StreamFilter::Raw,
            CliStreamFilter::All => StreamFilter::All,
        }
    }
}

pub fn run_with_args(argv: &[OsString]) -> Result<()> {
    let mut args = Vec::with_capacity(argv.len() + 1);
    args.push(OsString::from("trace_tui"));
    args.extend_from_slice(argv);
    let args = Args::parse_from(args);
    run(args)
}

fn run(args: Args) -> Result<()> {
    let mut terminal = setup_terminal()?;
    let stream_filter: StreamFilter = args.stream.into();

    let result = match args.paths.as_slice() {
        [single] => {
            let title = args
                .left_label
                .clone()
                .unwrap_or_else(|| label_from_path(single, "trace"));
            let entries = load_entries(single)?;
            render_single_loop(&mut terminal, SingleApp::new(title, entries, stream_filter))
        }
        [left, right] => {
            let left_title = args
                .left_label
                .clone()
                .unwrap_or_else(|| label_from_path(left, "left"));
            let right_title = args
                .right_label
                .clone()
                .unwrap_or_else(|| label_from_path(right, "right"));
            let left_entries = load_entries(left)?;
            let right_entries = load_entries(right)?;
            render_dual_loop(
                &mut terminal,
                DualApp::new(
                    left_title,
                    left_entries,
                    right_title,
                    right_entries,
                    stream_filter,
                ),
            )
        }
        _ => unreachable!("clap enforces one or two paths"),
    };

    cleanup_terminal(&mut terminal)?;
    result
}

fn render_single_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    mut app: SingleApp,
) -> Result<()> {
    let mut needs_redraw = true;
    loop {
        if needs_redraw {
            terminal.draw(|f| render_single(f, &mut app))?;
            needs_redraw = false;
        }
        if event::poll(std::time::Duration::from_millis(250))? {
            match event::read()? {
                Event::Key(key) => {
                    if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                        continue;
                    }
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => break,
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            break
                        }
                        KeyCode::Char('s') => app.cycle_stream_filter(),
                        KeyCode::Up => app.move_selection(-1),
                        KeyCode::Down => app.move_selection(1),
                        KeyCode::PageUp => app.move_selection(-20),
                        KeyCode::PageDown => app.move_selection(20),
                        KeyCode::Home | KeyCode::Char('g') => app.move_to_start(),
                        KeyCode::End | KeyCode::Char('G') => app.move_to_end(),
                        _ => {}
                    }
                    needs_redraw = true;
                }
                Event::Resize(_, _) => needs_redraw = true,
                _ => {}
            }
        }
    }
    Ok(())
}

fn render_dual_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    mut app: DualApp,
) -> Result<()> {
    let mut needs_redraw = true;
    loop {
        if needs_redraw {
            terminal.draw(|f| render_dual(f, &mut app))?;
            needs_redraw = false;
        }
        if event::poll(std::time::Duration::from_millis(250))? {
            match event::read()? {
                Event::Key(key) => {
                    if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                        continue;
                    }
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => break,
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            break
                        }
                        KeyCode::Up => app.move_selection(-1),
                        KeyCode::Down => app.move_selection(1),
                        KeyCode::PageUp => app.move_selection(-20),
                        KeyCode::PageDown => app.move_selection(20),
                        KeyCode::Home | KeyCode::Char('g') => app.move_to_start(),
                        KeyCode::End | KeyCode::Char('G') => app.move_to_end(),
                        KeyCode::Left | KeyCode::Char('h') => app.focus_left(),
                        KeyCode::Right | KeyCode::Char('l') => app.focus_right(),
                        KeyCode::Tab => app.toggle_focus(),
                        KeyCode::Char('s') => app.cycle_stream_filter(),
                        KeyCode::Char('f') => {
                            if app.jump_first_diff() {
                                needs_redraw = true;
                                continue;
                            }
                        }
                        _ => {}
                    }
                    needs_redraw = true;
                }
                Event::Resize(_, _) => needs_redraw = true,
                _ => {}
            }
        }
    }
    Ok(())
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<std::io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

fn cleanup_terminal(terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    Ok(())
}

fn label_from_path(path: &str, fallback: &str) -> String {
    if path == "-" {
        return fallback.to_string();
    }
    std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| fallback.to_string())
}
