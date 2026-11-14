pub mod app;
pub mod tree;

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::config::{load_index, save_last_dest};
use crate::disk::free_bytes;
use crate::headless::gb;
use crate::import::run;
use crate::plan::{build_plan, Action, HashProgress, Plan, PlanOptions, PlanStats};
use crate::scan::scan_source;
use app::{App, Screen};

/// Run the interactive terminal UI.
pub fn run_tui(
    source: Option<crate::detect::SourceInfo>,
    last_dest: Option<String>,
    cli_rename: bool,
    cli_force: bool,
    cli_dry_run: bool,
) -> Result<()> {
    // Restore the terminal even if something panics mid-run.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = ratatui::try_restore();
        default_hook(info);
    }));

    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, source, last_dest, cli_rename, cli_force, cli_dry_run);
    ratatui::restore();
    result
}

fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    source: Option<crate::detect::SourceInfo>,
    last_dest: Option<String>,
    cli_rename: bool,
    cli_force: bool,
    cli_dry_run: bool,
) -> Result<()> {
    let mut app = App::new(source, last_dest);
    app.rename = !cli_rename;
    app.force = cli_force;
    app.dry_run = cli_dry_run;
    app.sync_list_state();

    let mut last_tick = Instant::now();
    let tick = Duration::from_millis(120);

    loop {
        // Background work (plan building / import progress).
        pump_background(&mut app);

        terminal
            .draw(|f| app::draw(f, &mut app))
            .map_err(|e| anyhow::anyhow!("draw: {e}"))?;

        // Keyboard events.
        if event::poll(tick)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press && handle_key(&mut app, key) {
                    return Ok(());
                }
            }
        }
        if last_tick.elapsed() >= tick {
            app.spinner = app.spinner.wrapping_add(1);
            last_tick = Instant::now();
        }
    }
}

fn is_quit(key: &KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('q') | KeyCode::Char('Q'))
        || (matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
            && key.modifiers.contains(KeyModifiers::CONTROL))
}

fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    match app.screen {
        Screen::Browser => {
            let quit = handle_browser_key(app, key);
            app.sync_list_state();
            quit
        }
        Screen::Planning => is_quit(&key),
        Screen::Confirm => handle_confirm_key(app, key),
        Screen::Progress => {
            if is_quit(&key) {
                app.cancel.store(true, Ordering::SeqCst);
            }
            false
        }
        Screen::Summary => handle_summary_key(app, key),
    }
}

fn handle_browser_key(app: &mut App, key: KeyEvent) -> bool {
    if is_quit(&key) {
        return true;
    }
    match key.code {
        KeyCode::Up => app.tree.move_cursor(-1),
        KeyCode::Down => app.tree.move_cursor(1),
        KeyCode::Left => app.tree.collapse_selected(),
        KeyCode::Right => {
            app.tree.expand_selected();
            app.tree.revalidate_cursor();
        }
        KeyCode::Enter => browser_enter(app),
        KeyCode::Char('r') => app.rename = !app.rename,
        KeyCode::Char('f') => app.force = !app.force,
        KeyCode::Char('d') => app.dry_run = !app.dry_run,
        _ => {}
    }
    false
}

/// Enter on a directory expands it; Enter on an already-expanded directory
/// sets it as the import destination.
fn browser_enter(app: &mut App) {
    let Some(sel) = app.tree.selected() else { return };
    if !sel.is_dir {
        return;
    }
    let expanded_before = sel.expanded;
    app.tree.expand_selected();
    app.tree.revalidate_cursor();
    let now_expanded = app
        .tree
        .selected()
        .map(|s| s.expanded)
        .unwrap_or(false);
    if expanded_before && now_expanded {
        set_destination(app, sel.path);
    }
}

fn handle_confirm_key(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Esc => {
            app.screen = Screen::Browser;
            app.plan = None;
        }
        KeyCode::Enter => start_import(app),
        _ if is_quit(&key) => return true,
        _ => {}
    }
    false
}

