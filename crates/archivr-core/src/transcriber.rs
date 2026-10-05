//! Local transcription fallback for YouTube summaries.
//!
//! When a summary is requested for a `youtube`/`video` entry that has no usable
//! subtitles (none archived, none fetchable from the original video), the
//! summary worker can transcribe the audio locally with an engine the user
//! picked: Whisper (whisper.cpp or a wrapper script), NVIDIA Parakeet (wrapper
//! script) or Fermion Research Phonon-2 (English only). The transcript is stored
//! as an ordinary `subtitle` artifact with `kind: "transcribed"`, so ranking,
//! reduction and digesting need no special cases.
//!
//! Engines are configured by `ARCHIVR_*` env vars only; `ARCHIVR_TRANSCRIBE_ENGINES`
//! is the gate. Every engine ends with a VTT file inside the job's temp dir.

use anyhow::{Context, Result, anyhow, bail};
use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    sync::{Condvar, Mutex},
    time::{Duration, Instant},
};
use uuid::Uuid;

use crate::{
    archive::ArchivePaths,
    capture, database,
    downloader::ytdlp::{self, StagedSubtitle, SubtitleKind, is_safe_language_code, language_base},
    env_config::{env_or, env_timeout, optional_env, required_env, resolve_cli},
    subtitles,
    summarizer::NO_SUBTITLES_AFTER_TRANSCRIPTION_MESSAGE,
};

pub const TRANSCRIBE_ENGINE_KINDS: [&str; 3] = ["whisper", "parakeet", "phonon2"];
pub const DEFAULT_TRANSCRIBE_TIMEOUT_SECS: u64 = 3600;
pub const TRANSCRIBE_SAMPLE_RATE_HZ: u32 = 16_000;

/// Bytes per second of the 16 kHz mono s16le WAV every engine receives.
const WAV_BYTES_PER_SEC: f64 = (TRANSCRIBE_SAMPLE_RATE_HZ * 2) as f64;
/// Canonical PCM WAV header size written by ffmpeg.
const WAV_HEADER_BYTES: u64 = 44;
/// Phonon `words` grouping limits for one cue.
const PHONON_CUE_MAX_SECS: f64 = 7.0;
const PHONON_CUE_MAX_CHARS: usize = 84;

const AUDIO_EXTENSIONS: [&str; 12] = [
    "mp4", "m4a", "webm", "mkv", "mov", "mp3", "opus", "ogg", "oga", "flac", "wav", "aac",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhisperBackend {
    WhisperCpp,
    Script,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriberConfig {
    /// One of [`TRANSCRIBE_ENGINE_KINDS`].
    pub kind: &'static str,
    pub executable: PathBuf,
    pub model: String,
    /// Ignored unless `kind == "whisper"`.
    pub whisper_backend: WhisperBackend,
    /// Base language codes; `None` = any. phonon2: always `Some(["en"])`.
    pub languages: Option<Vec<String>>,
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptionSettings {
    pub ffmpeg: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TranscriberInfo {
    pub kind: &'static str,
    /// "Whisper" | "NVIDIA Parakeet" | "Phonon-2"
    pub label: &'static str,
    /// `languages == Some(["en"])`.
    pub english_only: bool,
    pub languages: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptOutput {
    /// Inside the job's `out_dir`.
    pub vtt_path: PathBuf,
    /// Detected or assumed language; validated with `is_safe_language_code`.
    pub language: Option<String>,
}

/// `Send + Sync` so a boxed transcriber can cross into the server's `spawn_blocking` worker.
pub trait Transcriber: Send + Sync {
    fn kind(&self) -> &'static str;
    fn label(&self) -> &'static str;
    fn model(&self) -> &str;
    fn timeout_secs(&self) -> u64;
    /// `None` = language unknown → always true.
    fn supports_language(&self, original_language: Option<&str>) -> bool;
    fn supported_languages(&self) -> Option<&[String]>;
    fn transcribe(
        &self,
        audio_wav: &Path,
        lang_hint: Option<&str>,
        out_dir: &Path,
        deadline: Instant,
    ) -> Result<TranscriptOutput>;
}

pub struct TranscriptionRequest {
    pub transcriber: Box<dyn Transcriber>,
    pub settings: TranscriptionSettings,
}

// ── User-visible copy ──────────────────────────────────────────────────────

/// User-safe copy for a failed transcription, attached as context to the
/// detailed diagnostic. Only this text reaches the summary row; the
/// diagnostic goes to the server log.
#[derive(Debug)]
pub struct TranscriptionUserMessage(pub String);

impl std::fmt::Display for TranscriptionUserMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TranscriptionUserMessage {}

/// The [`TranscriptionUserMessage`] carried by `error`, if any.
///
/// Context layers are not reachable through `chain()` downcasts, only through
/// `anyhow::Error::downcast_ref`, so both are tried.
pub fn transcription_user_message(error: &anyhow::Error) -> Option<String> {
    error
        .chain()
        .find_map(|c| c.downcast_ref::<TranscriptionUserMessage>())
        .or_else(|| error.downcast_ref::<TranscriptionUserMessage>())
        .map(|m| m.0.clone())
}

pub fn transcription_language_unsupported_message(
    label: &str,
    lang: &str,
    supported: &[String],
) -> String {
    let supported = if supported.len() == 1 && supported[0] == "en" {
        "English".to_string()
    } else {
        format!("these languages: {}", supported.join(", "))
    };
    format!(
        "This video can’t be transcribed with {label} because it only supports {supported}, and the video’s original language is “{lang}”. Choose a different transcription engine."
    )
}

fn no_audio_message(label: &str) -> String {
    format!(
        "Local transcription with {label} couldn’t start: the archived media file is missing and the original video couldn’t be downloaded."
    )
}

fn extraction_failed_message(label: &str) -> String {
    format!(
        "Local transcription with {label} failed: the audio couldn’t be extracted from this video (it may have no audio track)."
    )
}

fn engine_exit_message(label: &str) -> String {
    format!(
        "Local transcription with {label} failed: the transcription engine exited with an error. Check the server log for details."
    )
}

fn no_transcript_file_message(label: &str) -> String {
    format!(
        "Local transcription with {label} failed: the transcription engine produced no subtitle file. Check the server log for details."
    )
}

fn timed_out_message(label: &str, timeout_secs: u64) -> String {
    format!(
        "Local transcription with {label} timed out after {timeout_secs} seconds. Raise ARCHIVR_TRANSCRIBE_TIMEOUT or choose a faster engine."
    )
}

fn busy_message(label: &str) -> String {
    format!(
        "Local transcription with {label} didn’t start because another transcription was still running. Try again later."
    )
}

fn with_user_message(error: anyhow::Error, copy: String) -> anyhow::Error {
    error.context(TranscriptionUserMessage(copy))
}

/// Marker for an engine that ran but left no usable transcript output
/// (e.g. unparsable Phonon JSON).
#[derive(Debug)]
struct NoTranscriptOutput;

impl std::fmt::Display for NoTranscriptOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("transcription engine produced no usable transcript")
    }
}

impl std::error::Error for NoTranscriptOutput {}

fn is_no_transcript_output(error: &anyhow::Error) -> bool {
    error
        .chain()
        .find_map(|c| c.downcast_ref::<NoTranscriptOutput>())
        .or_else(|| error.downcast_ref::<NoTranscriptOutput>())
        .is_some()
}

// ── Configuration ──────────────────────────────────────────────────────────

fn engine_label(kind: &str) -> &'static str {
    match kind {
        "whisper" => "Whisper",
        "parakeet" => "NVIDIA Parakeet",
        "phonon2" => "Phonon-2",
        _ => "Unknown engine",
    }
}

fn unknown_engine_error(kind: &str) -> anyhow::Error {
    anyhow!(
        "unknown transcription engine: {kind} (expected one of {})",
        TRANSCRIBE_ENGINE_KINDS.join(", ")
    )
}

/// Comma-separated list → trimmed, lowercased, non-empty, deduplicated
/// (first wins). An empty result is `None`. Used for the engine list and the
/// language allowlists.
fn parse_language_list(raw: Option<&str>) -> Option<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for item in raw?.split(',') {
        let item = item.trim().to_ascii_lowercase();
        if !item.is_empty() && !out.contains(&item) {
            out.push(item);
        }
    }
    (!out.is_empty()).then_some(out)
}

/// Engine kinds listed in `ARCHIVR_TRANSCRIBE_ENGINES`. Unknown names are
/// logged and ignored.
pub fn enabled_engine_kinds() -> Vec<&'static str> {
    let Some(list) = parse_language_list(optional_env("ARCHIVR_TRANSCRIBE_ENGINES").as_deref())
    else {
        return Vec::new();
    };
    let mut kinds = Vec::new();
    for item in list {
        match TRANSCRIBE_ENGINE_KINDS.iter().find(|k| **k == item) {
            Some(kind) => kinds.push(*kind),
            None => eprintln!(
                "warn: ARCHIVR_TRANSCRIBE_ENGINES: unknown transcription engine '{item}' ignored"
            ),
        }
    }
    kinds
}

