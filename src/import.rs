use std::fs;
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use crate::config::{append_index, Index};
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

/// Execute a plan. Copy-only: there is no delete or move path anywhere.
/// Files are streamed to the destination while their SHA-256 is computed in
/// the same read pass, then size-verified. Every successful copy is appended
/// to the persistent index immediately, so an interrupted run can never
/// produce duplicates when re-run.
pub fn run(
    plan: &Plan,
    _index: &Index,
    dry_run: bool,
    cancel: &AtomicBool,
    tx: Sender<Progress>,
) -> Result<Summary> {
    let copies: Vec<&PlannedFile> = plan.files.iter().filter(|f| f.is_copy()).collect();
    let total_bytes: u64 = copies.iter().map(|f| f.size).sum();
    let mut progress = Progress {
        total_files: copies.len(),
        total_bytes,
        ..Default::default()
    };
    let mut consecutive_errors = 0usize;
    let mut summary = Summary {
        copied: 0,
        copied_bytes: 0,
        skipped: Vec::new(),
        errors: Vec::new(),
        cancelled: false,
    };

    // Record skips (warnings) for everything that is not copied.
    for f in &plan.files {
        if let Action::SkipDuplicate { known } = &f.action {
            summary.skipped.push(Skipped {
                original_name: f.original_name.clone(),
                size: f.size,
                known: known.clone(),
            });
        }
    }

    if dry_run {
        progress.finished = true;
        let _ = tx.send(progress);
        return Ok(summary);
    }

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
                // Index the content either way so future runs dedup against it.
                let entry = crate::config::IndexEntry {
                    hash,
                    size: f.size,
                    media_type: format!("{:?}", f.media_type),
                    original_name: f.original_name.clone(),
                    dest: dest_str,
                    imported_at: jiff::Timestamp::now().to_string(),
                };
                if let Err(e) = append_index(&entry) {
                    summary.errors.push(format!("index write failed: {e:#}"));
                }
            }
            Err(e) => {
                summary.errors.push(format!("{}: {e:#}", f.original_name));
                consecutive_errors += 1;
                // If the destination device has clearly vanished (unmount,
                // cable pull, USB drop), stop hammering it and report once.
                if consecutive_errors >= 25 {
                    summary.errors.push(
                        "ABORTED: the destination stopped responding after repeated \
                         failures — is the SSD still connected? Files already copied \
                         are kept and indexed; re-run when the drive is back."
                            .to_string(),
                    );
                    break;
                }
            }
        }
        let _ = tx.send(progress.clone());
    }

    progress.finished = true;
    progress.cancelled = summary.cancelled;
    let _ = tx.send(progress);
    Ok(summary)
}

enum Outcome {
    /// New copy landed; hash computed during the copy.
    Copied(String),
    /// Identical content already at destination; hash if known.
    AlreadyThere(Option<String>),
}

fn copy_one(source: &Path, dest: &PathBuf, expected_hash: Option<&str>) -> Result<Outcome> {
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

    // Streamed copy with inline hashing: one read pass for both.
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
    Ok(Outcome::Copied(format!("{:x}", hasher.finalize())))
}
