use std::path::Path;

/// What kind of media a file is, and where it lands in the folder structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaType {
    Jpeg,
    Arw,
    Clip,
}

impl MediaType {
    pub fn from_extension(ext: &str) -> Option<MediaType> {
        match ext.to_ascii_lowercase().as_str() {
            "jpg" | "jpeg" => Some(MediaType::Jpeg),
            "arw" => Some(MediaType::Arw),
            "mp4" | "mov" => Some(MediaType::Clip),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            MediaType::Jpeg => "Photos/JPEG",
            MediaType::Arw => "Photos/ARW",
            MediaType::Clip => "Clips",
        }
    }
}

/// True if any component of the path is hidden (starts with '.') — this keeps
/// us out of `.Trashes`, `._AppleDouble` files and other card junk.
pub fn is_hidden(path: &Path) -> bool {
    path.components().any(|c| {
        let s = c.as_os_str().to_string_lossy();
        s.starts_with('.') && s != "." && s != ".."
    })
}

/// True if the file name is AppleDouble metadata (._DSC01234.ARW etc).
pub fn is_apple_double(path: &Path) -> bool {
    path.file_name()
        .map(|n| n.to_string_lossy().starts_with("._"))
        .unwrap_or(false)
}
