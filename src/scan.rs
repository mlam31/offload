use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use anyhow::Result;
use jiff::civil::Date;
use walkdir::WalkDir;

use crate::media::{is_apple_double, is_hidden, MediaType};

/// A media file found on the card, before any dedup or planning.
#[derive(Debug, Clone)]
pub struct MediaFile {
    pub path: PathBuf,
    pub name: String,
    pub media_type: MediaType,
    pub size: u64,
    pub date: Option<Date>,
}

/// Walk a source root and collect every supported media file.
/// Hidden folders (.Trashes, Spotlight, AppleDouble) are skipped.
pub fn scan_source(root: &Path) -> Result<Vec<MediaFile>> {
    let mut files = Vec::new();
    for entry in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| !is_hidden(e.path()))
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if !entry.file_type().is_file() || is_apple_double(path) {
            continue;
        }
        let Some(media_type) = path
            .extension()
            .and_then(|e| MediaType::from_extension(&e.to_string_lossy()))
        else {
            continue;
        };

        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let metadata = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let size = metadata.len();

        let date = match media_type {
            MediaType::Clip => modified_date(&metadata),
            MediaType::Jpeg | MediaType::Arw => {
                exif_date(path).or_else(|| modified_date(&metadata))
            }
        };

        files.push(MediaFile {
            path: path.to_path_buf(),
            name,
            media_type,
            size,
            date,
        });
    }
    Ok(files)
}

/// EXIF DateTimeOriginal (the moment the shutter fired) as a civil date.
fn exif_date(path: &Path) -> Option<Date> {
    let file = File::open(path).ok()?;
    let exif = exif::Reader::new()
        .read_from_container(&mut BufReader::new(file))
        .ok()?;
    let field = exif.get_field(exif::Tag::DateTimeOriginal, exif::In::PRIMARY)?;
    parse_exif_datetime(&field.display_value().to_string())
}

/// EXIF datetimes look like "2026:03:05 10:11:12" raw, and kamadak's
/// display_value may render "2026-03-05 10:11:12". Accept both.
fn parse_exif_datetime(value: &str) -> Option<Date> {
    let date_part = value.split_whitespace().next()?;
    let normalized = date_part.replace(':', "-");
    let mut parts = normalized.split('-');
    let year: i16 = parts.next()?.parse().ok()?;
    let month: i16 = parts.next()?.parse().ok()?;
    let day: i16 = parts.next()?.parse().ok()?;
    Date::new(year, month as i8, day as i8).ok()
}

/// File modification time as a fallback date. Sony cameras write the capture
/// time as the modification date, so this is reliable for clips too.
fn modified_date(metadata: &std::fs::Metadata) -> Option<Date> {
    let modified = metadata.modified().ok()?;
    let ts = jiff::Timestamp::try_from(modified).ok()?;
    Some(ts.to_zoned(jiff::tz::TimeZone::system()).date())
}
