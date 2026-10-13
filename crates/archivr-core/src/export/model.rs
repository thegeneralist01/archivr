//! Data model for exports: loaded entries and their bodies.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Result;
use rusqlite::Connection;

use crate::archive::{self, EntryArtifactSummary, EntrySummary};
use crate::summarizer::article_parts;
use crate::twitter::parse_tweet_id;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// An asset file that will be written into the export bundle.
#[derive(Debug, Clone)]
pub(crate) struct Asset {
    /// Absolute on-disk path (canonical store path or a work-dir poster path).
    pub source: PathBuf,
    /// Bundle-relative file name (e.g. `sha256abc.mp4`, `sha256abc-poster.jpg`).
    pub name: String,
    pub byte_size: u64,
}

/// A resolved span in tweet body text.
#[derive(Debug, Clone)]
pub(crate) enum Span {
    Text(String),
    Link { text: String, href: String },
}

/// A single tweet / post.
#[derive(Debug, Clone)]
pub(crate) struct Post {
    pub _id: String,
    pub name: String,
    pub handle: Option<String>,
    pub avatar: Option<Asset>,
    pub created_at_secs: Option<i64>,
    pub spans: Vec<Span>,
    /// `Some((title, body, cover))` for X article statuses.
    pub article: Option<(Option<String>, String, Option<Asset>)>,
    pub photos: Vec<Asset>,
    /// `(asset, animated_gif)`.
    pub videos: Vec<(Asset, bool)>,
    pub quote: Option<Box<Post>>,
    /// `https://x.com/{handle|i}/status/{id}`.
    pub status_url: Option<String>,
}

/// The primary content of an exported entry.
#[derive(Debug)]
pub(crate) enum ExportBody {
    Video(Asset),
    Audio(Asset),
    Image(Asset),
    Pdf(Asset),
    /// An HTML/HTM snapshot (SingleFile page).
    Page(Asset),
    Text { _mime_markdown: bool, text: String },
    /// Text file > 5 MiB: offer a download link only.
    TextTooLarge(Asset),
    File(Asset),
    Posts(Vec<Post>),
    Container(Vec<ExportEntry>),
    Empty,
}

/// A fully loaded entry, ready for rendering.
#[derive(Debug)]
pub(crate) struct ExportEntry {
    pub summary: EntrySummary,
    /// Subtitle `.vtt` and `.srt` assets in track order.
    pub subtitles: Vec<Asset>,
    pub body: ExportBody,
}

// ---------------------------------------------------------------------------
// Extension helpers
// ---------------------------------------------------------------------------

const VIDEO_EXTS: &[&str] = &["mp4", "webm", "mov", "mkv", "avi", "m4v", "ogv"];
const AUDIO_EXTS: &[&str] = &["mp3", "ogg", "m4a", "opus", "wav", "flac", "aac"];
const IMAGE_EXTS: &[&str] = &["jpg", "jpeg", "png", "gif", "webp", "avif", "svg", "bmp"];
const TEXT_EXTS: &[&str] = &["md", "markdown", "txt"];
const TEXT_MAX_BYTES: u64 = 5 * 1024 * 1024;

/// Extract the file name component of a store-relative path like
/// `raw/A/B/sha256.mp4`.
pub(crate) fn file_name_of(relpath: &str) -> String {
    Path::new(relpath)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| relpath.to_string())
}

