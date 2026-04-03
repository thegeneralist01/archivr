use anyhow::{Context, Result};
use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::hash::hash_file;

/// Moves `file` into the content-addressed raw store under `store_path`.
///
/// The destination path is derived from the file's SHA-256 hash:
/// `raw/<first-char>/<second-char>/<hash><ext>`. If the destination already
/// exists the source file is removed (deduplication); otherwise it is renamed.
/// Returns the store-relative destination path.
pub fn archive_staged_file(file: &Path, store_path: &Path) -> Result<PathBuf> {
    let hash = hash_file(file)?;
    let destination = raw_relative_path(file, &hash)?;
    let absolute_destination = store_path.join(&destination);

    if let Some(parent) = absolute_destination.parent() {
        fs::create_dir_all(parent)?;
    }

    if absolute_destination.exists() {
        fs::remove_file(file)?;
    } else {
        fs::rename(file, &absolute_destination)?;
    }

    Ok(destination)
}

/// Computes the store-relative path for a file given its `hash`.
/// The layout is `raw/<c1>/<c2>/<hash><ext>` where `c1`/`c2` are the first
/// two characters of the hash, providing a two-level Trie.
fn raw_relative_path(file: &Path, hash: &str) -> Result<PathBuf> {
    let mut chars = hash.chars();
    let first_letter = chars.next().context("hash must not be empty")?;
    let second_letter = chars
        .next()
        .context("hash must be at least two characters")?;
    let extension = file
        .extension()
        .map_or(String::new(), |ext| format!(".{}", ext.to_string_lossy()));

    Ok(PathBuf::from("raw")
        .join(first_letter.to_string())
        .join(second_letter.to_string())
        .join(format!("{hash}{extension}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{env, fs};

    #[test]
    fn test_archive_staged_file_moves_into_raw_store() {
        let root = env::temp_dir().join(format!("archivr-store-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("temp")).unwrap();

        let staged = root.join("temp").join("photo.jpg");
        fs::write(&staged, b"image-bytes").unwrap();

        let relative = archive_staged_file(&staged, &root).unwrap();
        let absolute = root.join(&relative);

        assert!(absolute.is_file());
        assert!(!staged.exists());
        assert!(relative.starts_with("raw"));

        let _ = fs::remove_dir_all(&root);
    }
}
