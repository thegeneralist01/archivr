//! Subtitle artifacts: archiving staged yt-dlp subtitle files, registering
//! them as `subtitle` artifacts, fetching them on demand for existing entries,
//! ranking tracks, and reducing VTT/SRT to a plain transcript for summaries.

use anyhow::{anyhow, Context, Result};
use regex::Regex;
use rusqlite::{Connection, Transaction, TransactionBehavior};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::LazyLock,
};
use uuid::Uuid;

use crate::archive::ArchivePaths;
use crate::capture;
use crate::database::{self, BlobRecord, NewArtifact};
use crate::downloader::store;
use crate::downloader::ytdlp::{self, language_base, StagedSubtitle, SubtitleKind};

pub const SUBTITLE_ARTIFACT_ROLE: &str = "subtitle";
pub const SUBTITLE_ORIGIN_CAPTURE: &str = "capture";
pub const SUBTITLE_ORIGIN_SUMMARY_FETCH: &str = "summary_fetch";
/// Origin of a track produced by local transcription (`kind: "transcribed"`).
pub const SUBTITLE_ORIGIN_TRANSCRIPTION: &str = "transcription";

/// Result of [`fetch_subtitles_for_entry`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SubtitleFetchOutcome {
    /// Artifact rows inserted by this call.
    pub added: usize,
    /// The video's original language, from a successful metadata probe or
    /// else from existing subtitle artifacts' metadata.
    pub original_language: Option<String>,
}

/// Subtitle file formats archivr keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubtitleFormat {
    Vtt,
    Srt,
}

impl SubtitleFormat {
    /// Detects the format from a file extension (with or without the dot),
    /// falling back to the MIME type. Case-insensitive.
    pub fn detect(extension: &str, mime: &str) -> Option<Self> {
        let ext = extension.trim_start_matches('.').to_ascii_lowercase();
        match ext.as_str() {
            "vtt" => return Some(SubtitleFormat::Vtt),
            "srt" => return Some(SubtitleFormat::Srt),
            _ => {}
        }
        let mime = mime.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
        match mime.as_str() {
            "text/vtt" => Some(SubtitleFormat::Vtt),
            "application/x-subrip" => Some(SubtitleFormat::Srt),
            _ => None,
        }
    }

    pub fn mime(self) -> &'static str {
        match self {
            SubtitleFormat::Vtt => "text/vtt",
            SubtitleFormat::Srt => "application/x-subrip",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            SubtitleFormat::Vtt => "vtt",
            SubtitleFormat::Srt => "srt",
        }
    }
}

/// A subtitle file already moved into the content-addressed `raw/` store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchivedSubtitle {
    /// Store-relative path, e.g. `raw/a/b/<hash>.vtt`.
    pub raw_relpath: PathBuf,
    pub language: String,
    pub kind: SubtitleKind,
    pub format: SubtitleFormat,
    pub original_language: Option<String>,
}

/// Track description parsed from a `subtitle` artifact's `metadata_json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubtitleTrackMeta {
    pub language: String,
    pub kind: SubtitleKind,
    pub original_language: Option<String>,
}

/// Moves staged subtitle files into `raw/`. Files that fail to archive or have
/// an unsupported format are logged and skipped — subtitles never fail a capture.
pub fn archive_staged_subtitles(
    store_path: &Path,
    staged: Vec<StagedSubtitle>,
) -> Vec<ArchivedSubtitle> {
    let mut archived = Vec::with_capacity(staged.len());
    for sub in staged {
        let Some(format) = SubtitleFormat::detect(&sub.format, "") else {
            eprintln!(
                "warn: archive subtitle {}: unsupported format {}",
                sub.path.display(),
                sub.format
            );
            continue;
        };
        match store::archive_staged_file(&sub.path, store_path) {
            Ok(raw_relpath) => archived.push(ArchivedSubtitle {
                raw_relpath,
                language: sub.language,
                kind: sub.kind,
                format,
                original_language: sub.original_language,
            }),
            Err(e) => eprintln!("warn: archive subtitle {}: {e:#}", sub.path.display()),
        }
    }
    archived
}

