use anyhow::{anyhow, bail, Context, Result};
use std::{
    collections::HashMap,
    env,
    ffi::OsString,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::RwLock,
    time::{Duration, Instant},
};
use uuid::Uuid;
use serde_json;

use crate::downloader::cookies::{domain_from_url, write_netscape_cookie_file};
use crate::downloader::js_runtime::{js_runtime_args, resolve_js_runtime, JsRuntime};
use crate::hash::hash_file;

/// Env var that force-pins a specific yt-dlp binary, bypassing version comparison.
pub const YT_DLP_FORCE_ENV: &str = "ARCHIVR_YT_DLP_FORCE";
/// Env var set by the nix flake wrapper, pointing at the pinned yt-dlp.
pub const YT_DLP_ENV: &str = "ARCHIVR_YT_DLP";
/// Override for the mutable state directory (used by `archivr yt-dlp` and tests).
pub const STATE_DIR_ENV: &str = "ARCHIVR_STATE_DIR";

static RESOLVED_YT_DLP: RwLock<Option<PathBuf>> = RwLock::new(None);

/// Mutable per-user state directory for archivr.
///
/// `ARCHIVR_STATE_DIR` wins if set. Otherwise this mirrors what `dirs::state_dir()`
/// would give us without taking on the dependency: `~/Library/Application Support`
/// on macOS, `$XDG_STATE_HOME` (default `~/.local/state`) elsewhere.
pub fn state_dir() -> Option<PathBuf> {
    if let Some(dir) = env::var_os(STATE_DIR_ENV) {
        if !dir.is_empty() {
            return Some(PathBuf::from(dir));
        }
    }

    let home = PathBuf::from(env::var_os("HOME").filter(|h| !h.is_empty())?);

    if cfg!(target_os = "macos") {
        Some(home.join("Library").join("Application Support").join("archivr"))
    } else {
        let base = env::var_os("XDG_STATE_HOME")
            .filter(|d| !d.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local").join("state"));
        Some(base.join("archivr"))
    }
}

/// Path of the user-installed (self-updated) yt-dlp inside the state dir.
pub fn state_dir_yt_dlp() -> Option<PathBuf> {
    state_dir().map(|d| d.join("yt-dlp").join("yt-dlp"))
}

/// The explicit yt-dlp override, if it points to a file on disk.
pub fn forced_yt_dlp() -> Option<PathBuf> {
    let p = PathBuf::from(env::var_os(YT_DLP_FORCE_ENV).filter(|v| !v.is_empty())?);
    p.is_file().then_some(p)
}

/// The nix-pinned yt-dlp advertised via `ARCHIVR_YT_DLP`, if it exists on disk.
pub fn pinned_yt_dlp() -> Option<PathBuf> {
    let p = PathBuf::from(env::var_os(YT_DLP_ENV).filter(|v| !v.is_empty())?);
    p.is_file().then_some(p)
}

/// Runs `<binary> --version` and returns the trimmed stdout.
///
/// yt-dlp versions are `YYYY.MM.DD`, so plain string ordering is chronological
/// ordering — no semver parsing needed.
pub fn probe_version(binary: &Path) -> Option<String> {
    let out = Command::new(binary).arg("--version").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!version.is_empty()).then_some(version)
}

/// The candidate yt-dlp binaries, in priority order for tie-breaking
/// (later entries win ties, so the deliberately-installed state-dir copy is last).
pub fn yt_dlp_candidates() -> Vec<(&'static str, PathBuf)> {
    let mut candidates = Vec::new();
    if let Some(p) = pinned_yt_dlp() {
        candidates.push(("env (ARCHIVR_YT_DLP)", p));
    }
    if let Some(p) = state_dir_yt_dlp() {
        if p.is_file() {
            candidates.push(("state-dir", p));
        }
    }
    candidates
}

/// Picks the yt-dlp binary to run, without consulting the process-wide cache.
///
/// Priority: `ARCHIVR_YT_DLP_FORCE` > newest of (pinned, state-dir) by version
/// string > bare `yt-dlp` (PATH lookup, the historical behaviour).
pub fn resolve_yt_dlp_uncached() -> PathBuf {
    if let Some(forced) = forced_yt_dlp() {
        return forced;
    }

    yt_dlp_candidates()
        .into_iter()
        .filter_map(|(_, path)| probe_version(&path).map(|v| (v, path)))
        // `max_by` keeps the *last* maximum, and the state-dir candidate is last,
        // so an exact version tie resolves in favour of the user's own install.
        .max_by(|(a, _), (b, _)| a.cmp(b))
        .map(|(_, path)| path)
        .unwrap_or_else(|| PathBuf::from("yt-dlp"))
}

/// Cached [`resolve_yt_dlp_uncached`] until [`refresh_yt_dlp`] — `--version` is spawned
/// once per resolution no matter how many yt-dlp calls the run makes.
pub fn resolve_yt_dlp() -> PathBuf {
    if let Some(p) = RESOLVED_YT_DLP.read().unwrap_or_else(|e| e.into_inner()).as_ref() {
        return p.clone();
    }
    let mut slot = RESOLVED_YT_DLP.write().unwrap_or_else(|e| e.into_inner());
    slot.get_or_insert_with(resolve_yt_dlp_uncached).clone()
}

/// Re-resolves (outside the lock) and swaps the cache. Called after a successful
/// state-dir install; commands already built keep their old path.
pub fn refresh_yt_dlp() -> PathBuf {
    let fresh = resolve_yt_dlp_uncached();
    *RESOLVED_YT_DLP.write().unwrap_or_else(|e| e.into_inner()) = Some(fresh.clone());
    fresh
}

/// Builds a yt-dlp command for `ytdlp` with the JS runtime args for `runtime` prepended.
///
/// Every yt-dlp process archivr starts MUST be built here (via [`yt_dlp_command`]) so the
/// `--js-runtimes` selection is applied consistently. Only `probe_version` (`--version`)
/// spawns a binary directly.
pub(crate) fn yt_dlp_command_with(ytdlp: &Path, runtime: Option<&JsRuntime>) -> Command {
    let mut cmd = Command::new(ytdlp);
    cmd.args(js_runtime_args(runtime));
    cmd
}

/// [`yt_dlp_command_with`] using the cached resolver-chosen JS runtime.
fn yt_dlp_command(ytdlp: &Path) -> Command {
    yt_dlp_command_with(ytdlp, resolve_js_runtime().as_ref())
}

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

/// Whether a subtitle track was authored (manual) or generated by YouTube (auto).
/// `Unknown` when the download was not planned from metadata. `Transcribed`
/// tracks come from local transcription, never from yt-dlp staging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubtitleKind {
    Manual,
    Auto,
    Unknown,
    Transcribed,
}

