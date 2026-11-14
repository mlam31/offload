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
use crate::plan::{build_plan, Action, HashProgress, Plan, PlanOptions};
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
                if key.kind == KeyEventKind::Press {
                    if handle_key(&mut app, key) {
                        return Ok(());
                    }
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
            match key.code {
                _ if is_quit(&key) => return true,
                KeyCode::Up => app.tree.move_cursor(-1),
                KeyCode::Down => app.tree.move_cursor(1),
                KeyCode::Left => app.tree.collapse_selected(),
                KeyCode::Right => {
                    app.tree.expand_selected();
                    app.tree.revalidate_cursor();
                }
                KeyCode::Enter => {
                    let Some(sel) = app.tree.selected() else { return false };
                    if sel.is_dir {
                        let expanded_before = sel.expanded;
                        app.tree.expand_selected();
                        app.tree.revalidate_cursor();
                        let now_expanded = app
                            .tree
                            .selected()
                            .map(|s| s.expanded)
                            .unwrap_or(false);
                        // Enter on an already-expanded folder sets the destination.
                        if expanded_before && now_expanded {
                            set_destination(app, sel.path);
                        }
                    }
                }
                KeyCode::Char('r') => app.rename = !app.rename,
                KeyCode::Char('f') => app.force = !app.force,
                KeyCode::Char('d') => app.dry_run = !app.dry_run,
                _ => {}
            }
            app.sync_list_state();
        }
        Screen::Planning => {
            if is_quit(&key) {
                return true;
            }
        }
        Screen::Confirm => match key.code {
            KeyCode::Esc => {
                app.screen = Screen::Browser;
                app.plan = None;
            }
            _ if is_quit(&key) => return true,
            KeyCode::Enter => start_import(app),
            _ => {}
        },
        Screen::Progress => {
            if is_quit(&key) {
                app.cancel.store(true, Ordering::SeqCst);
            }
        }
        Screen::Summary => match key.code {
            _ if is_quit(&key) || key.code == KeyCode::Enter => return true,
            KeyCode::Up => app.summary_scroll = app.summary_scroll.saturating_sub(1),
            KeyCode::Down => app.summary_scroll += 1,
            _ => {}
        },
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

fn start_import(app: &mut App) {
    let Some(plan) = app.plan.take() else { return };
    let index = load_index().unwrap_or_default();
    let cancel = app.cancel.clone();
    let (tx, rx) = mpsc::channel();
    let (tx_sum, rx_sum) = mpsc::channel();
    app.import_rx = Some(rx);
    app.summary_rx = Some(rx_sum);
    app.progress = Some(crate::import::Progress::default());
    app.screen = Screen::Progress;
    let dry_run = app.dry_run;
    std::thread::spawn(move || {
        match run(&plan, &index, dry_run, &cancel, tx) {
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
    // Planning finished?
    if app.screen == Screen::Planning {
        if let Some(hash_rx) = &app.hash_rx {
            let mut latest: Option<HashProgress> = None;
            while let Ok(p) = hash_rx.try_recv() {
                latest = Some(p);
            }
            if let Some(p) = latest {
                app.hash_progress = p;
            }
        }
        if let Some(rx) = &app.plan_rx {
            if let Ok(result) = rx.try_recv() {
                app.plan_rx = None;
                app.hash_rx = None;
                match result {
                    Ok(plan) => {
                        let st = plan.stats();
                        if st.copy_count == 0 && st.skip_count > 0 {
                            // Everything already imported — straight to summary.
                            app.summary = Some(crate::import::Summary {
                                copied: 0,
                                copied_bytes: 0,
                                skipped: plan
                                    .files
                                    .iter()
                                    .filter_map(|f| match &f.action {
                                        Action::SkipDuplicate { known } => {
                                            Some(crate::import::Skipped {
                                                original_name: f.original_name.clone(),
                                                size: f.size,
                                                known: known.clone(),
                                            })
                                        }
                                        _ => None,
                                    })
                                    .collect(),
                                errors: Vec::new(),
                                cancelled: false,
                            });
                            app.screen = Screen::Summary;
                            return;
                        }
                        // Free-space check before asking to confirm.
                        if let (Some(dest), false) = (app.dest.clone(), app.dry_run) {
                            if let Some(free) = free_bytes(&dest) {
                                if free < st.copy_bytes + 1024 * 1024 {
                                    app.error = Some(format!(
                                        "Not enough free space: need {:.1} GB, have {:.1} GB",
                                        gb(st.copy_bytes),
                                        gb(free)
                                    ));
                                    app.screen = Screen::Browser;
                                    return;
                                }
                            }
                        }
                        app.plan = Some(plan);
                        app.screen = Screen::Confirm;
                    }
                    Err(e) => {
                        app.error = Some(format!("planning failed: {e:#}"));
                        app.screen = Screen::Browser;
                    }
                }
            }
        }
    }

    // Import progress updates (draw only; screen change happens when the
    // summary arrives on summary_rx).
    if app.screen == Screen::Progress {
        if let Some(rx) = &app.import_rx {
            let mut latest: Option<crate::import::Progress> = None;
            while let Ok(p) = rx.try_recv() {
                latest = Some(p);
            }
            if let Some(p) = latest {
                app.progress = Some(p);
            }
        }
    }

    // Collect the finished import thread's summary.
    if let Some(summary_rx) = &app.summary_rx {
        if let Ok(summary) = summary_rx.try_recv() {
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
    }
}