/// Lowercase file extension without the dot, or `""`.
pub(crate) fn ext_of_s(path: &str) -> String {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

/// Load a single entry and all its required data from the DB.
///
/// `depth` limits container recursion: pass `0` for top-level entries.
/// Containers found at depth ≥ 2 become `Empty`.
pub(crate) fn load_entry(
    conn: &Connection,
    store_path: &Path,
    summary: EntrySummary,
    caller_bits: u32,
    depth: usize,
) -> Result<ExportEntry> {
    let detail = archive::get_entry_detail(conn, &summary.entry_uid)?;
    let detail = match detail {
        Some(d) => d,
        None => {
            return Ok(ExportEntry {
                summary,
                subtitles: vec![],
                body: ExportBody::Empty,
            });
        }
    };

    // ----- build asset map from artifacts -----
    let mut asset_map: HashMap<String, Asset> = HashMap::new();
    let mut primary_media_artifact: Option<EntryArtifactSummary> = None;
    let mut subtitle_artifacts: Vec<EntryArtifactSummary> = Vec::new();
    let mut raw_json_artifacts: Vec<EntryArtifactSummary> = Vec::new();

    for artifact in &detail.artifacts {
        match artifact.artifact_role.as_str() {
            "primary_media" => {
                if artifact.relpath.to_lowercase().ends_with(".json") {
                    raw_json_artifacts.push(artifact.clone());
                } else {
                    primary_media_artifact = Some(artifact.clone());
                }
                if let Ok(p) = archive::resolve_artifact_path(store_path, artifact) {
                    asset_map.insert(
                        artifact.relpath.clone(),
                        Asset {
                            byte_size: artifact.byte_size.unwrap_or(0) as u64,
                            source: p,
                            name: file_name_of(&artifact.relpath),
                        },
                    );
                } else {
                    eprintln!("warn: export: artifact not found: {}", artifact.relpath);
                }
            }
            "media" | "avatar" => {
                match archive::resolve_artifact_path(store_path, artifact) {
                    Ok(p) => {
                        asset_map.insert(
                            artifact.relpath.clone(),
                            Asset {
                                byte_size: artifact.byte_size.unwrap_or(0) as u64,
                                source: p,
                                name: file_name_of(&artifact.relpath),
                            },
                        );
                    }
                    Err(e) => eprintln!("warn: export: artifact not found: {e:#}"),
                }
            }
            "raw_tweet_json" => {
                raw_json_artifacts.push(artifact.clone());
                if let Ok(p) = archive::resolve_artifact_path(store_path, artifact) {
                    asset_map.insert(
                        artifact.relpath.clone(),
                        Asset {
                            byte_size: artifact.byte_size.unwrap_or(0) as u64,
                            source: p,
                            name: file_name_of(&artifact.relpath),
                        },
                    );
                }
            }
            "subtitle" => subtitle_artifacts.push(artifact.clone()),
            _ => {} // favicon, font, etc.
        }
    }

    // ----- subtitles -----
    let subtitles: Vec<Asset> = subtitle_artifacts
        .iter()
        .filter(|a| matches!(ext_of_s(&a.relpath).as_str(), "vtt" | "srt"))
        .filter_map(|a| {
            archive::resolve_artifact_path(store_path, a)
                .map(|p| Asset {
                    byte_size: a.byte_size.unwrap_or(0) as u64,
                    source: p,
                    name: file_name_of(&a.relpath),
                })
                .map_err(|e| {
                    eprintln!("warn: export: subtitle not found: {e:#}");
                    e
                })
                .ok()
        })
        .collect();

    // ----- determine body -----
    let ek = summary.entity_kind.as_str();
    let sk = summary.source_kind.as_str();

    let body = if summary.child_count > 0 {
        if depth >= 2 {
            ExportBody::Empty
        } else {
            let children =
                archive::list_child_entries(conn, &summary.entry_uid, caller_bits)?;
            let loaded: Vec<ExportEntry> = children
                .into_iter()
                .map(|child| load_entry(conn, store_path, child, caller_bits, depth + 1))
                .collect::<Result<Vec<_>>>()?;
            ExportBody::Container(loaded)
        }
    } else if ek == "tweet"
        || ek == "tweet_thread"
        || (sk == "x" && ek == "post")
        || (sk == "twitter" && ek == "post")
    {
        let posts = load_posts(
            &detail.source_metadata_json,
            &raw_json_artifacts,
            store_path,
            &asset_map,
            ek,
        );
        if posts.is_empty() {
            body_from_primary_media(primary_media_artifact.as_ref(), &asset_map)
        } else {
            ExportBody::Posts(posts)
        }
    } else {
        body_from_primary_media(primary_media_artifact.as_ref(), &asset_map)
    };

    Ok(ExportEntry {
        summary,
        subtitles,
        body,
    })
}

fn body_from_primary_media(
    artifact: Option<&EntryArtifactSummary>,
    asset_map: &HashMap<String, Asset>,
) -> ExportBody {
    let artifact = match artifact {
        Some(a) => a,
        None => return ExportBody::Empty,
    };
    let relpath = &artifact.relpath;
    let asset = match asset_map.get(relpath) {
        Some(a) => a.clone(),
        None => return ExportBody::Empty,
    };
    let ext = ext_of_s(relpath);
    let ext = ext.as_str();
    if VIDEO_EXTS.contains(&ext) {
        ExportBody::Video(asset)
    } else if AUDIO_EXTS.contains(&ext) {
        ExportBody::Audio(asset)
    } else if IMAGE_EXTS.contains(&ext) {
        ExportBody::Image(asset)
    } else if ext == "pdf" {
        ExportBody::Pdf(asset)
    } else if ext == "html" || ext == "htm" {
        ExportBody::Page(asset)
    } else if TEXT_EXTS.contains(&ext) {
        if asset.byte_size > TEXT_MAX_BYTES {
            ExportBody::TextTooLarge(asset)
        } else {
            match std::fs::read(&asset.source) {
                Ok(bytes) => {
                    let text = String::from_utf8_lossy(&bytes).into_owned();
                    let _mime_markdown = ext == "md" || ext == "markdown";
                    ExportBody::Text { _mime_markdown, text }
                }
                Err(e) => {
                    eprintln!("warn: export: failed to read text artifact: {e:#}");
                    ExportBody::TextTooLarge(asset)
                }
            }
        }
    } else {
        ExportBody::File(asset)
    }
}

// ---------------------------------------------------------------------------
// Tweet / thread loading
// ---------------------------------------------------------------------------

fn load_posts(
    source_metadata_json: &str,
    json_artifacts: &[EntryArtifactSummary],
    store_path: &Path,
    asset_map: &HashMap<String, Asset>,
    entity_kind: &str,
) -> Vec<Post> {
    let seed_id: Option<String> = serde_json::from_str::<serde_json::Value>(source_metadata_json)
        .ok()
        .and_then(|v| v.get("tweet_id").and_then(|t| t.as_str()).map(str::to_string));

    let mut statuses: Vec<serde_json::Value> = Vec::new();
    for artifact in json_artifacts {
        let path = match archive::resolve_artifact_path(store_path, artifact) {
            Ok(p) => p,
            Err(_) => continue,
        };
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!(
                    "warn: export: failed to read tweet JSON {}: {e:#}",
                    path.display()
                );
                continue;
            }
        };
        let json: serde_json::Value = match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let tweet_obj = json.get("tweet").cloned().unwrap_or(json);
        if tweet_obj.get("id").is_some() || tweet_obj.get("id_str").is_some() {
            statuses.push(tweet_obj);
        }
    }

    if statuses.is_empty() {
        return vec![];
    }

    let status_map: HashMap<String, serde_json::Value> = statuses
        .iter()
        .filter_map(|s| tweet_id(s).map(|id| (id, s.clone())))
        .collect();

    if entity_kind == "tweet" {
        let status = seed_id
            .as_deref()
            .and_then(|sid| status_map.get(sid))
            .or_else(|| statuses.first())
            .cloned();
        match status {
            Some(s) => vec![parse_post(&s, asset_map, None)],
            None => vec![],
        }
    } else {
        build_thread(&statuses, &status_map, seed_id.as_deref(), asset_map)
    }
}