impl SubtitleKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SubtitleKind::Manual => "manual",
            SubtitleKind::Auto => "auto",
            SubtitleKind::Unknown => "unknown",
            SubtitleKind::Transcribed => "transcribed",
        }
    }

    /// Parses the `as_str` form; anything else is `Unknown`.
    pub fn parse(value: &str) -> Self {
        match value {
            "manual" => SubtitleKind::Manual,
            "auto" => SubtitleKind::Auto,
            "transcribed" => SubtitleKind::Transcribed,
            _ => SubtitleKind::Unknown,
        }
    }
}

/// Which subtitle tracks to ask yt-dlp for. Built by [`plan_subtitle_request`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubtitleRequest {
    /// Exact language codes when planned from metadata; fallback patterns otherwise.
    pub sub_langs: Vec<String>,
    pub write_manual: bool,
    pub write_auto: bool,
    /// Codes known to be manual (authored) tracks.
    pub manual_languages: Vec<String>,
    pub planned_from_metadata: bool,
    pub original_language: Option<String>,
}

/// `--sub-format` selector. No `--convert-subs` (it needs ffmpeg); YouTube
/// serves VTT natively and other formats are dropped at staging.
pub const SUBTITLE_FORMAT_SELECTOR: &str = "vtt/srt/best";
/// `--sub-langs` patterns used when no metadata was available to plan from.
pub const SUBTITLE_FALLBACK_LANGS: [&str; 2] = ["en", ".*-orig"];

/// Subtitle formats yt-dlp may produce that archivr does not keep.
const UNSUPPORTED_SUBTITLE_EXTENSIONS: [&str; 10] = [
    "ttml", "srv1", "srv2", "srv3", "json3", "dfxp", "sbv", "ass", "ssa", "lrc",
];

/// A subtitle file yt-dlp wrote into a staging directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedSubtitle {
    pub path: PathBuf,
    /// yt-dlp language code from the filename, e.g. `"en"`, `"de-orig"`.
    pub language: String,
    pub kind: SubtitleKind,
    /// `"vtt"` or `"srt"`.
    pub format: String,
    pub original_language: Option<String>,
}

/// Result of [`download`]: the media hash/extension plus any staged subtitles.
#[derive(Debug)]
pub struct YtDlpDownload {
    pub hash: String,
    /// File extension including the leading dot, e.g. `".mp4"`.
    pub extension: String,
    pub subtitles: Vec<StagedSubtitle>,
}

/// True for language codes safe to hand to `--sub-langs` (which yt-dlp treats
/// as regexes; a leading `-` means "exclude").
pub(crate) fn is_safe_language_code(code: &str) -> bool {
    let mut chars = code.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Lowercased primary subtag with any `-orig` suffix removed: `"de-orig"` → `"de"`,
/// `"en-GB"` → `"en"`.
pub(crate) fn language_base(code: &str) -> String {
    let lower = code.to_ascii_lowercase();
    let stripped = lower.strip_suffix("-orig").unwrap_or(&lower);
    stripped.split('-').next().unwrap_or("").to_string()
}

/// Sorted, safe keys of a yt-dlp subtitle map (`subtitles` / `automatic_captions`).
fn subtitle_map_keys(value: &serde_json::Value, field: &str) -> Vec<String> {
    let mut keys: Vec<String> = value
        .get(field)
        .and_then(|v| v.as_object())
        .map(|map| {
            map.keys()
                .filter(|k| k.as_str() != "live_chat" && is_safe_language_code(k))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    keys.sort();
    keys
}

/// The video's original language from a `--dump-json` response: the trimmed,
/// non-empty `language` field, else the first (sorted, safe) auto-caption key
/// ending in `-orig`, with that suffix removed.
pub fn original_language_from_metadata(value: &serde_json::Value) -> Option<String> {
    value
        .get("language")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            subtitle_map_keys(value, "automatic_captions")
                .iter()
                .find_map(|k| k.strip_suffix("-orig").map(str::to_string))
        })
}

/// Plans a bounded (≤ 2 tracks) subtitle download from a `--dump-json` response.
///
/// Prefers manual English and a manual original-language track; falls back to
/// the original-language auto track, then auto English. `None` metadata yields
/// the pattern-based fallback request; metadata with no usable tracks (or
/// invalid JSON) yields `None`.
pub fn plan_subtitle_request(metadata_json: Option<&str>) -> Option<SubtitleRequest> {
    let Some(json) = metadata_json else {
        return Some(SubtitleRequest {
            sub_langs: SUBTITLE_FALLBACK_LANGS.iter().map(|s| s.to_string()).collect(),
            write_manual: true,
            write_auto: true,
            manual_languages: Vec::new(),
            planned_from_metadata: false,
            original_language: None,
        });
    };
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let manual = subtitle_map_keys(&value, "subtitles");
    let auto = subtitle_map_keys(&value, "automatic_captions");

    let original_language = original_language_from_metadata(&value);

    let find_manual_base = |base: &str| manual.iter().find(|k| language_base(k) == base).cloned();

    let mut chosen_manual: Vec<String> = Vec::new();
    let manual_en = if manual.iter().any(|k| k == "en") {
        Some("en".to_string())
    } else {
        find_manual_base("en")
    };
    chosen_manual.extend(manual_en);

    let mut chosen_auto: Vec<String> = Vec::new();
    if let Some(orig) = &original_language {
        let orig_base = language_base(orig);
        if orig_base != "en" {
            let manual_orig = if manual.iter().any(|k| k == orig) {
                Some(orig.clone())
            } else {
                find_manual_base(&orig_base)
            };
            chosen_manual.extend(manual_orig);
        }
        if !chosen_manual.iter().any(|k| language_base(k) == orig_base) {
            let candidates = [
                format!("{orig}-orig"),
                format!("{orig_base}-orig"),
                orig.clone(),
                orig_base.clone(),
            ];
            if let Some(code) = candidates.into_iter().find(|c| auto.contains(c)) {
                chosen_auto.push(code);
            }
        }
    }
    if chosen_manual.is_empty() && chosen_auto.is_empty() {
        if let Some(code) = ["en-orig", "en"]
            .into_iter()
            .find(|c| auto.iter().any(|k| k == c))
        {
            chosen_auto.push(code.to_string());
        }
    }
    if chosen_manual.is_empty() && chosen_auto.is_empty() {
        return None;
    }

    let mut sub_langs: Vec<String> = Vec::new();
    for code in chosen_manual.iter().chain(chosen_auto.iter()) {
        if !sub_langs.contains(code) {
            sub_langs.push(code.clone());
        }
    }
    Some(SubtitleRequest {
        sub_langs,
        write_manual: !chosen_manual.is_empty(),
        write_auto: !chosen_auto.is_empty(),
        manual_languages: chosen_manual,
        planned_from_metadata: true,
        original_language,
    })
}

/// yt-dlp subtitle flags for `req`. Planned codes are re-filtered for safety;
/// fallback patterns are passed through as-is.
fn subtitle_args(req: &SubtitleRequest) -> Vec<String> {
    let mut args = Vec::new();
    if req.write_manual {
        args.push("--write-subs".to_string());
    }
    if req.write_auto {
        args.push("--write-auto-subs".to_string());
    }
    let langs: Vec<&str> = req
        .sub_langs
        .iter()
        .map(String::as_str)
        .filter(|code| !req.planned_from_metadata || is_safe_language_code(code))
        .collect();
    args.push("--sub-langs".to_string());
    args.push(langs.join(","));
    args.push("--sub-format".to_string());
    args.push(SUBTITLE_FORMAT_SELECTOR.to_string());
    args
}

/// Arguments for the media download. With `subtitles == None` this is exactly
/// the legacy media-only invocation.
fn media_download_args(
    url: &str,
    quality: Option<&str>,
    subtitles: Option<&SubtitleRequest>,
    cookie_file: Option<&Path>,
    out_template: &Path,
) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![
        url.into(),
        "-f".into(),
        quality_format(quality).into(),
        // Only called for single-item sources; --no-playlist prevents yt-dlp
        // from expanding a list= query parameter into a full playlist download
        // (e.g. music.youtube.com/watch?v=ID&list=RDAMVM…).
        "--no-playlist".into(),
    ];
    if quality == Some("audio") {
        // -x guarantees audio-only even when /best falls back to a combined
        // A/V format. No --audio-format → native remux only, no re-encode.
        args.push("-x".into());
    } else {
        // Force the video container to mp4 so we always have a known extension.
        args.push("--merge-output-format".into());
        args.push("mp4".into());
    }
    if let Some(req) = subtitles {
        args.extend(subtitle_args(req).into_iter().map(OsString::from));
        // Without -i a subtitle download error fails the whole video.
        args.push("--ignore-errors".into());
    }
    if let Some(cf) = cookie_file {
        args.push("--cookies".into());
        args.push(cf.into());
    }
    args.push("-o".into());
    args.push(out_template.into());
    args
}