/// Registers archived subtitles as `subtitle` artifacts of `entry_id`.
///
/// Runs in one `BEGIN IMMEDIATE` transaction so concurrent registrations of
/// the same content serialize; an `(entry, subtitle, blob)` that already
/// exists is skipped. Returns the number of artifact rows inserted.
pub fn register_subtitle_artifacts(
    conn: &Connection,
    store_path: &Path,
    entry_id: i64,
    subtitles: &[ArchivedSubtitle],
    origin: &str,
) -> Result<usize> {
    let rows: Vec<(&ArchivedSubtitle, serde_json::Value)> = subtitles
        .iter()
        .map(|sub| {
            let metadata = serde_json::json!({
                "language": sub.language,
                "kind": sub.kind.as_str(),
                "format": sub.format.extension(),
                "original_language": sub.original_language,
                "origin": origin,
            });
            (sub, metadata)
        })
        .collect();
    insert_subtitle_rows(conn, store_path, entry_id, &rows)
}

/// Registers a locally transcribed track (origin `transcription`), recording
/// the engine kind and model. A model given as a filesystem path is stored as
/// its file name only, so no host path is persisted. Same transaction and
/// dedup rules as [`register_subtitle_artifacts`].
pub fn register_transcript_artifact(
    conn: &Connection,
    store_path: &Path,
    entry_id: i64,
    sub: &ArchivedSubtitle,
    engine: &str,
    model: &str,
) -> Result<usize> {
    let metadata = serde_json::json!({
        "language": sub.language,
        "kind": sub.kind.as_str(),
        "format": sub.format.extension(),
        "original_language": sub.original_language,
        "origin": SUBTITLE_ORIGIN_TRANSCRIPTION,
        "engine": engine,
        "model": sanitize_model_name(model),
    });
    insert_subtitle_rows(conn, store_path, entry_id, &[(sub, metadata)])
}

/// Reduces a model that is a filesystem path (contains `\`, is absolute, or
/// exists) to its file name. Hugging Face ids such as
/// `nvidia/parakeet-tdt-0.6b-v3` are kept as-is.
pub(crate) fn sanitize_model_name(model: &str) -> String {
    let path = Path::new(model);
    if model.contains('\\') || path.is_absolute() || path.exists() {
        let name = model.rsplit(['/', '\\']).next().unwrap_or(model);
        if !name.is_empty() {
            return name.to_string();
        }
    }
    model.to_string()
}

/// Inserts one `subtitle` artifact per row with the given metadata, in one
/// `BEGIN IMMEDIATE` transaction; rows whose blob is already a subtitle of
/// the entry, or whose file can't be stat'ed, are skipped. Refreshes the
/// entry's cached bytes and returns the number of rows inserted.
fn insert_subtitle_rows(
    conn: &Connection,
    store_path: &Path,
    entry_id: i64,
    rows: &[(&ArchivedSubtitle, serde_json::Value)],
) -> Result<usize> {
    if rows.is_empty() {
        return Ok(0);
    }

    // No transaction is open on `conn` at any call site, so new_unchecked is safe.
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let mut inserted = 0;
    for (sub, metadata) in rows {
        let relpath = sub.raw_relpath.to_string_lossy().replace('\\', "/");
        let sha256 = sub
            .raw_relpath
            .file_stem()
            .and_then(|s| s.to_str())
            .with_context(|| format!("subtitle path has no hash stem: {relpath}"))?
            .to_string();
        let byte_size = match fs::metadata(store_path.join(&sub.raw_relpath)) {
            Ok(meta) => meta.len() as i64,
            Err(e) => {
                eprintln!("warn: skipping archived subtitle {relpath}: {e:#}");
                continue;
            }
        };
        let blob_id = database::upsert_blob(
            &tx,
            &BlobRecord {
                sha256,
                byte_size,
                mime_type: Some(sub.format.mime().to_string()),
                extension: Some(sub.format.extension().to_string()),
                raw_relpath: relpath.clone(),
            },
        )?;
        if database::entry_has_artifact_blob(&tx, entry_id, SUBTITLE_ARTIFACT_ROLE, blob_id)? {
            continue;
        }
        database::add_entry_artifact(
            &tx,
            &NewArtifact {
                entry_id,
                artifact_role: SUBTITLE_ARTIFACT_ROLE.to_string(),
                storage_area: "raw".to_string(),
                relpath,
                blob_id: Some(blob_id),
                logical_path: None,
                metadata_json: Some(metadata.to_string()),
            },
        )?;
        inserted += 1;
    }
    tx.commit()?;
    database::refresh_entry_cached_bytes(conn, entry_id)?;
    Ok(inserted)
}

