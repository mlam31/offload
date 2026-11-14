use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use anyhow::{Context, Result};

/// Full-file SHA-256, streamed in 1 MiB chunks. Content hashing is what makes
/// dedup work even when filenames differ between runs or after renames.
pub fn sha256_file(path: &Path) -> Result<String> {
    let file = File::open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    let mut reader = BufReader::with_capacity(1024 * 1024, file);
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn hashes_known_vector() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("hello.txt");
        std::fs::write(&path, "hello world").unwrap();
        let h = sha256_file(&path).unwrap();
        assert_eq!(
            h,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[test]
    fn hashes_empty_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("empty");
        std::fs::write(&path, b"").unwrap();
        let h = sha256_file(&path).unwrap();
        assert_eq!(
            h,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn chunked_reads_aggregate_correctly() {
        // 2.5 MiB crosses several 1 MiB buffer boundaries.
        let dir = tempfile::TempDir::new().unwrap();
        let content = vec![0xABu8; 2560 * 1024];
        let a = dir.path().join("a.bin");
        let b = dir.path().join("b.bin");
        std::fs::write(&a, &content).unwrap();
        std::fs::write(&b, &content).unwrap();
        assert_eq!(sha256_file(&a).unwrap(), sha256_file(&b).unwrap());

        std::fs::write(&b, vec![0xABu8; 2560 * 1024 - 1]).unwrap();
        assert_ne!(sha256_file(&a).unwrap(), sha256_file(&b).unwrap());
    }

    #[test]
    fn missing_file_is_an_error() {
        assert!(sha256_file(Path::new("/nonexistent-offload-file")).is_err());
    }
}