/// Arguments for a subtitles-only fetch (no media download).
fn subtitle_only_args(
    url: &str,
    req: &SubtitleRequest,
    cookie_file: Option<&Path>,
    out_template: &Path,
) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![
        url.into(),
        "--skip-download".into(),
        "--no-playlist".into(),
        // --skip-download still runs format selection, which can abort
        // before subtitles are written.
        "--ignore-no-formats-error".into(),
    ];
    args.extend(subtitle_args(req).into_iter().map(OsString::from));
    args.push("--ignore-errors".into());
    if let Some(cf) = cookie_file {
        args.push("--cookies".into());
        args.push(cf.into());
    }
    args.push("-o".into());
    args.push(out_template.into());
    args
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

/// Whether a failed combined media+subtitles run should be retried media-only:
/// only when subtitles were requested, the run failed, no media was staged,
/// and stderr blames subtitles. Other failures (private, deleted, geo-blocked
/// videos) would fail again, so retrying only adds requests.
fn should_retry_media_only(
    subtitles_requested: bool,
    succeeded: bool,
    media_staged: bool,
    stderr: &str,
) -> bool {
    subtitles_requested
        && !succeeded
        && !media_staged
        && stderr.to_ascii_lowercase().contains("subtitle")
}

/// Runs `cmd` capturing stdout/stderr. With `timeout`, the child runs in its
/// own process group and the whole group is killed and an error returned once
/// the deadline passes. Pipes are drained on threads so a chatty child can't
/// deadlock against a full pipe; after the child exits, readers held open by a
/// grandchild get a short grace before the group is killed.
fn run_with_timeout(mut cmd: Command, timeout: Option<Duration>) -> Result<Output> {
    use crate::process::{isolate_process_group, kill_tree, recv_after_exit};
    let Some(timeout) = timeout else {
        return cmd.output().context("failed to spawn yt-dlp process");
    };
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    isolate_process_group(&mut cmd);
    let mut child = cmd.spawn().context("failed to spawn yt-dlp process")?;
    let pid = child.id();
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut buf);
            }
            let _ = tx.send(buf);
        });
        rx
    };
    let stdout = drain(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let stderr = drain(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => {
                kill_tree(&mut child);
                return Err(anyhow::Error::new(e).context("failed to wait for yt-dlp"));
            }
        }
        if Instant::now() >= deadline {
            // Kill the whole group so grandchildren release the pipes too.
            kill_tree(&mut child);
            bail!("yt-dlp timed out after {}s", timeout.as_secs());
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let remaining = || deadline.saturating_duration_since(Instant::now());
    Ok(Output {
        status,
        stdout: recv_after_exit(&stdout, pid, remaining()).unwrap_or_default(),
        stderr: recv_after_exit(&stderr, pid, remaining()).unwrap_or_default(),
    })
}

/// Downloads `path` via yt-dlp into `store/temp/<timestamp>/` and returns the
/// media hash, its extension (with dot) and any subtitle files staged next to it.
///
/// For video the extension is always `.mp4` (forced via `--merge-output-format`).
/// For audio (`quality == Some("audio")`) `-x` is passed to guarantee audio-only
/// output even when only combined A/V formats exist (yt-dlp strips the video
/// track). `--audio-format` is intentionally omitted so the audio stream is
/// remuxed into its native container without re-encoding — no lossy transcode,
/// no size inflation. The actual output extension is discovered by scanning.
///
/// With `subtitles` set, the same call also writes the planned subtitle tracks
/// (with `--ignore-errors` so a subtitle failure cannot fail the media). If
/// that combined call still exits non-zero without staging media and its
/// stderr mentions subtitles, it is retried once media-only (see
/// [`should_retry_media_only`]); subtitle files from the first attempt are
/// still collected. Subtitles stay
/// in the temp dir — the caller archives them before removing it.
pub fn download(
    path: String,
    store_path: &Path,
    timestamp: &String,
    quality: Option<&str>,
    subtitles: Option<&SubtitleRequest>,
    cookies: &HashMap<String, String>,
) -> Result<YtDlpDownload> {
    println!("Downloading with yt-dlp: {path}");

    let ytdlp = resolve_yt_dlp();

    let temp_dir = store_path.join("temp").join(timestamp);
    std::fs::create_dir_all(&temp_dir)?;

    let cookie_file = write_stage_cookie_file(&temp_dir, &path, cookies)?;

    // %(ext)s lets yt-dlp write the correct extension for the chosen format.
    let out_template = temp_dir.join(format!("{timestamp}.%(ext)s"));

    let run = |subs: Option<&SubtitleRequest>| {
        yt_dlp_command(&ytdlp)
            .args(media_download_args(
                &path,
                quality,
                subs,
                cookie_file.as_deref(),
                &out_template,
            ))
            .output()
            .with_context(|| format!("failed to spawn {} process", ytdlp.display()))
    };

    let mut out = run(subtitles);
    if let Ok(first) = &out {
        let stderr = String::from_utf8_lossy(&first.stderr);
        let media_staged = collect_staged_outputs(&temp_dir, timestamp, None)
            .is_ok_and(|staged| staged.media.is_some());
        if should_retry_media_only(subtitles.is_some(), first.status.success(), media_staged, &stderr)
        {
            let first_line = stderr.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
            eprintln!(
                "warn: yt-dlp with subtitles failed for {path}; retrying media-only: {first_line}"
            );
            out = run(None);
        }
    }

    // Remove cookie file immediately regardless of outcome.
    if let Some(cf) = &cookie_file {
        let _ = std::fs::remove_file(cf);
    }

    let out = out?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("yt-dlp failed: {stderr}");
    }

    let staged = collect_staged_outputs(&temp_dir, timestamp, subtitles)?;
    let media = staged
        .media
        .ok_or_else(|| anyhow!("yt-dlp output file not found in {}", temp_dir.display()))?;
    let extension = media
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let hash = hash_file(&media)?;
    Ok(YtDlpDownload {
        hash,
        extension,
        subtitles: staged.subtitles,
    })
}