fn handle_summary_key(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Up => app.summary_scroll = app.summary_scroll.saturating_sub(1),
        KeyCode::Down => app.summary_scroll += 1,
        KeyCode::Char('e') => eject_volumes(app, true),
        KeyCode::Char('s') => eject_volumes(app, false),
        _ if is_quit(&key) || key.code == KeyCode::Enter => return true,
        _ => {}
    }
    false
}

fn set_destination(app: &mut App, path: PathBuf) {
    if let Some(src) = &app.source {
        if path.starts_with(&src.root) {
            app.error = Some(
                "Destination must not be inside the card itself.".to_string(),
            );
            return;
        }
    }
    if let Some(src) = &app.source {
        if src.root.starts_with(&path) {
            app.error = Some(
                "Destination must not contain the card.".to_string(),
            );
            return;
        }
    }
    app.error = None;
    app.dest = Some(path);
    // Everything else (index, plan) is resolved on the Confirm step.
    start_planning(app);
}

fn start_planning(app: &mut App) {
    let Some(dest) = app.dest.clone() else { return };
    let Some(src) = &app.source else {
        app.error = Some("No source card detected — connect the SD card first.".into());
        return;
    };

    // Fresh scan of the card (files may have changed since startup).
    let files = match scan_source(&src.root) {
        Ok(f) => f,
        Err(e) => {
            app.error = Some(format!("scan failed: {e:#}"));
            return;
        }
    };
    if files.is_empty() {
        app.error = Some("No media files found on the card.".into());
        return;
    }

    let index = match load_index() {
        Ok(i) => i,
        Err(e) => {
            app.error = Some(format!("cannot read import index: {e:#}"));
            return;
        }
    };
    let opts = PlanOptions {
        rename: app.rename,
        force: app.force,
    };
    let (tx, rx) = mpsc::channel::<anyhow::Result<Plan>>();
    let (hash_tx, hash_rx) = mpsc::channel::<HashProgress>();
    app.plan_rx = Some(rx);
    app.hash_rx = Some(hash_rx);
    app.hash_progress = (0, 0);
    app.plan = None;
    app.screen = Screen::Planning;
    std::thread::spawn(move || {
        let result = build_plan(files, &index, &dest, opts, Some(&hash_tx));
        let _ = tx.send(result);
    });
}

/// Eject the card (and optionally the SSD) after a completed import.
/// Each volume is only ejected if no other process has files open on it.
fn eject_volumes(app: &mut App, include_dest: bool) {
    let mut status = Vec::new();
    if let Some(src) = &app.source {
        status.push(format!(
            "card: {}",
            crate::eject::eject_if_unused(&src.root).message
        ));
    }
    if include_dest {
        if let Some(dest) = &app.dest {
            status.push(format!(
                "SSD: {}",
                crate::eject::eject_if_unused(dest).message
            ));
        }
    }
    app.eject_status.extend(status);
}

fn start_import(app: &mut App) {
    let Some(plan) = app.plan.take() else { return };
    let Ok(index_path) = crate::config::index_path() else {
        app.error = Some("cannot resolve the import index location.".into());
        app.screen = Screen::Browser;
        return;
    };
    let cancel = app.cancel.clone();
    let (tx, rx) = mpsc::channel();
    let (tx_sum, rx_sum) = mpsc::channel();
    app.import_rx = Some(rx);
    app.summary_rx = Some(rx_sum);
    app.progress = Some(crate::import::Progress::default());
    app.screen = Screen::Progress;
    let dry_run = app.dry_run;
    std::thread::spawn(move || {
        match run(&plan, &index_path, dry_run, &cancel, tx) {
            Ok(summary) => {
                let _ = tx_sum.send(summary);
            }
            Err(e) => {
                let mut s = crate::import::Summary::default();
                s.errors.push(format!("{e:#}"));
                let _ = tx_sum.send(s);
            }
        }
    });
}

