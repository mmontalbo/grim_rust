use std::{
    fs::File,
    io::{self, BufRead, BufReader},
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use grim_telemetry_common::{parse_seq_range, stream_kind_from_line, StreamFilter, StreamKind};
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


struct Field {
    key: String,
    value: String,
}

struct LogEntry {
    seq_display: String,
    seq_min: u64,
    seq_max: u64,
    orig_seq_display: String,
    orig_seq_min: u64,
    orig_seq_max: u64,
    event: String,
    stream: StreamKind,
    fields: Vec<Field>,
    summary: String,
    display: String,
}

impl LogEntry {
    fn is_composite(&self) -> bool {
        self.event.starts_with("semantic_")
    }

    fn compute_summary(&self) -> String {
        if matches!(
            self.event.as_str(),
            "set_table_entry" | "semantic_set_table_entry"
        ) {
            if let Some(text) = set_table_entry_summary(self) {
                return text;
            }
        }
        let mut parts = Vec::new();
        for field in self.fields.iter() {
            if field.key == "event" || field.key == "seq" || field.key == "stream" {
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

    fn display_event(&self) -> &str {
        self.event
            .strip_prefix("semantic_")
            .unwrap_or(self.event.as_str())
    }
}

struct Pane {
    title: String,
    entries: Vec<LogEntry>,
    visible_indices: Vec<usize>,
    selected: usize,
}

impl Pane {
    fn new(title: String, mut entries: Vec<LogEntry>, stream_filter: StreamFilter) -> Self {
        rebuild_display_lines(&mut entries);
        let mut pane = Self {
            title,
            entries,
            visible_indices: Vec::new(),
            selected: 0,
        };
        pane.rebuild_visible(stream_filter, None);
        pane
    }

    fn rebuild_visible(&mut self, stream_filter: StreamFilter, target_seq: Option<u64>) {
        let target_seq = target_seq.or_else(|| self.selected_seq());
        self.visible_indices.clear();
        for (idx, entry) in self.entries.iter().enumerate() {
            if !stream_filter.matches(entry.stream) {
                continue;
            }
            self.visible_indices.push(idx);
        }
        if self.visible_indices.is_empty() {
            self.selected = 0;
            return;
        }
        if let Some(seq) = target_seq {
            if let Some(pos) = self
                .visible_indices
                .iter()
                .position(|idx| self.entries[*idx].seq_min == seq)
            {
                self.selected = pos;
                return;
            }
        }
        if self.selected >= self.visible_indices.len() {
            self.selected = self.visible_indices.len() - 1;
        }
    }

    fn move_selection(&mut self, delta: isize) {
        if self.visible_indices.is_empty() {
            return;
        }
        let len = self.visible_indices.len() as isize;
        let mut next = self.selected as isize + delta;
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

    fn selected_seq(&self) -> Option<u64> {
        self.selected_entry().map(|entry| entry.orig_seq_min)
    }
}

struct SingleApp {
    pane: Pane,
    stream_filter: StreamFilter,
}

impl SingleApp {
    fn new(title: String, entries: Vec<LogEntry>, stream_filter: StreamFilter) -> Self {
        Self {
            pane: Pane::new(title, entries, stream_filter),
            stream_filter,
        }
    }

    fn move_selection(&mut self, delta: isize) {
        self.pane.move_selection(delta);
    }

    fn move_to_start(&mut self) {
        self.pane.move_to_start();
    }

    fn move_to_end(&mut self) {
        self.pane.move_to_end();
    }

    fn cycle_stream_filter(&mut self) {
        let target_seq = self.pane.selected_seq();
        self.stream_filter = self.stream_filter.next();
        self.pane.rebuild_visible(self.stream_filter, target_seq);
    }
}

struct DualApp {
    panes: [Pane; 2],
    active: usize,
    stream_filter: StreamFilter,
}

impl DualApp {
    fn new(
        left_title: String,
        left_entries: Vec<LogEntry>,
        right_title: String,
        right_entries: Vec<LogEntry>,
        stream_filter: StreamFilter,
    ) -> Self {
        Self {
            panes: [
                Pane::new(left_title, left_entries, stream_filter),
                Pane::new(right_title, right_entries, stream_filter),
            ],
            active: 0,
            stream_filter,
        }
    }

    fn move_selection(&mut self, delta: isize) {
        self.panes[self.active].move_selection(delta);
    }

    fn move_to_start(&mut self) {
        self.panes[self.active].move_to_start();
    }

    fn move_to_end(&mut self) {
        self.panes[self.active].move_to_end();
    }

    fn focus_left(&mut self) {
        self.active = 0;
    }

    fn focus_right(&mut self) {
        self.active = 1;
    }

    fn toggle_focus(&mut self) {
        self.active = 1 - self.active;
    }

    fn cycle_stream_filter(&mut self) {
        let target_seq = self.panes[self.active].selected_seq();
        self.stream_filter = self.stream_filter.next();
        for pane in self.panes.iter_mut() {
            pane.rebuild_visible(self.stream_filter, target_seq);
        }
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    let mut terminal = setup_terminal()?;
    let stream_filter: StreamFilter = args.stream.into();

    let result = match args.paths.as_slice() {
        [single] => {
            let title = args
                .left_label
                .clone()
                .unwrap_or_else(|| label_from_path(single, "trace"));
            let entries = load_entries(single)?;
            run_single(
                &mut terminal,
                SingleApp::new(title, entries, stream_filter),
            )
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
            run_dual(
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

fn run_single(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    mut app: SingleApp,
) -> Result<()> {
    let mut needs_redraw = true;
    loop {
        if needs_redraw {
            terminal.draw(|f| render_single(f, &app))?;
            needs_redraw = false;
        }
        if event::poll(Duration::from_millis(250))? {
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

fn run_dual(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    mut app: DualApp,
) -> Result<()> {
    let mut needs_redraw = true;
    loop {
        if needs_redraw {
            terminal.draw(|f| render_dual(f, &app))?;
            needs_redraw = false;
        }
        if event::poll(Duration::from_millis(250))? {
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

fn render_single(frame: &mut Frame, app: &SingleApp) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
        .split(frame.size());

    let items: Vec<ListItem> = app
        .pane
        .visible_indices
        .iter()
        .map(|idx| ListItem::new(app.pane.entries[*idx].display.as_str()))
        .collect();

    let mut list_state = ListState::default();
    if !app.pane.visible_indices.is_empty() {
        list_state.select(Some(app.pane.selected));
    }

    let list = List::new(items)
        .block(
            Block::default()
                .title(Line::from(vec![
                    Span::raw(format!("{} ", app.pane.title)),
                    Span::raw(format!("[{}] ", app.stream_filter.label())),
                    Span::raw(" (q/ctrl-c quit, s stream)"),
                ]))
                .borders(Borders::ALL),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));

    frame.render_stateful_widget(list, chunks[0], &mut list_state);

    let detail = app
        .pane
        .selected_entry()
        .map(detail_lines)
        .unwrap_or_else(|| Line::raw("no selection"));

    let detail_widget = Paragraph::new(detail)
        .block(Block::default().title("Details").borders(Borders::ALL))
        .wrap(Wrap { trim: false });

    frame.render_widget(detail_widget, chunks[1]);
}

fn render_dual(frame: &mut Frame, app: &DualApp) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
        .split(frame.size());

    let top = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(root[0]);

    let bottom = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(root[1]);

    for (idx, pane) in app.panes.iter().enumerate() {
        let items: Vec<ListItem> = pane
            .visible_indices
            .iter()
            .map(|row| ListItem::new(pane.entries[*row].display.as_str()))
            .collect();

        let mut list_state = ListState::default();
        if !pane.visible_indices.is_empty() {
            list_state.select(Some(pane.selected));
        }

        let mut title = vec![Span::raw(format!("{} ", pane.title))];
        title.push(Span::raw(format!("[{}] ", app.stream_filter.label())));
        title.push(Span::raw(" (q/ctrl-c quit, s stream, Tab/< /> switch)"));

        let mut block = Block::default()
            .title(Line::from(title))
            .borders(Borders::ALL);

        if app.active == idx {
            block = block.border_style(Style::default().add_modifier(Modifier::BOLD));
        }

        let list = List::new(items)
            .block(block)
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED));

        frame.render_stateful_widget(list, top[idx], &mut list_state);
    }

    let left_detail = app.panes[0]
        .selected_entry()
        .map(detail_lines)
        .unwrap_or_else(|| Line::raw("no selection"));
    let right_detail = app.panes[1]
        .selected_entry()
        .map(detail_lines)
        .unwrap_or_else(|| Line::raw("no selection"));

    let left_widget = Paragraph::new(left_detail)
        .block(
            Block::default()
                .title(Line::from(format!("{} details", app.panes[0].title)))
                .borders(Borders::ALL),
        )
        .wrap(Wrap { trim: false });
    let right_widget = Paragraph::new(right_detail)
        .block(
            Block::default()
                .title(Line::from(format!("{} details", app.panes[1].title)))
                .borders(Borders::ALL),
        )
        .wrap(Wrap { trim: false });

    frame.render_widget(left_widget, bottom[0]);
    frame.render_widget(right_widget, bottom[1]);
}

fn rebuild_display_lines(entries: &mut [LogEntry]) {
    for entry in entries.iter_mut() {
        entry.rebuild_display();
    }
}

fn render_display_line(entry: &LogEntry) -> String {
    let marker = if entry.is_composite() { "[S]" } else { "   " };
    let text = format!(
        "{} {:>10} {:<28} {}",
        marker,
        entry.seq_display,
        entry.display_event(),
        entry.summary
    );
    text
}

fn detail_lines(entry: &LogEntry) -> Line<'static> {
    let mut parts = Vec::new();
    parts.push(format!("seq={} ", entry.seq_display));
    if entry.seq_display != entry.orig_seq_display {
        parts.push(format!("log_seq={} ", entry.orig_seq_display));
    }
    parts.push(format!("event={} ", entry.display_event()));
    if entry.seq_min != entry.seq_max {
        parts.push(format!("range={:06}-{:06} ", entry.seq_min, entry.seq_max));
    } else if entry.seq_display != entry.orig_seq_display
        && entry.orig_seq_min != entry.orig_seq_max
    {
        parts.push(format!(
            "log_range={:06}-{:06} ",
            entry.orig_seq_min, entry.orig_seq_max
        ));
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
    assign_stream_sequences(&mut entries);
    Ok(entries)
}

fn label_from_path(path: &str, fallback: &str) -> String {
    if path == "-" {
        return fallback.to_string();
    }
    Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| fallback.to_string())
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
    let mut stream = StreamKind::Other;

    for token in tokens {
        if let Some((key, value)) = token.split_once('=') {
            let key = key.to_string();
            let value = value.to_string();
            if key == "seq" {
                seq_display = Some(value.clone());
            } else if key == "event" {
                event = Some(value.clone());
            } else if key == "stream" {
                stream = StreamKind::from_field(&value);
            }
            fields.push(Field { key, value });
        }
    }

    if matches!(stream, StreamKind::Other) {
        stream = stream_kind_from_line(&content);
    }

    let seq_display = seq_display?;
    let event = event?;
    let seq_range = parse_seq_range(&seq_display)?;
    let seq_min = seq_range.min;
    let seq_max = seq_range.max;

    let mut entry = LogEntry {
        seq_display: seq_display.clone(),
        seq_min,
        seq_max,
        orig_seq_display: seq_display,
        orig_seq_min: seq_min,
        orig_seq_max: seq_max,
        event,
        stream,
        fields,
        summary: String::new(),
        display: String::new(),
    };
    entry.summary = entry.compute_summary();
    Some(entry)
}

fn assign_stream_sequences(entries: &mut [LogEntry]) {
    let mut semantic_seq = 0u64;

    for entry in entries.iter_mut() {
        match entry.stream {
            StreamKind::Semantic => {
                semantic_seq = semantic_seq.saturating_add(1);
                entry.seq_display = format!("{semantic_seq:06}");
                entry.seq_min = semantic_seq;
                entry.seq_max = semantic_seq;
            }
            _ => {
                entry.seq_display = entry.orig_seq_display.clone();
                entry.seq_min = entry.orig_seq_min;
                entry.seq_max = entry.orig_seq_max;
            }
        }
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
