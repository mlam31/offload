use std::path::Path;

/// Free space available to unprivileged users at `path`.
/// Per-OS: statvfs on macOS/Linux, GetDiskFreeSpaceExW on Windows.
#[cfg(unix)]
pub fn free_bytes(path: &Path) -> Option<u64> {
    let cpath = std::ffi::CString::new(path.as_os_str().to_string_lossy().as_bytes()).ok()?;
    unsafe {
        let mut st: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(cpath.as_ptr(), &mut st) != 0 {
            return None;
        }
        Some(st.f_bavail as u64 * st.f_frsize as u64)
    }
}

#[cfg(windows)]
pub fn free_bytes(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut avail: u64 = 0;
    let mut total: u64 = 0;
    let mut free: u64 = 0;
    let ok = unsafe {
        GetDiskFreeSpaceExW(wide.as_ptr(), &mut avail, &mut total, &mut free)
    };
    if ok != 0 {
        Some(avail)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn reports_positive_free_space_for_existing_path() {
        let free = free_bytes(Path::new(".")).expect("statvfs on cwd should succeed");
        assert!(free > 0);
    }

    #[cfg(unix)]
    #[test]
    fn returns_none_for_missing_path() {
        assert_eq!(free_bytes(Path::new("/nonexistent-offload-probe")), None);
    }
}
