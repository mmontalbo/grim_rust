use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
    Frame,
};

use crate::trace_tui::data::{compute_parity, LogEntry, ParityStatus};
use grim_telemetry_common::StreamFilter;

const LIST_LEGEND: &str = "[= match ! diff ? missing] ";

pub struct Pane {
    pub title: String,
    pub entries: Vec<LogEntry>,
    pub visible_indices: Vec<usize>,
    pub selected: usize,
    pub list_state: ListState,
}

impl Pane {
    pub fn new(title: String, entries: Vec<LogEntry>, stream_filter: StreamFilter) -> Self {
        let mut pane = Self {
            title,
            entries,
            visible_indices: Vec::new(),
            selected: 0,
            list_state: ListState::default(),
        };
        pane.rebuild_visible(stream_filter, None);
        pane
    }

    pub fn rebuild_visible(&mut self, stream_filter: StreamFilter, target_seq: Option<u64>) {
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
            self.list_state = ListState::default();
            return;
        }
        if let Some(seq) = target_seq {
            if let Some(pos) = self
                .visible_indices
                .iter()
                .position(|idx| self.entries[*idx].seq_min == seq)
            {
                self.selected = pos;
                self.sync_state();
                return;
            }
        }
        if self.selected >= self.visible_indices.len() {
            self.selected = self.visible_indices.len() - 1;
        }
        self.sync_state();
    }

    pub fn move_selection(&mut self, delta: isize) {
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
        self.sync_state();
    }

    pub fn move_to_start(&mut self) {
        if !self.visible_indices.is_empty() {
            self.selected = 0;
            self.sync_state();
        }
    }

    pub fn move_to_end(&mut self) {
        if !self.visible_indices.is_empty() {
            self.selected = self.visible_indices.len() - 1;
            self.sync_state();
        }
    }

    pub fn selected_entry(&self) -> Option<&LogEntry> {
        self.visible_indices
            .get(self.selected)
            .and_then(|idx| self.entries.get(*idx))
    }

    pub fn selected_index(&self) -> Option<usize> {
        self.visible_indices.get(self.selected).copied()
    }

    pub fn selected_seq(&self) -> Option<u64> {
        self.selected_entry().map(|entry| entry.orig_seq_min)
    }

    pub fn sync_state(&mut self) {
        if self.visible_indices.is_empty() {
            self.list_state = ListState::default();
            return;
        }
        if self.selected >= self.visible_indices.len() {
            self.selected = self.visible_indices.len() - 1;
        }
        self.list_state.select(Some(self.selected));
        let max_offset = self.visible_indices.len().saturating_sub(1);
        if self.list_state.offset() > max_offset {
            *self.list_state.offset_mut() = max_offset;
        }
    }
}

pub struct SingleApp {
    pub pane: Pane,
    pub stream_filter: StreamFilter,
}

impl SingleApp {
    pub fn new(title: String, entries: Vec<LogEntry>, stream_filter: StreamFilter) -> Self {
        Self {
            pane: Pane::new(title, entries, stream_filter),
            stream_filter,
        }
    }

    pub fn move_selection(&mut self, delta: isize) {
        self.pane.move_selection(delta);
    }

    pub fn move_to_start(&mut self) {
        self.pane.move_to_start();
    }

    pub fn move_to_end(&mut self) {
        self.pane.move_to_end();
    }

    pub fn cycle_stream_filter(&mut self) {
        let target_seq = self.pane.selected_seq();
        self.stream_filter = self.stream_filter.next();
        self.pane.rebuild_visible(self.stream_filter, target_seq);
    }
}

pub struct DualApp {
    pub panes: [Pane; 2],
    pub active: usize,
    pub stream_filter: StreamFilter,
    pub parity: [Vec<Option<ParityStatus>>; 2],
}

