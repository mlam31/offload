//! offload — copy Sony A6700 media off an SD card into date-organized folders.
//!
//! Library surface so the pipeline can be exercised by integration tests
//! (the binary in `main.rs` wires this up to the CLI and TUI).

pub mod cli;
pub mod config;
pub mod dedup;
pub mod detect;
pub mod disk;
pub mod eject;
pub mod headless;
pub mod import;
pub mod media;
pub mod plan;
pub mod scan;
pub mod tui;
