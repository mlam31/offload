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
use crate::headless::{ellipsize, gb, mb};
use crate::import::{Progress, Summary};
use crate::plan::{date_folder, Plan};
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
    pub hash_rx: Option<Receiver<(usize, usize)>>,
    pub hash_progress: (usize, usize),
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
    pub eject_status: Vec<String>,
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
            hash_rx: None,
            hash_progress: (0, 0),
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
            eject_status: Vec::new(),
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
    let list = drive_list(&rows);
    f.render_stateful_widget(list, left, &mut app.list_state);

    // Right: info panel.
    let selected = app.tree.selected();
    f.render_widget(info_panel(app, selected), right);
}

fn drive_list(rows: &[crate::tui::tree::Row]) -> List<'static> {
    let items: Vec<ListItem> = rows.iter().map(row_item).collect();
    List::new(items)
        .block(
            Block::new()
                .borders(Borders::ALL)
                .title(" Drives — ↑↓ move · → expand · ← back · Enter select "),
        )
        .highlight_style(
            Style::default()
                .bg(Color::Blue)
                .add_modifier(Modifier::BOLD),
        )
}

fn row_item(r: &crate::tui::tree::Row) -> ListItem<'static> {
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
}

fn info_panel(app: &App, selected: Option<crate::tui::tree::Row>) -> Paragraph<'static> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.extend(header_lines());
    lines.extend(source_lines(app.source.as_ref()));
    lines.push(Line::from(""));
    lines.push(destination_line(app.dest.as_ref()));
    lines.push(Line::from(""));
    if let Some(sel) = &selected {
        lines.push(Line::from(format!(
            "Cursor: {}",
            sel.path.to_string_lossy()
        )));
        lines.push(Line::from(""));
    }
    lines.extend(settings_lines(app));
    if let Some(err) = &app.error {
        lines.extend(error_lines(err));
    }

    Paragraph::new(lines)
        .block(Block::new().borders(Borders::ALL).title(" Info "))
        .wrap(Wrap { trim: true })
}

fn header_lines() -> Vec<Line<'static>> {
    vec![
        Line::from(Span::styled(
            "offload",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ]
}

fn source_lines(source: Option<&SourceInfo>) -> Vec<Line<'static>> {
    match source {
        Some(src) => vec![
            Line::from(vec![
                Span::styled("Card detected: ", Style::default().fg(Color::Green)),
                Span::raw(src.root.to_string_lossy().into_owned()),
            ]),
            Line::from(format!(
                "  {} files, {:.1} GB ready to offload",
                src.total_count(),
                gb(src.total_bytes())
            )),
        ],
        None => vec![Line::from(Span::styled(
            "No camera card detected. Connect the SD card, or browse and pick any folder as source manually later.",
            Style::default().fg(Color::Yellow),
        ))],
    }
}

fn destination_line(dest: Option<&PathBuf>) -> Line<'static> {
    Line::from(vec![
        Span::styled("Destination: ", Style::default().fg(Color::Green)),
        Span::raw(
            dest.map(|d| d.to_string_lossy().into_owned())
                .unwrap_or_else(|| "not set (Enter on a folder)".into()),
        ),
    ])
}

fn settings_lines(app: &App) -> Vec<Line<'static>> {
    vec![
        Line::from(Span::styled(
            "Settings",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(format!(
            "  [r] rename on import : {}",
            on_off(app.rename)
        )),
        Line::from(format!(
            "  [d] dry run          : {}",
            on_off(app.dry_run)
        )),
        Line::from(format!(
            "  [f] force re-import  : {}",
            on_off(app.force)
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Keys: Enter on expanded folder = set destination",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(Span::styled(
            "      q quit",
            Style::default().fg(Color::DarkGray),
        )),
    ]
}

fn error_lines(err: &str) -> Vec<Line<'static>> {
    vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("⚠ {err}"),
            Style::default().fg(Color::Red),
        )),
    ]
}

fn draw_planning(f: &mut Frame, app: &mut App) {
    let icon = SPINNER[app.spinner % SPINNER.len()];
    let (done, total) = app.hash_progress;
    let ratio = if total > 0 { done as f64 / total as f64 } else { 0.0 };

    let [_, middle, _] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(7),
        Constraint::Fill(1),
    ])
    .areas(f.area());
    let [_, mid, _] = Layout::horizontal([
        Constraint::Percentage(20),
        Constraint::Fill(1),
        Constraint::Percentage(20),
    ])
    .areas(middle);

    let gauge = Gauge::default()
        .block(
            Block::new()
                .borders(Borders::ALL)
                .title(" Fingerprinting for dedup "),
        )
        .ratio(ratio.min(1.0))
        .label(format!(
            "{icon}  {done} / {total} files hashed  ({:.0}%)",
            ratio * 100.0
        ))
        .gauge_style(Style::default().fg(Color::Cyan));
    f.render_widget(gauge, mid);

    let note = Paragraph::new(vec![
        Line::from(""),
        Line::from(Span::styled(
            "Only files that could be duplicates are fingerprinted (same size as an\nimported file or as another file on the card). Everything else gets its\nhash computed during the copy itself — no extra pass, no extra time.",
            Style::default().fg(Color::DarkGray),
        )),
    ])
    .alignment(Alignment::Center);
    f.render_widget(note, f.area());
}

