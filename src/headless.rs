use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;

use anyhow::{bail, Context, Result};

use crate::config::{index_path, load_index, save_last_dest, Index};
use crate::detect::is_within;
use crate::disk::free_bytes;
use crate::import::{run, Summary};
use crate::plan::{build_plan, date_folder, Plan, PlanOptions, PlanStats};
use crate::scan::{scan_source, MediaFile};

pub struct HeadlessArgs {
    pub src: PathBuf,
    pub dest: PathBuf,
    pub dry_run: bool,
    pub rename: bool,
    pub force: bool,
    pub eject: bool,
}

/// Non-interactive mode: scan, hash, plan, copy, report.
pub fn run_headless(args: &HeadlessArgs) -> Result<()> {
    validate_args(args)?;
    let files = scan_source(&args.src)
        .with_context(|| format!("scanning {}", args.src.display()))?;
    if files.is_empty() {
        println!("No supported media found on {}.", args.src.display());
        return Ok(());
    }
    println!(
        "Found {} media files ({:.1} GB). Fingerprinting for dedup…",
        files.len(),
        gb(files.iter().map(|f| f.size).sum())
    );

    let index = load_index()?;
    let (plan, st) = build_plan_with_progress(files, &index, &args.dest, plan_opts(args))?;
    ensure_space(args, &st)?;
    print_plan(args, &plan, &st);

    if args.dry_run {
        println!();
        println!("Dry run — nothing was copied.");
        print_skips(&plan);
        return Ok(());
    }

    let summary = execute_import(plan)?;
    save_last_dest(&args.dest)?;
    report(&summary);
    if args.eject && !summary.cancelled && summary.errors.is_empty() {
        eject_volumes(args);
    }
    Ok(())
}

fn validate_args(args: &HeadlessArgs) -> Result<()> {
    if !args.src.exists() {
        bail!("source does not exist: {}", args.src.display());
    }
    if !args.dest.exists() {
        bail!("destination does not exist: {}", args.dest.display());
    }
    if is_within(&args.src, &args.dest) || is_within(&args.dest, &args.src) {
        bail!("source and destination must not overlap");
    }
    Ok(())
}

fn plan_opts(args: &HeadlessArgs) -> PlanOptions {
    PlanOptions {
        rename: args.rename,
        force: args.force,
    }
}

/// Build the plan while a hash progress bar renders on stderr.
fn build_plan_with_progress(
    files: Vec<MediaFile>,
    index: &Index,
    dest: &Path,
    opts: PlanOptions,
) -> Result<(Plan, PlanStats)> {
    let (hash_tx, hash_rx) = mpsc::channel();
    let printer = std::thread::spawn(move || {
        for (done, total) in hash_rx {
            if total == 0 {
                continue;
            }
            let width = 30;
            let filled = (done as f64 / total as f64 * width as f64).round() as usize;
            let bar = "█".repeat(filled) + &"░".repeat(width - filled);
            eprint!("\r  [{bar}] {done}/{total} files hashed  ");
        }
    });
    let plan = build_plan(files, index, dest, opts, Some(&hash_tx));
    drop(hash_tx); // ends the printer thread
    let _ = printer.join();
    eprintln!();
    let plan = plan?;
    let st = plan.stats();
    Ok((plan, st))
}

fn ensure_space(args: &HeadlessArgs, st: &PlanStats) -> Result<()> {
    if st.copy_count == 0 {
        return Ok(());
    }
    let Some(free) = free_bytes(&args.dest) else {
        return Ok(());
    };
    if free < st.copy_bytes + 1024 * 1024 {
        bail!(
            "not enough free space on {}: need {:.1} GB, have {:.1} GB",
            args.dest.display(),
            gb(st.copy_bytes),
            gb(free)
        );
    }
    Ok(())
}

fn print_plan(args: &HeadlessArgs, plan: &Plan, st: &PlanStats) {
    println!();
    println!("Plan for {}:", args.dest.display());
    for date in &st.dates {
        let count = count_for_date(plan, *date);
        println!("  {}/  {count} files", date_folder(*date));
    }
    println!(
        "Will copy {} files ({:.1} GB); skip {} already imported ({:.1} GB).",
        st.copy_count,
        gb(st.copy_bytes),
        st.skip_count,
        gb(st.skip_bytes)
    );
}

fn count_for_date(plan: &Plan, date: jiff::civil::Date) -> usize {
    plan.files
        .iter()
        .filter(|f| f.is_copy() && f.date == Some(date))
        .count()
}

