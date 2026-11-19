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
/// somewhere below DCIM or PRIVATE (the Sony A6700 layout). Only camera cards
/// are considered as sources — other drives are never picked automatically.
pub fn detect_source() -> Result<Option<SourceInfo>> {
    let candidates = mounted_volumes();

    // Camera cards have DCIM/ and/or PRIVATE/ at the root.
    let camera_cards: Vec<PathBuf> = candidates
        .into_iter()
        .filter(|v| v.join("DCIM").exists() || v.join("PRIVATE").exists())
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

    Ok(None)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_within_accepts_root_and_descendants_only() {
        let root = Path::new("/Volumes/SSD");
        assert!(is_within(root, Path::new("/Volumes/SSD")));
        assert!(is_within(root, Path::new("/Volumes/SSD/sub/f.jpg")));
        assert!(!is_within(root, Path::new("/Volumes/SSD2")));
        assert!(!is_within(root, Path::new("/etc")));
        assert!(!is_within(root, Path::new("/Volumes")));
    }
}
