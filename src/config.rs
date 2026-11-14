use std::fs;
use std::path::PathBuf;

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
    let path = index_path()?;
    if !path.exists() {
        return Ok(Index::default());
    }
    let data = fs::read_to_string(&path)
        .with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&data)
        .with_context(|| format!("parsing {}", path.display()))
}

/// Append one entry and rewrite the index file. The index stays small
/// (one line per imported file), so a full rewrite per file is fine.
pub fn append_index(entry: &IndexEntry) -> Result<()> {
    let mut index = load_index()?;
    index.entries.push(entry.clone());
    save_index(&index)
}

pub fn save_index(index: &Index) -> Result<()> {
    let path = index_path()?;
    let json = serde_json::to_string_pretty(index)?;
    fs::write(&path, json).with_context(|| format!("writing {}", path.display()))
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
    Ok(match serde_json::from_str(&data) {
        Ok(cfg) => cfg,
        Err(_) => Config::default(),
    })
}

pub fn save_last_dest(dest: &std::path::Path) -> Result<()> {
    let cfg = Config {
        last_dest: Some(dest.to_string_lossy().into_owned()),
    };
    let json = serde_json::to_string_pretty(&cfg)?;
    fs::write(config_path()?, json)?;
    Ok(())
}
