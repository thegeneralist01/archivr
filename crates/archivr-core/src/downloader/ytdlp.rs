use anyhow::{bail, Context, Result};
use std::{
    collections::HashMap,
    env,
    path::{Path, PathBuf},
    process::Command,
};
use uuid::Uuid;
use serde_json;

use crate::downloader::cookies::{domain_from_url, write_netscape_cookie_file};
use crate::hash::hash_file;

/// A single item in a flat playlist listing from `fetch_playlist_info`.
#[derive(Debug)]
pub struct PlaylistItem {
    pub id: String,
    pub url: String,
    pub title: Option<String>,
    pub uploader: Option<String>,
}

/// Container metadata returned by `fetch_playlist_info`.
#[derive(Debug)]
pub struct PlaylistInfo {
    pub playlist_id: String,
    pub title: Option<String>,
    pub uploader: Option<String>,
    pub items: Vec<PlaylistItem>,
}

/// Per-item quality data returned by `probe_playlist_qualities`.
#[derive(Debug, serde::Serialize)]
pub struct PlaylistItemProbe {
    pub id: String,
    pub url: String,
    pub title: Option<String>,
    /// Available video heights as strings (e.g. "1080p"), sorted highest-first.
    /// Empty vec means audio-only (no video track).
    pub qualities: Vec<String>,
    pub has_audio: bool,
}

/// Full playlist probe result with per-item quality data.
#[derive(Debug, serde::Serialize)]
pub struct PlaylistProbeResult {
    pub playlist_id: String,
    pub title: Option<String>,
    pub uploader: Option<String>,
    pub items: Vec<PlaylistItemProbe>,
}

/// Returns the yt-dlp `-f` format selector for `quality`.
///
/// - `"audio"` → prefers native Opus/WebM (most efficient), then native
///   AAC/M4A, then any best-audio fallback — no transcoding, smallest file
///   at equivalent perceptual quality.
/// - `"NNNp"` (e.g. `"1080p"`) → height-capped selector with `/best` fallback
/// - `None` / `"best"` / anything else → highest-quality video+audio
pub fn quality_format(quality: Option<&str>) -> String {
    if quality == Some("audio") {
        // Opus (WebM) is more efficient than AAC (M4A) at the same perceptual
        // quality, so prefer it first. Both are taken natively — no transcode.
        return "bestaudio[ext=webm]/bestaudio[ext=m4a]/bestaudio/best".to_string();
    }
    if let Some(q) = quality {
        if let Some(h) = q.strip_suffix('p').and_then(|n| n.parse::<u32>().ok()) {
            return format!("bestvideo[height<={h}]+bestaudio/best[height<={h}]/best");
        }
    }
    "bestvideo+bestaudio/best".to_string()
}


/// Combined result of a yt-dlp metadata probe.
pub struct ProbeResult {
    /// Distinct video heights available, sorted highest-first (e.g. `[1080, 720, 480]`).
    pub video_heights: Vec<u32>,
    /// True when at least one format with a real audio codec exists.
    pub has_audio: bool,
}

/// Parses a yt-dlp `--dump-json` response into a `ProbeResult`.
pub fn probe_result(json: &str) -> ProbeResult {
    ProbeResult {
        video_heights: available_video_heights(json),
        has_audio: has_audio_track(json),
    }
}

/// Distinct video heights from a yt-dlp `--dump-json` response, sorted highest-first.
/// Audio-only formats (`vcodec == "none"`) and zero-height entries are excluded.
pub fn available_video_heights(json: &str) -> Vec<u32> {
    let v: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => return vec![],
    };
    let Some(formats) = v.get("formats").and_then(|f| f.as_array()) else {
        return vec![];
    };
    let mut heights: Vec<u32> = formats
        .iter()
        .filter_map(|f| {
            let vcodec = f.get("vcodec")?.as_str()?;
            if vcodec == "none" {
                return None;
            }
            let h = f.get("height")?.as_u64()?;
            if h == 0 { None } else { Some(h as u32) }
        })
        .collect();
    heights.sort_unstable_by(|a, b| b.cmp(a));
    heights.dedup();
    heights
}

