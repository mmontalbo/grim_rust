use std::{
    collections::HashSet,
    fs::File,
    io::{self, BufRead, BufReader},
    path::PathBuf,
    time::Duration,
};

use anyhow::{Context, Result};
use clap::Parser;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
    Frame, Terminal,
};
use serde_json::Value as JsonValue;

#[derive(Parser, Debug)]
#[command(author, version, about = "Minimal TUI for grim telemetry logs")]
struct Args {
    /// Path to a telemetry log file (use - for stdin)
    path: String,
}

struct Field {
    key: String,
    value: String,
}

struct LogEntry {
    seq_display: String,
    seq_min: u64,
    seq_max: u64,
    event: String,
    fields: Vec<Field>,
    hidden_by: Option<usize>,
    summary: String,
    display: String,
}

impl LogEntry {
    fn is_composite(&self) -> bool {
        matches!(
            self.event.as_str(),
            "registered_global" | "registered_constant" | "set_table_entry"
        )
    }

    fn compute_summary(&self) -> String {
        if self.event == "set_table_entry" {
            if let Some(text) = set_table_entry_summary(self) {
                return text;
            }
        }
        let mut parts = Vec::new();
        for field in self.fields.iter() {
            if field.key == "event" || field.key == "seq" {
                continue;
            }
            parts.push(format!("{}={}", field.key, field.value));
            if parts.len() >= 4 {
                break;
            }
        }
        parts.join(" ")
    }

    fn rebuild_display(&mut self) {
        self.display = render_display_line(self);
    }
}

struct CompositeSpan {
    id: usize,
    seq_min: u64,
    seq_max: u64,
}

struct App {
    entries: Vec<LogEntry>,
    collapse: bool,
    visible_indices: Vec<usize>,
    selected: usize,
}

impl App {
    fn new(entries: Vec<LogEntry>) -> Self {
        let mut app = Self {
            entries,
            collapse: true,
            visible_indices: Vec::new(),
            selected: 0,
        };
        build_composites(&mut app.entries);
        rebuild_display_lines(&mut app.entries);
        app.rebuild_visible();
        app
    }

    fn toggle_collapse(&mut self) {
        self.collapse = !self.collapse;
        self.rebuild_visible();
    }

    fn move_selection(&mut self, delta: isize) {
        if self.visible_indices.is_empty() {
            return;
        }
        let len = self.visible_indices.len() as isize;
        let current = self.selected as isize;
        let mut next = current + delta;
        if next < 0 {
            next = 0;
        } else if next >= len {
            next = len - 1;
        }
        self.selected = next as usize;
    }

    fn move_to_start(&mut self) {
        if !self.visible_indices.is_empty() {
            self.selected = 0;
        }
    }

    fn move_to_end(&mut self) {
        if !self.visible_indices.is_empty() {
            self.selected = self.visible_indices.len() - 1;
        }
    }

    fn selected_entry(&self) -> Option<&LogEntry> {
        self.visible_indices
            .get(self.selected)
            .and_then(|idx| self.entries.get(*idx))
    }

