//! End-to-end pipeline test: scan a fake SD card, build a plan, import,
//! verify destinations and the import index, then re-run to confirm dedup.

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

use offload::config::{self, Index};
use offload::import::{run, Progress};
use offload::plan::{build_plan, Action, PlanOptions};
use offload::scan::scan_source;

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

fn recv_last(rx: &mpsc::Receiver<Progress>) -> Progress {
    let mut last = Progress::default();
    while let Ok(p) = rx.try_recv() {
        last = p;
    }
    last
}

fn count_files(dir: &Path) -> usize {
    let mut n = 0;
    if let Ok(entries) = fs::read_dir(dir) {
        for e in entries.flatten() {
            let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
            n += if is_dir {
                count_files(&e.path())
            } else {
                1
            };
        }
    }
    n
}

fn copies(plan: &offload::plan::Plan) -> Vec<std::path::PathBuf> {
    plan.files
        .iter()
        .filter_map(|f| match &f.action {
            Action::Copy { dest } => Some(dest.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn full_pipeline_copies_dedups_and_reindexes() {
    let tmp = tempfile::TempDir::new().unwrap();
    let card = tmp.path().join("card");
    let dest_root = tmp.path().join("ssd");
    let index_path = tmp.path().join("index.json");
    fs::create_dir_all(&dest_root).unwrap();

    // Fake Sony A6700 card layout: two unique ARWs, one MP4, and a JPEG
    // whose content appears twice in the batch under different names.
    write(&card.join("DCIM/100MSDCF/DSC00001.ARW"), "raw-content-one");
    write(&card.join("DCIM/100MSDCF/DSC00002.ARW"), "raw-content-two");
    write(&card.join("PRIVATE/SONY/CLIP/C0001.MP4"), "clip-content");
    write(&card.join("DCIM/100MSDCF/DSC00003.JPG"), "jpeg-content");
    write(&card.join("DCIM/100MSDCF/DSC00004.JPG"), "jpeg-content");
    // Card junk that must not be picked up.
    write(&card.join("._DSC00001.ARW"), "junk");
    write(&card.join("MISC/notes.txt"), "not media");

    let files = scan_source(&card).unwrap();
    assert_eq!(files.len(), 5, "hidden and non-media files are skipped");

    // --- Plan: 4 unique contents to copy, the twin JPEG is an intra-batch dupe.
    let plan = build_plan(
        files,
        &Index::default(),
        &dest_root,
        PlanOptions { rename: true, force: false },
        None,
    )
    .unwrap();
    let st = plan.stats();
    assert_eq!(st.copy_count, 4);
    assert_eq!(st.skip_count, 1);

    let planned = copies(&plan);
    assert_eq!(planned.len(), 4);
    // Renamed destinations are sortable YYYY-MM-DD_NNN.ext.
    for dest in &planned {
        let name = dest.file_name().unwrap().to_string_lossy().into_owned();
        let ext = name.rsplit('.').next().unwrap();
        assert!(
            name.matches('_').count() == 1 && matches!(ext, "arw" | "jpg" | "mp4"),
            "unexpected destination name {name}"
        );
    }
    // The intra-batch duplicate points at the first copy's destination.
    let dup_known = plan
        .files
        .iter()
        .find_map(|f| match &f.action {
            Action::SkipDuplicate { known } => known.clone(),
            _ => None,
        })
        .unwrap();
    assert!(planned.iter().any(|d| d == std::path::Path::new(&dup_known)));

    // --- Dry run: nothing lands on disk.
    let cancel = AtomicBool::new(false);
    let (tx, rx) = mpsc::channel();
    let summary = run(&plan, &index_path, true, &cancel, tx).unwrap();
    assert_eq!(summary.copied, 0);
    assert_eq!(summary.skipped.len(), 1);
    assert_eq!(count_files(&dest_root), 0);
    assert!(recv_last(&rx).finished);

    // --- Real run: 4 copies, index written, no errors.
    let (tx, rx) = mpsc::channel();
    let summary = run(&plan, &index_path, false, &cancel, tx).unwrap();
    assert_eq!(summary.copied, 4);
    assert_eq!(summary.copied_bytes, 54); // 15 + 15 + 12 + 12 bytes of unique contents
    assert!(summary.errors.is_empty(), "{:?}", summary.errors);
    assert_eq!(count_files(&dest_root), 4);
    let progress = recv_last(&rx);
    assert!(progress.finished && !progress.cancelled);

    // Every planned copy really landed with the right content.
    for f in &plan.files {
        if let Action::Copy { dest } = &f.action {
            assert!(dest.exists(), "missing {}", dest.display());
        }
    }
    let arw_dest = planned
        .iter()
        .find(|d| d.extension().unwrap() == "arw")
        .unwrap();
    assert_eq!(fs::read(arw_dest).unwrap(), b"raw-content-one");

    // Index now has one entry per unique content.
    let index = config::load_index_from(&index_path).unwrap();
    assert_eq!(index.entries.len(), 4);

    // --- Re-plan against the updated index: everything must be skipped.
    let files = scan_source(&card).unwrap();
    let index = config::load_index_from(&index_path).unwrap();
    let plan2 = build_plan(
        files,
        &index,
        &dest_root,
        PlanOptions { rename: true, force: false },
        None,
    )
    .unwrap();
    let st2 = plan2.stats();
    assert_eq!(st2.copy_count, 0, "re-import must dedup against the index");
    assert_eq!(st2.skip_count, 5);

    // --- Cancel flag is honored before any copy starts.
    // Plan a fresh copy (to a different root) so the loop actually has work.
    let files = scan_source(&card).unwrap();
    let dest2 = tmp.path().join("ssd2");
    fs::create_dir_all(&dest2).unwrap();
    let plan3 = build_plan(
        files,
        &Index::default(),
        &dest2,
        PlanOptions { rename: true, force: false },
        None,
    )
    .unwrap();
    assert!(plan3.stats().copy_count > 0);
    let cancelled = AtomicBool::new(true);
    let (tx, _rx) = mpsc::channel();
    let summary = run(&plan3, &index_path, false, &cancelled, tx).unwrap();
    assert!(summary.cancelled);
    assert_eq!(summary.copied, 0);

    // --- Index corruption is reported, not silently ignored.
    fs::write(&index_path, "{broken").unwrap();
    assert!(config::load_index_from(&index_path).is_err());

    let _ = cancel.load(Ordering::SeqCst); // silence unused warning if any
}
