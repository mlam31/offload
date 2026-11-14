use std::fs;
use std::io::{BufWriter, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use crate::dedup::sha256_file;
use crate::plan::{Action, Plan, PlannedFile};

/// Live progress sent from the copy thread to the UI.
#[derive(Debug, Clone, Default)]
pub struct Progress {
    pub total_files: usize,
    pub done_files: usize,
    pub total_bytes: u64,
    pub copied_bytes: u64,
    pub current: String,
    pub finished: bool,
    pub cancelled: bool,
    #[allow(dead_code)] // reserved for UI error surfacing
    pub error: Option<String>,
}

/// Final outcome of a run.
#[derive(Default)]
pub struct Summary {
    pub copied: usize,
    pub copied_bytes: u64,
    pub skipped: Vec<Skipped>,
    pub errors: Vec<String>,
    pub cancelled: bool,
}

pub struct Skipped {
    pub original_name: String,
    pub size: u64,
    pub known: Option<String>,
}

/// Largest run of failed copies we tolerate before concluding the
/// destination stopped responding.
const MAX_CONSECUTIVE_ERRORS: usize = 25;
const DEST_UNRESPONSIVE: &str = "ABORTED: the destination stopped responding after repeated \
     failures — is the SSD still connected? Files already copied \
     are kept and indexed; re-run when the drive is back.";

/// Execute a plan. Copy-only: there is no delete or move path anywhere.
/// Files are streamed to the destination while their SHA-256 is computed in
/// the same read pass, then size-verified. Every successful copy is appended
/// to the persistent index immediately, so an interrupted run can never
/// produce duplicates when re-run.
pub fn run(
    plan: &Plan,
    index_path: &Path,
    dry_run: bool,
    cancel: &AtomicBool,
    tx: Sender<Progress>,
) -> Result<Summary> {
    let copies: Vec<&PlannedFile> = plan.files.iter().filter(|f| f.is_copy()).collect();
    let mut progress = Progress {
        total_files: copies.len(),
        total_bytes: copies.iter().map(|f| f.size).sum(),
        ..Default::default()
    };
    let mut summary = Summary {
        skipped: collect_skips(plan),
        ..Default::default()
    };

    if dry_run {
        progress.finished = true;
        let _ = tx.send(progress);
        return Ok(summary);
    }

    run_copies(&copies, index_path, cancel, &tx, &mut progress, &mut summary);
    progress.finished = true;
    progress.cancelled = summary.cancelled;
    let _ = tx.send(progress);
    Ok(summary)
}

/// Copy every planned file, tracking progress and halting on cancel or when
/// the destination clearly stops responding.
fn run_copies(
    copies: &[&PlannedFile],
    index_path: &Path,
    cancel: &AtomicBool,
    tx: &Sender<Progress>,
    progress: &mut Progress,
    summary: &mut Summary,
) {
    let mut consecutive_errors = 0usize;
    for f in copies {
        if cancel.load(Ordering::SeqCst) {
            summary.cancelled = true;
            break;
        }
        let Action::Copy { dest } = &f.action else { unreachable!() };
        progress.current = f.original_name.clone();
        let _ = tx.send(progress.clone());

        match copy_one(&f.source, dest, f.hash.as_deref()) {
            Ok(outcome) => {
                consecutive_errors = 0;
                record_outcome(outcome, f, dest, index_path, progress, summary);
            }
            Err(e) => {
                summary.errors.push(format!("{}: {e:#}", f.original_name));
                consecutive_errors += 1;
                if consecutive_errors >= MAX_CONSECUTIVE_ERRORS {
                    summary.errors.push(DEST_UNRESPONSIVE.to_string());
                    break;
                }
            }
        }
        let _ = tx.send(progress.clone());
    }
}

/// Record (as warnings) every file the plan says not to copy.
fn collect_skips(plan: &Plan) -> Vec<Skipped> {
    plan.files
        .iter()
        .filter_map(|f| match &f.action {
            Action::SkipDuplicate { known } => Some(Skipped {
                original_name: f.original_name.clone(),
                size: f.size,
                known: known.clone(),
            }),
            _ => None,
        })
        .collect()
}

/// Account for one finished copy attempt and index its content either way,
/// so future runs dedup against it.
fn record_outcome(
    outcome: Outcome,
    f: &PlannedFile,
    dest: &Path,
    index_path: &Path,
    progress: &mut Progress,
    summary: &mut Summary,
) {
    let hash = match &outcome {
        Outcome::Copied(hash) => hash.clone(),
        Outcome::AlreadyThere(hash) => hash.clone().unwrap_or_default(),
    };
    let dest_str = dest.to_string_lossy().into_owned();
    match outcome {
        Outcome::Copied(_) => {
            summary.copied += 1;
            summary.copied_bytes += f.size;
            progress.copied_bytes += f.size;
            progress.done_files += 1;
        }
        Outcome::AlreadyThere(_) => {
            // Same content already at destination — not a new copy.
            progress.done_files += 1;
            summary.skipped.push(Skipped {
                original_name: f.original_name.clone(),
                size: f.size,
                known: Some(dest_str.clone()),
            });
        }
    }
    let entry = crate::config::IndexEntry {
        hash,
        size: f.size,
        media_type: format!("{:?}", f.media_type),
        original_name: f.original_name.clone(),
        dest: dest_str,
        imported_at: jiff::Timestamp::now().to_string(),
    };
    if let Err(e) = crate::config::append_index_to(index_path, &entry) {
        summary.errors.push(format!("index write failed: {e:#}"));
    }
}

#[derive(Debug)]
enum Outcome {
    /// New copy landed; hash computed during the copy.
    Copied(String),
    /// Identical content already at destination; hash if known.
    AlreadyThere(Option<String>),
}

fn copy_one(source: &Path, dest: &Path, expected_hash: Option<&str>) -> Result<Outcome> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }

    // Airtight no-clobber guarantee: if anything now sits at the destination
    // path, it is either identical content (skip) or a hard error — never an
    // overwrite.
    let source_size = fs::metadata(source)?.len();
    if let Ok(md) = fs::metadata(dest) {
        if let Some(expected) = expected_hash {
            if md.len() == source_size && sha256_file(dest)? == expected {
                return Ok(Outcome::AlreadyThere(Some(expected.to_string())));
            }
        }
        anyhow::bail!(
            "destination already exists, refusing to touch it: {}",
            dest.display()
        );
    }

    let hash = stream_copy(source, dest, source_size)?;
    Ok(Outcome::Copied(hash))
}

