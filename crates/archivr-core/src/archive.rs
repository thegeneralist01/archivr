use anyhow::{Context, Result, bail};
use std::{
    env, fs,
    path::{Path, PathBuf},
};

use crate::database;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchivePaths {
    pub archive_path: PathBuf,
    pub store_path: PathBuf,
    pub name: String,
}

pub fn find_archive_path_from(start: &Path) -> Result<Option<PathBuf>> {
    let mut dir = start.to_path_buf();
    loop {
        let candidate = dir.join(".archivr");
        if candidate.is_dir() {
            return Ok(Some(candidate));
        }
        if !dir.pop() {
            return Ok(None);
        }
    }
}

pub fn find_archive_path() -> Result<Option<PathBuf>> {
    let cwd = env::current_dir().context("failed to read current working directory")?;
    find_archive_path_from(&cwd)
}

pub fn read_archive_paths(archive_path: &Path) -> Result<ArchivePaths> {
    if !archive_path.is_dir() {
        bail!("archive path does not exist: {}", archive_path.display());
    }

    let name = fs::read_to_string(archive_path.join("name"))
        .with_context(|| format!("failed to read archive name in {}", archive_path.display()))?
        .trim()
        .to_string();
    let store_path = fs::read_to_string(archive_path.join("store_path"))
        .with_context(|| format!("failed to read store path in {}", archive_path.display()))?;

    Ok(ArchivePaths {
        archive_path: archive_path.to_path_buf(),
        store_path: PathBuf::from(store_path.trim()),
        name,
    })
}

pub fn initialize_archive(
    archive_parent: &Path,
    store_path: &Path,
    archive_name: &str,
    force_with_info_removal: bool,
) -> Result<ArchivePaths> {
    let archive_path = archive_parent.join(".archivr");

    if archive_path.exists() {
        if !archive_path.is_dir() {
            bail!(
                "Archive path exists and is not a directory: {}",
                archive_path.display()
            );
        }

        if force_with_info_removal {
            fs::remove_dir_all(&archive_path)?;
        } else if fs::read_dir(&archive_path)?.next().is_some() {
            bail!(
                "Archive already exists at {} and is not empty. Use --force-with-info-removal to reinitialize.",
                archive_path.display()
            );
        }
    }

    if store_path.exists() && !force_with_info_removal {
        bail!("Store path already exists at {}", store_path.display());
    }

    fs::create_dir_all(&archive_path)?;
    fs::create_dir_all(store_path)?;
    fs::write(archive_path.join("name"), archive_name)?;
    let canonical_store_path = store_path
        .canonicalize()
        .with_context(|| format!("failed to canonicalize {}", store_path.display()))?;
    fs::write(
        archive_path.join("store_path"),
        canonical_store_path
            .to_str()
            .context("store path is not valid UTF-8")?,
    )?;

    initialize_store_directories(&canonical_store_path)?;
    let conn = database::open_or_initialize(&archive_path)?;
    let _ = database::ensure_default_user(&conn)?;

    Ok(ArchivePaths {
        archive_path,
        store_path: canonical_store_path,
        name: archive_name.to_string(),
    })
}

pub fn initialize_store_directories(store_path: &Path) -> Result<()> {
    fs::create_dir_all(store_path.join("raw"))?;
    fs::create_dir_all(store_path.join("raw_tweets"))?;
    fs::create_dir_all(store_path.join("structured"))?;
    fs::create_dir_all(store_path.join("temp"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_path(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        env::temp_dir().join(format!("{prefix}-{nanos}-{}", std::process::id()))
    }

    #[test]
    fn find_archive_path_walks_up_to_dot_archivr() {
        let root = unique_path("archivr-core-find");
        let nested = root.join("a").join("b");
        fs::create_dir_all(root.join(".archivr")).unwrap();
        fs::create_dir_all(&nested).unwrap();

        let found = find_archive_path_from(&nested).unwrap().unwrap();

        assert_eq!(found, root.join(".archivr"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn read_archive_paths_returns_name_and_store_path() {
        let root = unique_path("archivr-core-open");
        let archive_path = root.join(".archivr");
        let store_path = root.join("store");
        fs::create_dir_all(&archive_path).unwrap();
        fs::create_dir_all(&store_path).unwrap();
        fs::write(archive_path.join("name"), "Personal").unwrap();
        fs::write(
            archive_path.join("store_path"),
            store_path.display().to_string(),
        )
        .unwrap();

        let paths = read_archive_paths(&archive_path).unwrap();

        assert_eq!(paths.archive_path, archive_path);
        assert_eq!(paths.store_path, store_path);
        assert_eq!(paths.name, "Personal");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn initialize_archive_creates_database_store_and_metadata() {
        let root = unique_path("archivr-core-init");
        let archive_parent = root.join("archive");
        let store_path = root.join("store");

        let paths = initialize_archive(&archive_parent, &store_path, "Personal", false).unwrap();

        assert_eq!(paths.archive_path, archive_parent.join(".archivr"));
        assert!(
            paths
                .archive_path
                .join(database::DATABASE_FILE_NAME)
                .is_file()
        );
        assert!(paths.store_path.join("raw").is_dir());
        assert!(paths.store_path.join("raw_tweets").is_dir());
        assert!(paths.store_path.join("structured").is_dir());
        assert!(paths.store_path.join("temp").is_dir());
        let _ = fs::remove_dir_all(root);
    }
}
