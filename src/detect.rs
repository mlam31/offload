use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::scan::{scan_source, MediaFile};

/// Info about an import source (usually the camera's SD card).
pub struct SourceInfo {
    pub root: PathBuf,
    pub files: Vec<MediaFile>,
}

impl SourceInfo {
    pub fn total_bytes(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }
    pub fn total_count(&self) -> usize {
        self.files.len()
    }
}

/// Find mounted volumes that look like a camera card: contain media files
/// somewhere below DCIM or PRIVATE (the Sony A6700 layout). Falls back to
/// any volume that contains supported media files at all.
pub fn detect_source() -> Result<Option<SourceInfo>> {
    let candidates = mounted_volumes();

    // Cheap structural check first: Sony cards have DCIM/ and/or PRIVATE/.
    let camera_cards: Vec<PathBuf> = candidates
        .iter()
        .filter(|v| v.join("DCIM").exists() || v.join("PRIVATE").exists())
        .cloned()
        .collect();

    for vol in &camera_cards {
        let files = scan_source(vol).unwrap_or_default();
        if !files.is_empty() {
            return Ok(Some(SourceInfo {
                root: vol.clone(),
                files,
            }));
        }
    }

    // Fallback: any volume that contains supported media files at all.
    let mut fallback: Option<(PathBuf, Vec<MediaFile>)> = None;
    for vol in candidates {
        if camera_cards.contains(&vol) {
            continue;
        }
        let files = scan_source(&vol).unwrap_or_default();
        if files.is_empty() {
            continue;
        }
        if fallback
            .as_ref()
            .map(|(_, f)| f.len() < files.len())
            .unwrap_or(true)
        {
            fallback = Some((vol, files));
        }
    }
    Ok(fallback.map(|(root, files)| SourceInfo { root, files }))
}

fn mounted_volumes() -> Vec<PathBuf> {
    let mut vols = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/Volumes") {
        for e in entries.flatten() {
            let path = e.path();
            // Skip the system disk itself; only external/removable volumes.
            if path.file_name().map(|n| n == "Macintosh HD").unwrap_or(false) {
                continue;
            }
            vols.push(path);
        }
    }
    vols.sort();
    vols
}

/// True if `child` is `root` itself or lives somewhere below it.
pub fn is_within(root: &Path, child: &Path) -> bool {
    child.starts_with(root)
}