impl DualApp {
    pub fn new(
        left_title: String,
        left_entries: Vec<LogEntry>,
        right_title: String,
        right_entries: Vec<LogEntry>,
        stream_filter: StreamFilter,
    ) -> Self {
        let panes = [
            Pane::new(left_title, left_entries, stream_filter),
            Pane::new(right_title, right_entries, stream_filter),
        ];
        let parity = compute_parity(&panes[0].entries, &panes[1].entries);
        Self {
            panes,
            active: 0,
            stream_filter,
            parity,
        }
    }

    pub fn move_selection(&mut self, delta: isize) {
        self.panes[self.active].move_selection(delta);
    }

    pub fn move_to_start(&mut self) {
        self.panes[self.active].move_to_start();
    }

    pub fn move_to_end(&mut self) {
        self.panes[self.active].move_to_end();
    }

    pub fn focus_left(&mut self) {
        self.active = 0;
    }

    pub fn focus_right(&mut self) {
        self.active = 1;
    }

    pub fn toggle_focus(&mut self) {
        self.active = 1 - self.active;
    }

    pub fn cycle_stream_filter(&mut self) {
        let target_seq = self.panes[self.active].selected_seq();
        self.stream_filter = self.stream_filter.next();
        for pane in self.panes.iter_mut() {
            pane.rebuild_visible(self.stream_filter, target_seq);
        }
    }
}

pub fn render_single(frame: &mut Frame, app: &mut SingleApp) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
        .split(frame.size());

    render_pane_list(
        frame,
        chunks[0],
        &mut app.pane,
        None,
        app.stream_filter.label(),
        LIST_LEGEND,
        false,
    );

    let detail = app
        .pane
        .selected_entry()
        .map(|entry| detail_lines(entry, None))
        .unwrap_or_else(|| Line::raw("no selection"));

    let detail_widget = Paragraph::new(detail)
        .block(Block::default().title("Details").borders(Borders::ALL))
        .wrap(Wrap { trim: false });

    frame.render_widget(detail_widget, chunks[1]);
}

pub fn render_dual(frame: &mut Frame, app: &mut DualApp) {
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

    let stream_label = app.stream_filter.label();
    for idx in 0..app.panes.len() {
        render_pane_list(
            frame,
            top[idx],
            &mut app.panes[idx],
            Some(&app.parity[idx]),
            stream_label,
            LIST_LEGEND,
            app.active == idx,
        );
    }

    let left_detail = app.panes[0]
        .selected_index()
        .and_then(|idx| {
            let entry = app.panes[0].entries.get(idx)?;
            let parity = app.parity[0].get(idx).and_then(|p| p.as_ref());
            Some(detail_lines(entry, parity))
        })
        .unwrap_or_else(|| Line::raw("no selection"));
    let right_detail = app.panes[1]
        .selected_index()
        .and_then(|idx| {
            let entry = app.panes[1].entries.get(idx)?;
            let parity = app.parity[1].get(idx).and_then(|p| p.as_ref());
            Some(detail_lines(entry, parity))
        })
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

fn render_pane_list(
    frame: &mut Frame,
    area: Rect,
    pane: &mut Pane,
    parity: Option<&[Option<ParityStatus>]>,
    stream_label: &str,
    legend: &str,
    active: bool,
) {
    pane.sync_state();
    let items: Vec<ListItem> = pane
        .visible_indices
        .iter()
        .map(|row| {
            let parity = parity.and_then(|p| p.get(*row).and_then(|p| p.as_ref()));
            render_list_item(&pane.entries[*row], parity)
        })
        .collect();

    let mut title = vec![Span::raw(format!("{} ", pane.title))];
    title.push(Span::raw(format!("[{}] ", stream_label)));
    title.push(Span::raw(legend));
    let mut block = Block::default()
        .title(Line::from(title))
        .borders(Borders::ALL);

    if active {
        block = block.border_style(Style::default().add_modifier(Modifier::BOLD));
    }

    let list = List::new(items).block(block).highlight_style(
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );

    frame.render_stateful_widget(list, area, &mut pane.list_state);
}

fn render_list_item(entry: &LogEntry, parity: Option<&ParityStatus>) -> ListItem<'static> {
    ListItem::new(render_display_line(entry, parity))
}