/// Extracts distinct video heights from a serde_json entry Value's `formats` array,
/// sorted highest-first. Audio-only formats (vcodec == "none") are excluded.
fn available_video_heights_from_value(entry: &serde_json::Value) -> Vec<u32> {
    let Some(fmts) = entry.get("formats").and_then(|v| v.as_array()) else {
        return vec![];
    };
    let mut heights: Vec<u32> = fmts
        .iter()
        .filter(|f| f.get("vcodec").and_then(|c| c.as_str()).unwrap_or("none") != "none")
        .filter_map(|f| f.get("height").and_then(|h| h.as_u64()).map(|h| h as u32))
        .filter(|&h| h > 0)
        .collect();
    heights.sort_unstable_by(|a, b| b.cmp(a));
    heights.dedup();
    heights
}

/// Returns true when the yt-dlp `--dump-json` response contains at least one
/// format with a real audio codec (i.e. `acodec != "none"`).
pub fn has_audio_track(json: &str) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return false;
    };
    let Some(formats) = v.get("formats").and_then(|f| f.as_array()) else {
        return false;
    };
    formats.iter().any(|f| {
        f.get("acodec")
            .and_then(|a| a.as_str())
            .is_some_and(|a| a != "none")
    })
}

/// Downloads `path` via yt-dlp and returns `(hash, file_extension_with_dot)`.
///
/// For video the extension is always `.mp4` (forced via `--merge-output-format`).
/// For audio (`quality == Some("audio")`) `-x` is passed to guarantee audio-only
/// output even when only combined A/V formats exist (yt-dlp strips the video
/// track). `--audio-format` is intentionally omitted so the audio stream is
/// remuxed into its native container without re-encoding — no lossy transcode,
/// no size inflation. The actual output extension is discovered by globbing.
pub fn download(
    path: String,
    store_path: &Path,
    timestamp: &String,
    quality: Option<&str>,
    cookies: &HashMap<String, String>,
) -> Result<(String, String)> {
    println!("Downloading with yt-dlp: {path}");

    let ytdlp = env::var("ARCHIVR_YT_DLP").unwrap_or_else(|_| "yt-dlp".to_string());
    let is_audio = quality == Some("audio");

    let temp_dir = store_path.join("temp").join(timestamp);
    std::fs::create_dir_all(&temp_dir)?;

    // Write a restrictive-permissions cookie file if cookies are provided.
    // Never pass cookie values in process args (ps exposure).
    let cookie_file: Option<PathBuf> = if !cookies.is_empty() {
        let cf_path = temp_dir.join("cookies.txt");
        let domain = domain_from_url(&path);
        write_netscape_cookie_file(cookies, &domain, &cf_path)
            .context("failed to write yt-dlp cookie file")?;
        Some(cf_path)
    } else {
        None
    };

    // %(ext)s lets yt-dlp write the correct extension for the chosen format.
    let out_template = temp_dir.join(format!("{timestamp}.%(ext)s"));

    let mut cmd = Command::new(&ytdlp);
    cmd.arg(&path)
        .arg("-f").arg(quality_format(quality))
        // This function is only called for single-item sources; --no-playlist
        // prevents yt-dlp from expanding a list= query parameter into a full
        // playlist download (e.g. music.youtube.com/watch?v=ID&list=RDAMVM…).
        .arg("--no-playlist");
    if is_audio {
        // -x guarantees audio-only even when /best falls back to a combined
        // A/V format. No --audio-format → native remux only, no re-encode.
        cmd.arg("-x");
    } else {
        // Force the video container to mp4 so we always have a known extension.
        cmd.arg("--merge-output-format").arg("mp4");
    }
    if let Some(cf) = &cookie_file {
        cmd.arg("--cookies").arg(cf);
    }
    let out = cmd
        .arg("-o")
        .arg(&out_template)
        .output()
        .with_context(|| format!("failed to spawn {ytdlp} process"));

    // Remove cookie file immediately regardless of outcome.
    if let Some(cf) = &cookie_file {
        let _ = std::fs::remove_file(cf);
    }

    let out = out?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("yt-dlp failed: {stderr}");
    }

    let actual_file = find_downloaded_file(&temp_dir, timestamp)?;
    let ext = actual_file
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let hash = hash_file(&actual_file)?;
    Ok((hash, ext))
}

/// Finds the file yt-dlp wrote to `temp_dir` whose stem is `timestamp`.
/// Ignores `.part` files (incomplete downloads).
fn find_downloaded_file(temp_dir: &Path, timestamp: &str) -> Result<PathBuf> {
    let entries = std::fs::read_dir(temp_dir)
        .with_context(|| format!("failed to read temp dir {}", temp_dir.display()))?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with(timestamp) && !name_str.ends_with(".part") {
            return Ok(entry.path());
        }
    }
    bail!(
        "yt-dlp output file not found in {}",
        temp_dir.display()
    )
}

