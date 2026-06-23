use anyhow::{Context, Result, bail};
use std::{env, path::Path, process::Command};

use crate::hash::hash_file;

pub fn download(path: String, store_path: &Path, timestamp: &String) -> Result<String> {
    println!("Downloading with yt-dlp: {path}");

    let ytdlp = env::var("ARCHIVR_YT_DLP").unwrap_or_else(|_| "yt-dlp".to_string());

    let temp_dir = store_path.join("temp").join(timestamp);
    std::fs::create_dir_all(&temp_dir)?;

    let out_file = temp_dir.join(format!("{timestamp}.mp4"));

    let out = Command::new(&ytdlp)
        .arg(&path)
        .arg("-f")
        .arg("bestvideo+bestaudio/best")
        .arg("--merge-output-format")
        .arg("mp4")
        .arg("-o")
        .arg(&out_file)
        .output()
        .with_context(|| format!("failed to spawn {ytdlp} process"))?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("yt-dlp failed: {stderr}");
    }

    hash_file(&out_file)
}

/// Fetches metadata JSON for `path` via `yt-dlp --dump-json`.
///
/// This is a simulate call — it does NOT download any media.
/// Returns `None` silently if yt-dlp fails or produces no output,
/// so callers can proceed with the download regardless.
pub fn fetch_metadata(path: &str) -> Option<String> {
    let ytdlp = std::env::var("ARCHIVR_YT_DLP").unwrap_or_else(|_| "yt-dlp".to_string());

    let out = std::process::Command::new(&ytdlp)
        .arg("--dump-json")
        .arg(path)
        .output()
        .ok()?;

    if !out.status.success() {
        return None;
    }

    let json = String::from_utf8(out.stdout).ok()?;
    if json.trim().is_empty() { None } else { Some(json) }
}
