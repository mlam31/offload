use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;

use anyhow::{bail, Context, Result};

use crate::config::{load_index, save_last_dest};
use crate::detect::is_within;
use crate::disk::free_bytes;
use crate::import::run;
use crate::plan::{build_plan, PlanOptions};
use crate::scan::scan_source;

pub struct HeadlessArgs {
    pub src: PathBuf,
    pub dest: PathBuf,
    pub dry_run: bool,
    pub rename: bool,
    pub force: bool,
}

/// Non-interactive mode: scan, hash, plan, copy, report.
pub fn run_headless(args: &HeadlessArgs) -> Result<()> {
    if !args.src.exists() {
        bail!("source does not exist: {}", args.src.display());
    }
    if !args.dest.exists() {
        bail!("destination does not exist: {}", args.dest.display());
    }
    if is_within(&args.src, &args.dest) || is_within(&args.dest, &args.src) {
        bail!("source and destination must not overlap");
    }

    println!("Scanning {} …", args.src.display());
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
    let opts = PlanOptions {
        rename: args.rename,
        force: args.force,
    };
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
    let plan = build_plan(files, &index, &args.dest, opts, Some(&hash_tx));
    drop(hash_tx); // ends the printer thread
    let _ = printer.join();
    eprintln!();
    let plan = plan?;
    let st = plan.stats();

    if st.copy_count > 0 {
        if let Some(free) = free_bytes(&args.dest) {
            if free < st.copy_bytes + 1024 * 1024 {
                bail!(
                    "not enough free space on {}: need {:.1} GB, have {:.1} GB",
                    args.dest.display(),
                    gb(st.copy_bytes),
                    gb(free)
                );
            }
        }
    }

    println!();
    println!("Plan for {}:", args.dest.display());
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
        println!("  {folder}/  {count} files");
    }
    println!(
        "Will copy {} files ({:.1} GB); skip {} already imported ({:.1} GB).",
        st.copy_count,
        gb(st.copy_bytes),
        st.skip_count,
        gb(st.skip_bytes)
    );

    if args.dry_run {
        println!();
        println!("Dry run — nothing was copied.");
        print_skips(&plan);
        return Ok(());
    }

    let cancel = AtomicBool::new(false);
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || run(&plan, &index, false, &cancel, tx));
    // Progress lines from the copy thread.
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
    let summary = handle
        .join()
        .map_err(|_| anyhow::anyhow!("import thread panicked"))??;
    save_last_dest(&args.dest)?;

    println!();
    println!(
        "Copied {} files ({:.1} GB).",
        summary.copied,
        gb(summary.copied_bytes)
    );
    if !summary.skipped.is_empty() {
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
    if !summary.errors.is_empty() {
        println!();
        println!("Errors:");
        for e in &summary.errors {
            println!("  {e}");
        }
    }
    Ok(())
}

fn print_skips(plan: &crate::plan::Plan) {
    let skips: Vec<_> = plan
        .files
        .iter()
        .filter_map(|f| match &f.action {
            crate::plan::Action::SkipDuplicate { known } => {
                Some((f.original_name.clone(), f.size, known.clone()))
            }
            _ => None,
        })
        .collect();
    if !skips.is_empty() {
        println!();
        println!("⚠ Already imported — will be skipped:");
        for (name, size, known) in skips {
            match known {
                Some(k) => println!("  {name}  →  {k}  ({:.1} MB)", mb(size)),
                None => println!("  {name}  ({:.1} MB)", mb(size)),
            }
        }
    }
}

pub fn gb(bytes: u64) -> f64 {
    bytes as f64 / 1_073_741_824.0
}
pub fn mb(bytes: u64) -> f64 {
    bytes as f64 / 1_048_576.0
}fn pct(done: u64, total: u64) -> f64 {
    if total == 0 {
        100.0
    } else {
        done as f64 / total as f64 * 100.0
    }
}
fn ellipsize(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[s.len() - max + 1..])
    }
}
