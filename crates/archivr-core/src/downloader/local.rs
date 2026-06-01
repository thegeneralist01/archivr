use anyhow::{Context, Result, bail};
use std::{path::Path, process::Command};

use crate::hash::hash_file;

pub fn save(path: String, store_path: &Path, timestamp: &String) -> Result<String> {
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

    hash_file(&out_file)
}
