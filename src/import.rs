use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;

use anyhow::{Context, Result};

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
/// Files are copied with fs::copy, then verified by size. Every successful
/// copy is appended to the persistent index immediately, so an interrupted
/// run can never produce duplicates when re-run.
pub fn run(
    plan: &Plan,
    _index: &Index, // kept for symmetry with the TUI call site
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

        match copy_one(&f.source, dest, &f.hash) {
            Ok(Imported::Copied) => {
                summary.copied += 1;
                summary.copied_bytes += f.size;
                progress.copied_bytes += f.size;
                progress.done_files += 1;
                let entry = crate::config::IndexEntry {
                    hash: f.hash.clone(),
                    size: f.size,
                    media_type: format!("{:?}", f.media_type),
                    original_name: f.original_name.clone(),
                    dest: dest.to_string_lossy().into_owned(),
                    imported_at: jiff::Timestamp::now().to_string(),
                };
                if let Err(e) = append_index(&entry) {
                    summary
                        .errors
                        .push(format!("index write failed: {e:#}"));
                }
            }
            Ok(Imported::AlreadyThere) => {
                // Same content already at destination — not a new copy.
                progress.done_files += 1;
                summary.skipped.push(Skipped {
                    original_name: f.original_name.clone(),
                    size: f.size,
                    known: Some(dest.to_string_lossy().into_owned()),
                });
            }
            Err(e) => summary.errors.push(format!(
                "{}: {e:#}",
                f.original_name
            )),
        }
        let _ = tx.send(progress.clone());
    }

    if !summary.cancelled {
        // Keep any partial progress; index already persisted per file.
    }
    progress.finished = true;
    progress.cancelled = summary.cancelled;
    let _ = tx.send(progress);
    Ok(summary)
}

enum Imported {
    Copied,
    AlreadyThere,
}

fn copy_one(source: &Path, dest: &PathBuf, expected_hash: &str) -> Result<Imported> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }

    // Belt and braces: destination exists with identical content → skip.
    if let Ok(md) = fs::metadata(dest) {
        if md.len() == sha_size(source)? && sha256_file(dest)? == expected_hash {
            return Ok(Imported::AlreadyThere);
        }
    }

    let copied = fs::copy(source, dest)
        .with_context(|| format!("copying to {}", dest.display()))?;

    // Verify the copy landed complete.
    let md = fs::metadata(dest)
        .with_context(|| format!("verifying {}", dest.display()))?;
    if md.len() != copied {
        anyhow::bail!("size mismatch after copy: {}", dest.display());
    }
    Ok(Imported::Copied)
}

/// Cheap size read used for verification.
fn sha_size(path: &Path) -> Result<u64> {
    Ok(fs::metadata(path)?.len())
}
