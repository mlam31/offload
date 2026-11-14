use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use anyhow::Result;
use jiff::civil::Date;

use crate::config::{find_by_hash, Index};
use crate::dedup::sha256_file;
use crate::media::MediaType;
use crate::scan::MediaFile;

/// What will happen to one card file when the plan runs.
#[derive(Debug, Clone)]
pub enum Action {
    /// Copy to this absolute destination path.
    Copy { dest: PathBuf },
    /// Content already imported (or duplicated later in this same batch).
    SkipDuplicate { known: Option<String> },
}

#[derive(Debug, Clone)]
pub struct PlannedFile {
    pub source: PathBuf,
    pub original_name: String,
    pub media_type: MediaType,
    pub size: u64,
    /// Content hash when we computed one. `None` means the size is unique on
    /// the card AND absent from the import index, so the file provably cannot
    /// be a duplicate — the hash is computed during the copy instead.
    pub hash: Option<String>,
    pub date: Option<Date>,
    pub action: Action,
}

impl PlannedFile {
    pub fn is_copy(&self) -> bool {
        matches!(self.action, Action::Copy { .. })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PlanOptions {
    pub rename: bool,
    pub force: bool,
}

/// A full import plan: every card file resolved to Copy or SkipDuplicate.
pub struct Plan {
    pub files: Vec<PlannedFile>,
}

pub struct PlanStats {
    pub copy_count: usize,
    pub copy_bytes: u64,
    pub skip_count: usize,
    pub skip_bytes: u64,
    pub by_type: HashMap<MediaType, (usize, u64)>,
    pub dates: Vec<Date>,
}

impl Plan {
    pub fn stats(&self) -> PlanStats {
        let mut st = PlanStats {
            copy_count: 0,
            copy_bytes: 0,
            skip_count: 0,
            skip_bytes: 0,
            by_type: HashMap::new(),
            dates: Vec::new(),
        };
        let mut seen_dates: Vec<Date> = Vec::new();
        for f in &self.files {
            match &f.action {
                Action::Copy { dest: _ } => {
                    st.copy_count += 1;
                    st.copy_bytes += f.size;
                    let e = st.by_type.entry(f.media_type).or_insert((0, 0));
                    e.0 += 1;
                    e.1 += f.size;
                    if let Some(d) = f.date {
                        if !seen_dates.contains(&d) {
                            seen_dates.push(d);
                        }
                    }
                }
                Action::SkipDuplicate { .. } => {
                    st.skip_count += 1;
                    st.skip_bytes += f.size;
                }
            }
        }
        seen_dates.sort();
        st.dates = seen_dates;
        st
    }
}

/// Progress of the hashing phase: (files hashed so far, files that need it).
pub type HashProgress = (usize, usize);

/// Build the plan: hash only the files that could be duplicates (same size as
/// an index entry, or same size as another card file), resolve dedup, assign
/// destination names. Rename-on-import numbers files per (capture date, type)
/// in original camera order (Sony names are zero-padded, so plain sorting is
/// correct).
pub fn build_plan(
    mut files: Vec<MediaFile>,
    index: &Index,
    dest_root: &Path,
    opts: PlanOptions,
    hash_progress: Option<&Sender<HashProgress>>,
) -> Result<Plan> {
    let mut planned: Vec<PlannedFile> = Vec::with_capacity(files.len());

    // Files whose size matches an index entry could be re-imports.
    let index_sizes: Vec<u64> = index.entries.iter().map(|e| e.size).collect();

    // Files whose size matches another card file could be intra-batch dupes.
    files.sort_by(|a, b| a.name.cmp(&b.name));
    let mut size_counts: HashMap<u64, usize> = HashMap::new();
    for f in &files {
        *size_counts.entry(f.size).or_default() += 1;
    }

    let to_hash: Vec<usize> = files
        .iter()
        .enumerate()
        .filter(|(_, f)| index_sizes.contains(&f.size) || size_counts[&f.size] > 1)
        .map(|(i, _)| i)
        .collect();
    let total_hash = to_hash.len();
    let mut hashed_so_far = 0usize;
    if let Some(tx) = hash_progress {
        let _ = tx.send((0, total_hash));
    }

    let mut hashes: Vec<Option<String>> = vec![None; files.len()];
    for &i in &to_hash {
        match sha256_file(&files[i].path) {
            Ok(h) => hashes[i] = Some(h),
            Err(_) => hashes[i] = None, // unreadable: still copy, report later
        }
        hashed_so_far += 1;
        if let Some(tx) = hash_progress {
            let _ = tx.send((hashed_so_far, total_hash));
        }
    }

    // Hash → index of the first planned copy with that content, for
    // intra-batch dedup. Resolved to the final destination after numbering.
    let mut batch_hashes: HashMap<String, usize> = HashMap::new();
    let mut pending_dups: Vec<usize> = Vec::new();

    for (i, f) in files.iter().enumerate() {
        let hash = hashes[i].clone();

        if let (Some(h), false) = (hash.as_deref(), opts.force) {
            if let Some(known) = find_by_hash(index, h) {
                planned.push(PlannedFile {
                    source: f.path.clone(),
                    original_name: f.name.clone(),
                    media_type: f.media_type,
                    size: f.size,
                    hash,
                    date: f.date,
                    action: Action::SkipDuplicate {
                        known: Some(known.dest.clone()),
                    },
                });
                continue;
            }
        }

        // Intra-batch duplicate (same content appearing twice on the card).
        if let Some(h) = &hash {
            if batch_hashes.contains_key(h) {
                pending_dups.push(planned.len());
                planned.push(PlannedFile {
                    source: f.path.clone(),
                    original_name: f.name.clone(),
                    media_type: f.media_type,
                    size: f.size,
                    hash,
                    date: f.date,
                    action: Action::SkipDuplicate { known: None },
                });
                continue;
            }
        }

        let date = f.date;
        let dir = sub_dir(dest_root, date, f.media_type);
        let base = if opts.rename {
            seq_name(date, f.media_type, 0) // placeholder, fixed in numbering pass
        } else {
            f.name.clone()
        };
        let dest = unique_dest(&dir, &base, f.size, hash.as_deref());
        if let Some(h) = &hash {
            batch_hashes.insert(h.clone(), planned.len());
        }
        planned.push(PlannedFile {
            source: f.path.clone(),
            original_name: f.name.clone(),
            media_type: f.media_type,
            size: f.size,
            hash,
            date,
            action: Action::Copy { dest },
        });
    }

    // Numbering pass: group copies by (date, type) and assign 001, 002, ...
    if opts.rename {
        let mut groups: HashMap<(Option<Date>, MediaType), Vec<usize>> = HashMap::new();
        for (i, p) in planned.iter().enumerate() {
            if p.is_copy() {
                groups.entry((p.date, p.media_type)).or_default().push(i);
            }
        }
        // `planned` is sorted by original name, so group order = camera order.
        for (_, mut indices) in groups {
            indices.sort();
            for (n, i) in indices.into_iter().enumerate() {
                let date = planned[i].date;
                let name = seq_name(date, planned[i].media_type, n + 1);
                let dir = sub_dir(dest_root, date, planned[i].media_type);
                let hash = planned[i].hash.clone();
                let dest = unique_dest(&dir, &name, planned[i].size, hash.as_deref());
                planned[i].action = Action::Copy { dest };
            }
        }
    }

    // Resolve intra-batch duplicate references now that numbering is final.
    for dup_idx in pending_dups {
        let hash = planned[dup_idx].hash.clone();
        if let Some(h) = hash {
            if let Some(&first) = batch_hashes.get(&h) {
                if let Action::Copy { dest } = &planned[first].action {
                    planned[dup_idx].action = Action::SkipDuplicate {
                        known: Some(dest.to_string_lossy().into_owned()),
                    };
                }
            }
        }
    }

    Ok(Plan { files: planned })
}

/// <SSD>/DD-MM-YYYY/… with Clips/, Photos/JPEG/, Photos/ARW/ under it.
pub fn dest_dir(dest_root: &Path, date: Option<Date>) -> PathBuf {
    let Some(date) = date else {
        return dest_root.join("Unsorted");
    };
    let folder = format!(
        "{:02}-{:02}-{:04}",
        date.day(),
        date.month() as i64,
        date.year()
    );
    dest_root.join(folder)
}

#[allow(dead_code)]
pub fn sub_dir(dest_root: &Path, date: Option<Date>, media_type: MediaType) -> PathBuf {
    dest_dir(dest_root, date).join(media_type.label())
}

/// YYYY-MM-DD_NNN.ext — sortable, per capture date, per media type.
fn seq_name(date: Option<Date>, media_type: MediaType, n: usize) -> String {
    let (date_part, ext): (String, String) = match (date, media_type) {
        (Some(d), MediaType::Arw) => (iso(d), "arw".into()),
        (Some(d), MediaType::Jpeg) => (iso(d), "jpg".into()),
        (Some(d), MediaType::Clip) => (iso(d), "mp4".into()),
        (None, MediaType::Arw) => ("unknown".into(), "arw".into()),
        (None, MediaType::Jpeg) => ("unknown".into(), "jpg".into()),
        (None, MediaType::Clip) => ("unknown".into(), "mp4".into()),
    };
    format!("{}_{}.{}", date_part, seq_num(n), ext)
}

fn seq_num(n: usize) -> String {
    format!("{:03}", n)
}

fn iso(d: Date) -> String {
    format!("{:04}-{:02}-{:02}", d.year(), d.month() as i64, d.day())
}

/// Choose a destination path that does not clobber an existing file.
/// If the existing file has the same size and the same content hash, the file
/// is already there — keep pointing at it (the import loop then skips).
/// Otherwise add a -2, -3, … suffix before the extension.
fn unique_dest(dir: &Path, base: &str, size: u64, source_hash: Option<&str>) -> PathBuf {
    let candidate = dir.join(base);
    if !candidate.exists() {
        return candidate;
    }
    if let Ok(md) = fs::metadata(&candidate) {
        if md.len() == size {
            if let (Some(hash), Some(src_hash)) =
                (sha256_file_for_plan(&candidate), source_hash)
            {
                if hash == src_hash {
                    return candidate;
                }
            }
        }
    }
    // Different content: suffix -2, -3, … until free.
    let (stem, ext) = match base.rsplit_once('.') {
        Some((s, e)) => (s.to_string(), e.to_string()),
        None => (base.to_string(), String::new()),
    };
    let mut n = 2;
    loop {
        let name = if ext.is_empty() {
            format!("{}-{}", stem, n)
        } else {
            format!("{}-{}.{}", stem, n, ext)
        };
        let candidate = dir.join(name);
        if !candidate.exists() {
            return candidate;
        }
        n += 1;
    }
}

fn sha256_file_for_plan(path: &Path) -> Option<String> {
    sha256_file(path).ok()
}