/// Fetches metadata JSON for `path` via `yt-dlp --dump-json`.
///
/// This is a simulate call — it does NOT download any media.
/// On failure (non-zero exit or no stdout), prints the captured stderr
/// to stderr (for debugging) then returns `None` so callers can proceed.
pub fn fetch_metadata(path: &str, cookies: &HashMap<String, String>) -> Option<String> {
    let ytdlp = std::env::var("ARCHIVR_YT_DLP").unwrap_or_else(|_| "yt-dlp".to_string());

    // Write a temp cookie file if needed; UUID-named to avoid collisions.
    let cookie_file: Option<PathBuf> = if !cookies.is_empty() {
        let domain = domain_from_url(path);
        let p = std::env::temp_dir()
            .join(format!("archivr-cookies-{}.txt", Uuid::new_v4().simple()));
        write_netscape_cookie_file(cookies, &domain, &p).ok()?;
        Some(p)
    } else {
        None
    };

    let mut cmd = std::process::Command::new(&ytdlp);
    cmd.arg("--dump-json")
        // Same rationale as download(): only called for single-item sources;
        // prevents --dump-json from emitting one JSON object per playlist item
        // when the URL contains a list= parameter.
        .arg("--no-playlist");
    if let Some(cf) = &cookie_file {
        cmd.arg("--cookies").arg(cf);
    }
    cmd.arg(path);

    let out = cmd.output().ok();

    // Remove cookie file regardless of outcome.
    if let Some(cf) = &cookie_file {
        let _ = std::fs::remove_file(cf);
    }

    let out = out?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        eprintln!(
            "yt-dlp --dump-json failed for {path} (status {:?}): {stderr}",
            out.status
        );
        return None;
    }

    let json = String::from_utf8(out.stdout).ok()?;
    if json.trim().is_empty() { None } else { Some(json) }
}

/// Resolves an absolute item URL from a flat-playlist entry JSON object.
///
/// Priority:
/// 1. `webpage_url` — yt-dlp makes this absolute when present.
/// 2. `url` when it is already an absolute HTTP(S) URL.
/// 3. Platform-specific fallback constructed from `id` + `container_url`:
///    - YouTube Music → `https://music.youtube.com/watch?v={id}`
///    - YouTube       → `https://www.youtube.com/watch?v={id}`
///    - Spotify       → `https://open.spotify.com/track/{id}`
///    - Other         → `None` (caller should skip the item and warn).
fn normalize_item_url(
    entry: &serde_json::Value,
    id: &str,
    container_url: &str,
) -> Option<String> {
    let is_abs = |s: &str| s.starts_with("http://") || s.starts_with("https://");
    if let Some(u) = entry.get("webpage_url").and_then(|v| v.as_str()).filter(|s| is_abs(s)) {
        return Some(u.to_owned());
    }
    if let Some(u) = entry.get("url").and_then(|v| v.as_str()).filter(|s| is_abs(s)) {
        return Some(u.to_owned());
    }
    // Bare-ID fallback keyed on the container's platform.
    if container_url.contains("music.youtube.com") {
        Some(format!("https://music.youtube.com/watch?v={id}"))
    } else if container_url.contains("youtube.com") || container_url.contains("youtu.be") {
        Some(format!("https://www.youtube.com/watch?v={id}"))
    } else if container_url.contains("open.spotify.com") {
        Some(format!("https://open.spotify.com/track/{id}"))
    } else {
        eprintln!("warn: skipping playlist item {id:?} — no absolute URL from yt-dlp");
        None
    }
}