/// Builds an engine configuration for `kind` purely from the environment.
pub fn transcriber_from_env(kind: &str) -> Result<TranscriberConfig> {
    let timeout_secs = env_timeout("ARCHIVR_TRANSCRIBE_TIMEOUT", DEFAULT_TRANSCRIBE_TIMEOUT_SECS);
    match kind {
        "whisper" => {
            let backend = match env_or("ARCHIVR_WHISPER_BACKEND", "whisper_cpp").trim() {
                "whisper_cpp" => WhisperBackend::WhisperCpp,
                "script" => WhisperBackend::Script,
                other => bail!(
                    "invalid ARCHIVR_WHISPER_BACKEND: {other} (expected whisper_cpp or script)"
                ),
            };
            let executable = match backend {
                // Auto-discovery would find whisper-cli, which does not follow
                // the script contract.
                WhisperBackend::Script => {
                    PathBuf::from(required_env("ARCHIVR_WHISPER_CLI")?.trim())
                }
                WhisperBackend::WhisperCpp => resolve_cli(
                    "ARCHIVR_WHISPER_CLI",
                    &["/opt/homebrew/bin/whisper-cli", "/usr/local/bin/whisper-cli"],
                    "whisper-cli",
                ),
            };
            Ok(TranscriberConfig {
                kind: "whisper",
                executable,
                model: required_env("ARCHIVR_WHISPER_MODEL")?.trim().to_string(),
                whisper_backend: backend,
                languages: parse_language_list(optional_env("ARCHIVR_WHISPER_LANGUAGES").as_deref()),
                timeout_secs,
            })
        }
        "parakeet" => Ok(TranscriberConfig {
            kind: "parakeet",
            executable: PathBuf::from(required_env("ARCHIVR_PARAKEET_CLI")?.trim()),
            model: env_or("ARCHIVR_PARAKEET_MODEL", "nvidia/parakeet-tdt-0.6b-v3")
                .trim()
                .to_string(),
            whisper_backend: WhisperBackend::Script,
            languages: parse_language_list(optional_env("ARCHIVR_PARAKEET_LANGUAGES").as_deref()),
            timeout_secs,
        }),
        "phonon2" => Ok(TranscriberConfig {
            kind: "phonon2",
            executable: resolve_cli(
                "ARCHIVR_PHONON2_CLI",
                &["/opt/homebrew/bin/fermion", "/usr/local/bin/fermion"],
                "fermion",
            ),
            model: env_or("ARCHIVR_PHONON2_MODEL", "phonon-2").trim().to_string(),
            whisper_backend: WhisperBackend::Script,
            // English only, regardless of env.
            languages: Some(vec!["en".to_string()]),
            timeout_secs,
        }),
        other => Err(unknown_engine_error(other)),
    }
}

pub fn transcriber_from_config(cfg: TranscriberConfig) -> Box<dyn Transcriber> {
    match (cfg.kind, cfg.whisper_backend) {
        ("phonon2", _) => Box::new(Phonon2Transcriber(cfg)),
        ("whisper", WhisperBackend::WhisperCpp) => Box::new(WhisperCppTranscriber(cfg)),
        _ => Box::new(ScriptTranscriber(cfg)),
    }
}

pub fn transcription_settings_from_env() -> TranscriptionSettings {
    TranscriptionSettings {
        ffmpeg: PathBuf::from(env_or("ARCHIVR_FFMPEG", "ffmpeg").trim()),
    }
}

/// Enabled AND configured engines, in [`TRANSCRIBE_ENGINE_KINDS`] order. A
/// configuration error is logged once per call and the engine is left out.
pub fn available_transcribers() -> Vec<TranscriberInfo> {
    let enabled = enabled_engine_kinds();
    TRANSCRIBE_ENGINE_KINDS
        .iter()
        .filter(|kind| enabled.contains(*kind))
        .filter_map(|kind| match transcriber_from_env(kind) {
            Ok(cfg) => Some(TranscriberInfo {
                kind: cfg.kind,
                label: engine_label(cfg.kind),
                english_only: cfg.languages.as_deref().is_some_and(|l| l == ["en"]),
                languages: cfg.languages,
            }),
            Err(e) => {
                eprintln!("warn: transcription engine '{kind}' is enabled but not configured: {e:#}");
                None
            }
        })
        .collect()
}

/// Server entry point: `kind` must be known, enabled in
/// `ARCHIVR_TRANSCRIBE_ENGINES`, and fully configured.
pub fn request_from_env(kind: &str) -> Result<TranscriptionRequest> {
    let kind = kind.trim().to_ascii_lowercase();
    if !TRANSCRIBE_ENGINE_KINDS.contains(&kind.as_str()) {
        return Err(unknown_engine_error(&kind));
    }
    if !enabled_engine_kinds().contains(&kind.as_str()) {
        bail!("transcription engine '{kind}' is not enabled (add it to ARCHIVR_TRANSCRIBE_ENGINES)");
    }
    let cfg = transcriber_from_env(&kind)?;
    Ok(TranscriptionRequest {
        transcriber: transcriber_from_config(cfg),
        settings: transcription_settings_from_env(),
    })
}

// ── Engines ────────────────────────────────────────────────────────────────

fn language_supported(languages: Option<&[String]>, original_language: Option<&str>) -> bool {
    match (languages, original_language) {
        (None, _) | (_, None) => true,
        (Some(allowed), Some(lang)) => allowed.contains(&language_base(lang)),
    }
}

/// Time left before `deadline`; a spent budget is a timeout error before any
/// subprocess is spawned.
fn remaining_budget(deadline: Instant) -> Result<Duration> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(anyhow::Error::new(crate::process::ProcessTimedOut { secs: 0 })
            .context("transcription budget exhausted"));
    }
    Ok(left)
}

fn safe_language(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    (!trimmed.is_empty() && is_safe_language_code(trimmed)).then(|| trimmed.to_string())
}

macro_rules! impl_engine_accessors {
    () => {
        fn kind(&self) -> &'static str {
            self.0.kind
        }
        fn label(&self) -> &'static str {
            engine_label(self.0.kind)
        }
        fn model(&self) -> &str {
            &self.0.model
        }
        fn timeout_secs(&self) -> u64 {
            self.0.timeout_secs
        }
        fn supports_language(&self, original_language: Option<&str>) -> bool {
            language_supported(self.0.languages.as_deref(), original_language)
        }
        fn supported_languages(&self) -> Option<&[String]> {
            self.0.languages.as_deref()
        }
    };
}

/// whisper.cpp's `whisper-cli`, invoked directly.
struct WhisperCppTranscriber(TranscriberConfig);

impl Transcriber for WhisperCppTranscriber {
    impl_engine_accessors!();

    fn transcribe(
        &self,
        audio_wav: &Path,
        lang_hint: Option<&str>,
        out_dir: &Path,
        deadline: Instant,
    ) -> Result<TranscriptOutput> {
        let hint = whisper_language_hint(lang_hint);
        let args = whisper_cpp_args(&self.0.model, audio_wav, hint.as_deref(), &out_dir.join("transcript"));
        crate::process::run_with_timeout(&self.0.executable, &args, None, remaining_budget(deadline)?)?;
        // `-oj` JSON carries the detected language at `result.language`.
        let detected = fs::read_to_string(out_dir.join("transcript.json"))
            .ok()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
            .and_then(|v| v["result"]["language"].as_str().and_then(safe_language));
        Ok(TranscriptOutput {
            vtt_path: out_dir.join("transcript.vtt"),
            language: detected.or(hint),
        })
    }
}

/// Any executable following the script contract (Whisper `script` backend, Parakeet).
struct ScriptTranscriber(TranscriberConfig);

impl Transcriber for ScriptTranscriber {
    impl_engine_accessors!();

