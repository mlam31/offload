use std::ffi::CString;
use std::path::Path;

/// Free space available to unprivileged users at `path`, via statvfs.
pub fn free_bytes(path: &Path) -> Option<u64> {
    let cpath = CString::new(path.as_os_str().to_string_lossy().as_bytes()).ok()?;
    unsafe {
        let mut st: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(cpath.as_ptr(), &mut st) != 0 {
            return None;
        }
        Some(st.f_bavail as u64 * st.f_frsize as u64)
    }
}
