use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Persistent record of one imported file, keyed by content hash elsewhere.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexEntry {
    pub hash: String,
    pub size: u64,
    pub media_type: String,
    /// Original camera filename, so renames stay traceable.
    pub original_name: String,
    /// Absolute path of the imported copy.
    pub dest: String,
    /// ISO timestamp of the import.
    pub imported_at: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Index {
    pub entries: Vec<IndexEntry>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    pub last_dest: Option<String>,
}

/// All durable state lives in ~/.config/offload/.
pub fn state_dir() -> Result<PathBuf> {
    let dir = dirs::config_dir()
        .context("cannot determine config directory")?
        .join("offload");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn index_path() -> Result<PathBuf> {
    Ok(state_dir()?.join("index.json"))
}

pub fn config_path() -> Result<PathBuf> {
    Ok(state_dir()?.join("config.json"))
}

pub fn load_index() -> Result<Index> {
    load_index_from(&index_path()?)
}

pub fn load_index_from(path: &Path) -> Result<Index> {
    if !path.exists() {
        return Ok(Index::default());
    }
    let data = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&data).with_context(|| format!("parsing {}", path.display()))
}

/// Append one entry and rewrite the index file. The index stays small
/// (one line per imported file), so a full rewrite per file is fine.
pub fn append_index(entry: &IndexEntry) -> Result<()> {
    append_index_to(&index_path()?, entry)
}

pub fn append_index_to(path: &Path, entry: &IndexEntry) -> Result<()> {
    let mut index = load_index_from(path)?;
    index.entries.push(entry.clone());
    save_index_to(path, &index)
}

pub fn save_index(index: &Index) -> Result<()> {
    save_index_to(&index_path()?, index)
}

pub fn save_index_to(path: &Path, index: &Index) -> Result<()> {
    let json = serde_json::to_string_pretty(index)?;
    fs::write(path, json).with_context(|| format!("writing {}", path.display()))
}

pub fn find_by_hash<'a>(index: &'a Index, hash: &str) -> Option<&'a IndexEntry> {
    index.entries.iter().find(|e| e.hash == hash)
}

pub fn load_config() -> Result<Config> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(Config::default());
    }
    let data = fs::read_to_string(&path)?;
    Ok(serde_json::from_str(&data).unwrap_or_default())
}

pub fn save_last_dest(dest: &std::path::Path) -> Result<()> {
    let cfg = Config {
        last_dest: Some(dest.to_string_lossy().into_owned()),
    };
    let json = serde_json::to_string_pretty(&cfg)?;
    fs::write(config_path()?, json)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(hash: &str, name: &str) -> IndexEntry {
        IndexEntry {
            hash: hash.to_string(),
            size: 42,
            media_type: "Jpeg".to_string(),
            original_name: name.to_string(),
            dest: format!("/dest/{name}"),
            imported_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn missing_index_file_loads_as_default() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("index.json");
        let index = load_index_from(&path).unwrap();
        assert!(index.entries.is_empty());
    }

    #[test]
    fn save_load_and_append_round_trip() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("index.json");

        save_index_to(&path, &Index::default()).unwrap();
        assert!(load_index_from(&path).unwrap().entries.is_empty());

        append_index_to(&path, &entry("aaa", "DSC001.JPG")).unwrap();
        append_index_to(&path, &entry("bbb", "DSC002.JPG")).unwrap();

        let index = load_index_from(&path).unwrap();
        assert_eq!(index.entries.len(), 2);
        assert_eq!(index.entries[0].hash, "aaa");
        assert_eq!(index.entries[1].original_name, "DSC002.JPG");
    }

    #[test]
    fn corrupt_index_is_an_error_not_a_silent_default() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("index.json");
        std::fs::write(&path, "{not json").unwrap();
        assert!(load_index_from(&path).is_err());
    }

    #[test]
    fn find_by_hash_returns_first_match() {
        let index = Index {
            entries: vec![entry("h1", "a.jpg"), entry("h2", "b.jpg"), entry("h1", "c.jpg")],
        };
        assert_eq!(find_by_hash(&index, "h1").unwrap().original_name, "a.jpg");
        assert_eq!(find_by_hash(&index, "h2").unwrap().original_name, "b.jpg");
        assert!(find_by_hash(&index, "h3").is_none());
        assert!(find_by_hash(&Index::default(), "h1").is_none());
    }

    #[test]
    fn config_serialization_round_trip() {
        let cfg = Config {
            last_dest: Some("/Volumes/SSD".to_string()),
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let back: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(back.last_dest.as_deref(), Some("/Volumes/SSD"));

        let default: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(default.last_dest, None);
    }
}