fn build_thread(
    statuses: &[serde_json::Value],
    status_map: &HashMap<String, serde_json::Value>,
    seed_id: Option<&str>,
    asset_map: &HashMap<String, Asset>,
) -> Vec<Post> {
    if statuses.is_empty() {
        return vec![];
    }

    let start_id = seed_id
        .and_then(|id| status_map.get(id).map(|_| id.to_string()))
        .or_else(|| statuses.last().and_then(tweet_id))
        .unwrap_or_default();

    // Walk upward to find the root.
    let mut root_id = start_id;
    let mut visited: HashSet<String> = HashSet::new();
    visited.insert(root_id.clone());
    loop {
        let status = match status_map.get(&root_id) {
            Some(s) => s,
            None => break,
        };
        match in_reply_to_id(status) {
            Some(pid) if status_map.contains_key(&pid) && !visited.contains(&pid) => {
                visited.insert(pid.clone());
                root_id = pid;
            }
            _ => break,
        }
    }

    // Build children map.
    let mut children_map: HashMap<String, Vec<String>> = HashMap::new();
    for status in statuses {
        let id = match tweet_id(status) {
            Some(id) => id,
            None => continue,
        };
        if let Some(parent) = in_reply_to_id(status) {
            if status_map.contains_key(&parent) {
                children_map.entry(parent).or_default().push(id);
            }
        }
    }

    // Sort siblings by created_at then by id.
    for children in children_map.values_mut() {
        children.sort_by(|a, b| {
            let ca = status_map
                .get(a)
                .and_then(|s| created_at_secs(s))
                .unwrap_or(0);
            let cb = status_map
                .get(b)
                .and_then(|s| created_at_secs(s))
                .unwrap_or(0);
            ca.cmp(&cb).then(a.cmp(b))
        });
    }

    let mut result = Vec::new();
    let mut conversation_ids: HashSet<String> = HashSet::new();
    dfs(
        &root_id,
        status_map,
        &children_map,
        asset_map,
        &mut result,
        &mut conversation_ids,
    );
    result
}