/// Runs `yt-dlp -J --flat-playlist <url>` and parses the single-JSON result.
///
/// `-J` / `--dump-single-json` returns one JSON object for the whole
/// container with reliable top-level `title` / `uploader` fields plus an
/// `entries` array of shallow per-item objects.
///
/// Returns an error if yt-dlp fails, the output is not valid JSON, or
/// the root `_type` is not `"playlist"`.
pub fn fetch_playlist_info(url: &str, cookies: &HashMap<String, String>) -> Result<PlaylistInfo> {
    let ytdlp = std::env::var("ARCHIVR_YT_DLP").unwrap_or_else(|_| "yt-dlp".to_string());

    let cookie_file: Option<PathBuf> = if !cookies.is_empty() {
        let domain = domain_from_url(url);
        let p = std::env::temp_dir()
            .join(format!("archivr-cookies-{}.txt", Uuid::new_v4().simple()));
        write_netscape_cookie_file(cookies, &domain, &p)
            .context("failed to write yt-dlp cookie file")?;
        Some(p)
    } else {
        None
    };

    let mut cmd = std::process::Command::new(&ytdlp);
    cmd.arg("-J").arg("--flat-playlist");
    if let Some(cf) = &cookie_file {
        cmd.arg("--cookies").arg(cf);
    }
    cmd.arg(url);

    let out = cmd.output();
    if let Some(cf) = &cookie_file {
        let _ = std::fs::remove_file(cf);
    }
    let out = out.with_context(|| format!("failed to spawn {ytdlp}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("yt-dlp -J --flat-playlist failed for {url}: {stderr}");
    }

    let json: serde_json::Value = serde_json::from_slice(&out.stdout)
        .context("yt-dlp -J output is not valid JSON")?;

    let ty = json.get("_type").and_then(|v| v.as_str()).unwrap_or("");
    if ty != "playlist" {
        bail!("yt-dlp output _type is {ty:?}, expected \"playlist\"");
    }

    let playlist_id = json
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let title = json.get("title").and_then(|v| v.as_str()).map(str::to_owned);
    let uploader = json.get("uploader").and_then(|v| v.as_str()).map(str::to_owned);

    let raw_entries = json
        .get("entries")
        .and_then(|v| v.as_array())
        .map(|a| a.as_slice())
        .unwrap_or(&[]);

    let mut items = Vec::with_capacity(raw_entries.len());
    for entry in raw_entries {
        if entry.is_null() {
            continue; // unavailable/private item in flat listing
        }
        let id = match entry.get("id").and_then(|v| v.as_str()) {
            Some(s) => s.to_owned(),
            None => continue,
        };
        let item_url = match normalize_item_url(entry, &id, url) {
            Some(u) => u,
            None => continue,
        };
        let item_title = entry.get("title").and_then(|v| v.as_str()).map(str::to_owned);
        let item_uploader = entry.get("uploader").and_then(|v| v.as_str()).map(str::to_owned);
        items.push(PlaylistItem { id, url: item_url, title: item_title, uploader: item_uploader });
    }

    Ok(PlaylistInfo { playlist_id, title, uploader, items })
}

/// Runs `yt-dlp -J <url>` (full metadata, NOT --flat-playlist) and returns
/// per-item quality data for every entry in the playlist.
///
/// This makes one yt-dlp subprocess call that fetches full format data for
/// all videos — expensive for large playlists but gives accurate per-video
/// quality lists. Intended for pre-capture quality selection only.
pub fn probe_playlist_qualities(
    url: &str,
    cookies: &HashMap<String, String>,
) -> Result<PlaylistProbeResult> {
    let ytdlp = std::env::var("ARCHIVR_YT_DLP").unwrap_or_else(|_| "yt-dlp".to_string());

    let cookie_file: Option<PathBuf> = if !cookies.is_empty() {
        let domain = domain_from_url(url);
        let p = std::env::temp_dir()
            .join(format!("archivr-cookies-{}.txt", Uuid::new_v4().simple()));
        write_netscape_cookie_file(cookies, &domain, &p)
            .context("failed to write yt-dlp cookie file")?;
        Some(p)
    } else {
        None
    };

    let mut cmd = std::process::Command::new(&ytdlp);
    cmd.arg("-J"); // full metadata — NOT --flat-playlist
    if let Some(cf) = &cookie_file {
        cmd.arg("--cookies").arg(cf);
    }
    cmd.arg(url);

    let out = cmd.output();
    if let Some(cf) = &cookie_file {
        let _ = std::fs::remove_file(cf);
    }
    let out = out.with_context(|| format!("failed to spawn {ytdlp}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("yt-dlp -J failed for {url}: {stderr}");
    }

    let json: serde_json::Value = serde_json::from_slice(&out.stdout)
        .context("yt-dlp -J output is not valid JSON")?;

    let ty = json.get("_type").and_then(|v| v.as_str()).unwrap_or("");
    if ty != "playlist" {
        bail!("yt-dlp output _type is {ty:?}, expected \"playlist\"");
    }

    let playlist_id = json.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let title = json.get("title").and_then(|v| v.as_str()).map(str::to_owned);
    let uploader = json.get("uploader").and_then(|v| v.as_str()).map(str::to_owned);

    let raw_entries = json
        .get("entries")
        .and_then(|v| v.as_array())
        .map(|a| a.as_slice())
        .unwrap_or(&[]);

    let mut items = Vec::with_capacity(raw_entries.len());
    for entry in raw_entries {
        if entry.is_null() { continue; }
        let id = match entry.get("id").and_then(|v| v.as_str()) {
            Some(s) => s.to_owned(),
            None => continue,
        };
        let item_url = match normalize_item_url(entry, &id, url) {
            Some(u) => u,
            None => continue,
        };
        let item_title = entry.get("title").and_then(|v| v.as_str()).map(str::to_owned);
        let heights = available_video_heights_from_value(entry);
        let qualities: Vec<String> = heights.iter().map(|h| format!("{h}p")).collect();
        let has_audio = entry
            .get("formats").and_then(|v| v.as_array())
            .map(|fmts| fmts.iter().any(|f| {
                f.get("acodec").and_then(|c| c.as_str()).unwrap_or("none") != "none"
            }))
            .unwrap_or(false);
        items.push(PlaylistItemProbe { id, url: item_url, title: item_title, qualities, has_audio });
    }

    Ok(PlaylistProbeResult { playlist_id, title, uploader, items })
}

#[cfg(test)]
mod tests {
    use super::{available_video_heights, has_audio_track, quality_format};

    #[test]
    fn quality_format_audio() {
        assert_eq!(quality_format(Some("audio")), "bestaudio[ext=webm]/bestaudio[ext=m4a]/bestaudio/best");
    }

    #[test]
    fn quality_format_known_heights() {
        assert_eq!(
            quality_format(Some("1080p")),
            "bestvideo[height<=1080]+bestaudio/best[height<=1080]/best"
        );
        assert_eq!(
            quality_format(Some("720p")),
            "bestvideo[height<=720]+bestaudio/best[height<=720]/best"
        );
        assert_eq!(
            quality_format(Some("2160p")),
            "bestvideo[height<=2160]+bestaudio/best[height<=2160]/best"
        );
    }

    #[test]
    fn quality_format_defaults_to_best() {
        assert_eq!(quality_format(None), "bestvideo+bestaudio/best");
        assert_eq!(quality_format(Some("best")), "bestvideo+bestaudio/best");
        assert_eq!(quality_format(Some("bogus")), "bestvideo+bestaudio/best");
    }


    #[test]
    fn available_video_heights_parses_formats() {
        let json = r#"{
            "formats": [
                {"height": 1080, "vcodec": "avc1.640028", "acodec": "none"},
                {"height": 720,  "vcodec": "avc1.4d401f", "acodec": "none"},
                {"height": 1080, "vcodec": "avc1.640028", "acodec": "mp4a.40.2"},
                {"height": null, "vcodec": "none",        "acodec": "mp4a.40.2"},
                {"height": 360,  "vcodec": "none",        "acodec": "mp4a.40.2"}
            ]
        }"#;
        assert_eq!(available_video_heights(json), vec![1080, 720]);
    }

    #[test]
    fn available_video_heights_empty_on_audio_only() {
        let json = r#"{"formats": [{"height": null, "vcodec": "none", "acodec": "mp4a.40.2"}]}"#;
        assert_eq!(available_video_heights(json), vec![0u32; 0]);
    }

    #[test]
    fn available_video_heights_empty_on_bad_json() {
        assert_eq!(available_video_heights("not json"), vec![0u32; 0]);
        assert_eq!(available_video_heights("{}"), vec![0u32; 0]);
    }

    #[test]
    fn has_audio_track_detects_audio() {
        let with_audio = r#"{"formats": [
            {"vcodec": "avc1", "acodec": "mp4a.40.2"},
            {"vcodec": "none", "acodec": "mp4a.40.2"}
        ]}"#;
        assert!(has_audio_track(with_audio));

        let video_only = r#"{"formats": [
            {"vcodec": "avc1", "acodec": "none"}
        ]}"#;
        assert!(!has_audio_track(video_only));

        assert!(!has_audio_track("not json"));
        assert!(!has_audio_track("{}"));
    }
}