/// Number of the entry's `subtitle` artifacts that reduce to a non-empty
/// transcript. Unreadable or unsupported files do not count.
pub(crate) fn usable_subtitle_count(
    conn: &Connection,
    store_path: &Path,
    entry_id: i64,
) -> Result<usize> {
    let artifacts = database::list_entry_artifacts_by_role(conn, entry_id, SUBTITLE_ARTIFACT_ROLE)?;
    Ok(artifacts
        .iter()
        .filter(|a| {
            let ext = Path::new(&a.relpath)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");
            SubtitleFormat::detect(ext, a.mime_type.as_deref().unwrap_or("")).is_some()
                && fs::read_to_string(store_path.join(&a.relpath))
                    .map(|raw| !subtitle_to_transcript(&raw).is_empty())
                    .unwrap_or(false)
        })
        .count())
}

/// `original_language` of the entry's first `subtitle` artifact (id order)
/// whose metadata records one.
fn existing_original_language(conn: &Connection, entry_id: i64) -> Result<Option<String>> {
    let artifacts = database::list_entry_artifacts_by_role(conn, entry_id, SUBTITLE_ARTIFACT_ROLE)?;
    Ok(artifacts
        .iter()
        .find_map(|a| parse_subtitle_metadata(a.metadata_json.as_deref()).original_language))
}

/// Downloads subtitles for an existing YouTube video entry from its original
/// URL and registers them. Returns the number of rows added by this call plus
/// the video's original language, taken from the metadata probe when it
/// succeeds and otherwise from existing subtitle artifacts.
///
/// Returns `added: 0` (no yt-dlp call) for non-YouTube-video entries or a
/// missing / non-http(s) canonical URL, and without fetching when the entry
/// already has a usable subtitle (concurrency re-check). An unreachable video
/// or any yt-dlp failure is logged and counts as zero subtitles; only DB/IO
/// errors propagate.
pub fn fetch_subtitles_for_entry(
    paths: &ArchivePaths,
    entry_uid: &str,
    cookie_rules: &[database::CookieRule],
) -> Result<SubtitleFetchOutcome> {
    let conn = database::open_or_initialize(&paths.archive_path)?;
    let info = database::entry_source_info(&conn, entry_uid)?
        .ok_or_else(|| anyhow!("entry not found: {entry_uid}"))?;
    let existing_language = existing_original_language(&conn, info.entry_id)?;
    let nothing_added = || SubtitleFetchOutcome {
        added: 0,
        original_language: existing_language.clone(),
    };
    if info.source_kind != "youtube" || info.entity_kind != "video" {
        return Ok(nothing_added());
    }
    let Some(url) = info
        .canonical_url
        .filter(|u| u.starts_with("https://") || u.starts_with("http://"))
    else {
        return Ok(nothing_added());
    };

    let store_path = &paths.store_path;
    if usable_subtitle_count(&conn, store_path, info.entry_id)? > 0 {
        return Ok(nothing_added());
    }

    let cookies = capture::resolve_cookies_for_url(cookie_rules, &url);
    let timeout = crate::summarizer::summary_cli_timeout();
    let Some(metadata) = ytdlp::fetch_metadata_with_timeout(&url, &cookies, Some(timeout)) else {
        eprintln!("warn: subtitle fetch for {entry_uid}: video unreachable ({url})");
        return Ok(nothing_added());
    };
    // Before planning: a video without captions is exactly the case that
    // needs its language for a transcription fallback.
    let original_language = serde_json::from_str::<serde_json::Value>(&metadata)
        .ok()
        .and_then(|v| ytdlp::original_language_from_metadata(&v))
        .or_else(|| existing_language.clone());
    let Some(request) = ytdlp::plan_subtitle_request(Some(&metadata)) else {
        eprintln!("info: subtitle fetch for {entry_uid}: no subtitle tracks available ({url})");
        return Ok(SubtitleFetchOutcome {
            added: 0,
            original_language,
        });
    };

    let stage_key = format!("subs-{}", Uuid::new_v4().simple());
    let stage_dir = store_path.join("temp").join(&stage_key);
    let staged = match ytdlp::download_subtitles(&url, store_path, &stage_key, &request, &cookies, timeout) {
        Ok(staged) => staged,
        Err(e) => {
            eprintln!("warn: subtitle fetch for {entry_uid} failed: {e:#}");
            let _ = fs::remove_dir_all(&stage_dir);
            return Ok(SubtitleFetchOutcome {
                added: 0,
                original_language,
            });
        }
    };
    let archived = archive_staged_subtitles(store_path, staged);
    let _ = fs::remove_dir_all(&stage_dir);

    let added = register_subtitle_artifacts(
        &conn,
        store_path,
        info.entry_id,
        &archived,
        SUBTITLE_ORIGIN_SUMMARY_FETCH,
    )?;
    eprintln!("info: subtitle fetch for {entry_uid}: registered {added} subtitle artifact(s)");
    Ok(SubtitleFetchOutcome {
        added,
        original_language,
    })
}