fn render_display_line(entry: &LogEntry, parity: Option<&ParityStatus>) -> Line<'static> {
    let mut spans = Vec::new();
    spans.push(parity_marker(parity));
    spans.push(Span::raw(" "));
    spans.push(stream_marker(entry.stream));
    spans.push(Span::raw(" "));
    spans.push(seq_span(&entry.seq_display));
    spans.push(Span::raw(" "));
    spans.push(event_span(entry));
    spans.push(Span::raw(" "));
    spans.push(summary_span(&entry.summary));
    Line::from(spans)
}

fn parity_marker(parity: Option<&ParityStatus>) -> Span<'static> {
    match parity {
        Some(ParityStatus::Match) => Span::styled("[=]", Style::default().fg(Color::Green)),
        Some(ParityStatus::Mismatch { .. }) => Span::styled(
            "[!]",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Some(ParityStatus::MissingOther) => Span::styled("[?]", Style::default().fg(Color::Yellow)),
        None => Span::styled("   ", Style::default().fg(Color::DarkGray)),
    }
}

fn stream_marker(stream: grim_telemetry_common::StreamKind) -> Span<'static> {
    match stream {
        grim_telemetry_common::StreamKind::Semantic => {
            Span::styled("[S]", Style::default().fg(Color::Cyan))
        }
        grim_telemetry_common::StreamKind::Raw => {
            Span::styled("[R]", Style::default().fg(Color::Gray))
        }
        grim_telemetry_common::StreamKind::Other => {
            Span::styled("[?]", Style::default().fg(Color::Gray))
        }
    }
}

fn seq_span(seq: &str) -> Span<'static> {
    Span::styled(format!("{:>8}", seq), Style::default().fg(Color::Gray))
}

fn event_span(entry: &LogEntry) -> Span<'static> {
    Span::styled(
        format!("{:<28}", entry.display_event()),
        event_style(entry.display_event()),
    )
}

fn summary_span(summary: &str) -> Span<'static> {
    Span::styled(summary.to_string(), Style::default().fg(Color::White))
}

fn event_style(event: &str) -> Style {
    let color = if event.contains("error") {
        Color::Red
    } else if event.contains("fallback") || event.contains("tag") {
        Color::LightCyan
    } else if event.contains("set_table") || event.contains("table_entry") {
        Color::Cyan
    } else if event.contains("bind")
        || event.contains("registered")
        || event.contains("setglobal")
        || event.contains("set_constant")
    {
        Color::Yellow
    } else if event.contains("ref") {
        Color::Magenta
    } else if event.contains("push") {
        Color::LightBlue
    } else if event.contains("call") || event.contains("dostring") || event.contains("dofile") {
        Color::Green
    } else if event.contains("cutscene") {
        Color::LightYellow
    } else {
        Color::White
    };
    Style::default().fg(color)
}

fn detail_lines(entry: &LogEntry, parity: Option<&ParityStatus>) -> Line<'static> {
    let mut parts = Vec::new();
    parts.push(format!("seq={} ", entry.seq_display));
    if entry.seq_display != entry.orig_seq_display {
        parts.push(format!("log_seq={} ", entry.orig_seq_display));
    }
    parts.push(format!("event={} ", entry.display_event()));
    if let Some(note) = parity_label(parity) {
        parts.push(format!("parity={} ", note));
    }
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

fn parity_label(parity: Option<&ParityStatus>) -> Option<String> {
    match parity {
        Some(ParityStatus::Match) => Some("match".to_string()),
        Some(ParityStatus::MissingOther) => Some("missing_other".to_string()),
        Some(ParityStatus::Mismatch { other }) => Some(format!("mismatch vs {other}")),
        None => None,
    }
}
