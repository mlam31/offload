use clap::Parser;

/// offload — copy Sony A6700 media off an SD card into date-organized folders.
///
/// Without --src/--dest, an interactive terminal UI opens where you can browse
/// your drives with the arrow keys and pick a destination with Enter.
#[derive(Parser, Debug)]
#[command(name = "offload", version, about)]
pub struct Cli {
    /// Source to import from (e.g. /Volumes/SD_CARD). Enables headless mode.
    #[arg(long)]
    pub src: Option<std::path::PathBuf>,

    /// Destination root (e.g. /Volumes/MySSD). Required in headless mode.
    #[arg(long)]
    pub dest: Option<std::path::PathBuf>,

    /// Show what would be imported without copying anything.
    #[arg(long)]
    pub dry_run: bool,

    /// Keep the original camera filenames instead of renaming to YYYY-MM-DD_NNN.ext
    #[arg(long)]
    pub no_rename: bool,

    /// Re-import files even if their content hash is already in the import index.
    #[arg(long)]
    pub force: bool,

    /// Eject the card and destination drive after a clean import (headless only).
    #[arg(long)]
    pub eject: bool,
}