fn dfs(
    id: &str,
    status_map: &HashMap<String, serde_json::Value>,
    children_map: &HashMap<String, Vec<String>>,
    asset_map: &HashMap<String, Asset>,
    out: &mut Vec<Post>,
    conversation_ids: &mut HashSet<String>,
) {
    let status = match status_map.get(id) {
        Some(s) => s,
        None => return,
    };
    if !conversation_ids.insert(id.to_string()) {
        return;
    }
    let post = parse_post(status, asset_map, Some(conversation_ids));
    out.push(post);
    if let Some(children) = children_map.get(id) {
        for child_id in children.clone() {
            dfs(&child_id, status_map, children_map, asset_map, out, conversation_ids);
        }
    }
}

// ---------------------------------------------------------------------------
// Tweet parsing helpers
// ---------------------------------------------------------------------------

fn tweet_id(status: &serde_json::Value) -> Option<String> {
    let id = status
        .get("id_str")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| {
            status.get("id").and_then(|v| {
                if let Some(s) = v.as_str() {
                    Some(s.to_string())
                } else {
                    v.as_u64().map(|n| n.to_string())
                }
            })
        })?;
    if id.is_empty() { None } else { Some(id) }
}

fn in_reply_to_id(status: &serde_json::Value) -> Option<String> {
    status
        .get("in_reply_to_status_id_str")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty() && *s != "null")
        .map(str::to_string)
        .or_else(|| {
            status
                .get("in_reply_to_status_id")
                .and_then(|v| v.as_u64())
                .map(|n| n.to_string())
        })
}

fn created_at_secs(status: &serde_json::Value) -> Option<i64> {
    if let Some(n) = status.get("created_at_secs").and_then(|v| v.as_i64()) {
        return Some(n);
    }
    if let Some(s) = status.get("created_at").and_then(|v| v.as_str()) {
        use chrono::DateTime;
        if let Ok(dt) = DateTime::parse_from_str(s, "%a %b %d %T %z %Y") {
            return Some(dt.timestamp());
        }
        if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
            return Some(dt.timestamp());
        }
    }
    None
}

fn parse_post(
    status: &serde_json::Value,
    asset_map: &HashMap<String, Asset>,
    conversation_ids: Option<&HashSet<String>>,
) -> Post {
    let id = tweet_id(status).unwrap_or_default();

    let author = status.get("author").or_else(|| status.get("user"));
    let name = author
        .and_then(|a| a.get("name").and_then(|v| v.as_str()))
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            author
                .and_then(|a| a.get("screen_name").and_then(|v| v.as_str()))
                .map(str::to_string)
        })
        .unwrap_or_else(|| "Unknown".to_string());
    let handle = author
        .and_then(|a| a.get("screen_name").and_then(|v| v.as_str()))
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let avatar_local = author
        .and_then(|a| a.get("avatar_local_path").and_then(|v| v.as_str()))
        .map(str::to_string);
    let avatar = avatar_local.as_deref().and_then(|lp| asset_map.get(lp)).cloned();

    let created_at_secs = created_at_secs(status);

    // Article.
    let article = article_parts(status).map(|(title, body)| {
        let cover_local = status
            .pointer("/article/cover_media/local_path")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let cover = cover_local.as_deref().and_then(|lp| asset_map.get(lp)).cloned();
        (title, body, cover)
    });

    // Spans (only when no article).
    let raw_spans = if article.is_some() {
        vec![]
    } else {
        build_spans(status)
    };

    // Media.
    let media_list = status
        .get("extended_entities")
        .or_else(|| status.get("entities"))
        .and_then(|e| e.get("media"))
        .and_then(|m| m.as_array())
        .cloned()
        .unwrap_or_default();

    let media_url_set: HashSet<String> = media_list
        .iter()
        .filter_map(|m| m.get("url").and_then(|u| u.as_str()))
        .map(str::to_string)
        .collect();
    let spans = filter_spans(raw_spans, &media_url_set);

    let mut photos: Vec<Asset> = Vec::new();
    let mut videos: Vec<(Asset, bool)> = Vec::new();
    for m in &media_list {
        let local_path = m
            .get("local_path")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let asset = match local_path.as_deref().and_then(|lp| asset_map.get(lp)).cloned() {
            Some(a) => a,
            None => continue,
        };
        match m.get("type").and_then(|v| v.as_str()).unwrap_or("") {
            "photo" => photos.push(asset),
            "video" => videos.push((asset, false)),
            "animated_gif" => videos.push((asset, true)),
            _ => {}
        }
    }

    // Quote.
    let quoted_id = status
        .get("quoted_status_id_str")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let quote = quoted_id.as_deref().and_then(|qid| {
        if conversation_ids.map_or(false, |c| c.contains(qid)) {
            return None;
        }
        status
            .get("quoted_status")
            .map(|qs| Box::new(parse_post(qs, asset_map, None)))
    });

    // Status URL.
    let status_url = parse_tweet_id(&id).map(|tid| {
        handle
            .as_deref()
            .map(|h| format!("https://x.com/{h}/status/{tid}"))
            .unwrap_or_else(|| format!("https://x.com/i/status/{tid}"))
    });

    Post {
        _id: id,
        name,
        handle,
        avatar,
        created_at_secs,
        spans,
        article,
        photos,
        videos,
        quote,
        status_url,
    }
}

