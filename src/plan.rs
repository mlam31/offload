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
    files.sort_by(|a, b| a.name.cmp(&b.name));
    let to_hash = select_to_hash(&files, index);
    let hashes = hash_selected(&files, &to_hash, hash_progress);

    let mut planned: Vec<PlannedFile> = Vec::with_capacity(files.len());
    let mut batch = BatchState::default();

    for (i, f) in files.iter().enumerate() {
        if let Some(file) =
            classify_file(f, hashes[i].clone(), index, dest_root, opts, &mut batch, planned.len())
        {
            planned.push(file);
        }
    }

    if opts.rename {
        assign_sequence_names(&mut planned, dest_root);
    }
    resolve_batch_dups(&mut planned, &batch.hashes, &batch.pending_dups);

    Ok(Plan { files: planned })
}

/// Mutable state accumulated while classifying files in a single batch.
#[derive(Default)]
struct BatchState {
    /// Hash → index of the first planned copy with that content.
    hashes: HashMap<String, usize>,
    /// Indices of intra-batch duplicates to resolve after numbering.
    pending_dups: Vec<usize>,
}

/// Decide Copy vs SkipDuplicate for one card file and record batch state.
fn classify_file(
    f: &MediaFile,
    hash: Option<String>,
    index: &Index,
    dest_root: &Path,
    opts: PlanOptions,
    batch: &mut BatchState,
    next_idx: usize,
) -> Option<PlannedFile> {
    if let Some(known) = index_duplicate(&hash, index, opts.force) {
        return Some(skip_file(f, hash, Some(known)));
    }
    if let Some(h) = &hash {
        if batch.hashes.contains_key(h) {
            batch.pending_dups.push(next_idx);
            return Some(skip_file(f, hash, None));
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
        batch.hashes.insert(h.clone(), next_idx);
    }
    Some(copy_file(f, hash, date, dest))
}

/// Files whose size matches an index entry (possible re-import) or another
/// card file (possible intra-batch dupe) must be fingerprinted to decide.
/// Everything else provably cannot be a duplicate and is not hashed here.
fn select_to_hash(files: &[MediaFile], index: &Index) -> Vec<usize> {
    let index_sizes: Vec<u64> = index.entries.iter().map(|e| e.size).collect();
    let mut size_counts: HashMap<u64, usize> = HashMap::new();
    for f in files {
        *size_counts.entry(f.size).or_default() += 1;
    }
    files
        .iter()
        .enumerate()
        .filter(|(_, f)| index_sizes.contains(&f.size) || size_counts[&f.size] > 1)
        .map(|(i, _)| i)
        .collect()
}

/// Hash only the selected files, reporting (done, total) progress.
fn hash_selected(
    files: &[MediaFile],
    to_hash: &[usize],
    progress: Option<&Sender<HashProgress>>,
) -> Vec<Option<String>> {
    let total = to_hash.len();
    if let Some(tx) = progress {
        let _ = tx.send((0, total));
    }
    let mut hashes: Vec<Option<String>> = vec![None; files.len()];
    for (n, &i) in to_hash.iter().enumerate() {
        hashes[i] = sha256_file(&files[i].path).ok(); // unreadable: still copy, report later
        if let Some(tx) = progress {
            let _ = tx.send((n + 1, total));
        }
    }
    hashes
}

/// If the content is already in the import index (and not forcing), return
/// the destination of the known copy.
fn index_duplicate(hash: &Option<String>, index: &Index, force: bool) -> Option<String> {
    if force {
        return None;
    }
    let h = hash.as_deref()?;
    find_by_hash(index, h).map(|e| e.dest.clone())
}

fn skip_file(f: &MediaFile, hash: Option<String>, known: Option<String>) -> PlannedFile {
    PlannedFile {
        source: f.path.clone(),
        original_name: f.name.clone(),
        media_type: f.media_type,
        size: f.size,
        hash,
        date: f.date,
        action: Action::SkipDuplicate { known },
    }
}

fn copy_file(
    f: &MediaFile,
    hash: Option<String>,
    date: Option<Date>,
    dest: PathBuf,
) -> PlannedFile {
    PlannedFile {
        source: f.path.clone(),
        original_name: f.name.clone(),
        media_type: f.media_type,
        size: f.size,
        hash,
        date,
        action: Action::Copy { dest },
    }
}

/// Numbering pass: group copies by (date, type) and assign 001, 002, …
/// `planned` is sorted by original name, so group order = camera order.
fn assign_sequence_names(planned: &mut [PlannedFile], dest_root: &Path) {
    let mut groups: HashMap<(Option<Date>, MediaType), Vec<usize>> = HashMap::new();
    for (i, p) in planned.iter().enumerate() {
        if p.is_copy() {
            groups.entry((p.date, p.media_type)).or_default().push(i);
        }
    }
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

/// Point intra-batch duplicate references at the first copy's final
/// destination now that numbering is final.
fn resolve_batch_dups(
    planned: &mut [PlannedFile],
    batch_hashes: &HashMap<String, usize>,
    pending_dups: &[usize],
) {
    for &dup_idx in pending_dups {
        let Some(h) = planned[dup_idx].hash.clone() else { continue };
        let Some(&first) = batch_hashes.get(&h) else { continue };
        if let Action::Copy { dest } = &planned[first].action {
            planned[dup_idx].action = Action::SkipDuplicate {
                known: Some(dest.to_string_lossy().into_owned()),
            };
        }
    }
}

/// DD-MM-YYYY folder name for a capture date (the on-disk sorting key).
pub fn date_folder(date: Date) -> String {
    format!(
        "{:02}-{:02}-{:04}",
        date.day(),
        date.month() as i64,
        date.year()
    )
}

/// <SSD>/DD-MM-YYYY/… with Clips/, Photos/JPEG/, Photos/ARW/ under it.
pub fn dest_dir(dest_root: &Path, date: Option<Date>) -> PathBuf {
    let Some(date) = date else {
        return dest_root.join("Unsorted");
    };
    dest_root.join(date_folder(date))
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::Digest;
    use tempfile::TempDir;

    /// Date::new returns a Result; tests always use valid dates.
    fn d(y: i16, m: i8, day: i8) -> Date {
        Date::new(y, m, day).unwrap()
    }

    fn media_file(path: PathBuf, size: u64, media_type: MediaType, date: Option<Date>) -> MediaFile {
        MediaFile {
            name: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            path,
            media_type,
            size,
            date,
        }
    }

    #[test]
    fn dest_dir_uses_day_first_folder_or_unsorted() {
        let root = Path::new("/dest");
        assert_eq!(
            dest_dir(root, Some(d(2026, 3, 5))),
            PathBuf::from("/dest/05-03-2026")
        );
        assert_eq!(dest_dir(root, None), PathBuf::from("/dest/Unsorted"));
        assert_eq!(date_folder(d(2026, 12, 31)), "31-12-2026");
    }

    #[test]
    fn seq_names_are_sortable_per_date_and_type() {
        assert_eq!(seq_name(Some(d(2026, 3, 5)), MediaType::Arw, 7), "2026-03-05_007.arw");
        assert_eq!(seq_name(Some(d(2026, 3, 5)), MediaType::Jpeg, 1), "2026-03-05_001.jpg");
        assert_eq!(seq_name(Some(d(2026, 3, 5)), MediaType::Clip, 12), "2026-03-05_012.mp4");
        assert_eq!(seq_name(None, MediaType::Arw, 2), "unknown_002.arw");
        assert_eq!(seq_name(None, MediaType::Jpeg, 2), "unknown_002.jpg");
        assert_eq!(seq_name(None, MediaType::Clip, 2), "unknown_002.mp4");
    }

    #[test]
    fn stats_aggregate_counts_bytes_and_dates() {
        let d1 = d(2026, 3, 5);
        let d2 = d(2026, 3, 6);
        let plan = Plan {
            files: vec![
                copy_file(&media_file(PathBuf::from("a.jpg"), 100, MediaType::Jpeg, Some(d1)), None, Some(d1), PathBuf::from("a")),
                skip_file(&media_file(PathBuf::from("b.jpg"), 50, MediaType::Jpeg, Some(d1)), None, None),
                copy_file(&media_file(PathBuf::from("c.mp4"), 200, MediaType::Clip, Some(d1)), None, Some(d1), PathBuf::from("c")),
                copy_file(&media_file(PathBuf::from("d.jpg"), 30, MediaType::Jpeg, Some(d2)), None, Some(d2), PathBuf::from("d")),
                copy_file(&media_file(PathBuf::from("e.arw"), 0, MediaType::Arw, None), None, None, PathBuf::from("e")),
            ],
        };
        let st = plan.stats();
        assert_eq!(st.copy_count, 4);
        assert_eq!(st.copy_bytes, 330);
        assert_eq!(st.skip_count, 1);
        assert_eq!(st.skip_bytes, 50);
        assert_eq!(st.by_type[&MediaType::Jpeg], (2, 130));
        assert_eq!(st.by_type[&MediaType::Clip], (1, 200));
        assert_eq!(st.dates, vec![d1, d2]);
    }

    #[test]
    fn unique_sizes_are_copied_without_hashing_even_with_rename() {
        let tmp = TempDir::new().unwrap();
        let dest_root = tmp.path().join("ssd");
        let d = d(2026, 1, 2);
        let files = vec![
            media_file(PathBuf::from("DSC00002.JPG"), 11, MediaType::Jpeg, Some(d)),
            media_file(PathBuf::from("DSC00001.JPG"), 22, MediaType::Jpeg, Some(d)),
            media_file(PathBuf::from("C0001.MP4"), 33, MediaType::Clip, Some(d)),
        ];
        let plan = build_plan(
            files,
            &Index::default(),
            &dest_root,
            PlanOptions { rename: true, force: false },
            None,
        )
        .unwrap();
        let st = plan.stats();
        assert_eq!(st.copy_count, 3);
        assert_eq!(st.skip_count, 0);

        // Numbering is per (date, type), in camera (name) order.
        let dests: Vec<PathBuf> = plan
            .files
            .iter()
            .filter_map(|f| match &f.action {
                Action::Copy { dest } => Some(dest.clone()),
                _ => None,
            })
            .collect();
        let expected = sub_dir(&dest_root, Some(d), MediaType::Jpeg);
        assert!(dests.contains(&expected.join("2026-01-02_001.jpg")));
        assert!(dests.contains(&expected.join("2026-01-02_002.jpg")));
        let clips = sub_dir(&dest_root, Some(d), MediaType::Clip);
        assert!(dests.contains(&clips.join("2026-01-02_001.mp4")));
    }

    #[test]
    fn undated_files_land_in_unsorted_with_unknown_names() {
        let tmp = TempDir::new().unwrap();
        let dest_root = tmp.path().join("ssd");
        let files = vec![media_file(
            PathBuf::from("DSC00001.ARW"),
            5,
            MediaType::Arw,
            None,
        )];
        let plan = build_plan(
            files,
            &Index::default(),
            &dest_root,
            PlanOptions { rename: true, force: false },
            None,
        )
        .unwrap();
        match &plan.files[0].action {
            Action::Copy { dest } => {
                assert_eq!(dest, &dest_root.join("Unsorted").join("Photos/ARW").join("unknown_001.arw"));
            }
            _ => panic!("expected a copy"),
        }
    }

    #[test]
    fn file_already_in_index_is_skipped_with_known_dest() {
        let tmp = TempDir::new().unwrap();
        let card = tmp.path().join("card");
        std::fs::create_dir_all(&card).unwrap();
        let card_file = card.join("DSC00001.JPG");
        std::fs::write(&card_file, "jpeg-content").unwrap();

        let hash = sha256_file(&card_file).unwrap();
        let index = Index {
            entries: vec![crate::config::IndexEntry {
                hash,
                size: 12,
                media_type: "Jpeg".into(),
                original_name: "DSC00001.JPG".into(),
                dest: "/ssd/01-01-2026/Photos/JPEG/2026-01-01_001.jpg".into(),
                imported_at: "2026-01-01T00:00:00Z".into(),
            }],
        };
        let plan = build_plan(
            vec![media_file(card_file, 12, MediaType::Jpeg, None)],
            &index,
            &tmp.path().join("ssd"),
            PlanOptions { rename: false, force: false },
            None,
        )
        .unwrap();
        match &plan.files[0].action {
            Action::SkipDuplicate { known } => assert_eq!(
                known.as_deref(),
                Some("/ssd/01-01-2026/Photos/JPEG/2026-01-01_001.jpg")
            ),
            _ => panic!("expected a skip"),
        }
    }

    #[test]
    fn force_flag_re_imports_indexed_content() {
        let tmp = TempDir::new().unwrap();
        let card = tmp.path().join("card");
        std::fs::create_dir_all(&card).unwrap();
        let card_file = card.join("DSC00001.JPG");
        std::fs::write(&card_file, "jpeg-content").unwrap();

        let hash = sha256_file(&card_file).unwrap();
        let index = Index {
            entries: vec![crate::config::IndexEntry {
                hash,
                size: 12,
                media_type: "Jpeg".into(),
                original_name: "DSC00001.JPG".into(),
                dest: "/ssd/old.jpg".into(),
                imported_at: "2026-01-01T00:00:00Z".into(),
            }],
        };
        let plan = build_plan(
            vec![media_file(card_file, 12, MediaType::Jpeg, None)],
            &index,
            &tmp.path().join("ssd"),
            PlanOptions { rename: false, force: true },
            None,
        )
        .unwrap();
        assert!(plan.files[0].is_copy());
    }

    #[test]
    fn same_content_twice_on_card_dedups_within_the_batch() {
        let tmp = TempDir::new().unwrap();
        let card = tmp.path().join("card");
        std::fs::create_dir_all(&card).unwrap();
        let a = card.join("DSC00001.JPG");
        let b = card.join("DSC00002.JPG");
        std::fs::write(&a, "twin-content").unwrap();
        std::fs::write(&b, "twin-content").unwrap();

        let plan = build_plan(
            vec![
                media_file(a, 12, MediaType::Jpeg, None),
                media_file(b, 12, MediaType::Jpeg, None),
            ],
            &Index::default(),
            &tmp.path().join("ssd"),
            PlanOptions { rename: false, force: false },
            None,
        )
        .unwrap();
        assert_eq!(plan.stats().copy_count, 1);
        let first_dest = match &plan.files[0].action {
            Action::Copy { dest } => dest.to_string_lossy().into_owned(),
            _ => panic!("expected first file to be copied"),
        };
        match &plan.files[1].action {
            Action::SkipDuplicate { known } => {
                assert_eq!(known.as_deref(), Some(first_dest.as_str()));
            }
            _ => panic!("expected second file to be skipped"),
        }
    }

    #[test]
    fn unreadable_file_still_gets_copied() {
        let tmp = TempDir::new().unwrap();
        // Path whose content cannot be read (missing file) — build_plan must
        // still plan a copy for it instead of dropping it.
        let plan = build_plan(
            vec![media_file(
                tmp.path().join("does-not-exist.ARW"),
                7,
                MediaType::Arw,
                None,
            )],
            &Index::default(),
            &tmp.path().join("ssd"),
            PlanOptions { rename: false, force: false },
            None,
        )
        .unwrap();
        assert!(plan.files[0].is_copy());
        assert!(plan.files[0].hash.is_none());
    }

    #[test]
    fn unique_dest_reuses_destination_for_identical_content_and_suffers_otherwise() {
        let dir = TempDir::new().unwrap();
        let dir = dir.path();
        std::fs::write(dir.join("a.jpg"), "xxxx").unwrap();

        // Same size, same content → point at the existing file.
        let same = unique_dest(dir, "a.jpg", 4, Some(&sha256_file(&dir.join("a.jpg")).unwrap()));
        assert_eq!(same, dir.join("a.jpg"));

        // Same size, different content → suffix -2.
        let other_hash = sha2::Sha256::digest(b"yyyy");
        let other = unique_dest(dir, "a.jpg", 4, Some(&format!("{other_hash:x}")));
        assert_eq!(other, dir.join("a-2.jpg"));

        // No hash known → conservative, suffix -2.
        let unknown = unique_dest(dir, "a.jpg", 4, None);
        assert_eq!(unknown, dir.join("a-2.jpg"));

        // Different size → suffix, and the chain continues past -2.
        std::fs::write(dir.join("a-2.jpg"), "zz").unwrap();
        let third = unique_dest(dir, "a.jpg", 9, None);
        assert_eq!(third, dir.join("a-3.jpg"));

        // Extensionless names also get suffixes.
        std::fs::write(dir.join("readme"), "r").unwrap();
        assert_eq!(unique_dest(dir, "readme", 1, None), dir.join("readme-2"));
    }
}