/// Streamed copy with inline hashing: one read pass for both. The copy is
/// only "done" if the written length matches the source exactly.
fn stream_copy(source: &Path, dest: &Path, source_size: u64) -> Result<String> {
    let source_file = fs::File::open(source)
        .with_context(|| format!("reading {}", source.display()))?;
    let mut reader = std::io::BufReader::with_capacity(1024 * 1024, source_file);
    let dest_file = fs::File::create(dest)
        .with_context(|| format!("writing {}", dest.display()))?;
    let mut writer = BufWriter::with_capacity(1024 * 1024, dest_file);
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    let mut written: u64 = 0;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        writer
            .write_all(&buf[..n])
            .with_context(|| format!("writing {}", dest.display()))?;
        hasher.update(&buf[..n]);
        written += n as u64;
    }
    writer.flush()?;
    drop(writer);

    // Verify the copy landed complete.
    let md = fs::metadata(dest)
        .with_context(|| format!("verifying {}", dest.display()))?;
    if md.len() != written || written != source_size {
        anyhow::bail!("size mismatch after copy: {}", dest.display());
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::MediaType;
    use std::sync::mpsc;
    use tempfile::TempDir;

    fn planned_copy(source: &Path, dest: &Path, size: u64, hash: Option<String>) -> PlannedFile {
        PlannedFile {
            source: source.to_path_buf(),
            original_name: source
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            media_type: MediaType::Jpeg,
            size,
            hash,
            date: None,
            action: Action::Copy {
                dest: dest.to_path_buf(),
            },
        }
    }

    fn last_progress(rx: &mpsc::Receiver<Progress>) -> Progress {
        let mut last = Progress::default();
        while let Ok(p) = rx.try_recv() {
            last = p;
        }
        last
    }

    #[test]
    fn copy_one_copies_and_hashes() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("src.jpg");
        std::fs::write(&source, "hello").unwrap();
        let dest = tmp.path().join("out").join("dst.jpg");

        let outcome = copy_one(&source, &dest, None).unwrap();
        match outcome {
            Outcome::Copied(hash) => {
                assert_eq!(hash, crate::dedup::sha256_file(&source).unwrap());
                assert_eq!(std::fs::read(&dest).unwrap(), b"hello");
            }
            _ => panic!("expected a fresh copy"),
        }
    }

    #[test]
    fn copy_one_skips_identical_content_at_destination() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("src.jpg");
        std::fs::write(&source, "hello").unwrap();
        let dest = tmp.path().join("dst.jpg");
        std::fs::write(&dest, "hello").unwrap();

        let hash = crate::dedup::sha256_file(&source).unwrap();
        match copy_one(&source, &dest, Some(&hash)).unwrap() {
            Outcome::AlreadyThere(known) => assert_eq!(known.as_deref(), Some(hash.as_str())),
            _ => panic!("expected AlreadyThere"),
        }
        // Content untouched.
        assert_eq!(std::fs::read(&dest).unwrap(), b"hello");
    }

    #[test]
    fn copy_one_never_overwrites_different_content() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("src.jpg");
        std::fs::write(&source, "hello").unwrap();
        let dest = tmp.path().join("dst.jpg");
        std::fs::write(&dest, "something else").unwrap();

        let err = copy_one(&source, &dest, None).unwrap_err();
        assert!(err.to_string().contains("refusing"), "{err}");
        assert_eq!(std::fs::read(&dest).unwrap(), b"something else");
    }

    #[test]
    fn copy_one_fails_when_source_is_missing() {
        let tmp = TempDir::new().unwrap();
        let err = copy_one(&tmp.path().join("missing"), &tmp.path().join("dst.jpg"), None)
            .unwrap_err();
        assert!(!err.to_string().is_empty());
    }

    #[test]
    fn dry_run_records_skips_and_copies_nothing() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("a.jpg");
        std::fs::write(&source, "data").unwrap();
        let dest = tmp.path().join("out").join("a.jpg");
        let plan = Plan {
            files: vec![
                planned_copy(&source, &dest, 4, None),
                PlannedFile {
                    source: source.clone(),
                    original_name: "b.jpg".into(),
                    media_type: MediaType::Jpeg,
                    size: 9,
                    hash: None,
                    date: None,
                    action: Action::SkipDuplicate { known: Some("/old".into()) },
                },
            ],
        };
        let (tx, rx) = mpsc::channel();
        let summary = run(&plan, &tmp.path().join("index.json"), true, &AtomicBool::new(false), tx).unwrap();
        assert_eq!(summary.copied, 0);
        assert_eq!(summary.skipped.len(), 1);
        assert_eq!(summary.skipped[0].original_name, "b.jpg");
        assert!(last_progress(&rx).finished);
        assert!(!dest.exists());
    }

    #[test]
    fn run_copies_and_indexes_every_success() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("a.jpg");
        std::fs::write(&source, "payload").unwrap();
        let dest = tmp.path().join("out").join("a.jpg");
        let index_path = tmp.path().join("index.json");
        let plan = Plan {
            files: vec![planned_copy(&source, &dest, 7, None)],
        };

        let (tx, rx) = mpsc::channel();
        let summary = run(&plan, &index_path, false, &AtomicBool::new(false), tx).unwrap();

        assert_eq!(summary.copied, 1);
        assert_eq!(summary.copied_bytes, 7);
        assert!(summary.errors.is_empty());
        assert_eq!(std::fs::read(&dest).unwrap(), b"payload");
        let p = last_progress(&rx);
        assert!(p.finished && p.done_files == 1 && p.total_files == 1 && p.copied_bytes == 7);

        let index = crate::config::load_index_from(&index_path).unwrap();
        assert_eq!(index.entries.len(), 1);
        assert_eq!(index.entries[0].dest, dest.to_string_lossy());
        assert_eq!(index.entries[0].size, 7);
    }

    #[test]
    fn cancelled_run_copies_nothing() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("a.jpg");
        std::fs::write(&source, "payload").unwrap();
        let dest = tmp.path().join("out").join("a.jpg");
        let plan = Plan {
            files: vec![planned_copy(&source, &dest, 7, None)],
        };
        let (tx, _rx) = mpsc::channel();
        let summary = run(&plan, &tmp.path().join("index.json"), false, &AtomicBool::new(true), tx).unwrap();
        assert!(summary.cancelled);
        assert_eq!(summary.copied, 0);
        assert!(!dest.exists());
    }

    #[test]
    fn run_aborts_after_repeated_destination_failures() {
        let tmp = TempDir::new().unwrap();
        // 30 planned copies whose sources do not exist → 30 straight errors;
        // the loop must stop hammering after 25 and report once.
        let files: Vec<PlannedFile> = (0..30)
            .map(|i| {
                planned_copy(
                    &tmp.path().join(format!("missing-{i}.jpg")),
                    &tmp.path().join(format!("out/{i}.jpg")),
                    1,
                    None,
                )
            })
            .collect();
        let plan = Plan { files };
        let (tx, _rx) = mpsc::channel();
        let summary = run(&plan, &tmp.path().join("index.json"), false, &AtomicBool::new(false), tx).unwrap();
        assert!(summary.errors.iter().any(|e| e.contains("ABORTED")));
        assert!(summary.errors.len() <= MAX_CONSECUTIVE_ERRORS + 1);
        assert!(summary.copied == 0 && !summary.cancelled);
    }
}