fn draw_confirm(f: &mut Frame, app: &mut App) {
    let Some(plan) = &app.plan else { return };
    centered(f, " Confirm import ", confirm_lines(app, plan), 70);
}

fn confirm_lines(app: &App, plan: &Plan) -> Vec<Line<'static>> {
    let st = plan.stats();
    let mut lines: Vec<Line<'static>> = Vec::new();

    lines.push(Line::from(""));
    lines.push(destination_line(app.dest.as_ref()));
    lines.push(Line::from(""));
    lines.extend(date_lines(plan, &st));
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
    lines
}

fn date_lines(plan: &Plan, st: &crate::plan::PlanStats) -> Vec<Line<'static>> {
    if st.dates.is_empty() {
        return vec![Line::from("Nothing to import.")];
    }
    st.dates
        .iter()
        .map(|date| {
            let count = plan
                .files
                .iter()
                .filter(|f| f.is_copy() && f.date == Some(*date))
                .count();
            Line::from(format!("  {}/   {count} files", date_folder(*date)))
        })
        .collect()
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
    let para = Paragraph::new(summary_lines(app, s))
        .block(Block::new().borders(Borders::ALL).title(" Summary "))
        .wrap(Wrap { trim: true })
        .scroll((app.summary_scroll as u16, 0));
    f.render_widget(para, f.area());
}

fn summary_lines(app: &App, s: &Summary) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
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
    lines.extend(summary_skip_lines(s));
    lines.extend(summary_error_lines(s));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "q / Enter = quit    e = eject card + SSD    s = eject card only",
        Style::default().fg(Color::Cyan),
    )));
    for status in &app.eject_status {
        lines.push(Line::from(Span::styled(
            format!("  {status}"),
            Style::default().fg(Color::Green),
        )));
    }
    lines
}

fn summary_skip_lines(s: &Summary) -> Vec<Line<'static>> {
    if s.skipped.is_empty() {
        return Vec::new();
    }
    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "⚠ Already imported — skipped:",
            Style::default().fg(Color::Yellow),
        )),
    ];
    for sk in &s.skipped {
        let known = sk.known.as_deref().unwrap_or("(same content)");
        lines.push(Line::from(format!(
            "  {}  →  {}  ({:.1} MB)",
            sk.original_name,
            known,
            mb(sk.size)
        )));
    }
    lines
}

fn summary_error_lines(s: &Summary) -> Vec<Line<'static>> {
    if s.errors.is_empty() {
        return Vec::new();
    }
    let mut lines = vec![Line::from("")];
    for e in &s.errors {
        lines.push(Line::from(Span::styled(
            format!("  {e}"),
            Style::default().fg(Color::Red),
        )));
    }
    lines
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

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;

    #[test]
    fn on_off_renders_booleans() {
        assert_eq!(on_off(true), "on");
        assert_eq!(on_off(false), "off");
    }

    #[test]
    fn centered_rect_shrinks_the_parent_area() {
        let parent = Rect::new(0, 0, 100, 100);
        let r = centered_rect(70, 55, parent);
        assert!(r.width > 0 && r.width < parent.width);
        assert!(r.height > 0 && r.height < parent.height);
        // Roughly centered horizontally.
        let left_margin = r.x - parent.x;
        let right_margin = (parent.x + parent.width) - (r.x + r.width);
        assert!(
            (left_margin as i32 - right_margin as i32).abs() <= 2,
            "left {left_margin} vs right {right_margin}"
        );
    }

    #[test]
    fn new_app_picks_up_existing_last_dest_only() {
        let tmp = tempfile::TempDir::new().unwrap();
        let existing = tmp.path().join("exists");
        std::fs::create_dir_all(&existing).unwrap();
        let missing = tmp.path().join("missing");

        let app = App::new(None, Some(existing.to_string_lossy().into_owned()));
        assert_eq!(app.dest.as_deref(), Some(existing.as_path()));
        assert_eq!(app.screen, Screen::Browser);
        assert!(app.rename && !app.force && !app.dry_run);

        let app = App::new(None, Some(missing.to_string_lossy().into_owned()));
        assert_eq!(app.dest, None);

        let app = App::new(None, None);
        assert_eq!(app.dest, None);
    }

    #[test]
    fn new_app_keeps_detected_source() {
        let src = SourceInfo {
            root: PathBuf::from("/Volumes/CARD"),
            files: Vec::new(),
        };
        let app = App::new(Some(src), None);
        assert!(app.source.is_some());
        let src = app.source.as_ref().unwrap();
        assert_eq!(src.total_count(), 0);
        assert_eq!(src.total_bytes(), 0);
    }
}
