use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Receiver;
use std::sync::Arc;

use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, List, ListItem, ListState, Paragraph, Wrap},
    Frame,
};

use crate::detect::SourceInfo;
use crate::headless::{gb, mb};
use crate::import::{Progress, Summary};
use crate::plan::Plan;
use crate::tui::tree::Tree;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Browser,
    Planning,
    Confirm,
    Progress,
    Summary,
}

pub struct App {
    pub screen: Screen,
    pub tree: Tree,
    pub list_state: ListState,
    pub source: Option<SourceInfo>,
    pub dest: Option<PathBuf>,
    pub plan: Option<Plan>,
    pub plan_rx: Option<Receiver<anyhow::Result<Plan>>>,
    pub progress: Option<Progress>,
    pub import_rx: Option<Receiver<Progress>>,
    pub summary_rx: Option<Receiver<Summary>>,
    pub summary: Option<Summary>,
    pub cancel: Arc<AtomicBool>,
    pub error: Option<String>,
    pub rename: bool,
    pub force: bool,
    pub dry_run: bool,
    pub spinner: usize,
    pub summary_scroll: usize,
}

impl App {
    pub fn new(source: Option<SourceInfo>, last_dest: Option<String>) -> App {
        let dest = last_dest
            .map(PathBuf::from)
            .filter(|p| p.exists());
        App {
            screen: Screen::Browser,
            tree: Tree::new(),
            list_state: ListState::default(),
            source,
            dest,
            plan: None,
            plan_rx: None,
            progress: None,
            import_rx: None,
            summary_rx: None,
            summary: None,
            cancel: Arc::new(AtomicBool::new(false)),
            error: None,
            rename: true,
            force: false,
            dry_run: false,
            spinner: 0,
            summary_scroll: 0,
        }
    }

    pub fn sync_list_state(&mut self) {
        self.list_state.select(Some(self.tree.cursor));
    }
}

const SPINNER: [&str; 4] = ["⠋", "⠙", "⠹", "⠸"];

/// ---------- drawing ----------

pub fn draw(f: &mut Frame, app: &mut App) {
    match app.screen {
        Screen::Browser => draw_browser(f, app),
        Screen::Planning => draw_planning(f, app),
        Screen::Confirm => draw_confirm(f, app),
        Screen::Progress => draw_progress(f, app),
        Screen::Summary => draw_summary(f, app),
    }
}

fn draw_browser(f: &mut Frame, app: &mut App) {
    let [left, right] = Layout::horizontal([Constraint::Percentage(48), Constraint::Fill(1)])
        .areas(f.area());

    // Left: drive tree.
    let rows = app.tree.rows();
    let items: Vec<ListItem> = rows
        .iter()
        .map(|r| {
            let indent = "  ".repeat(r.depth);
            let marker = if r.is_dir {
                if r.expanded { "▾ " } else { "▸ " }
            } else {
                "  "
            };
            let style = if r.is_dir {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default()
            };
            ListItem::new(Line::from(Span::styled(
                format!("{indent}{marker}{}", r.name),
                style,
            )))
        })
        .collect();
    let list = List::new(items)
        .block(
            Block::new()
                .borders(Borders::ALL)
                .title(" Drives — ↑↓ move · → expand · ← back · Enter select "),
        )
        .highlight_style(
            Style::default()
                .bg(Color::Blue)
                .add_modifier(Modifier::BOLD),
        );
    f.render_stateful_widget(list, left, &mut app.list_state);

    // Right: info panel.
    let selected = app.tree.selected();
    let mut lines: Vec<Line> = Vec::new();

    lines.push(Line::from(Span::styled(
        "offload",
        Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));

    match &app.source {
        Some(src) => {
            lines.push(Line::from(vec![
                Span::styled("Card detected: ", Style::default().fg(Color::Green)),
                Span::raw(src.root.to_string_lossy().into_owned()),
            ]));
            lines.push(Line::from(format!(
                "  {} files, {:.1} GB ready to offload",
                src.total_count(),
                gb(src.total_bytes())
            )));
        }
        None => {
            lines.push(Line::from(Span::styled(
                "No camera card detected. Connect the SD card, or browse and pick any folder as source manually later.",
                Style::default().fg(Color::Yellow),
            )));
        }
    }
    lines.push(Line::from(""));

    lines.push(Line::from(vec![
        Span::styled("Destination: ", Style::default().fg(Color::Green)),
        Span::raw(
            app.dest
                .as_ref()
                .map(|d| d.to_string_lossy().into_owned())
                .unwrap_or_else(|| "not set (Enter on a folder)".into()),
        ),
    ]));
    lines.push(Line::from(""));

    if let Some(sel) = &selected {
        lines.push(Line::from(format!("Cursor: {}", sel.path.to_string_lossy())));
    }
    lines.push(Line::from(""));

    lines.push(Line::from(Span::styled(
        "Settings",
        Style::default().add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(format!(
        "  [r] rename on import : {}",
        on_off(app.rename)
    )));
    lines.push(Line::from(format!(
        "  [d] dry run          : {}",
        on_off(app.dry_run)
    )));
    lines.push(Line::from(format!(
        "  [f] force re-import  : {}",
        on_off(app.force)
    )));

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Keys: Enter on expanded folder = set destination",
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::from(Span::styled(
        "      q quit",
        Style::default().fg(Color::DarkGray),
    )));

    if let Some(err) = &app.error {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("⚠ {err}"),
            Style::default().fg(Color::Red),
        )));
    }

    let para = Paragraph::new(lines)
        .block(Block::new().borders(Borders::ALL).title(" Info "))
        .wrap(Wrap { trim: true });
    f.render_widget(para, right);
}