    fn transcribe(
        &self,
        audio_wav: &Path,
        lang_hint: Option<&str>,
        out_dir: &Path,
        deadline: Instant,
    ) -> Result<TranscriptOutput> {
        let vtt_path = out_dir.join("transcript.vtt");
        let hint = whisper_language_hint(lang_hint);
        let args = script_args(audio_wav, &vtt_path, &self.0.model, hint.as_deref());
        crate::process::run_with_timeout(&self.0.executable, &args, None, remaining_budget(deadline)?)?;
        let detected = fs::read_to_string(out_dir.join("transcript.vtt.lang"))
            .ok()
            .and_then(|raw| safe_language(&raw));
        Ok(TranscriptOutput {
            vtt_path,
            language: detected,
        })
    }
}

/// Fermion Research Phonon-2 via `fermion transcribe <model> <wav> --json`.
struct Phonon2Transcriber(TranscriberConfig);

impl Transcriber for Phonon2Transcriber {
    impl_engine_accessors!();

    fn transcribe(
        &self,
        audio_wav: &Path,
        _lang_hint: Option<&str>,
        out_dir: &Path,
        deadline: Instant,
    ) -> Result<TranscriptOutput> {
        let args = phonon2_args(&self.0.model, audio_wav);
        let out = crate::process::run_with_timeout(
            &self.0.executable,
            &args,
            None,
            remaining_budget(deadline)?,
        )?;
        if serde_json::from_str::<serde_json::Value>(out.stdout.trim())
            .ok()
            .and_then(|v| v.get("truncated").and_then(|t| t.as_bool()))
            == Some(true)
        {
            eprintln!("warn: phonon2 reported truncated segments");
        }
        let wav_len = fs::metadata(audio_wav).map(|m| m.len()).unwrap_or(0);
        let vtt = phonon_json_to_vtt(&out.stdout, wav_duration_secs(wav_len))
            .map_err(|e| e.context(NoTranscriptOutput))?;
        let vtt_path = out_dir.join("transcript.vtt");
        fs::write(&vtt_path, vtt)
            .with_context(|| format!("failed to write {}", vtt_path.display()))?;
        Ok(TranscriptOutput {
            vtt_path,
            language: Some("en".to_string()),
        })
    }
}

// ── Pure helpers ───────────────────────────────────────────────────────────

fn whisper_cpp_args(
    model: &str,
    wav: &Path,
    lang_hint: Option<&str>,
    out_prefix: &Path,
) -> Vec<OsString> {
    vec![
        "-m".into(),
        model.into(),
        "-f".into(),
        wav.into(),
        "-l".into(),
        lang_hint.unwrap_or("auto").into(),
        "-ovtt".into(),
        "-oj".into(),
        "-of".into(),
        out_prefix.into(),
        "-np".into(),
    ]
}

fn script_args(wav: &Path, out_vtt: &Path, model: &str, lang_hint: Option<&str>) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![
        "--input".into(),
        wav.into(),
        "--output".into(),
        out_vtt.into(),
        "--model".into(),
        model.into(),
    ];
    if let Some(lang) = lang_hint {
        args.push("--language".into());
        args.push(lang.into());
    }
    args
}

fn phonon2_args(model: &str, wav: &Path) -> Vec<OsString> {
    vec!["transcribe".into(), model.into(), wav.into(), "--json".into()]
}

fn ffmpeg_resample_args(input: &Path, out_wav: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> = ["-nostdin", "-hide_banner", "-loglevel", "error", "-y", "-i"]
        .into_iter()
        .map(OsString::from)
        .collect();
    args.push(input.into());
    args.extend(
        [
            "-map", "0:a:0", "-vn", "-sn", "-dn", "-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le",
        ]
        .into_iter()
        .map(OsString::from),
    );
    args.push(out_wav.into());
    args
}

/// Base of `original_language` when it is a two-letter code (`de-orig` → `de`);
/// otherwise `None`. Never yields an unvalidated string.
fn whisper_language_hint(original_language: Option<&str>) -> Option<String> {
    let base = language_base(original_language?);
    (base.len() == 2 && base.chars().all(|c| c.is_ascii_lowercase())).then_some(base)
}

/// `HH:MM:SS.mmm`; negative or non-finite input clamps to zero.
fn format_vtt_timestamp(seconds: f64) -> String {
    let seconds = if seconds.is_finite() { seconds.max(0.0) } else { 0.0 };
    let total_ms = (seconds * 1000.0).round() as u64;
    let (h, rest) = (total_ms / 3_600_000, total_ms % 3_600_000);
    let (m, rest) = (rest / 60_000, rest % 60_000);
    let (s, ms) = (rest / 1000, rest % 1000);
    format!("{h:02}:{m:02}:{s:02}.{ms:03}")
}

/// Duration of a 16 kHz mono s16le WAV of `byte_len` bytes.
fn wav_duration_secs(byte_len: u64) -> f64 {
    byte_len.saturating_sub(WAV_HEADER_BYTES) as f64 / WAV_BYTES_PER_SEC
}

fn escape_vtt_text(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn number(v: &serde_json::Value, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|k| v.get(*k).and_then(|n| n.as_f64()))
}

fn text_field<'a>(v: &'a serde_json::Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| v.get(*k).and_then(|t| t.as_str()))
}

/// `(start, end, text)` items of a segment-like array; `None` unless every
/// item has numeric start/end and a text string.
fn timed_items(items: &[serde_json::Value], text_keys: &[&str]) -> Option<Vec<(f64, f64, String)>> {
    if items.is_empty() {
        return None;
    }
    items
        .iter()
        .map(|item| {
            Some((
                number(item, &["start", "start_s", "start_time"])?,
                number(item, &["end", "end_s", "end_time"])?,
                text_field(item, text_keys)?.to_string(),
            ))
        })
        .collect()
}

/// Converts `fermion transcribe … --json` stdout to WebVTT.
///
/// The vendor's key names are undocumented, so this accepts, in order:
/// a segment array (`segments`, else any other top-level array of objects
/// with numeric start/end and text); the `words` list grouped into cues of at
/// most 7 s or 84 characters; the top-level `text` as one cue spanning the WAV.
fn phonon_json_to_vtt(json: &str, wav_duration_secs: f64) -> Result<String> {
    let value: serde_json::Value =
        serde_json::from_str(json.trim()).context("phonon2 output is not JSON")?;
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("phonon2 output is not a JSON object"))?;

    let segment_arrays = object
        .get("segments")
        .into_iter()
        .chain(object.iter().filter(|(k, _)| *k != "segments" && *k != "words").map(|(_, v)| v));
    let segments = segment_arrays
        .filter_map(|v| v.as_array())
        .find_map(|items| timed_items(items, &["text", "segment", "transcript"]));

    let cues: Vec<(f64, f64, String)> = if let Some(segments) = segments {
        segments
    } else if let Some(words) = object
        .get("words")
        .and_then(|w| w.as_array())
        .and_then(|items| timed_items(items, &["word", "text"]))
    {
        group_words(&words)
    } else if let Some(text) = object.get("text").and_then(|t| t.as_str()) {
        vec![(0.0, wav_duration_secs, text.to_string())]
    } else {
        bail!("phonon2 JSON has no segments, words or text");
    };

    let mut vtt = String::from("WEBVTT\n\n");
    for (start, end, text) in cues {
        let text = escape_vtt_text(&text);
        if text.is_empty() {
            continue;
        }
        vtt.push_str(&format!(
            "{} --> {}\n{text}\n\n",
            format_vtt_timestamp(start),
            format_vtt_timestamp(end.max(start))
        ));
    }
    Ok(vtt)
}

fn group_words(words: &[(f64, f64, String)]) -> Vec<(f64, f64, String)> {
    let mut cues: Vec<(f64, f64, String)> = Vec::new();
    let mut current: Option<(f64, f64, String)> = None;
    for (start, end, word) in words {
        let word = word.trim();
        if word.is_empty() {
            continue;
        }
        if let Some((cue_start, cue_end, text)) = current.as_mut() {
            let too_long = end - *cue_start > PHONON_CUE_MAX_SECS
                || text.chars().count() + 1 + word.chars().count() > PHONON_CUE_MAX_CHARS;
            if !too_long {
                text.push(' ');
                text.push_str(word);
                *cue_end = *end;
                continue;
            }
            cues.extend(current.take());
        }
        current = Some((*start, *end, word.to_string()));
    }
    cues.extend(current);
    cues
}