/// Any `<...>` markup: `<c>`, `<c.colorE5E5E5>`, `<00:00:01.000>`, `<v Speaker>`, `<i>`.
static TAG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<[^>]*>").expect("valid tag regex"));

/// SRT ASS override blocks such as `{\an8}`.
static ASS_OVERRIDE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{\\[^}]*\}").expect("valid ASS override regex"));

/// Strips markup from one cue text line and normalizes whitespace.
fn clean_cue_line(line: &str) -> String {
    let without_tags = TAG_RE.replace_all(line, "");
    let without_ass = ASS_OVERRIDE_RE.replace_all(&without_tags, "");
    // `&amp;` last so `&amp;lt;` decodes to `&lt;`, not `<`.
    let decoded = without_ass
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&");
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Appends `line` unless it repeats one of the last two lines; a line that
/// extends the previous one (rolling auto-captions) replaces it.
fn push_deduped(out: &mut Vec<String>, line: String) {
    let recent = &out[out.len().saturating_sub(2)..];
    if recent.iter().any(|l| *l == line) {
        return;
    }
    if let Some(last) = out.last_mut() {
        if line.len() > last.len() && line.starts_with(last.as_str()) {
            *last = line;
            return;
        }
    }
    out.push(line);
}

/// Text lines of one cue block (everything after its timing line).
fn reduce_block(block: &[&str], out: &mut Vec<String>) {
    let Some(timing) = block.iter().position(|l| l.contains("-->")) else {
        return; // WEBVTT header, NOTE, STYLE, REGION, bare index
    };
    for line in &block[timing + 1..] {
        let cleaned = clean_cue_line(line);
        if !cleaned.is_empty() {
            push_deduped(out, cleaned);
        }
    }
}

/// Reduces a VTT or SRT document to plain transcript text, one line per
/// caption line, with markup, timings and rolling-caption repeats removed.
///
/// Blocks split only on truly empty lines: YouTube auto-caption cues contain
/// lines holding a single space, which belong to the cue.
pub fn subtitle_to_transcript(raw: &str) -> String {
    let text = raw
        .strip_prefix('\u{feff}')
        .unwrap_or(raw)
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let mut out: Vec<String> = Vec::new();
    let mut block: Vec<&str> = Vec::new();
    for line in text.split('\n') {
        if line.is_empty() {
            reduce_block(&block, &mut out);
            block.clear();
        } else {
            block.push(line);
        }
    }
    reduce_block(&block, &mut out);
    out.join("\n")
}

/// Parses a `subtitle` artifact's `metadata_json`. Missing or invalid metadata
/// yields language `""` and `Unknown` kind.
pub fn parse_subtitle_metadata(metadata_json: Option<&str>) -> SubtitleTrackMeta {
    let value: serde_json::Value = metadata_json
        .and_then(|json| serde_json::from_str(json).ok())
        .unwrap_or(serde_json::Value::Null);
    let text = |key: &str| {
        value
            .get(key)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    SubtitleTrackMeta {
        language: text("language").unwrap_or_default(),
        kind: text("kind")
            .map(|k| SubtitleKind::parse(&k))
            .unwrap_or(SubtitleKind::Unknown),
        original_language: text("original_language"),
    }
}

/// Preference rank of a subtitle track for summaries; lower is better.
///
/// 0 manual English, 1 manual original-language, 2 other manual,
/// 3 transcribed (any language), 4 auto/unknown original-language,
/// 5 auto/unknown English, 6 anything else.
pub fn subtitle_track_rank(meta: &SubtitleTrackMeta) -> u8 {
    let base = language_base(&meta.language);
    let is_en = base == "en";
    let is_orig = meta.language.to_ascii_lowercase().ends_with("-orig")
        || (!base.is_empty()
            && meta
                .original_language
                .as_deref()
                .is_some_and(|orig| language_base(orig) == base));
    match meta.kind {
        SubtitleKind::Manual if is_en => 0,
        SubtitleKind::Manual if is_orig => 1,
        SubtitleKind::Manual => 2,
        SubtitleKind::Transcribed => 3,
        _ if is_orig => 4,
        _ if is_en => 5,
        _ => 6,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(language: &str, kind: SubtitleKind, original: Option<&str>) -> SubtitleTrackMeta {
        SubtitleTrackMeta {
            language: language.to_string(),
            kind,
            original_language: original.map(str::to_string),
        }
    }

    #[test]
    fn vtt_reduction_strips_header_timestamps_settings_and_tags() {
        let vtt = "WEBVTT\nKind: captions\nLanguage: en\n\nNOTE a comment\nspanning lines\n\nSTYLE\n::cue { color: red }\n\ncue-1\n00:00:01.000 --> 00:00:03.000 align:start position:0%\n<v Speaker>Hello <i>there</i></v>\n\n00:00:03.000 --> 00:00:05.000\n<c.colorE5E5E5>General</c> <00:00:03.500><c>Kenobi</c>\n";
        assert_eq!(subtitle_to_transcript(vtt), "Hello there\nGeneral Kenobi");
    }

    #[test]
    fn vtt_reduction_collapses_rolling_auto_captions() {
        // Real-shaped YouTube auto-caption VTT: each cue repeats the previous
        // line, 10 ms "freeze" cues duplicate it, and lines holding a single
        // space sit inside cues (they must not split blocks).
        let vtt = "WEBVTT\nKind: captions\nLanguage: en\n\n\
00:00:00.000 --> 00:00:02.030 align:start position:0%\n \nhello<00:00:00.320><c> world</c><00:00:00.640><c> this</c>\n\n\
00:00:02.030 --> 00:00:02.040 align:start position:0%\nhello world this\n \n\n\
00:00:02.040 --> 00:00:04.110 align:start position:0%\nhello world this\nis<00:00:02.360><c> a</c><00:00:02.600><c> test</c>\n\n\
00:00:04.110 --> 00:00:04.120 align:start position:0%\nis a test\n \n\n\
00:00:04.120 --> 00:00:06.000 align:start position:0%\nis a test\nof<00:00:04.500><c> captions</c>\n\n\
00:00:06.000 --> 00:00:06.010 align:start position:0%\nof captions\n \n";
        assert_eq!(
            subtitle_to_transcript(vtt),
            "hello world this\nis a test\nof captions"
        );
    }

    #[test]
    fn vtt_reduction_extends_growing_lines() {
        let vtt = "WEBVTT\n\n00:00:00.000 --> 00:00:01.000\nhello\n\n00:00:01.000 --> 00:00:02.000\nhello world\n";
        assert_eq!(subtitle_to_transcript(vtt), "hello world");
    }

    #[test]
    fn srt_reduction_strips_indices_italics_and_ass_overrides() {
        let srt = "1\n00:00:01,000 --> 00:00:02,000\n<i>Hello</i> there\n\n2\n00:00:02,500 --> 00:00:04,000\n{\\an8}Second line\n<b>continues</b>   here\n\n3\n00:00:04,000 --> 00:00:05,000\n42\n";
        assert_eq!(
            subtitle_to_transcript(srt),
            "Hello there\nSecond line\ncontinues here\n42"
        );
    }

    #[test]
    fn reduction_handles_bom_crlf_and_entities() {
        let vtt = "\u{feff}WEBVTT\r\n\r\n00:00:01.000 --> 00:00:02.000\r\nTom &amp; Jerry &lt;3&nbsp;&quot;cheese&quot; it&#39;s &amp;lt;\r\n\r\n00:00:02.000 --> 00:00:03.000\rold mac line\r";
        assert_eq!(
            subtitle_to_transcript(vtt),
            "Tom & Jerry <3 \"cheese\" it's &lt;\nold mac line"
        );
        assert_eq!(subtitle_to_transcript(""), "");
        assert_eq!(subtitle_to_transcript("WEBVTT\n\n"), "");
    }

    #[test]
    fn track_rank_prefers_manual_english_then_manual_original_then_auto_original() {
        let de = Some("de");
        assert_eq!(subtitle_track_rank(&meta("en", SubtitleKind::Manual, de)), 0);
        assert_eq!(subtitle_track_rank(&meta("en-GB", SubtitleKind::Manual, de)), 0);
        assert_eq!(subtitle_track_rank(&meta("de", SubtitleKind::Manual, de)), 1);
        assert_eq!(subtitle_track_rank(&meta("fr", SubtitleKind::Manual, de)), 2);
        assert_eq!(subtitle_track_rank(&meta("de-orig", SubtitleKind::Auto, de)), 4);
        assert_eq!(subtitle_track_rank(&meta("de-orig", SubtitleKind::Unknown, None)), 4);
        assert_eq!(subtitle_track_rank(&meta("en", SubtitleKind::Auto, de)), 5);
        assert_eq!(subtitle_track_rank(&meta("fr", SubtitleKind::Auto, de)), 6);
        assert_eq!(subtitle_track_rank(&meta("", SubtitleKind::Unknown, None)), 6);
    }

    #[test]
    fn track_rank_places_transcribed_below_manual_above_auto() {
        let de = Some("de");
        let transcribed = subtitle_track_rank(&meta("fr", SubtitleKind::Transcribed, de));
        assert_eq!(transcribed, 3);
        assert_eq!(subtitle_track_rank(&meta("en", SubtitleKind::Transcribed, None)), 3);
        assert!(subtitle_track_rank(&meta("fr", SubtitleKind::Manual, de)) < transcribed);
        assert!(subtitle_track_rank(&meta("de-orig", SubtitleKind::Auto, de)) > transcribed);
        assert!(subtitle_track_rank(&meta("en", SubtitleKind::Auto, de)) > transcribed);
    }

    #[test]
    fn parse_subtitle_metadata_defaults_and_round_trip() {
        assert_eq!(
            parse_subtitle_metadata(None),
            meta("", SubtitleKind::Unknown, None)
        );
        assert_eq!(
            parse_subtitle_metadata(Some("not json")),
            meta("", SubtitleKind::Unknown, None)
        );
        assert_eq!(
            parse_subtitle_metadata(Some(
                r#"{"language":"de-orig","kind":"auto","format":"vtt","original_language":"de","origin":"capture"}"#
            )),
            meta("de-orig", SubtitleKind::Auto, Some("de"))
        );
    }

    #[test]
    fn subtitle_format_detects_by_extension_or_mime() {
        assert_eq!(SubtitleFormat::detect("vtt", ""), Some(SubtitleFormat::Vtt));
        assert_eq!(SubtitleFormat::detect(".SRT", ""), Some(SubtitleFormat::Srt));
        assert_eq!(
            SubtitleFormat::detect("", "text/vtt; charset=utf-8"),
            Some(SubtitleFormat::Vtt)
        );
        assert_eq!(
            SubtitleFormat::detect("txt", "application/x-subrip"),
            Some(SubtitleFormat::Srt)
        );
        assert_eq!(SubtitleFormat::detect("ttml", "application/ttml+xml"), None);
    }

    fn archive_fixture(
        source_kind: &str,
        entity_kind: &str,
        canonical_url: Option<&str>,
    ) -> (tempfile::TempDir, ArchivePaths, database::ArchivedEntry) {
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
            source_kind,
            entity_kind,
            Some("fixture-1"),
            canonical_url,
            canonical_url.unwrap_or("fixture:1"),
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
                source_kind: source_kind.to_string(),
                entity_kind: entity_kind.to_string(),
                title: None,
                visibility: "private".to_string(),
                representation_kind: entity_kind.to_string(),
                source_metadata_json: "{}".to_string(),
                display_metadata_json: None,
            },
        )
        .unwrap();
        (temp, paths, entry)
    }

    fn stage_vtt(store_path: &Path, name: &str, body: &str) -> StagedSubtitle {
        let dir = store_path.join("temp").join("stage");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        fs::write(&path, body).unwrap();
        StagedSubtitle {
            path,
            language: "de-orig".to_string(),
            kind: SubtitleKind::Auto,
            format: "vtt".to_string(),
            original_language: Some("de".to_string()),
        }
    }

    #[test]
    fn register_subtitle_artifacts_dedups_same_blob() {
        let (_temp, paths, entry) =
            archive_fixture("youtube", "video", Some("https://www.youtube.com/watch?v=x"));
        let store_path = &paths.store_path;
        let body = "WEBVTT\n\n00:00:00.000 --> 00:00:01.000\nHallo Welt\n";
        let conn = database::open_or_initialize(&paths.archive_path).unwrap();

        let first = archive_staged_subtitles(store_path, vec![stage_vtt(store_path, "a.de-orig.vtt", body)]);
        assert_eq!(first.len(), 1);
        assert!(store_path.join(&first[0].raw_relpath).is_file());
        assert_eq!(
            register_subtitle_artifacts(&conn, store_path, entry.id, &first, SUBTITLE_ORIGIN_CAPTURE)
                .unwrap(),
            1
        );

        // Same bytes fetched again: raw move dedupes, registration skips.
        let second = archive_staged_subtitles(store_path, vec![stage_vtt(store_path, "b.de-orig.vtt", body)]);
        assert_eq!(second[0].raw_relpath, first[0].raw_relpath);
        assert_eq!(
            register_subtitle_artifacts(
                &conn,
                store_path,
                entry.id,
                &second,
                SUBTITLE_ORIGIN_SUMMARY_FETCH
            )
            .unwrap(),
            0
        );

        let rows =
            database::list_entry_artifacts_by_role(&conn, entry.id, SUBTITLE_ARTIFACT_ROLE).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].mime_type.as_deref(), Some("text/vtt"));
        assert_eq!(rows[0].relpath, first[0].raw_relpath.to_string_lossy());
        assert_eq!(
            parse_subtitle_metadata(rows[0].metadata_json.as_deref()),
            meta("de-orig", SubtitleKind::Auto, Some("de"))
        );
        let stored: serde_json::Value =
            serde_json::from_str(rows[0].metadata_json.as_deref().unwrap()).unwrap();
        assert_eq!(stored["format"], "vtt");
        assert_eq!(stored["origin"], SUBTITLE_ORIGIN_CAPTURE);

        assert_eq!(usable_subtitle_count(&conn, store_path, entry.id).unwrap(), 1);
        assert_eq!(
            register_subtitle_artifacts(&conn, store_path, entry.id, &[], SUBTITLE_ORIGIN_CAPTURE)
                .unwrap(),
            0
        );
    }

    #[test]
    fn fetch_subtitles_for_entry_skips_non_youtube_and_non_http_entries() {
        // Each of these returns before any yt-dlp process could be spawned.
        let (_t1, web_paths, web) = archive_fixture("web", "page", Some("https://example.com/"));
        assert_eq!(
            fetch_subtitles_for_entry(&web_paths, &web.entry_uid, &[]).unwrap(),
            SubtitleFetchOutcome::default()
        );

        let (_t2, offline_paths, offline) =
            archive_fixture("youtube", "video", Some("youtube-test:offline"));
        assert_eq!(
            fetch_subtitles_for_entry(&offline_paths, &offline.entry_uid, &[]).unwrap(),
            SubtitleFetchOutcome::default()
        );

        let (_t3, no_url_paths, no_url) = archive_fixture("youtube", "video", None);
        assert_eq!(
            fetch_subtitles_for_entry(&no_url_paths, &no_url.entry_uid, &[]).unwrap(),
            SubtitleFetchOutcome::default()
        );

        assert!(fetch_subtitles_for_entry(&web_paths, "entry_missing", &[]).is_err());
    }

    #[test]
    fn fetch_outcome_reports_original_language_from_existing_artifacts() {
        let (_temp, paths, entry) =
            archive_fixture("youtube", "video", Some("youtube-test:offline"));
        let store_path = &paths.store_path;
        let conn = database::open_or_initialize(&paths.archive_path).unwrap();
        // An unusable (empty) track still carries the original language.
        let archived =
            archive_staged_subtitles(store_path, vec![stage_vtt(store_path, "e.de-orig.vtt", "WEBVTT\n")]);
        register_subtitle_artifacts(&conn, store_path, entry.id, &archived, SUBTITLE_ORIGIN_CAPTURE)
            .unwrap();
        assert_eq!(
            fetch_subtitles_for_entry(&paths, &entry.entry_uid, &[]).unwrap(),
            SubtitleFetchOutcome {
                added: 0,
                original_language: Some("de".to_string()),
            }
        );
    }

    #[test]
    fn register_transcript_artifact_writes_engine_metadata_and_dedups() {
        let (_temp, paths, entry) =
            archive_fixture("youtube", "video", Some("https://www.youtube.com/watch?v=x"));
        let store_path = &paths.store_path;
        let conn = database::open_or_initialize(&paths.archive_path).unwrap();
        let body = "WEBVTT\n\n00:00:00.000 --> 00:00:02.000\nhello world\n";
        let staged = |name: &str| {
            let mut s = stage_vtt(store_path, name, body);
            s.language = "en".to_string();
            s.kind = SubtitleKind::Transcribed;
            s.original_language = None;
            s
        };

        let first = archive_staged_subtitles(store_path, vec![staged("t1.vtt")]);
        assert_eq!(first.len(), 1);
        assert_eq!(
            register_transcript_artifact(
                &conn,
                store_path,
                entry.id,
                &first[0],
                "whisper",
                "/models/ggml-tiny.bin"
            )
            .unwrap(),
            1
        );
        let second = archive_staged_subtitles(store_path, vec![staged("t2.vtt")]);
        assert_eq!(
            register_transcript_artifact(
                &conn,
                store_path,
                entry.id,
                &second[0],
                "whisper",
                "/models/ggml-tiny.bin"
            )
            .unwrap(),
            0
        );

        let rows =
            database::list_entry_artifacts_by_role(&conn, entry.id, SUBTITLE_ARTIFACT_ROLE).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].mime_type.as_deref(), Some("text/vtt"));
        let stored: serde_json::Value =
            serde_json::from_str(rows[0].metadata_json.as_deref().unwrap()).unwrap();
        assert_eq!(stored["kind"], "transcribed");
        assert_eq!(stored["origin"], SUBTITLE_ORIGIN_TRANSCRIPTION);
        assert_eq!(stored["engine"], "whisper");
        assert_eq!(stored["model"], "ggml-tiny.bin");
        assert_eq!(stored["language"], "en");
        assert_eq!(
            parse_subtitle_metadata(rows[0].metadata_json.as_deref()).kind,
            SubtitleKind::Transcribed
        );

        assert_eq!(sanitize_model_name("nvidia/parakeet-tdt-0.6b-v3"), "nvidia/parakeet-tdt-0.6b-v3");
        assert_eq!(sanitize_model_name("phonon-2"), "phonon-2");
        assert_eq!(sanitize_model_name("C:\\models\\x.bin"), "x.bin");
    }
}