fn draw_planning(f: &mut Frame, app: &mut App) {
    let icon = SPINNER[app.spinner % SPINNER.len()];
    let text = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("{icon}  Hashing card files for dedup…"),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Large cards take a moment — this is the fingerprinting pass.",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    centered(f, " Working ", text, 60);
    let _ = app;
}

fn draw_confirm(f: &mut Frame, app: &mut App) {
    let Some(plan) = &app.plan else { return };
    let st = plan.stats();
    let mut lines = Vec::new();

    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("Destination: ", Style::default().fg(Color::Green)),
        Span::raw(
            app.dest
                .as_ref()
                .map(|d| d.to_string_lossy().into_owned())
                .unwrap_or_default(),
        ),
    ]));
    lines.push(Line::from(""));

    if st.dates.is_empty() {
        lines.push(Line::from("Nothing to import."));
    }
    for date in &st.dates {
        let folder = format!(
            "{:02}-{:02}-{:04}",
            date.day(),
            date.month() as i64,
            date.year()
        );
        let count = plan
            .files
            .iter()
            .filter(|f| f.is_copy() && f.date == Some(*date))
            .count();
        lines.push(Line::from(format!(
            "  {folder}/   {count} files"
        )));
    }
    lines.push(Line::from(""));
    for (mt, (count, bytes)) in &st.by_type {
        lines.push(Line::from(format!(
            "  {:<11} {} files, {:.1} GB",
            mt.label(),
            count,
            gb(*bytes)
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(format!(
        "Copy: {} files ({:.1} GB)   Skip already imported: {} files ({:.1} GB)",
        st.copy_count,
        gb(st.copy_bytes),
        st.skip_count,
        gb(st.skip_bytes)
    )));
    if app.rename {
        lines.push(Line::from(Span::styled(
            "Rename: YYYY-MM-DD_NNN.ext",
            Style::default().fg(Color::DarkGray),
        )));
    }
    if app.dry_run {
        lines.push(Line::from(Span::styled(
            "DRY RUN — nothing will be copied.",
            Style::default().fg(Color::Yellow),
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Enter = start   Esc = back   q = quit",
        Style::default().fg(Color::Cyan),
    )));

    centered(f, " Confirm import ", lines, 70);
}

fn draw_progress(f: &mut Frame, app: &mut App) {
    let Some(p) = &app.progress else { return };
    let [_, middle] = Layout::vertical([
        Constraint::Length(4),
        Constraint::Fill(1),
    ])
    .areas(f.area());

    let ratio = if p.total_bytes > 0 {
        p.copied_bytes as f64 / p.total_bytes as f64
    } else {
        1.0
    };
    let gauge = Gauge::default()
        .block(Block::new().borders(Borders::ALL).title(" Importing "))
        .ratio(ratio.min(1.0))
        .label(format!(
            "{}/{} files — {:.0}% — {}",
            p.done_files,
            p.total_files,
            ratio * 100.0,
            ellipsize(&p.current, 40)
        ))
        .gauge_style(Style::default().fg(Color::Green));
    f.render_widget(gauge, middle);

    if p.cancelled {
        let warn = Paragraph::new("Cancelled — files already copied are kept and indexed.")
            .alignment(Alignment::Center)
            .style(Style::default().fg(Color::Yellow));
        f.render_widget(warn, f.area());
    }
    let _ = app;
}

fn draw_summary(f: &mut Frame, app: &mut App) {
    let Some(s) = &app.summary else { return };
    let mut lines = Vec::new();
    lines.push(Line::from(""));
    if s.cancelled {
        lines.push(Line::from(Span::styled(
            "Import cancelled — partial results kept:",
            Style::default().fg(Color::Yellow),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            "Import complete ✔",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(format!(
        "  Copied: {} files ({:.1} GB)",
        s.copied,
        gb(s.copied_bytes)
    )));
    lines.push(Line::from(format!(
        "  Skipped (already imported): {} files",
        s.skipped.len()
    )));
    if !s.errors.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("  Errors: {}", s.errors.len()),
            Style::default().fg(Color::Red),
        )));
    }
    if !s.skipped.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "⚠ Already imported — skipped:",
            Style::default().fg(Color::Yellow),
        )));
        for sk in &s.skipped {
            let known = sk
                .known
                .as_deref()
                .unwrap_or("(same content)");
            lines.push(Line::from(format!(
                "  {}  →  {}  ({:.1} MB)",
                sk.original_name,
                known,
                mb(sk.size)
            )));
        }
    }
    if !s.errors.is_empty() {
        lines.push(Line::from(""));
        for e in &s.errors {
            lines.push(Line::from(Span::styled(
                format!("  {e}"),
                Style::default().fg(Color::Red),
            )));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "q / Enter = quit",
        Style::default().fg(Color::Cyan),
    )));

    let para = Paragraph::new(lines)
        .block(Block::new().borders(Borders::ALL).title(" Summary "))
        .wrap(Wrap { trim: true })
        .scroll((app.summary_scroll as u16, 0));
    f.render_widget(para, f.area());
}

fn centered(f: &mut Frame, title: &str, lines: Vec<Line>, width_pct: u16) {
    let area = centered_rect(width_pct, 55, f.area());
    let para = Paragraph::new(lines)
        .block(Block::new().borders(Borders::ALL).title(title))
        .wrap(Wrap { trim: true });
    f.render_widget(para, area);
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let [_, middle, _] = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .areas(r);
    let [_, mid, _] = Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .areas(middle);
    mid
}

fn on_off(b: bool) -> &'static str {
    if b { "on" } else { "off" }
}

fn ellipsize(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().rev().take(max - 1).collect::<String>()
            .chars()
            .rev()
            .collect();
        format!("…{cut}")
    }
}