/// Run the import on a worker thread, forwarding progress lines to stderr.
/// Takes ownership of the plan: the worker thread must own its data.
fn execute_import(plan: Plan) -> Result<Summary> {
    let index_path = index_path()?;
    let cancel = AtomicBool::new(false);
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || run(&plan, &index_path, false, &cancel, tx));
    for progress in rx {
        if !progress.current.is_empty() {
            eprint!(
                "\r[{}/{}] {:.0}%  {}          ",
                progress.done_files,
                progress.total_files,
                pct(progress.copied_bytes, progress.total_bytes),
                ellipsize(&progress.current, 40)
            );
        }
    }
    handle
        .join()
        .map_err(|_| anyhow::anyhow!("import thread panicked"))?
}

fn report(summary: &Summary) {
    println!();
    println!(
        "Copied {} files ({:.1} GB).",
        summary.copied,
        gb(summary.copied_bytes)
    );
    print_summary_skips(summary);
    print_errors(summary);
}

fn print_summary_skips(summary: &Summary) {
    if summary.skipped.is_empty() {
        return;
    }
    println!();
    println!("⚠ Already imported — skipped:");
    for s in &summary.skipped {
        match &s.known {
            Some(known) => println!(
                "  {}  →  {}  ({:.1} MB)",
                s.original_name,
                known,
                mb(s.size)
            ),
            None => println!("  {}  ({:.1} MB)", s.original_name, mb(s.size)),
        }
    }
}

fn print_errors(summary: &Summary) {
    if summary.errors.is_empty() {
        return;
    }
    println!();
    println!("Errors:");
    for e in &summary.errors {
        println!("  {e}");
    }
}

fn print_skips(plan: &Plan) {
    let skips: Vec<(String, u64, Option<String>)> = plan
        .files
        .iter()
        .filter_map(|f| match &f.action {
            crate::plan::Action::SkipDuplicate { known } => {
                Some((f.original_name.clone(), f.size, known.clone()))
            }
            _ => None,
        })
        .collect();
    if skips.is_empty() {
        return;
    }
    println!();
    println!("⚠ Already imported — will be skipped:");
    for (name, size, known) in skips {
        match known {
            Some(k) => println!("  {name}  →  {k}  ({:.1} MB)", mb(size)),
            None => println!("  {name}  ({:.1} MB)", mb(size)),
        }
    }
}

fn eject_volumes(args: &HeadlessArgs) {
    println!();
    println!(
        "card: {}",
        crate::eject::eject_if_unused(&args.src).message
    );
    println!(
        "SSD:  {}",
        crate::eject::eject_if_unused(&args.dest).message
    );
}

pub fn gb(bytes: u64) -> f64 {
    bytes as f64 / 1_073_741_824.0
}

pub fn mb(bytes: u64) -> f64 {
    bytes as f64 / 1_048_576.0
}

pub(crate) fn pct(done: u64, total: u64) -> f64 {
    if total == 0 {
        100.0
    } else {
        done as f64 / total as f64 * 100.0
    }
}

/// Keep the tail of `s`, at most `max` chars, prefixed with an ellipsis.
/// Char-safe: never panics on multi-byte names.
pub(crate) fn ellipsize(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    let cut: String = chars[chars.len() - (max - 1)..].iter().collect();
    format!("…{cut}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gb_and_mb_convert() {
        assert!((gb(1_073_741_824) - 1.0).abs() < 1e-9);
        assert!((mb(1_048_576) - 1.0).abs() < 1e-9);
        assert!((gb(0)).abs() < 1e-9);
    }

    #[test]
    fn pct_is_full_when_nothing_is_expected() {
        assert_eq!(pct(0, 0), 100.0);
        assert_eq!(pct(5, 0), 100.0);
        assert!((pct(50, 100) - 50.0).abs() < 1e-9);
        assert!((pct(100, 100) - 100.0).abs() < 1e-9);
    }

    #[test]
    fn ellipsize_passes_short_strings_through() {
        assert_eq!(ellipsize("DSC001.JPG", 40), "DSC001.JPG");
        assert_eq!(ellipsize("", 40), "");
    }

    #[test]
    fn ellipsize_truncates_long_strings_keeping_the_tail() {
        let long = "abcdefghij".repeat(10); // 100 chars
        let cut = ellipsize(&long, 40);
        assert_eq!(cut.chars().count(), 40);
        assert!(cut.starts_with('…'));
        assert!(long.ends_with(cut.trim_start_matches('…')));
    }

    #[test]
    fn ellipsize_is_multibyte_safe() {
        // The old byte-sliced version panicked exactly on inputs like this.
        let name = "Sony ⠋⠙⠹⠸ camcorder 📷 file name.mov";
        let cut = ellipsize(name, 10);
        assert_eq!(cut.chars().count(), 10);
        assert!(cut.starts_with('…'));
        assert!(name.ends_with(cut.trim_start_matches('…')));
    }
}