    fn rebuild_visible(&mut self) {
        let previous = self.selected_entry().map(|entry| entry.seq_min);
        self.visible_indices.clear();
        for (idx, entry) in self.entries.iter().enumerate() {
            if self.collapse && entry.hidden_by.is_some() {
                continue;
            }
            self.visible_indices.push(idx);
        }
        if self.visible_indices.is_empty() {
            self.selected = 0;
            return;
        }
        if let Some(target_seq) = previous {
            if let Some(pos) = self
                .visible_indices
                .iter()
                .position(|idx| self.entries[*idx].seq_min == target_seq)
            {
                self.selected = pos;
                return;
            }
        }
        if self.selected >= self.visible_indices.len() {
            self.selected = self.visible_indices.len() - 1;
        }
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    let entries = load_entries(&args.path)?;
    let mut terminal = setup_terminal()?;
    let result = run_app(&mut terminal, App::new(entries));
    cleanup_terminal(&mut terminal)?;
    result
}

fn run_app(terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>, mut app: App) -> Result<()> {
    let mut needs_redraw = true;
    loop {
        if needs_redraw {
            terminal.draw(|f| render(f, &app))?;
            needs_redraw = false;
        }
        if event::poll(Duration::from_millis(250))? {
            match event::read()? {
                Event::Key(key) => {
                    if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                        continue;
                    }
                    match key.code {
                        KeyCode::Char('q') => break,
                        KeyCode::Char('c') => app.toggle_collapse(),
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

fn render(frame: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
        .split(frame.size());

    let items: Vec<ListItem> = app
        .visible_indices
        .iter()
        .map(|idx| ListItem::new(app.entries[*idx].display.as_str()))
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .title(Line::from(vec![
                    Span::raw("Trace "),
                    Span::raw(if app.collapse { "[collapsed]" } else { "[raw]" }),
                    Span::raw(" (q to quit, c to toggle)"),
                ]))
                .borders(Borders::ALL),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));

    let mut list_state = ListState::default();
    list_state.select(Some(app.selected));
    frame.render_stateful_widget(list, chunks[0], &mut list_state);

    let detail = app
        .selected_entry()
        .map(detail_lines)
        .unwrap_or_else(|| Line::raw("no selection"));

    let detail_widget = Paragraph::new(detail)
        .block(Block::default().title("Details").borders(Borders::ALL))
        .wrap(Wrap { trim: false });

    frame.render_widget(detail_widget, chunks[1]);
}

fn rebuild_display_lines(entries: &mut [LogEntry]) {
    for entry in entries.iter_mut() {
        entry.rebuild_display();
    }
}

fn render_display_line(entry: &LogEntry) -> String {
    let marker = if entry.is_composite() { "[C]" } else { "   " };
    let mut text = format!(
        "{} {:>10} {:<28} {}",
        marker, entry.seq_display, entry.event, entry.summary
    );
    if let Some(hider) = entry.hidden_by {
        text.push_str(&format!(" (covered by #{})", hider + 1));
    }
    text
}

fn detail_lines(entry: &LogEntry) -> Line<'static> {
    let mut parts = Vec::new();
    parts.push(format!("seq={} ", entry.seq_display));
    parts.push(format!("event={} ", entry.event));
    if entry.seq_min != entry.seq_max {
        parts.push(format!("range={:06}-{:06} ", entry.seq_min, entry.seq_max));
    }
    for field in entry.fields.iter() {
        if field.key == "seq" || field.key == "event" {
            continue;
        }
        parts.push(format!("{}={} ", field.key, field.value));
    }
    Line::from(parts.join(""))
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

fn load_entries(path: &str) -> Result<Vec<LogEntry>> {
    let reader: Box<dyn BufRead> = if path == "-" {
        Box::new(BufReader::new(io::stdin()))
    } else {
        let file = File::open(PathBuf::from(path)).with_context(|| format!("open {path}"))?;
        Box::new(BufReader::new(file))
    };

    let mut entries = Vec::new();
    for line in reader.lines() {
        let raw_line = line?;
        if let Some(entry) = parse_line(&raw_line) {
            entries.push(entry);
        }
    }
    Ok(entries)
}

fn parse_line(raw: &str) -> Option<LogEntry> {
    let without_metadata = raw
        .split(" | ")
        .next()
        .map(str::to_string)
        .unwrap_or_else(|| raw.to_string());

    let content = if let Some(rest) = without_metadata.strip_prefix('[') {
        if let Some(idx) = rest.find("] ") {
            rest[idx + 2..].to_string()
        } else {
            without_metadata
        }
    } else {
        without_metadata
    };

    let tokens = split_fields(&content);
    if tokens.is_empty() {
        return None;
    }

    let mut fields = Vec::new();
    let mut seq_display = None;
    let mut event = None;

    for token in tokens {
        if let Some((key, value)) = token.split_once('=') {
            let key = key.to_string();
            let value = value.to_string();
            if key == "seq" {
                seq_display = Some(value.clone());
            } else if key == "event" {
                event = Some(value.clone());
            }
            fields.push(Field { key, value });
        }
    }

    let seq_display = seq_display?;
    let event = event?;
    let (seq_min, seq_max) = parse_seq_range(&seq_display);

    let mut entry = LogEntry {
        seq_display,
        seq_min,
        seq_max,
        event,
        fields,
        hidden_by: None,
        summary: String::new(),
        display: String::new(),
    };
    entry.summary = entry.compute_summary();
    Some(entry)
}

fn parse_seq_range(text: &str) -> (u64, u64) {
    if let Some((min, max)) = text.split_once('-') {
        let seq_min = min.parse::<u64>().unwrap_or(0);
        let seq_max = max.parse::<u64>().unwrap_or(seq_min);
        (seq_min, seq_max)
    } else {
        let seq = text.parse::<u64>().unwrap_or(0);
        (seq, seq)
    }
}

fn split_fields(line: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '"' => {
                in_quotes = !in_quotes;
            }
            '\\' => {
                if let Some('"') = chars.peek() {
                    current.push('"');
                    chars.next();
                } else {
                    current.push('\\');
                }
            }
            ' ' if !in_quotes => {
                if !current.is_empty() {
                    tokens.push(current.clone());
                    current.clear();
                }
            }
            _ => current.push(ch),
        }
    }

    if !current.is_empty() {
        tokens.push(current);
    }

    tokens
}

fn build_composites(entries: &mut [LogEntry]) -> Vec<CompositeSpan> {
    let mut composites = Vec::new();
    for entry in entries.iter() {
        if entry.is_composite() {
            let span = CompositeSpan {
                id: composites.len(),
                seq_min: entry.seq_min,
                seq_max: entry.seq_max,
            };
            composites.push(span);
        }
    }

    let hide_targets: HashSet<&str> = [
        "lua_setglobal",
        "lua_pushcclosure",
        "lua_pushnumber",
        "lua_pushnil",
        "lua_pushstring",
        "lua_pushlstring",
        "lua_pushusertag",
        "lua_pushobject",
        "lua_settable",
        "lua_rawsettable",
    ]
    .into_iter()
    .collect();

    for comp in composites.iter() {
        for entry in entries.iter_mut() {
            if entry.is_composite() || !hide_targets.contains(entry.event.as_str()) {
                continue;
            }
            if entry.seq_min >= comp.seq_min && entry.seq_max <= comp.seq_max {
                entry.hidden_by.get_or_insert(comp.id);
            }
        }
    }

    composites
}

fn set_table_entry_summary(entry: &LogEntry) -> Option<String> {
    let table = field_value(entry, "table_handle_label")
        .or_else(|| field_value(entry, "table_handle"))
        .map(|s| s.to_string())
        .unwrap_or_else(|| "table".to_string());
    let key = value_preview(entry, "key");
    let value = value_preview(entry, "value");
    Some(format!("{table}[{key}] = {value}"))
}

fn field_value<'a>(entry: &'a LogEntry, key: &str) -> Option<&'a str> {
    entry
        .fields
        .iter()
        .find(|f| f.key == key)
        .map(|f| f.value.as_str())
}

fn render_preview(raw: &str) -> Option<String> {
    let json: JsonValue = serde_json::from_str(raw).ok()?;
    let obj = json.as_object()?;
    if let Some(preview) = obj.get("preview").and_then(|v| v.as_str()) {
        return Some(preview.to_string());
    }
    if let Some(value) = obj.get("value").and_then(|v| v.as_str()) {
        return Some(value.to_string());
    }
    if let Some(kind) = obj.get("kind").and_then(|v| v.as_str()) {
        return Some(kind.to_string());
    }
    None
}

fn value_preview(entry: &LogEntry, key: &str) -> String {
    field_value(entry, key)
        .and_then(render_preview)
        .or_else(|| field_value(entry, key).map(|s| s.to_string()))
        .unwrap_or_else(|| "?".to_string())
}