/// Fetches only the subtitle tracks in `request` for `url` (no media) into
/// `store/temp/<stage_key>/`. The caller removes that directory.
///
/// A non-zero exit still returns whatever subtitle files were written; it is
/// an error only when none were. The call is killed after `timeout`.
pub fn download_subtitles(
    url: &str,
    store_path: &Path,
    stage_key: &str,
    request: &SubtitleRequest,
    cookies: &HashMap<String, String>,
    timeout: Duration,
) -> Result<Vec<StagedSubtitle>> {
    let ytdlp = resolve_yt_dlp();

    let temp_dir = store_path.join("temp").join(stage_key);
    std::fs::create_dir_all(&temp_dir)?;

    let cookie_file = write_stage_cookie_file(&temp_dir, url, cookies)?;
    let out_template = temp_dir.join(format!("{stage_key}.%(ext)s"));

    let mut cmd = yt_dlp_command(&ytdlp);
    cmd.args(subtitle_only_args(
        url,
        request,
        cookie_file.as_deref(),
        &out_template,
    ));
    let out = run_with_timeout(cmd, Some(timeout))
        .with_context(|| format!("yt-dlp subtitle download for {url}"));

    if let Some(cf) = &cookie_file {
        let _ = std::fs::remove_file(cf);
    }

    let out = out?;
    let staged = collect_staged_outputs(&temp_dir, stage_key, Some(request))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        if staged.subtitles.is_empty() {
            bail!("yt-dlp subtitle download failed: {stderr}");
        }
        eprintln!(
            "warn: yt-dlp subtitle download for {url} exited with {} but wrote {} file(s): {stderr}",
            out.status,
            staged.subtitles.len()
        );
    }
    Ok(staged.subtitles)
}

/// Arguments for an audio-only fetch used by local transcription. No `-x`:
/// the transcriber's own ffmpeg step converts, and `-x` would make yt-dlp run
/// ffmpeg a second time.
fn audio_only_args(url: &str, cookie_file: Option<&Path>, out_template: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![
        url.into(),
        "-f".into(),
        "bestaudio/best".into(),
        "--no-playlist".into(),
    ];
    if let Some(cf) = cookie_file {
        args.push("--cookies".into());
        args.push(cf.as_os_str().to_os_string());
    }
    args.push("-o".into());
    args.push(out_template.as_os_str().to_os_string());
    args
}

/// Downloads only the audio of `url` into `store/temp/<stage_key>/` for local
/// transcription and returns the staged file (`<stage_key>.audio.<ext>`). The
/// file is transient: the caller owns and removes that directory. The call is
/// killed after `timeout`.
pub fn download_audio_for_transcription(
    url: &str,
    store_path: &Path,
    stage_key: &str,
    cookies: &HashMap<String, String>,
    timeout: Duration,
) -> Result<PathBuf> {
    let temp_dir = store_path.join("temp").join(stage_key);
    std::fs::create_dir_all(&temp_dir)?;

    let cookie_file = write_stage_cookie_file(&temp_dir, url, cookies)?;
    let stem = format!("{stage_key}.audio");
    let out_template = temp_dir.join(format!("{stem}.%(ext)s"));

    let mut cmd = yt_dlp_command(&resolve_yt_dlp());
    cmd.args(audio_only_args(url, cookie_file.as_deref(), &out_template));
    let out = run_with_timeout(cmd, Some(timeout))
        .with_context(|| format!("yt-dlp audio download for {url}"));

    if let Some(cf) = &cookie_file {
        let _ = std::fs::remove_file(cf);
    }

    let out = out?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("yt-dlp audio download failed: {stderr}");
    }
    collect_staged_outputs(&temp_dir, &stem, None)?
        .media
        .ok_or_else(|| anyhow!("yt-dlp wrote no audio file"))
}

/// Writes a restrictive-permissions cookie file into `temp_dir` if cookies are
/// provided. Never pass cookie values in process args (ps exposure).
fn write_stage_cookie_file(
    temp_dir: &Path,
    url: &str,
    cookies: &HashMap<String, String>,
) -> Result<Option<PathBuf>> {
    if cookies.is_empty() {
        return Ok(None);
    }
    let cf_path = temp_dir.join("cookies.txt");
    let domain = domain_from_url(url);
    write_netscape_cookie_file(cookies, &domain, &cf_path)
        .context("failed to write yt-dlp cookie file")?;
    Ok(Some(cf_path))
}

/// Files yt-dlp left in a staging directory, split by role.
#[derive(Debug)]
struct StagedOutputs {
    media: Option<PathBuf>,
    subtitles: Vec<StagedSubtitle>,
}