/// Poll background channels; move between screens when jobs finish.
fn pump_background(app: &mut App) {
    pump_planning(app);
    pump_import(app);
    pump_summary(app);
}

/// Poll planning channels: hash progress display, plan completion.
fn pump_planning(app: &mut App) {
    if app.screen != Screen::Planning {
        return;
    }
    update_hash_progress(app);
    let Some(rx) = &app.plan_rx else { return };
    let Ok(result) = rx.try_recv() else { return };
    app.plan_rx = None;
    app.hash_rx = None;
    match result {
        Ok(plan) => finish_planning(app, plan),
        Err(e) => {
            app.error = Some(format!("planning failed: {e:#}"));
            app.screen = Screen::Browser;
        }
    }
}

fn update_hash_progress(app: &mut App) {
    let Some(hash_rx) = &app.hash_rx else { return };
    let mut latest: Option<HashProgress> = None;
    while let Ok(p) = hash_rx.try_recv() {
        latest = Some(p);
    }
    if let Some(p) = latest {
        app.hash_progress = p;
    }
}

/// Move from a finished plan to Confirm — or straight to Summary when
/// everything is already imported.
fn finish_planning(app: &mut App, plan: Plan) {
    let st = plan.stats();
    if st.copy_count == 0 && st.skip_count > 0 {
        app.summary = Some(skips_to_summary(&plan));
        app.screen = Screen::Summary;
        return;
    }
    if !destination_has_space(app, &st) {
        return;
    }
    app.plan = Some(plan);
    app.screen = Screen::Confirm;
}

fn skips_to_summary(plan: &Plan) -> crate::import::Summary {
    crate::import::Summary {
        skipped: plan
            .files
            .iter()
            .filter_map(|f| match &f.action {
                Action::SkipDuplicate { known } => Some(crate::import::Skipped {
                    original_name: f.original_name.clone(),
                    size: f.size,
                    known: known.clone(),
                }),
                _ => None,
            })
            .collect(),
        ..Default::default()
    }
}

/// Free-space check before asking to confirm. Returns true when OK to proceed.
fn destination_has_space(app: &mut App, st: &PlanStats) -> bool {
    if app.dry_run {
        return true;
    }
    let Some(dest) = &app.dest else { return true };
    let Some(free) = free_bytes(dest) else { return true };
    if free >= st.copy_bytes + 1024 * 1024 {
        return true;
    }
    app.error = Some(format!(
        "Not enough free space: need {:.1} GB, have {:.1} GB",
        gb(st.copy_bytes),
        gb(free)
    ));
    app.screen = Screen::Browser;
    false
}

/// Poll import progress updates (draw only; the screen change happens when
/// the summary arrives on summary_rx).
fn pump_import(app: &mut App) {
    if app.screen != Screen::Progress {
        return;
    }
    let Some(rx) = &app.import_rx else { return };
    let mut latest: Option<crate::import::Progress> = None;
    while let Ok(p) = rx.try_recv() {
        latest = Some(p);
    }
    if let Some(p) = latest {
        app.progress = Some(p);
    }
}

