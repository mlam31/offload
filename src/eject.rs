use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};

/// Result of an eject attempt, for display.
#[derive(Debug, Clone)]
pub struct EjectReport {
    #[allow(dead_code)] // kept for future scripting/automation use
    pub mount: Option<PathBuf>,
    pub message: String,
    #[allow(dead_code)] // kept for future scripting/automation use
    pub ejected: bool,
}

/// Resolve which disk a path lives on and where that volume is mounted.
struct VolInfo {
    device: String,
    mount: Option<PathBuf>,
}

fn volume_info(path: &Path) -> Result<VolInfo> {
    let out = Command::new("diskutil")
        .arg("info")
        .arg(path)
        .output()
        .context("running diskutil info")?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut device = None;
    let mut mount = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Device Node:") {
            device = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("Mount Point:") {
            let value = rest.trim();
            if !value.is_empty() && value != "Not applicable" {
                mount = Some(PathBuf::from(value));
            }
        }
    }
    Ok(VolInfo {
        device: device.context("no device node found for path")?,
        mount,
    })
}

/// Processes (by command name) with files open anywhere on the volume.
/// `lsof +f -- <mount>` matches by filesystem device — fast, no tree walk.
fn processes_using(mount: &Path) -> Vec<String> {
    let Ok(out) = Command::new("lsof")
        .args(["-w", "+f", "--"])
        .arg(mount)
        .output()
    else {
        return Vec::new(); // lsof unavailable — fall through to diskutil's own check
    };
    // lsof exits non-zero when nothing matches; only parse real output.
    let text = String::from_utf8_lossy(&out.stdout);
    let mut users: BTreeSet<String> = BTreeSet::new();
    for line in text.lines().skip(1) {
        // Columns: COMMAND PID USER FD TYPE DEVICE SIZE/OFF NODE NAME
        let mut tokens = line.split_whitespace();
        let Some(cmd) = tokens.next() else { continue };
        let name = tokens.last().unwrap_or("");
        if name.starts_with(mount.to_string_lossy().as_ref()) {
            users.insert(cmd.trim_start_matches('(').to_string());
        }
    }
    users.into_iter().collect()
}

/// Eject the volume containing `path`, but only when no other process has
/// files open on it. Returns a human-readable report either way.
pub fn eject_if_unused(path: &Path) -> EjectReport {
    match volume_info(path) {
        Ok(info) => eject_volume(info),
        Err(e) => EjectReport {
            mount: None,
            message: format!("cannot resolve volume: {e:#}"),
            ejected: false,
        },
    }
}

fn eject_volume(info: VolInfo) -> EjectReport {
    if info.device.starts_with("/dev/disk") && info.device.contains("disk0") {
        return EjectReport {
            mount: info.mount,
            message: "that's the system disk — refusing".to_string(),
            ejected: false,
        };
    }
    let Some(mount) = info.mount else {
        return EjectReport {
            mount: None,
            message: "volume not mounted".to_string(),
            ejected: false,
        };
    };
    let users = processes_using(&mount);
    if !users.is_empty() {
        return EjectReport {
            mount: Some(mount),
            message: format!("in use — kept mounted (open by: {})", users.join(", ")),
            ejected: false,
        };
    }
    eject_device(&info.device, mount)
}

fn eject_device(device: &str, mount: PathBuf) -> EjectReport {
    match Command::new("diskutil").arg("eject").arg(device).output() {
        Ok(out) if out.status.success() => EjectReport {
            mount: Some(mount),
            message: "ejected ✓ (safe to unplug)".to_string(),
            ejected: true,
        },
        Ok(out) => {
            let err = String::from_utf8_lossy(&out.stderr);
            EjectReport {
                mount: Some(mount),
                message: format!(
                    "could not eject: {}",
                    err.trim().split('\n').next().unwrap_or("unknown error")
                ),
                ejected: false,
            }
        }
        Err(e) => EjectReport {
            mount: Some(mount),
            message: format!("could not run diskutil: {e}"),
            ejected: false,
        },
    }
}