/// Classifies the files yt-dlp wrote to `temp_dir` for output stem `stem`.
///
/// `{stem}.<ext>` is media; `{stem}.<lang>.<vtt|srt>` is a subtitle. The exact
/// `{stem}.` prefix is stripped (capture stems contain dots). Partial/temp
/// files, `cookies.txt`, intermediate format files (`{stem}.f137.mp4`) and
/// unsupported subtitle formats are ignored. Entries are visited in name
/// order so the result is deterministic.
fn collect_staged_outputs(
    temp_dir: &Path,
    stem: &str,
    request: Option<&SubtitleRequest>,
) -> Result<StagedOutputs> {
    let mut names: Vec<(String, PathBuf)> = std::fs::read_dir(temp_dir)
        .with_context(|| format!("failed to read temp dir {}", temp_dir.display()))?
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_str()?.to_string();
            Some((name, entry.path()))
        })
        .collect();
    names.sort();

    let prefix = format!("{stem}.");
    let mut media = None;
    let mut subtitles = Vec::new();
    for (name, path) in names {
        if name == "cookies.txt"
            || name.ends_with(".part")
            || name.ends_with(".ytdl")
            || name.ends_with(".temp")
        {
            continue;
        }
        let Some(rest) = name.strip_prefix(&prefix) else {
            continue;
        };
        match rest.rsplit_once('.') {
            None => {
                if !rest.is_empty() && !matches!(rest, "vtt" | "srt" | "json") && media.is_none() {
                    media = Some(path);
                }
            }
            Some((language, format)) => {
                if !is_safe_language_code(language) {
                    continue;
                }
                if matches!(format, "vtt" | "srt") {
                    let kind = match request {
                        Some(r) if r.planned_from_metadata => {
                            if r.manual_languages.iter().any(|m| m == language) {
                                SubtitleKind::Manual
                            } else {
                                SubtitleKind::Auto
                            }
                        }
                        _ => SubtitleKind::Unknown,
                    };
                    subtitles.push(StagedSubtitle {
                        path,
                        language: language.to_string(),
                        kind,
                        format: format.to_string(),
                        original_language: request.and_then(|r| r.original_language.clone()),
                    });
                } else if UNSUPPORTED_SUBTITLE_EXTENSIONS.contains(&format) {
                    eprintln!(
                        "warn: ignoring unsupported subtitle format {format} for language {language}: {}",
                        path.display()
                    );
                }
            }
        }
    }
    Ok(StagedOutputs { media, subtitles })
}

/// Fetches metadata JSON for `path` via `yt-dlp --dump-json`.
///
/// This is a simulate call — it does NOT download any media.
/// On failure (non-zero exit or no stdout), prints the captured stderr
/// to stderr (for debugging) then returns `None` so callers can proceed.
/// Unbounded; summary-time callers use [`fetch_metadata_with_timeout`].
pub fn fetch_metadata(path: &str, cookies: &HashMap<String, String>) -> Option<String> {
    fetch_metadata_with_timeout(path, cookies, None)
}

