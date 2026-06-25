use anyhow::Result;
use sha3::{Digest, Sha3_256};
use std::path::Path;

/// Computes the SHA3-256 hex digest of `data` directly on raw bytes.
pub fn hash_bytes(data: &[u8]) -> String {
    let mut hasher = Sha3_256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

/// Reads `path` fully and returns its SHA3-256 hex digest.
/// Uses raw bytes — correct for both text and binary files.
pub fn hash_file(path: &Path) -> Result<String> {
    let buf = std::fs::read(path)?;
    Ok(hash_bytes(&buf))
}

/// Hashes an arbitrary string (used for legacy callers; prefer `hash_bytes`).
pub fn hash(s: String) -> String {
    hash_bytes(s.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_bytes_and_hash_file_agree_on_same_data() {
        let data = b"hello archivr fonts";
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), data).unwrap();
        assert_eq!(hash_bytes(data), hash_file(tmp.path()).unwrap());
    }

    #[test]
    fn hash_bytes_is_stable() {
        let h = hash_bytes(b"archivr");
        assert_eq!(h.len(), 64);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn hash_bytes_differs_from_hash_of_lossy_utf8_for_binary_data() {
        let binary = b"\xff\xfe\xfd";
        let direct = hash_bytes(binary);
        let lossy = String::from_utf8_lossy(binary).to_string();
        let via_old_path = {
            use sha3::{Digest, Sha3_256};
            let mut h = Sha3_256::new();
            h.update(lossy.as_bytes());
            hex::encode(h.finalize())
        };
        assert_ne!(direct, via_old_path);
    }
}