fn build_spans(status: &serde_json::Value) -> Vec<Span> {
    let text = status
        .get("full_text")
        .or_else(|| status.get("text"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let url_entities = status
        .pointer("/entities/urls")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut text = decode_html_entities(text);

    let mut replacements: Vec<(String, String, String)> = url_entities
        .iter()
        .filter_map(|e| {
            let short = e.get("url").and_then(|v| v.as_str())?;
            let display = e
                .get("display_url")
                .and_then(|v| v.as_str())
                .unwrap_or(short);
            let expanded = e
                .get("expanded_url")
                .and_then(|v| v.as_str())
                .unwrap_or(short);
            if short.is_empty() {
                None
            } else {
                Some((short.to_string(), display.to_string(), expanded.to_string()))
            }
        })
        .collect();

    if replacements.is_empty() {
        let trimmed = text.trim().to_string();
        return if trimmed.is_empty() { vec![] } else { vec![Span::Text(trimmed)] };
    }

    let mut spans: Vec<Span> = Vec::new();
    loop {
        let earliest = replacements
            .iter()
            .enumerate()
            .filter_map(|(i, (short, _, _))| text.find(short.as_str()).map(|pos| (pos, i, short.len())))
            .min_by_key(|(pos, _, _)| *pos);
        match earliest {
            None => break,
            Some((pos, idx, len)) => {
                let (_, display_url, expanded_url) = replacements[idx].clone();
                if pos > 0 {
                    spans.push(Span::Text(text[..pos].to_string()));
                }
                spans.push(Span::Link { text: display_url, href: expanded_url });
                text = text[pos + len..].to_string();
                replacements.remove(idx);
            }
        }
    }
    let remainder = text.trim().to_string();
    if !remainder.is_empty() {
        spans.push(Span::Text(remainder));
    }
    spans
}

fn filter_spans(spans: Vec<Span>, strip_set: &HashSet<String>) -> Vec<Span> {
    spans
        .into_iter()
        .filter(|span| match span {
            Span::Link { href, .. } => !strip_set.contains(href),
            Span::Text(t) => !strip_set.contains(t.as_str()),
        })
        .collect()
}

fn decode_html_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn thread_export_visits_each_post_once_when_replies_cycle() {
        for statuses in [
            vec![json!({"id_str": "1", "in_reply_to_status_id_str": "1"})],
            vec![
                json!({"id_str": "1", "in_reply_to_status_id_str": "2"}),
                json!({"id_str": "2", "in_reply_to_status_id_str": "1"}),
            ],
        ] {
            let status_map: HashMap<String, serde_json::Value> = statuses
                .iter()
                .map(|status| (tweet_id(status).unwrap(), status.clone()))
                .collect();
            let posts = build_thread(&statuses, &status_map, Some("1"), &HashMap::new());
            let unique_ids: HashSet<&str> = posts.iter().map(|post| post._id.as_str()).collect();

            assert_eq!(posts.len(), statuses.len());
            assert_eq!(unique_ids.len(), statuses.len());
        }
    }
}