/// [`fetch_metadata`] with an optional wall-clock bound; a timeout returns `None`.
pub fn fetch_metadata_with_timeout(
    path: &str,
    cookies: &HashMap<String, String>,
    timeout: Option<Duration>,
) -> Option<String> {
    let ytdlp = resolve_yt_dlp();

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

    let mut cmd = yt_dlp_command(&ytdlp);
    cmd.arg("--dump-json")
        // Same rationale as download(): only called for single-item sources;
        // prevents --dump-json from emitting one JSON object per playlist item
        // when the URL contains a list= parameter.
        .arg("--no-playlist");
    if let Some(cf) = &cookie_file {
        cmd.arg("--cookies").arg(cf);
    }
    cmd.arg(path);

    let out = match run_with_timeout(cmd, timeout) {
        Ok(out) => Some(out),
        Err(e) => {
            eprintln!("warn: yt-dlp --dump-json for {path}: {e:#}");
            None
        }
    };

    // Remove cookie file regardless of outcome.
    if let Some(cf) = &cookie_file {
        let _ = std::fs::remove_file(cf);
    }

    let out = out?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        eprintln!(
            "warn: yt-dlp --dump-json failed for {path} (status {:?}): {stderr}",
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
    let ytdlp = resolve_yt_dlp();

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

    let mut cmd = yt_dlp_command(&ytdlp);
    cmd.arg("-J").arg("--flat-playlist");
    if let Some(cf) = &cookie_file {
        cmd.arg("--cookies").arg(cf);
    }
    cmd.arg(url);

    let out = cmd.output();
    if let Some(cf) = &cookie_file {
        let _ = std::fs::remove_file(cf);
    }
    let out = out.with_context(|| format!("failed to spawn {}", ytdlp.display()))?;
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
    let ytdlp = resolve_yt_dlp();

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

    let mut cmd = yt_dlp_command(&ytdlp);
    cmd.arg("-J"); // full metadata — NOT --flat-playlist
    if let Some(cf) = &cookie_file {
        cmd.arg("--cookies").arg(cf);
    }
    cmd.arg(url);

    let out = cmd.output();
    if let Some(cf) = &cookie_file {
        let _ = std::fs::remove_file(cf);
    }
    let out = out.with_context(|| format!("failed to spawn {}", ytdlp.display()))?;
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
    use super::{
        available_video_heights, collect_staged_outputs, has_audio_track, media_download_args,
        plan_subtitle_request, quality_format, resolve_yt_dlp_uncached, run_with_timeout,
        should_retry_media_only, state_dir,
        subtitle_only_args, SubtitleKind, SubtitleRequest, STATE_DIR_ENV,
        SUBTITLE_FALLBACK_LANGS, SUBTITLE_FORMAT_SELECTOR, YT_DLP_ENV, YT_DLP_FORCE_ENV,
        yt_dlp_command_with, refresh_yt_dlp, resolve_yt_dlp,
    };
    use crate::downloader::js_runtime::{JsRuntime, JsRuntimeKind, DENO_ENV, JS_RUNTIME_ENV};
    use std::time::Duration;
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};
    use std::sync::MutexGuard;

    /// Clears every env var the resolvers read and hands back the shared serialising guard.
    fn env_guard() -> MutexGuard<'static, ()> {
        let guard = crate::downloader::RESOLVER_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for key in [YT_DLP_FORCE_ENV, YT_DLP_ENV, STATE_DIR_ENV, JS_RUNTIME_ENV, DENO_ENV] {
            unsafe { std::env::remove_var(key) };
        }
        guard
    }

    /// Writes an executable stub that reports `version` when asked for `--version`.
    fn fake_yt_dlp(path: &Path, version: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, format!("#!/bin/sh\necho {version}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    #[test]
    fn resolve_yt_dlp_prefers_state_dir_when_newer() {
        let _guard = env_guard();
        let tmp = tempfile::tempdir().unwrap();

        let pinned = tmp.path().join("nix/yt-dlp");
        fake_yt_dlp(&pinned, "2026.08.19");

        let state = tmp.path().join("state");
        fake_yt_dlp(&state.join("yt-dlp/yt-dlp"), "2026.09.01");

        unsafe {
            std::env::set_var(YT_DLP_ENV, &pinned);
            std::env::set_var(STATE_DIR_ENV, &state);
        }

        assert_eq!(resolve_yt_dlp_uncached(), state.join("yt-dlp/yt-dlp"));
    }

    #[test]
    fn resolve_yt_dlp_prefers_pinned_when_newer() {
        let _guard = env_guard();
        let tmp = tempfile::tempdir().unwrap();

        let pinned = tmp.path().join("nix/yt-dlp");
        fake_yt_dlp(&pinned, "2026.09.15");

        let state = tmp.path().join("state");
        fake_yt_dlp(&state.join("yt-dlp/yt-dlp"), "2026.08.19");

        unsafe {
            std::env::set_var(YT_DLP_ENV, &pinned);
            std::env::set_var(STATE_DIR_ENV, &state);
        }

        assert_eq!(resolve_yt_dlp_uncached(), pinned);
    }

    #[test]
    fn resolve_yt_dlp_breaks_version_ties_toward_state_dir() {
        let _guard = env_guard();
        let tmp = tempfile::tempdir().unwrap();

        let pinned = tmp.path().join("nix/yt-dlp");
        fake_yt_dlp(&pinned, "2026.09.01");

        let state = tmp.path().join("state");
        fake_yt_dlp(&state.join("yt-dlp/yt-dlp"), "2026.09.01");

        unsafe {
            std::env::set_var(YT_DLP_ENV, &pinned);
            std::env::set_var(STATE_DIR_ENV, &state);
        }

        assert_eq!(resolve_yt_dlp_uncached(), state.join("yt-dlp/yt-dlp"));
    }

    #[test]
    fn resolve_yt_dlp_honours_force_override_regardless_of_version() {
        let _guard = env_guard();
        let tmp = tempfile::tempdir().unwrap();

        let forced = tmp.path().join("forced/yt-dlp");
        fake_yt_dlp(&forced, "2020.01.01");

        let state = tmp.path().join("state");
        fake_yt_dlp(&state.join("yt-dlp/yt-dlp"), "2026.09.01");

        unsafe {
            std::env::set_var(YT_DLP_FORCE_ENV, &forced);
            std::env::set_var(STATE_DIR_ENV, &state);
        }

        assert_eq!(resolve_yt_dlp_uncached(), forced);
    }

    #[test]
    fn resolve_yt_dlp_falls_back_to_bare_when_no_candidate_exists() {
        let _guard = env_guard();
        let tmp = tempfile::tempdir().unwrap();

        unsafe {
            std::env::set_var(YT_DLP_ENV, tmp.path().join("missing/yt-dlp"));
            std::env::set_var(STATE_DIR_ENV, tmp.path().join("empty-state"));
        }

        assert_eq!(resolve_yt_dlp_uncached(), PathBuf::from("yt-dlp"));
    }

    #[test]
    fn state_dir_override_wins_over_platform_default() {
        let _guard = env_guard();
        unsafe { std::env::set_var(STATE_DIR_ENV, "/tmp/archivr-state-override") };
        assert_eq!(state_dir(), Some(PathBuf::from("/tmp/archivr-state-override")));
    }

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

    fn os_args(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    fn has_arg(args: &[OsString], needle: &str) -> bool {
        args.iter().any(|a| a == needle)
    }

    #[test]
    fn plan_subtitle_request_prefers_manual_english() {
        let json = r#"{
            "language": "en",
            "subtitles": {"en": [], "en-GB": [], "live_chat": []},
            "automatic_captions": {"en": [], "en-orig": [], "fr": []}
        }"#;
        let req = plan_subtitle_request(Some(json)).unwrap();
        assert_eq!(req.sub_langs, vec!["en".to_string()]);
        assert!(req.write_manual);
        assert!(!req.write_auto);
        assert_eq!(req.manual_languages, vec!["en".to_string()]);
        assert!(req.planned_from_metadata);
        assert_eq!(req.original_language.as_deref(), Some("en"));
    }

    #[test]
    fn plan_subtitle_request_adds_original_language_auto_track() {
        let json = r#"{
            "language": "de",
            "subtitles": {},
            "automatic_captions": {"de-orig": [], "de": [], "en": []}
        }"#;
        let req = plan_subtitle_request(Some(json)).unwrap();
        assert_eq!(req.sub_langs, vec!["de-orig".to_string()]);
        assert!(!req.write_manual);
        assert!(req.write_auto);
        assert!(req.manual_languages.is_empty());
        assert_eq!(req.original_language.as_deref(), Some("de"));
    }

    #[test]
    fn plan_subtitle_request_manual_en_plus_auto_original() {
        // No `language` field: the original language comes from the -orig auto key.
        let json = r#"{
            "subtitles": {"en": []},
            "automatic_captions": {"ja-orig": [], "ja": [], "en": []}
        }"#;
        let req = plan_subtitle_request(Some(json)).unwrap();
        assert_eq!(req.sub_langs, vec!["en".to_string(), "ja-orig".to_string()]);
        assert!(req.write_manual);
        assert!(req.write_auto);
        assert_eq!(req.manual_languages, vec!["en".to_string()]);
        assert_eq!(req.original_language.as_deref(), Some("ja"));
    }

    #[test]
    fn plan_subtitle_request_none_when_no_tracks() {
        let json = r#"{"language": "de", "subtitles": {}, "automatic_captions": {}}"#;
        assert_eq!(plan_subtitle_request(Some(json)), None);
        assert_eq!(plan_subtitle_request(Some("{}")), None);
        assert_eq!(plan_subtitle_request(Some("not json")), None);
    }

    #[test]
    fn plan_subtitle_request_fallback_without_metadata() {
        let req = plan_subtitle_request(None).unwrap();
        assert_eq!(req.sub_langs, SUBTITLE_FALLBACK_LANGS.map(String::from).to_vec());
        assert!(req.write_manual);
        assert!(req.write_auto);
        assert!(!req.planned_from_metadata);
        assert!(req.manual_languages.is_empty());
        assert_eq!(req.original_language, None);
    }

    #[test]
    fn plan_subtitle_request_drops_unsafe_codes() {
        let json = r#"{
            "subtitles": {"en.*": [], "-en": [], "en,de": []},
            "automatic_captions": {"en": []}
        }"#;
        let req = plan_subtitle_request(Some(json)).unwrap();
        assert_eq!(req.sub_langs, vec!["en".to_string()]);
        assert!(!req.write_manual);
        assert!(req.write_auto);

        let only_unsafe = r#"{"subtitles": {"en|.*": []}, "automatic_captions": {"-en": []}}"#;
        assert_eq!(plan_subtitle_request(Some(only_unsafe)), None);
    }

    #[test]
    fn media_download_args_without_subtitles_matches_legacy() {
        let template = Path::new("/store/temp/ts/ts.%(ext)s");
        let args = media_download_args("https://youtu.be/x", None, None, None, template);
        for flag in ["--write-subs", "--write-auto-subs", "--sub-langs", "--ignore-errors"] {
            assert!(!has_arg(&args, flag), "unexpected {flag}");
        }
        assert_eq!(
            args,
            os_args(&[
                "https://youtu.be/x",
                "-f",
                "bestvideo+bestaudio/best",
                "--no-playlist",
                "--merge-output-format",
                "mp4",
                "-o",
                "/store/temp/ts/ts.%(ext)s",
            ])
        );

        let audio = media_download_args(
            "https://youtu.be/x",
            Some("audio"),
            None,
            Some(Path::new("/c/cookies.txt")),
            template,
        );
        assert_eq!(
            audio,
            os_args(&[
                "https://youtu.be/x",
                "-f",
                "bestaudio[ext=webm]/bestaudio[ext=m4a]/bestaudio/best",
                "--no-playlist",
                "-x",
                "--cookies",
                "/c/cookies.txt",
                "-o",
                "/store/temp/ts/ts.%(ext)s",
            ])
        );
    }

    fn en_de_request() -> SubtitleRequest {
        SubtitleRequest {
            sub_langs: vec!["en".to_string(), "de-orig".to_string()],
            write_manual: true,
            write_auto: true,
            manual_languages: vec!["en".to_string()],
            planned_from_metadata: true,
            original_language: Some("de".to_string()),
        }
    }

    #[test]
    fn media_download_args_with_subtitles() {
        let template = Path::new("/t/ts.%(ext)s");
        let req = en_de_request();
        let args = media_download_args("https://youtu.be/x", None, Some(&req), None, template);
        for flag in [
            "--write-subs",
            "--write-auto-subs",
            "--sub-langs",
            "en,de-orig",
            "--sub-format",
            SUBTITLE_FORMAT_SELECTOR,
            "--ignore-errors",
        ] {
            assert!(has_arg(&args, flag), "missing {flag}");
        }
        assert!(!has_arg(&args, "--convert-subs"));
        // Output template stays last.
        assert_eq!(args[args.len() - 2], OsString::from("-o"));

        let auto_only = SubtitleRequest {
            write_manual: false,
            manual_languages: Vec::new(),
            ..req
        };
        let args = media_download_args("https://youtu.be/x", None, Some(&auto_only), None, template);
        assert!(!has_arg(&args, "--write-subs"));
        assert!(has_arg(&args, "--write-auto-subs"));
    }

    #[test]
    fn subtitle_only_args_skip_download() {
        let req = en_de_request();
        let args = subtitle_only_args(
            "https://youtu.be/x",
            &req,
            Some(Path::new("/t/cookies.txt")),
            Path::new("/t/k.%(ext)s"),
        );
        assert_eq!(args[0], OsString::from("https://youtu.be/x"));
        for flag in [
            "--skip-download",
            "--no-playlist",
            "--ignore-no-formats-error",
            "--write-subs",
            "--write-auto-subs",
            "en,de-orig",
            "--ignore-errors",
            "--cookies",
        ] {
            assert!(has_arg(&args, flag), "missing {flag}");
        }
        assert!(!has_arg(&args, "-f"));
        assert!(!has_arg(&args, "--merge-output-format"));
        assert!(!has_arg(&args, "--convert-subs"));
        assert_eq!(&args[args.len() - 2..], &os_args(&["-o", "/t/k.%(ext)s"])[..]);
    }

    #[test]
    fn collect_staged_outputs_separates_media_and_subtitles() {
        let tmp = tempfile::tempdir().unwrap();
        // Realistic capture stem: contains a dot from `%.3f`.
        let stem = "2026-10-05T12-30-45.123-0123456789abcdef";
        for name in [
            format!("{stem}.mp4"),
            format!("{stem}.en.vtt"),
            format!("{stem}.de-orig.vtt"),
            format!("{stem}.en.ttml"),
            format!("{stem}.f137.mp4.part"),
            format!("{stem}.f137.mp4"),
            format!("{stem}.info.json"),
            "cookies.txt".to_string(),
            "other.en.vtt".to_string(),
        ] {
            std::fs::write(tmp.path().join(name), b"x").unwrap();
        }

        let req = en_de_request();
        let staged = collect_staged_outputs(tmp.path(), stem, Some(&req)).unwrap();
        assert_eq!(staged.media, Some(tmp.path().join(format!("{stem}.mp4"))));
        assert_eq!(staged.subtitles.len(), 2);

        let de = &staged.subtitles[0];
        assert_eq!(de.language, "de-orig");
        assert_eq!(de.kind, SubtitleKind::Auto);
        assert_eq!(de.format, "vtt");
        assert_eq!(de.original_language.as_deref(), Some("de"));
        assert_eq!(de.path, tmp.path().join(format!("{stem}.de-orig.vtt")));

        let en = &staged.subtitles[1];
        assert_eq!(en.language, "en");
        assert_eq!(en.kind, SubtitleKind::Manual);

        // Without a metadata plan the kind is unknown.
        let fallback = plan_subtitle_request(None);
        let unplanned = collect_staged_outputs(tmp.path(), stem, fallback.as_ref()).unwrap();
        assert!(unplanned.subtitles.iter().all(|s| s.kind == SubtitleKind::Unknown));
    }

    #[test]
    fn collect_staged_outputs_reports_missing_media() {
        let tmp = tempfile::tempdir().unwrap();
        let stem = "subs-abc";
        std::fs::write(tmp.path().join(format!("{stem}.en.srt")), b"x").unwrap();
        let staged = collect_staged_outputs(tmp.path(), stem, None).unwrap();
        assert_eq!(staged.media, None);
        assert_eq!(staged.subtitles.len(), 1);
        assert_eq!(staged.subtitles[0].format, "srt");
        assert_eq!(staged.subtitles[0].kind, SubtitleKind::Unknown);
    }

    #[test]
    fn subtitle_kind_round_trips() {
        for kind in [SubtitleKind::Manual, SubtitleKind::Auto, SubtitleKind::Unknown] {
            assert_eq!(SubtitleKind::parse(kind.as_str()), kind);
        }
        assert_eq!(SubtitleKind::parse("bogus"), SubtitleKind::Unknown);
    }

    #[cfg(unix)]
    #[test]
    fn run_with_timeout_kills_a_hung_child() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let stub = tmp.path().join("yt-dlp");
        std::fs::write(&stub, "#!/bin/sh\necho started\nexec sleep 30\n").unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let started = std::time::Instant::now();
        let err = run_with_timeout(std::process::Command::new(&stub), Some(Duration::from_secs(1)))
            .unwrap_err()
            .to_string();
        assert!(err.contains("timed out"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(10));

        let stub2 = tmp.path().join("yt-dlp-exit");
        std::fs::write(&stub2, "#!/bin/sh\necho hello\necho oops >&2\nexit 3\n").unwrap();
        std::fs::set_permissions(&stub2, std::fs::Permissions::from_mode(0o755)).unwrap();
        let out = run_with_timeout(std::process::Command::new(&stub2), Some(Duration::from_secs(5)))
            .unwrap();
        assert_eq!(out.status.code(), Some(3));
        assert_eq!(out.stdout, b"hello\n");
        assert_eq!(out.stderr, b"oops\n");
    }

    #[cfg(unix)]
    #[test]
    fn run_with_timeout_kills_grandchildren() {
        use crate::process::test_support::{assert_grandchild_gone, spawn_grandchild_snippet};
        let tmp = tempfile::tempdir().unwrap();

        // Timeout: the background grandchild dies with the group.
        let pid_file = tmp.path().join("timeout.pid");
        let mut cmd = std::process::Command::new("sh");
        cmd.args(["-c", &format!("{}sleep 30", spawn_grandchild_snippet(&pid_file))]);
        let started = std::time::Instant::now();
        let err = run_with_timeout(cmd, Some(Duration::from_secs(1))).unwrap_err().to_string();
        assert!(err.contains("timed out"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_grandchild_gone(&pid_file);

        // Child exits but a grandchild holds the pipes: no wait for the whole budget.
        let pid_file = tmp.path().join("exit.pid");
        let mut cmd = std::process::Command::new("sh");
        cmd.args(["-c", &format!("{}echo done", spawn_grandchild_snippet(&pid_file))]);
        let started = std::time::Instant::now();
        let out = run_with_timeout(cmd, Some(Duration::from_secs(60))).unwrap();
        assert_eq!(out.stdout, b"done\n");
        assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
        assert_grandchild_gone(&pid_file);
    }

    #[test]
    fn media_only_retry_requires_subtitle_failure_without_media() {
        let subs_err = "ERROR: Unable to download video subtitles for 'en': HTTP Error 429";
        assert!(should_retry_media_only(true, false, false, subs_err));
        // Private/unavailable video: stderr doesn't blame subtitles.
        assert!(!should_retry_media_only(true, false, false, "ERROR: Private video"));
        assert!(!should_retry_media_only(true, false, true, subs_err));
        assert!(!should_retry_media_only(true, true, false, subs_err));
        assert!(!should_retry_media_only(false, false, false, subs_err));
        assert!(should_retry_media_only(true, false, false, "SUBTITLE error"));
    }

    #[test]
    fn yt_dlp_command_with_deno_prepends_js_runtime_args() {
        let rt = JsRuntime { kind: JsRuntimeKind::Deno, path: Some(PathBuf::from("/d/deno")) };
        let cmd = yt_dlp_command_with(Path::new("/x/yt-dlp"), Some(&rt));
        assert_eq!(cmd.get_program(), "/x/yt-dlp");
        let args: Vec<_> = cmd.get_args().collect();
        assert_eq!(args, ["--js-runtimes", "deno:/d/deno"]);
    }

    #[test]
    fn yt_dlp_command_with_no_runtime_adds_no_args() {
        let cmd = yt_dlp_command_with(Path::new("/x/yt-dlp"), None);
        assert_eq!(cmd.get_program(), "/x/yt-dlp");
        assert_eq!(cmd.get_args().count(), 0);
    }

    #[test]
    fn refresh_yt_dlp_swaps_cached_binary() {
        let _guard = env_guard();
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a/yt-dlp");
        let b = tmp.path().join("b/yt-dlp");
        fake_yt_dlp(&a, "2020.01.01");
        fake_yt_dlp(&b, "2020.01.02");

        unsafe { std::env::set_var(YT_DLP_FORCE_ENV, &a) };
        assert_eq!(refresh_yt_dlp(), a);
        assert_eq!(resolve_yt_dlp(), a);

        unsafe { std::env::set_var(YT_DLP_FORCE_ENV, &b) };
        assert_eq!(resolve_yt_dlp(), a);
        assert_eq!(refresh_yt_dlp(), b);
        assert_eq!(resolve_yt_dlp(), b);

        unsafe { std::env::remove_var(YT_DLP_FORCE_ENV) };
        refresh_yt_dlp();
    }

    #[test]
    fn original_language_from_metadata_prefers_language_field_then_orig_key() {
        use super::original_language_from_metadata;
        let v: serde_json::Value = serde_json::from_str(
            r#"{"language": " de ", "automatic_captions": {"ja-orig": []}}"#,
        )
        .unwrap();
        assert_eq!(original_language_from_metadata(&v).as_deref(), Some("de"));
        let v: serde_json::Value = serde_json::from_str(
            r#"{"language": "", "automatic_captions": {"en": [], "ja-orig": [], "-x-orig": []}}"#,
        )
        .unwrap();
        assert_eq!(original_language_from_metadata(&v).as_deref(), Some("ja"));
        let v: serde_json::Value =
            serde_json::from_str(r#"{"automatic_captions": {"en": []}}"#).unwrap();
        assert_eq!(original_language_from_metadata(&v), None);
    }

    #[test]
    fn audio_only_args_bestaudio_without_extract() {
        let args = super::audio_only_args(
            "https://www.youtube.com/watch?v=x",
            Some(Path::new("/t/cookies.txt")),
            Path::new("/t/k.audio.%(ext)s"),
        );
        let args: Vec<String> = args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(
            args,
            [
                "https://www.youtube.com/watch?v=x",
                "-f",
                "bestaudio/best",
                "--no-playlist",
                "--cookies",
                "/t/cookies.txt",
                "-o",
                "/t/k.audio.%(ext)s",
            ]
        );
        assert!(!args.iter().any(|a| a == "-x" || a == "--extract-audio"));
        let bare = super::audio_only_args("u", None, Path::new("/t/o"));
        assert!(!bare.iter().any(|a| a == "--cookies"));
    }

    #[test]
    fn subtitle_kind_transcribed_round_trips() {
        assert_eq!(SubtitleKind::Transcribed.as_str(), "transcribed");
        assert_eq!(SubtitleKind::parse("transcribed"), SubtitleKind::Transcribed);
    }
}
