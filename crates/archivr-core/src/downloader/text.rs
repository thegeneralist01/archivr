use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

use crate::hash::hash_bytes;

/// Represents a staged text file ready to be moved into the raw store.
#[derive(Debug)]
pub struct StagedText {
    pub staged_path: PathBuf,
    pub hash: String,
    pub extension: String,
    pub byte_size: u64,
}

/// Stages a text body (plain or Markdown) in the temp directory and computes its hash.
///
/// # Arguments
/// * `body` - The raw bytes of the text content
/// * `mime` - MIME type, must be "text/plain" or "text/markdown"
/// * `store_path` - Root store path where temp/ subdirectory will be created
/// * `timestamp` - Timestamp string used in the staged file name
///
/// # Returns
/// * `StagedText` with the staged path, hash, extension, and byte size
///
/// # Errors
/// * Rejects MIME types other than "text/plain" or "text/markdown"
/// * IO errors during directory creation or file writing
pub fn save(body: &[u8], mime: &str, store_path: &Path, timestamp: &str) -> Result<StagedText> {
    // Validate MIME type
    let extension = match mime {
        "text/markdown" => ".md",
        "text/plain" => ".txt",
        _ => bail!("unsupported MIME type: {mime}. Must be 'text/plain' or 'text/markdown'"),
    };

    // Create temp directory
    let temp_dir = store_path.join("temp").join(timestamp);
    std::fs::create_dir_all(&temp_dir)?;

    // Stage under temp/<timestamp>/<timestamp><ext>
    let staged_path = temp_dir.join(format!("{timestamp}{extension}"));

    // Write the content
    std::fs::write(&staged_path, body)?;

    // Compute SHA3 hash
    let hash = hash_bytes(body);
    let byte_size = body.len() as u64;
    let extension_str = extension.trim_start_matches('.').to_string();

    Ok(StagedText {
        staged_path,
        hash,
        extension: extension_str,
        byte_size,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_save_markdown() {
        let temp_dir = TempDir::new().unwrap();
        let store_path = temp_dir.path();
        let content = b"# Hello\n\nThis is markdown.";
        let mime = "text/markdown";

        let result = save(content, mime, store_path, "2024-01-01T12-00-00.000-abc123").unwrap();

        assert_eq!(result.extension, "md");
        assert_eq!(result.byte_size, content.len() as u64);
        assert!(result.staged_path.exists());
        assert_eq!(std::fs::read(&result.staged_path).unwrap(), content);
    }

    #[test]
    fn test_save_plain_text() {
        let temp_dir = TempDir::new().unwrap();
        let store_path = temp_dir.path();
        let content = b"Plain text content";
        let mime = "text/plain";

        let result = save(content, mime, store_path, "2024-01-01T12-00-00.000-abc123").unwrap();

        assert_eq!(result.extension, "txt");
        assert_eq!(result.byte_size, content.len() as u64);
        assert!(result.staged_path.exists());
    }

    #[test]
    fn test_save_rejects_unsupported_mime() {
        let temp_dir = TempDir::new().unwrap();
        let store_path = temp_dir.path();
        let content = b"test";

        let result = save(content, "text/html", store_path, "2024-01-01T12-00-00.000-abc123");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unsupported MIME type"));
    }

    #[test]
    fn test_save_hash_is_consistent() {
        let temp_dir = TempDir::new().unwrap();
        let store_path = temp_dir.path();
        let content = b"archivr text";

        let result1 = save(content, "text/plain", store_path, "2024-01-01T12-00-00.000-abc123").unwrap();

        let temp_dir2 = TempDir::new().unwrap();
        let result2 = save(content, "text/plain", temp_dir2.path(), "2024-01-01T12-00-01.000-def456").unwrap();

        assert_eq!(result1.hash, result2.hash);
    }
}