/// The first `primary_media` artifact (id order) that is an audio/video file
/// present in the store.
fn select_audio_source(store_path: &Path, primary: &[database::RoleArtifact]) -> Option<PathBuf> {
    primary.iter().find_map(|artifact| {
        let ext = Path::new(&artifact.relpath)
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        let mime = artifact.mime_type.as_deref().unwrap_or("").to_ascii_lowercase();
        let media = AUDIO_EXTENSIONS.contains(&ext.as_str())
            || mime.starts_with("audio/")
            || mime.starts_with("video/");
        let path = store_path.join(&artifact.relpath);
        (media && path.is_file()).then_some(path)
    })
}

// ── Job orchestration ──────────────────────────────────────────────────────

/// One transcription at a time per process: engines saturate CPU/GPU.
static SLOT: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());

struct SlotGuard;

impl Drop for SlotGuard {
    fn drop(&mut self) {
        let mut busy = SLOT.0.lock().unwrap_or_else(|e| e.into_inner());
        *busy = false;
        drop(busy);
        SLOT.1.notify_one();
    }
}

fn acquire_slot(timeout: Duration) -> Option<SlotGuard> {
    let guard = SLOT.0.lock().unwrap_or_else(|e| e.into_inner());
    let (mut busy, _) = SLOT
        .1
        .wait_timeout_while(guard, timeout, |busy| *busy)
        .unwrap_or_else(|e| e.into_inner());
    if *busy {
        return None;
    }
    *busy = true;
    Some(SlotGuard)
}

/// Removes the job dir on success, error and panic.
struct TempDirGuard(PathBuf);

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
pub(crate) static TRANSCRIBE_TEST_LOCK: Mutex<()> = Mutex::new(());

/// Transcribes a YouTube video entry's audio with `request`'s engine and
/// registers the transcript as a `subtitle` artifact (`kind: "transcribed"`).
///
/// Returns the number of artifact rows inserted; `Ok(0)` without transcribing
/// when a usable subtitle track already exists (re-checked under the
/// process-wide slot). Every failure carries a [`TranscriptionUserMessage`];
/// the full diagnostic is logged here.
pub fn transcribe_entry(
    paths: &ArchivePaths,
    entry_uid: &str,
    request: &TranscriptionRequest,
    original_language: Option<&str>,
    cookie_rules: &[database::CookieRule],
) -> Result<usize> {
    let result = transcribe_entry_inner(paths, entry_uid, request, original_language, cookie_rules);
    if let Err(e) = &result {
        eprintln!("warn: transcription {entry_uid}: {e:#}");
    }
    result
}

