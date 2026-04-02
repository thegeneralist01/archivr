use anyhow::{Context, Result, bail};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use crate::hash::hash_file;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RawArchiveResult {
    Archived(PathBuf),
    AlreadyArchived(PathBuf),
}

impl RawArchiveResult {
    pub fn relative_path(&self) -> &Path {
        match self {
            Self::Archived(path) | Self::AlreadyArchived(path) => path,
        }
    }
}

pub fn save(path: String, store_path: &Path, timestamp: &str) -> Result<PathBuf> {
    println!("Saving path: {path}");

    let temp_dir = store_path.join("temp").join(timestamp);
    std::fs::create_dir_all(&temp_dir)?;

    let in_file = Path::new(path.trim_start_matches("file://"));
    let extension = in_file
        .extension()
        .map_or(String::new(), |ext| format!(".{}", ext.to_string_lossy()));
    let out_file = temp_dir.join(format!("{timestamp}{extension}"));

    let mut binding = Command::new("cp");
    let cmd = binding.arg(in_file).arg(&out_file);
    let out = cmd.output().with_context(|| "failed to spawn cp process")?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("yt-dlp failed: {stderr}");
    }

    Ok(out_file)
}

pub fn archive_staged_file(file: &Path, store_path: &Path) -> Result<RawArchiveResult> {
    let hash = hash_file(file)?;
    let destination = raw_relative_path(file, &hash)?;
    let absolute_destination = store_path.join(&destination);

    if let Some(parent) = absolute_destination.parent() {
        fs::create_dir_all(parent)?;
    }

    if absolute_destination.exists() {
        fs::remove_file(file)?;
        Ok(RawArchiveResult::AlreadyArchived(destination))
    } else {
        fs::rename(file, &absolute_destination)?;
        Ok(RawArchiveResult::Archived(destination))
    }
}

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
        let root = env::temp_dir().join(format!("archivr-local-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("temp")).unwrap();

        let staged = root.join("temp").join("photo.jpg");
        fs::write(&staged, b"image-bytes").unwrap();

        let result = archive_staged_file(&staged, &root).unwrap();
        let absolute = root.join(result.relative_path());

        assert!(absolute.is_file());
        assert!(!staged.exists());
        assert!(result.relative_path().starts_with("raw"));

        let _ = fs::remove_dir_all(&root);
    }
}