/// Collect the finished import thread's summary.
fn pump_summary(app: &mut App) {
    let Some(summary_rx) = &app.summary_rx else { return };
    let Ok(summary) = summary_rx.try_recv() else { return };
    let cancelled = summary.cancelled;
    let dest = app.dest.clone();
    app.summary = Some(summary);
    if !cancelled {
        if let Some(d) = dest {
            let _ = save_last_dest(&d);
        }
    }
    app.import_rx = None;
    app.summary_rx = None;
    app.screen = Screen::Summary;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn q_and_ctrl_c_quit_others_do_not() {
        assert!(is_quit(&key(KeyCode::Char('q'))));
        assert!(is_quit(&key(KeyCode::Char('Q'))));
        assert!(is_quit(&KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL
        )));
        assert!(!is_quit(&key(KeyCode::Char('c'))));
        assert!(!is_quit(&key(KeyCode::Char('x'))));
        assert!(!is_quit(&key(KeyCode::Enter)));
    }

    #[test]
    fn browser_keys_toggle_settings() {
        let mut app = App::new(None, None);
        assert!(app.rename);
        handle_browser_key(&mut app, key(KeyCode::Char('r')));
        assert!(!app.rename);
        handle_browser_key(&mut app, key(KeyCode::Char('f')));
        assert!(app.force);
        handle_browser_key(&mut app, key(KeyCode::Char('d')));
        assert!(app.dry_run);
        // Unhandled keys are a no-op and do not quit.
        assert!(!handle_browser_key(&mut app, key(KeyCode::Char('z'))));
    }

    #[test]
    fn q_in_browser_quits() {
        let mut app = App::new(None, None);
        assert!(handle_browser_key(&mut app, key(KeyCode::Char('q'))));
    }

    #[test]
    fn enter_with_no_selection_in_browser_is_a_no_op() {
        let mut app = App::new(None, None);
        // Tree starts with /Volumes entries on macOS; force an empty tree so
        // nothing is selected.
        app.tree.roots.clear();
        assert!(!handle_browser_key(&mut app, key(KeyCode::Enter)));
        assert!(app.dest.is_none());
        assert_eq!(app.screen, Screen::Browser);
    }

    #[test]
    fn esc_from_confirm_returns_to_browser_and_drops_the_plan() {
        let mut app = App::new(None, None);
        app.screen = Screen::Confirm;
        app.plan = Some(Plan { files: Vec::new() });
        assert!(!handle_confirm_key(&mut app, key(KeyCode::Esc)));
        assert_eq!(app.screen, Screen::Browser);
        assert!(app.plan.is_none());
    }

    #[test]
    fn summary_keys_scroll() {
        let mut app = App::new(None, None);
        assert_eq!(app.summary_scroll, 0);
        assert!(!handle_summary_key(&mut app, key(KeyCode::Down)));
        assert_eq!(app.summary_scroll, 1);
        assert!(!handle_summary_key(&mut app, key(KeyCode::Up)));
        assert_eq!(app.summary_scroll, 0);
        // Saturates at the top.
        assert!(!handle_summary_key(&mut app, key(KeyCode::Up)));
        assert_eq!(app.summary_scroll, 0);
    }

    #[test]
    fn q_and_enter_on_summary_quit() {
        let mut app = App::new(None, None);
        assert!(handle_summary_key(&mut app, key(KeyCode::Char('q'))));
        assert!(handle_summary_key(&mut app, key(KeyCode::Enter)));
        assert!(!handle_summary_key(&mut app, key(KeyCode::Left)));
    }

    #[test]
    fn skips_to_summary_maps_plan_skips() {
        let plan = Plan {
            files: vec![
                planned_skip("a.jpg", 10, Some("/dest/a.jpg".into())),
                planned_skip("b.jpg", 20, None),
            ],
        };
        let summary = skips_to_summary(&plan);
        assert_eq!(summary.copied, 0);
        assert_eq!(summary.skipped.len(), 2);
        assert_eq!(summary.skipped[0].known.as_deref(), Some("/dest/a.jpg"));
        assert_eq!(summary.skipped[1].known, None);
        assert_eq!(summary.errors.len(), 0);
    }

    fn planned_skip(name: &str, size: u64, known: Option<String>) -> crate::plan::PlannedFile {
        crate::plan::PlannedFile {
            source: std::path::PathBuf::from(name),
            original_name: name.to_string(),
            media_type: crate::media::MediaType::Jpeg,
            size,
            hash: None,
            date: None,
            action: crate::plan::Action::SkipDuplicate { known },
        }
    }
}
