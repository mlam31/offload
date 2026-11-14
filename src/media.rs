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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn from_extension_is_case_insensitive() {
        assert_eq!(MediaType::from_extension("jpg"), Some(MediaType::Jpeg));
        assert_eq!(MediaType::from_extension("JPG"), Some(MediaType::Jpeg));
        assert_eq!(MediaType::from_extension("Jpeg"), Some(MediaType::Jpeg));
        assert_eq!(MediaType::from_extension("arw"), Some(MediaType::Arw));
        assert_eq!(MediaType::from_extension("ARW"), Some(MediaType::Arw));
        assert_eq!(MediaType::from_extension("mp4"), Some(MediaType::Clip));
        assert_eq!(MediaType::from_extension("MOV"), Some(MediaType::Clip));
        assert_eq!(MediaType::from_extension("txt"), None);
        assert_eq!(MediaType::from_extension(""), None);
    }

    #[test]
    fn labels_match_folder_structure() {
        assert_eq!(MediaType::Jpeg.label(), "Photos/JPEG");
        assert_eq!(MediaType::Arw.label(), "Photos/ARW");
        assert_eq!(MediaType::Clip.label(), "Clips");
    }

    #[test]
    fn is_hidden_detects_dot_components() {
        assert!(is_hidden(Path::new("/Volumes/CARD/.Trashes/x.MP4")));
        assert!(is_hidden(Path::new("/a/.Spotlight-V100")));
        assert!(is_hidden(Path::new("._weird")));
        assert!(!is_hidden(Path::new("/Volumes/CARD/DCIM/a.jpg")));
        assert!(!is_hidden(Path::new("normal")));
    }

    #[test]
    fn is_apple_double_detects_underscore_prefix() {
        assert!(is_apple_double(Path::new("._DSC01234.ARW")));
        assert!(is_apple_double(Path::new("/x/._y.jpg")));
        assert!(!is_apple_double(Path::new("DSC01234.ARW")));
        assert!(!is_apple_double(PathBuf::from("/x").as_path()));
    }
}