fn transcribe_entry_inner(
    paths: &ArchivePaths,
    entry_uid: &str,
    request: &TranscriptionRequest,
    original_language: Option<&str>,
    cookie_rules: &[database::CookieRule],
) -> Result<usize> {
    let transcriber = request.transcriber.as_ref();
    let kind = transcriber.kind();
    let label = transcriber.label();
    let timeout_secs = transcriber.timeout_secs();
    let timed_out = || timed_out_message(label, timeout_secs);

    // 1. Slot; queueing does not use up the job budget.
    let _slot = acquire_slot(Duration::from_secs(timeout_secs)).ok_or_else(|| {
        with_user_message(
            anyhow!("another transcription held the slot for {timeout_secs}s"),
            busy_message(label),
        )
    })?;
    let started = Instant::now();
    let deadline = started + Duration::from_secs(timeout_secs);

    // 2. Re-check under the slot.
    let conn = database::open_or_initialize(&paths.archive_path)?;
    let info = database::entry_source_info(&conn, entry_uid)?
        .ok_or_else(|| anyhow!("entry not found: {entry_uid}"))?;
    if info.source_kind != "youtube" || info.entity_kind != "video" {
        bail!("entry {entry_uid} is not a YouTube video");
    }
    let store_path = &paths.store_path;
    if subtitles::usable_subtitle_count(&conn, store_path, info.entry_id)? > 0 {
        eprintln!("info: transcription {entry_uid}: a usable subtitle track already exists; skipping");
        return Ok(0);
    }

    // 3. Language gate.
    if !transcriber.supports_language(original_language) {
        let lang = original_language.unwrap_or_default();
        return Err(with_user_message(
            anyhow!("{kind} does not support original language {lang}"),
            transcription_language_unsupported_message(
                label,
                lang,
                transcriber.supported_languages().unwrap_or_default(),
            ),
        ));
    }
    if original_language.is_none() && transcriber.supported_languages().is_some() {
        eprintln!("warn: {kind}: original language unknown for {entry_uid}; assuming it is supported");
    }

    // 4. Job dir.
    let job_name = format!("transcribe-{}", Uuid::new_v4().simple());
    let job = store_path.join("temp").join(&job_name);
    fs::create_dir_all(&job).with_context(|| format!("failed to create {}", job.display()))?;
    let _job_guard = TempDirGuard(job.clone());

    // 5. Audio source: archived media, else an audio-only yt-dlp download.
    let primary = database::list_entry_artifacts_by_role(&conn, info.entry_id, "primary_media")?;
    let input = match select_audio_source(store_path, &primary) {
        Some(path) => path,
        None => {
            let Some(url) = info
                .canonical_url
                .as_deref()
                .filter(|u| u.starts_with("https://") || u.starts_with("http://"))
            else {
                return Err(with_user_message(
                    anyhow!("no archived media file and no http(s) URL to download audio from"),
                    no_audio_message(label),
                ));
            };
            let budget = remaining_budget(deadline).map_err(|e| with_user_message(e, timed_out()))?;
            let cookies = capture::resolve_cookies_for_url(cookie_rules, url);
            ytdlp::download_audio_for_transcription(url, store_path, &job_name, &cookies, budget)
                .map_err(|e| {
                    let copy = if Instant::now() >= deadline {
                        timed_out()
                    } else {
                        no_audio_message(label)
                    };
                    with_user_message(e, copy)
                })?
        }
    };

    // 6. Resample to 16 kHz mono PCM WAV.
    let wav = job.join("audio.wav");
    let budget = remaining_budget(deadline).map_err(|e| with_user_message(e, timed_out()))?;
    crate::process::run_with_timeout(
        &request.settings.ffmpeg,
        &ffmpeg_resample_args(&input, &wav),
        None,
        budget,
    )
    .map_err(|e| {
        let copy = if crate::process::is_process_timeout(&e) {
            timed_out()
        } else {
            extraction_failed_message(label)
        };
        with_user_message(e, copy)
    })?;

    // 7. Transcribe.
    remaining_budget(deadline).map_err(|e| with_user_message(e, timed_out()))?;
    let output = transcriber
        .transcribe(&wav, original_language, &job, deadline)
        .map_err(|e| {
            let copy = if crate::process::is_process_timeout(&e) {
                timed_out()
            } else if is_no_transcript_output(&e) {
                no_transcript_file_message(label)
            } else {
                engine_exit_message(label)
            };
            with_user_message(e, copy)
        })?;

    // 8. Validate.
    let raw = fs::read_to_string(&output.vtt_path)
        .ok()
        .filter(|raw| !raw.trim().is_empty())
        .ok_or_else(|| {
            with_user_message(
                anyhow!("{kind} wrote no transcript at {}", output.vtt_path.display()),
                no_transcript_file_message(label),
            )
        })?;
    if subtitles::subtitle_to_transcript(&raw).is_empty() {
        return Err(with_user_message(
            anyhow!("{kind} transcript of {entry_uid} contains no speech"),
            NO_SUBTITLES_AFTER_TRANSCRIPTION_MESSAGE.to_string(),
        ));
    }

    // 9. Stage and archive.
    let language = if kind == "phonon2" {
        "en".to_string()
    } else {
        output
            .language
            .as_deref()
            .and_then(safe_language)
            .or_else(|| original_language.and_then(safe_language))
            .unwrap_or_else(|| "und".to_string())
    };
    let staged = StagedSubtitle {
        path: output.vtt_path.clone(),
        language,
        kind: SubtitleKind::Transcribed,
        format: "vtt".to_string(),
        original_language: original_language.map(str::to_string),
    };
    let archived = subtitles::archive_staged_subtitles(store_path, vec![staged])
        .into_iter()
        .next()
        .ok_or_else(|| {
            with_user_message(
                anyhow!("failed to archive the transcript of {entry_uid}"),
                no_transcript_file_message(label),
            )
        })?;

    // 10. Register.
    let added = subtitles::register_transcript_artifact(
        &conn,
        store_path,
        info.entry_id,
        &archived,
        kind,
        transcriber.model(),
    )?;
    eprintln!(
        "info: transcribed {entry_uid} with {kind} ({}) in {:.1}s",
        subtitles::sanitize_model_name(transcriber.model()),
        started.elapsed().as_secs_f64()
    );
    Ok(added)
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::path::{Path, PathBuf};

    /// Stub ffmpeg: writes a 44-byte header's worth plus 0.1 s of zeros to its last argument.
    pub(crate) const STUB_FFMPEG: &str =
        "#!/bin/sh\nfor a; do last=\"$a\"; done\nhead -c 3244 /dev/zero > \"$last\"\n";

    /// Writes an executable `#!/bin/sh` stub into `dir` via
    /// [`crate::downloader::write_script`] (ETXTBSY-safe). That helper probes
    /// the script with `--version`, so a guard line answering that probe is
    /// inserted after the shebang: stub bodies (hanging engines, ffmpeg writing
    /// to its last argument) must not run their real behaviour during the probe.
    #[cfg(unix)]
    pub(crate) fn write_stub_script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        let (shebang, rest) = body.split_once('\n').unwrap_or((body, ""));
        let guarded = format!("{shebang}\n[ \"$1\" = --version ] && exit 0\n{rest}");
        crate::downloader::write_script(&path, &guarded);
        path
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{STUB_FFMPEG, write_stub_script};
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, MutexGuard};

    const ENV_VARS: [&str; 12] = [
        "ARCHIVR_TRANSCRIBE_ENGINES",
        "ARCHIVR_WHISPER_CLI",
        "ARCHIVR_WHISPER_MODEL",
        "ARCHIVR_WHISPER_BACKEND",
        "ARCHIVR_WHISPER_LANGUAGES",
        "ARCHIVR_PARAKEET_CLI",
        "ARCHIVR_PARAKEET_MODEL",
        "ARCHIVR_PARAKEET_LANGUAGES",
        "ARCHIVR_PHONON2_CLI",
        "ARCHIVR_PHONON2_MODEL",
        "ARCHIVR_TRANSCRIBE_TIMEOUT",
        "ARCHIVR_FFMPEG",
    ];

    /// Serializes with every other transcription test and clears the engine env.
    struct EnvGuard(#[allow(dead_code)] MutexGuard<'static, ()>);

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for k in ENV_VARS {
                unsafe { std::env::remove_var(k) };
            }
        }
    }

    fn env_guard() -> EnvGuard {
        let guard = TRANSCRIBE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        for k in ENV_VARS {
            unsafe { std::env::remove_var(k) };
        }
        EnvGuard(guard)
    }

    fn set(k: &str, v: &str) {
        unsafe { std::env::set_var(k, v) };
    }

    fn os(v: &[&str]) -> Vec<OsString> {
        v.iter().map(OsString::from).collect()
    }

    /// `languages` empty = any language.
    fn config(kind: &'static str, executable: &Path, languages: &[&str]) -> TranscriberConfig {
        TranscriberConfig {
            kind,
            executable: executable.to_path_buf(),
            model: "/models/ggml-tiny.bin".to_string(),
            whisper_backend: WhisperBackend::WhisperCpp,
            languages: (!languages.is_empty())
                .then(|| languages.iter().map(|s| s.to_string()).collect()),
            timeout_secs: 30,
        }
    }

    // ── Pure helpers ───────────────────────────────────────────────────────

    #[test]
    fn whisper_cpp_args_with_language_hint() {
        let args = whisper_cpp_args("M", Path::new("W"), Some("de"), Path::new("P"));
        assert_eq!(
            args,
            os(&["-m", "M", "-f", "W", "-l", "de", "-ovtt", "-oj", "-of", "P", "-np"])
        );
    }

    #[test]
    fn whisper_cpp_args_without_hint_uses_auto() {
        let args = whisper_cpp_args("M", Path::new("W"), None, Path::new("P"));
        assert_eq!(args[5], OsString::from("auto"));
    }

    #[test]
    fn whisper_language_hint_only_for_two_letter_bases() {
        assert_eq!(whisper_language_hint(Some("de-orig")).as_deref(), Some("de"));
        assert_eq!(whisper_language_hint(Some("en-GB")).as_deref(), Some("en"));
        assert_eq!(whisper_language_hint(Some("yue")), None);
        assert_eq!(whisper_language_hint(Some("zh-Hans")).as_deref(), Some("zh"));
        assert_eq!(whisper_language_hint(None), None);
        assert_eq!(whisper_language_hint(Some("x;rm")), None);
    }

    #[test]
    fn script_args_follow_contract() {
        let args = script_args(Path::new("a.wav"), Path::new("t.vtt"), "large-v3", Some("de"));
        assert_eq!(
            args,
            os(&["--input", "a.wav", "--output", "t.vtt", "--model", "large-v3", "--language", "de"])
        );
        let args = script_args(Path::new("a.wav"), Path::new("t.vtt"), "m", None);
        assert!(!args.contains(&OsString::from("--language")));
    }

    #[test]
    fn phonon2_args_are_transcribe_model_wav_json() {
        assert_eq!(
            phonon2_args("phonon-2", Path::new("a.wav")),
            os(&["transcribe", "phonon-2", "a.wav", "--json"])
        );
    }

    #[test]
    fn ffmpeg_resample_args_are_16k_mono_pcm() {
        let args = ffmpeg_resample_args(Path::new("in.mp4"), Path::new("out.wav"));
        let joined: Vec<String> = args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
        let joined = joined.join(" ");
        for needle in ["-map 0:a:0", "-ac 1", "-ar 16000", "-c:a pcm_s16le", "-nostdin", "-i in.mp4"] {
            assert!(joined.contains(needle), "{needle} missing in {joined}");
        }
        assert_eq!(args.last().unwrap(), &OsString::from("out.wav"));
    }

    #[test]
    fn phonon2_refuses_known_non_english_language() {
        let t = transcriber_from_config(config("phonon2", Path::new("fermion"), &["en"]));
        for lang in ["de", "de-orig", "pt-BR"] {
            assert!(!t.supports_language(Some(lang)), "{lang}");
        }
    }

    #[test]
    fn phonon2_accepts_english_variants_and_unknown() {
        let t = transcriber_from_config(config("phonon2", Path::new("fermion"), &["en"]));
        for lang in [Some("en"), Some("en-US"), Some("en-orig"), None] {
            assert!(t.supports_language(lang), "{lang:?}");
        }
        assert_eq!(t.label(), "Phonon-2");
    }

    #[test]
    fn allowlist_gating_for_whisper_and_parakeet() {
        for kind in ["whisper", "parakeet"] {
            let t = transcriber_from_config(config(kind, Path::new("x"), &["en", "de"]));
            assert!(t.supports_language(Some("de-orig")));
            assert!(!t.supports_language(Some("fr")));
            let any = transcriber_from_config(config(kind, Path::new("x"), &[]));
            assert!(any.supports_language(Some("fr")));
        }
    }

    #[test]
    fn parse_language_list_trims_lowercases_dedups() {
        assert_eq!(
            parse_language_list(Some(" EN, de ,,en, Fr ")),
            Some(vec!["en".to_string(), "de".to_string(), "fr".to_string()])
        );
        assert_eq!(parse_language_list(Some(" , ")), None);
        assert_eq!(parse_language_list(None), None);
    }

    #[test]
    fn enabled_engine_kinds_ignores_unknown_and_duplicates() {
        let _g = env_guard();
        assert!(enabled_engine_kinds().is_empty());
        set("ARCHIVR_TRANSCRIBE_ENGINES", "Phonon2, bogus, whisper, phonon2");
        assert_eq!(enabled_engine_kinds(), vec!["phonon2", "whisper"]);
    }

    #[test]
    fn transcriber_from_env_whisper_missing_model_names_variable() {
        let _g = env_guard();
        let err = transcriber_from_env("whisper").unwrap_err().to_string();
        assert!(err.contains("ARCHIVR_WHISPER_MODEL"), "{err}");
    }

    #[test]
    fn transcriber_from_env_whisper_script_backend_requires_cli() {
        let _g = env_guard();
        set("ARCHIVR_WHISPER_BACKEND", "script");
        set("ARCHIVR_WHISPER_MODEL", "large-v3-turbo");
        let err = transcriber_from_env("whisper").unwrap_err().to_string();
        assert!(err.contains("ARCHIVR_WHISPER_CLI"), "{err}");
        set("ARCHIVR_WHISPER_CLI", "/opt/fw.py");
        let cfg = transcriber_from_env("whisper").unwrap();
        assert_eq!(cfg.whisper_backend, WhisperBackend::Script);
        assert_eq!(cfg.executable, PathBuf::from("/opt/fw.py"));
    }

    #[test]
    fn transcriber_from_env_rejects_bad_backend() {
        let _g = env_guard();
        set("ARCHIVR_WHISPER_BACKEND", "cuda");
        set("ARCHIVR_WHISPER_MODEL", "m");
        let err = transcriber_from_env("whisper").unwrap_err().to_string();
        assert!(err.contains("ARCHIVR_WHISPER_BACKEND"), "{err}");
        assert!(err.contains("whisper_cpp or script"), "{err}");
    }

    #[test]
    fn transcriber_from_env_parakeet_missing_cli_names_variable() {
        let _g = env_guard();
        let err = transcriber_from_env("parakeet").unwrap_err().to_string();
        assert!(err.contains("ARCHIVR_PARAKEET_CLI"), "{err}");
        set("ARCHIVR_PARAKEET_CLI", "/opt/parakeet.py");
        set("ARCHIVR_PARAKEET_LANGUAGES", "en");
        let cfg = transcriber_from_env("parakeet").unwrap();
        assert_eq!(cfg.model, "nvidia/parakeet-tdt-0.6b-v3");
        assert_eq!(cfg.languages, Some(vec!["en".to_string()]));
    }

    #[test]
    fn transcriber_from_env_phonon2_defaults_to_fermion_and_phonon_2() {
        let _g = env_guard();
        let cfg = transcriber_from_env("phonon2").unwrap();
        assert_eq!(cfg.executable.file_name().unwrap(), "fermion");
        assert_eq!(cfg.model, "phonon-2");
        assert_eq!(cfg.languages, Some(vec!["en".to_string()]));
    }

    #[test]
    fn transcriber_from_env_rejects_unknown_kind() {
        let _g = env_guard();
        let err = transcriber_from_env("vosk").unwrap_err().to_string();
        assert_eq!(
            err,
            "unknown transcription engine: vosk (expected one of whisper, parakeet, phonon2)"
        );
    }

    #[test]
    fn transcriber_from_env_timeout_default_and_override() {
        let _g = env_guard();
        assert_eq!(
            transcriber_from_env("phonon2").unwrap().timeout_secs,
            DEFAULT_TRANSCRIBE_TIMEOUT_SECS
        );
        set("ARCHIVR_TRANSCRIBE_TIMEOUT", "90");
        assert_eq!(transcriber_from_env("phonon2").unwrap().timeout_secs, 90);
    }

    #[test]
    fn request_from_env_rejects_configured_but_not_enabled_engine() {
        let _g = env_guard();
        set("ARCHIVR_PHONON2_CLI", "/usr/bin/false");
        let err = request_from_env("phonon2").err().unwrap().to_string();
        assert!(err.contains("ARCHIVR_TRANSCRIBE_ENGINES"), "{err}");
        let err = request_from_env("vosk").err().unwrap().to_string();
        assert!(err.contains("unknown transcription engine"), "{err}");
        set("ARCHIVR_TRANSCRIBE_ENGINES", "phonon2");
        set("ARCHIVR_FFMPEG", "/x/ffmpeg");
        let req = request_from_env(" phonon2 ").unwrap();
        assert_eq!(req.transcriber.kind(), "phonon2");
        assert_eq!(req.settings.ffmpeg, PathBuf::from("/x/ffmpeg"));
    }

    #[test]
    fn available_transcribers_lists_enabled_and_configured_only() {
        let _g = env_guard();
        set("ARCHIVR_TRANSCRIBE_ENGINES", "phonon2,whisper");
        set("ARCHIVR_PHONON2_CLI", "/usr/bin/false");
        let list = available_transcribers();
        assert_eq!(list.len(), 1, "{list:?}");
        assert_eq!(list[0].kind, "phonon2");
        assert_eq!(list[0].label, "Phonon-2");
        assert!(list[0].english_only);
        assert_eq!(list[0].languages, Some(vec!["en".to_string()]));

        set("ARCHIVR_WHISPER_MODEL", "/m/ggml-tiny.bin");
        let kinds: Vec<_> = available_transcribers().iter().map(|t| t.kind).collect();
        assert_eq!(kinds, vec!["whisper", "phonon2"]);
    }

    #[test]
    fn format_vtt_timestamp_formats_and_clamps() {
        assert_eq!(format_vtt_timestamp(3661.5), "01:01:01.500");
        assert_eq!(format_vtt_timestamp(-1.0), "00:00:00.000");
        assert_eq!(format_vtt_timestamp(0.0004), "00:00:00.000");
    }

    #[test]
    fn wav_duration_from_byte_length() {
        assert_eq!(wav_duration_secs(44 + 32_000 * 3), 3.0);
        assert_eq!(wav_duration_secs(10), 0.0);
    }

    /// Real `fermion transcribe phonon-2 <wav> --json` stdout (fermion-research
    /// 0.2.9, MLX backend, 2026-10-05), with the `words` list trimmed.
    const PHONON2_SAMPLE_JSON: &str = r#"{"text": "Hello World. This is a short test of the transcription engine. It should produce a few segments of English speech.", "model": "FermionResearch/Phonon-2", "profile": "five-value", "backend": "phonon2-five-value", "engine": "mlx", "duration_seconds": 6.545, "decode_seconds": 3.117, "wall_seconds": 88.42, "segment_count": 1, "segments": [{"id": 0, "start": 0.0, "end": 6.545, "text": "Hello World. This is a short test of the transcription engine. It should produce a few segments of English speech."}], "words": [{"text": "Hello", "start": 0.0, "end": 0.4}, {"text": "World.", "start": 0.48, "end": 1.04}, {"text": "This", "start": 1.04, "end": 1.2}], "truncated": false}"#;

    #[test]
    fn phonon_json_to_vtt_from_real_sample() {
        let vtt = phonon_json_to_vtt(PHONON2_SAMPLE_JSON, 6.545).unwrap();
        assert_eq!(
            vtt,
            "WEBVTT\n\n00:00:00.000 --> 00:00:06.545\nHello World. This is a short test of the transcription engine. It should produce a few segments of English speech.\n\n"
        );
        // Without segments, the real `words` shape (`text`/`start`/`end`) is used.
        let mut value: serde_json::Value = serde_json::from_str(PHONON2_SAMPLE_JSON).unwrap();
        value.as_object_mut().unwrap().remove("segments");
        let vtt = phonon_json_to_vtt(&value.to_string(), 6.545).unwrap();
        assert!(vtt.contains("00:00:00.000 --> 00:00:01.200\nHello World. This\n"), "{vtt}");
    }

    #[test]
    fn phonon_json_to_vtt_from_segments_fixture() {
        let json = r#"{"text":"Hello world. Second & last.","model":"phonon-2","truncated":false,
            "segments":[{"start":0.0,"end":1.5,"text":" Hello world."},{"start":1.5,"end":3.25,"text":"Second & last."}],
            "words":[{"word":"Hello","start":0.0,"end":0.4}]}"#;
        let vtt = phonon_json_to_vtt(json, 3.25).unwrap();
        assert!(vtt.starts_with("WEBVTT\n\n00:00:00.000 --> 00:00:01.500\nHello world.\n"), "{vtt}");
        assert!(vtt.contains("Second &amp; last."));
        assert_eq!(
            subtitles::subtitle_to_transcript(&vtt),
            "Hello world.\nSecond & last."
        );
    }

    #[test]
    fn phonon_json_to_vtt_groups_words_when_no_segments() {
        let words: Vec<String> = (0..20)
            .map(|i| format!(r#"{{"word":"w{i}","start":{},"end":{}}}"#, i as f64, i as f64 + 0.5))
            .collect();
        let json = format!(r#"{{"text":"ignored","words":[{}]}}"#, words.join(","));
        let vtt = phonon_json_to_vtt(&json, 20.0).unwrap();
        let cues = vtt.matches(" --> ").count();
        assert_eq!(cues, 3, "{vtt}");
        assert!(vtt.contains("00:00:00.000 --> 00:00:06.500\nw0 w1 w2 w3 w4 w5 w6\n"), "{vtt}");
    }

    #[test]
    fn phonon_json_to_vtt_falls_back_to_single_cue_from_text() {
        let vtt = phonon_json_to_vtt(r#"{"text":"just <text>","model":"phonon-2"}"#, 2.5).unwrap();
        assert_eq!(vtt, "WEBVTT\n\n00:00:00.000 --> 00:00:02.500\njust &lt;text&gt;\n\n");
    }

    #[test]
    fn phonon_json_to_vtt_rejects_non_json() {
        assert!(phonon_json_to_vtt("Hello world", 1.0).is_err());
        assert!(phonon_json_to_vtt("[1,2]", 1.0).is_err());
        assert!(phonon_json_to_vtt(r#"{"model":"phonon-2"}"#, 1.0).is_err());
    }

    #[test]
    fn transcription_user_message_found_through_context_layers() {
        let err = with_user_message(anyhow!("diagnostic /secret/path"), "copy".to_string());
        assert_eq!(transcription_user_message(&err).as_deref(), Some("copy"));
        let wrapped = err.context("outer layer").context("outermost");
        assert_eq!(transcription_user_message(&wrapped).as_deref(), Some("copy"));
        assert_eq!(transcription_user_message(&anyhow!("plain")), None);
        assert!(is_no_transcript_output(&anyhow!("x").context(NoTranscriptOutput).context("y")));
    }

    #[test]
    fn language_unsupported_copy_names_supported_languages() {
        let en = transcription_language_unsupported_message("Phonon-2", "de", &["en".to_string()]);
        assert!(en.contains("only supports English"), "{en}");
        assert!(en.contains("“de”"), "{en}");
        let many = transcription_language_unsupported_message(
            "NVIDIA Parakeet",
            "ja",
            &["en".to_string(), "de".to_string()],
        );
        assert!(many.contains("these languages: en, de"), "{many}");
    }

    // ── Archive fixtures ───────────────────────────────────────────────────

    fn youtube_entry(
        canonical_url: &str,
        with_media: bool,
    ) -> (tempfile::TempDir, ArchivePaths, String, i64) {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::archive::initialize_archive(
            temp.path(),
            &temp.path().join("store"),
            "Test archive",
            false,
        )
        .unwrap();
        let conn = database::open_or_initialize(&paths.archive_path).unwrap();
        let user_id = database::ensure_default_user(&conn).unwrap();
        let run = database::create_archive_run(&conn, user_id, 1).unwrap();
        let source_id = database::upsert_source_identity(
            &conn,
            "youtube",
            "video",
            Some("vid-1"),
            Some(canonical_url),
            canonical_url,
        )
        .unwrap();
        let entry = database::create_archived_entry(
            &conn,
            &database::NewEntry {
                source_identity_id: source_id,
                archive_run_id: run.id,
                parent_entry_id: None,
                root_entry_id: None,
                created_by_user_id: user_id,
                owned_by_user_id: user_id,
                source_kind: "youtube".to_string(),
                entity_kind: "video".to_string(),
                title: Some("A video".to_string()),
                visibility: "private".to_string(),
                representation_kind: "video".to_string(),
                source_metadata_json: "{}".to_string(),
                display_metadata_json: None,
            },
        )
        .unwrap();
        if with_media {
            add_primary_media(&paths, entry.id, "raw/media.mp4");
        }
        (temp, paths, entry.entry_uid, entry.id)
    }

    fn add_primary_media(paths: &ArchivePaths, entry_id: i64, relpath: &str) {
        let file = paths.store_path.join(relpath);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, b"fake mp4 bytes").unwrap();
        let conn = database::open_or_initialize(&paths.archive_path).unwrap();
        let blob_id = database::upsert_blob(
            &conn,
            &database::BlobRecord {
                sha256: format!("media-{entry_id}"),
                byte_size: 14,
                mime_type: Some("video/mp4".to_string()),
                extension: Some("mp4".to_string()),
                raw_relpath: relpath.to_string(),
            },
        )
        .unwrap();
        database::add_entry_artifact(
            &conn,
            &database::NewArtifact {
                entry_id,
                artifact_role: "primary_media".to_string(),
                storage_area: "raw".to_string(),
                relpath: relpath.to_string(),
                blob_id: Some(blob_id),
                logical_path: None,
                metadata_json: None,
            },
        )
        .unwrap();
    }

    fn subtitle_rows(paths: &ArchivePaths, entry_id: i64) -> Vec<database::RoleArtifact> {
        let conn = database::open_or_initialize(&paths.archive_path).unwrap();
        database::list_entry_artifacts_by_role(&conn, entry_id, subtitles::SUBTITLE_ARTIFACT_ROLE)
            .unwrap()
    }

    fn no_transcribe_dirs_left(paths: &ArchivePaths) -> bool {
        let temp = paths.store_path.join("temp");
        !fs::read_dir(&temp).map_or(false, |rd| {
            rd.flatten()
                .any(|e| e.file_name().to_string_lossy().starts_with("transcribe-"))
        })
    }

    const STUB_WHISPER: &str = "#!/bin/sh\nprefix=\"\"\nwhile [ $# -gt 0 ]; do\n  if [ \"$1\" = \"-of\" ]; then prefix=\"$2\"; shift; fi\n  shift\ndone\nprintf 'WEBVTT\\n\\n00:00:00.000 --> 00:00:02.000\\nhello world\\n' > \"$prefix.vtt\"\nprintf '{\"result\":{\"language\":\"en\"}}' > \"$prefix.json\"\n";

    fn stub_request(dir: &Path, engine_body: &str, timeout_secs: u64) -> TranscriptionRequest {
        let ffmpeg = write_stub_script(dir, "ffmpeg", STUB_FFMPEG);
        let engine = write_stub_script(dir, "whisper-cli", engine_body);
        let mut cfg = config("whisper", &engine, &[]);
        cfg.timeout_secs = timeout_secs;
        TranscriptionRequest {
            transcriber: transcriber_from_config(cfg),
            settings: TranscriptionSettings { ffmpeg },
        }
    }

    #[test]
    fn select_audio_source_prefers_existing_archived_media() {
        let store = tempfile::tempdir().unwrap();
        fs::create_dir_all(store.path().join("raw")).unwrap();
        fs::write(store.path().join("raw/a.mp4"), b"x").unwrap();
        fs::write(store.path().join("raw/p.html"), b"x").unwrap();
        let artifact = |id, relpath: &str, mime: Option<&str>| database::RoleArtifact {
            id,
            relpath: relpath.to_string(),
            mime_type: mime.map(str::to_string),
            metadata_json: None,
        };
        assert_eq!(
            select_audio_source(store.path(), &[artifact(1, "raw/a.mp4", Some("video/mp4"))]),
            Some(store.path().join("raw/a.mp4"))
        );
        assert_eq!(
            select_audio_source(store.path(), &[artifact(1, "raw/missing.mp4", None)]),
            None
        );
        assert_eq!(
            select_audio_source(store.path(), &[artifact(1, "raw/p.html", Some("text/html"))]),
            None
        );
        assert_eq!(
            select_audio_source(
                store.path(),
                &[artifact(1, "raw/missing.m4a", None), artifact(2, "raw/a.mp4", None)]
            ),
            Some(store.path().join("raw/a.mp4"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn transcribe_entry_with_stub_engine_registers_transcribed_artifact() {
        let _g = env_guard();
        let (temp, paths, uid, entry_id) = youtube_entry("youtube-test:offline", true);
        let request = stub_request(temp.path(), STUB_WHISPER, 30);

        assert_eq!(transcribe_entry(&paths, &uid, &request, Some("en"), &[]).unwrap(), 1);

        let rows = subtitle_rows(&paths, entry_id);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].mime_type.as_deref(), Some("text/vtt"));
        assert!(rows[0].relpath.starts_with("raw/") && rows[0].relpath.ends_with(".vtt"));
        assert!(paths.store_path.join(&rows[0].relpath).is_file());
        let meta: serde_json::Value =
            serde_json::from_str(rows[0].metadata_json.as_deref().unwrap()).unwrap();
        assert_eq!(meta["kind"], "transcribed");
        assert_eq!(meta["origin"], "transcription");
        assert_eq!(meta["engine"], "whisper");
        assert_eq!(meta["model"], "ggml-tiny.bin");
        assert_eq!(meta["language"], "en");
        assert_eq!(meta["original_language"], "en");
        assert!(no_transcribe_dirs_left(&paths));
    }

    #[cfg(unix)]
    #[test]
    fn transcribe_entry_timeout_cleans_temp_and_returns_timeout_copy() {
        let _g = env_guard();
        let (temp, paths, uid, _) = youtube_entry("youtube-test:offline", true);
        let request = stub_request(temp.path(), "#!/bin/sh\nexec sleep 30\n", 1);
        let started = Instant::now();
        let err = transcribe_entry(&paths, &uid, &request, None, &[]).unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(10));
        let msg = transcription_user_message(&err).unwrap();
        assert!(msg.contains("timed out after 1 seconds"), "{msg}");
        assert!(no_transcribe_dirs_left(&paths));
    }

    #[cfg(unix)]
    #[test]
    fn transcribe_entry_engine_failure_message_is_sanitized() {
        let _g = env_guard();
        let (temp, paths, uid, _) = youtube_entry("youtube-test:offline", true);
        let request = stub_request(
            temp.path(),
            "#!/bin/sh\necho 'cannot open /secret/path' >&2\nexit 1\n",
            30,
        );
        let err = transcribe_entry(&paths, &uid, &request, None, &[]).unwrap_err();
        let msg = transcription_user_message(&err).unwrap();
        assert_eq!(msg, engine_exit_message("Whisper"));
        assert!(!msg.contains("/secret/path"));
        assert!(format!("{err:#}").contains("/secret/path"));
        assert!(no_transcribe_dirs_left(&paths));
    }

    #[cfg(unix)]
    #[test]
    fn transcribe_entry_empty_transcript_is_no_speech() {
        let _g = env_guard();
        let (temp, paths, uid, entry_id) = youtube_entry("youtube-test:offline", true);
        let body = "#!/bin/sh\nprefix=\"\"\nwhile [ $# -gt 0 ]; do\n  if [ \"$1\" = \"-of\" ]; then prefix=\"$2\"; shift; fi\n  shift\ndone\nprintf 'WEBVTT\\n' > \"$prefix.vtt\"\n";
        let request = stub_request(temp.path(), body, 30);
        let err = transcribe_entry(&paths, &uid, &request, None, &[]).unwrap_err();
        assert_eq!(
            transcription_user_message(&err).as_deref(),
            Some(NO_SUBTITLES_AFTER_TRANSCRIPTION_MESSAGE)
        );
        assert!(subtitle_rows(&paths, entry_id).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn transcribe_entry_missing_vtt_is_no_file_copy() {
        let _g = env_guard();
        let (temp, paths, uid, _) = youtube_entry("youtube-test:offline", true);
        let request = stub_request(temp.path(), "#!/bin/sh\nexit 0\n", 30);
        let err = transcribe_entry(&paths, &uid, &request, None, &[]).unwrap_err();
        assert_eq!(
            transcription_user_message(&err),
            Some(no_transcript_file_message("Whisper"))
        );
    }

    #[test]
    fn transcribe_entry_skips_when_usable_subtitle_already_exists() {
        let _g = env_guard();
        let (temp, paths, uid, entry_id) = youtube_entry("youtube-test:offline", true);
        let staged_path = temp.path().join("existing.en.vtt");
        fs::write(&staged_path, "WEBVTT\n\n00:00:00.000 --> 00:00:01.000\nexisting words\n").unwrap();
        let archived = subtitles::archive_staged_subtitles(
            &paths.store_path,
            vec![StagedSubtitle {
                path: staged_path,
                language: "en".to_string(),
                kind: SubtitleKind::Manual,
                format: "vtt".to_string(),
                original_language: None,
            }],
        );
        let conn = database::open_or_initialize(&paths.archive_path).unwrap();
        subtitles::register_subtitle_artifacts(
            &conn,
            &paths.store_path,
            entry_id,
            &archived,
            subtitles::SUBTITLE_ORIGIN_CAPTURE,
        )
        .unwrap();
        // Spawning either of these would fail.
        let request = TranscriptionRequest {
            transcriber: transcriber_from_config(config(
                "whisper",
                Path::new("/nonexistent/whisper-cli"),
                &[],
            )),
            settings: TranscriptionSettings {
                ffmpeg: PathBuf::from("/nonexistent/ffmpeg"),
            },
        };
        assert_eq!(transcribe_entry(&paths, &uid, &request, None, &[]).unwrap(), 0);
        assert_eq!(subtitle_rows(&paths, entry_id).len(), 1);
    }

    #[test]
    fn transcribe_entry_without_audio_and_non_http_url_fails_with_no_audio_copy() {
        let _g = env_guard();
        let (_temp, paths, uid, _) = youtube_entry("youtube-test:offline", false);
        let request = TranscriptionRequest {
            transcriber: transcriber_from_config(config(
                "whisper",
                Path::new("/nonexistent/whisper-cli"),
                &[],
            )),
            settings: TranscriptionSettings {
                ffmpeg: PathBuf::from("/nonexistent/ffmpeg"),
            },
        };
        let err = transcribe_entry(&paths, &uid, &request, None, &[]).unwrap_err();
        assert_eq!(transcription_user_message(&err), Some(no_audio_message("Whisper")));
        assert!(no_transcribe_dirs_left(&paths));
    }

    #[cfg(unix)]
    #[test]
    fn transcribe_entry_ffmpeg_failure_is_extraction_copy() {
        let _g = env_guard();
        let (temp, paths, uid, _) = youtube_entry("youtube-test:offline", true);
        let mut request = stub_request(temp.path(), STUB_WHISPER, 30);
        request.settings.ffmpeg =
            write_stub_script(temp.path(), "bad-ffmpeg", "#!/bin/sh\necho 'no audio stream' >&2\nexit 1\n");
        let err = transcribe_entry(&paths, &uid, &request, None, &[]).unwrap_err();
        assert_eq!(
            transcription_user_message(&err),
            Some(extraction_failed_message("Whisper"))
        );
    }

    #[test]
    fn transcribe_entry_refuses_unsupported_language_before_audio_work() {
        let _g = env_guard();
        let (_temp, paths, uid, _) = youtube_entry("youtube-test:offline", false);
        let request = TranscriptionRequest {
            transcriber: transcriber_from_config(config("phonon2", Path::new("/nonexistent/fermion"), &["en"])),
            settings: TranscriptionSettings {
                ffmpeg: PathBuf::from("/nonexistent/ffmpeg"),
            },
        };
        let err = transcribe_entry(&paths, &uid, &request, Some("de-orig"), &[]).unwrap_err();
        let msg = transcription_user_message(&err).unwrap();
        assert!(msg.contains("Phonon-2") && msg.contains("English") && msg.contains("de-orig"), "{msg}");
    }

    /// In-process engine that records how many jobs ran at once.
    struct OverlapProbe {
        active: Arc<AtomicUsize>,
        max_seen: Arc<AtomicUsize>,
    }

    impl Transcriber for OverlapProbe {
        fn kind(&self) -> &'static str {
            "whisper"
        }
        fn label(&self) -> &'static str {
            "Whisper"
        }
        fn model(&self) -> &str {
            "probe"
        }
        fn timeout_secs(&self) -> u64 {
            30
        }
        fn supports_language(&self, _: Option<&str>) -> bool {
            true
        }
        fn supported_languages(&self) -> Option<&[String]> {
            None
        }
        fn transcribe(&self, _: &Path, _: Option<&str>, out_dir: &Path, _: Instant) -> Result<TranscriptOutput> {
            let now = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_seen.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(200));
            self.active.fetch_sub(1, Ordering::SeqCst);
            let vtt_path = out_dir.join("transcript.vtt");
            fs::write(&vtt_path, "WEBVTT\n\n00:00:00.000 --> 00:00:01.000\nprobe words\n")?;
            Ok(TranscriptOutput { vtt_path, language: None })
        }
    }

    #[cfg(unix)]
    #[test]
    fn transcription_slot_serializes_jobs() {
        let _g = env_guard();
        let active = Arc::new(AtomicUsize::new(0));
        let max_seen = Arc::new(AtomicUsize::new(0));
        let bin = tempfile::tempdir().unwrap();
        let ffmpeg = write_stub_script(bin.path(), "ffmpeg", STUB_FFMPEG);
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let (active, max_seen, ffmpeg) = (active.clone(), max_seen.clone(), ffmpeg.clone());
                std::thread::spawn(move || {
                    let (_temp, paths, uid, _) = youtube_entry("youtube-test:offline", true);
                    let request = TranscriptionRequest {
                        transcriber: Box::new(OverlapProbe { active, max_seen }),
                        settings: TranscriptionSettings { ffmpeg },
                    };
                    transcribe_entry(&paths, &uid, &request, None, &[]).unwrap()
                })
            })
            .collect();
        for h in handles {
            assert_eq!(h.join().unwrap(), 1);
        }
        assert_eq!(max_seen.load(Ordering::SeqCst), 1);
    }
}
