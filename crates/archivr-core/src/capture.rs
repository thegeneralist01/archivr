use anyhow::{Context, Result};
use chrono::Local;
use uuid::Uuid;
use serde_json::json;
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};
use crate::{archive::ArchivePaths, database, downloader, twitter::parse_tweet_id};

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Source {
    YouTubeVideo,
    YouTubePlaylist,
    YouTubeChannel,
    YouTubeMusicTrack,
    YouTubeMusicPlaylist,
    SpotifyTrack,
    SpotifyAlbum,
    SpotifyPlaylist,
    X,
    Tweet,
    TweetThread,
    Instagram,
    Facebook,
    TikTok,
    Reddit,
    Snapchat,
    Local,
    Url,
    WebPage,
    Other,
}

#[derive(Debug, serde::Serialize)]
pub struct CaptureResult {
    pub run_uid: String,
    pub status: String,
}

#[derive(Debug, Clone, Default)]
pub struct PlatformMetadata {
    /// Uploader / creator handle (without @)
    pub author: Option<String>,
    /// Video title, playlist name, post title, or filename
    pub title: Option<String>,
    /// Tweet text, Instagram caption, TikTok caption
    pub caption: Option<String>,
    /// Reddit subreddit name (without r/)
    pub subreddit: Option<String>,
    /// Reddit post author handle (without u/)
    pub post_author: Option<String>,
}

impl PlatformMetadata {
    /// Returns caption trimmed to 100 chars followed by "..." when longer.
    pub fn caption_excerpt(&self) -> Option<String> {
        self.caption.as_ref().and_then(|c| {
            let t = c.trim();
            if t.is_empty() {
                None
            } else if t.len() > 100 {
                Some(format!("{}...", &t[..100]))
            } else {
                Some(t.to_string())
            }
        })
    }
}

/// Configuration passed to `perform_capture` to supply per-instance settings
/// that live outside the archive (e.g. cookies stored in the auth DB).
#[derive(Debug, Clone, Default)]
pub struct CaptureConfig {
    pub cookie_rules: Vec<database::CookieRule>,
}

/// Resolves which cookies apply to `url` by evaluating all rules in ordinal order.
///
/// Global rules (`url_pattern = None`) always apply.
/// URL-specific rules apply when their pattern matches:
/// - `wildcard`: `*` and `?` glob; matched against the URL hostname when the
///   pattern does not contain `://`, or the full URL when it does.
/// - `regex`: full regex matched against the full URL.
///
/// Later rules in ordinal order override earlier ones for the same cookie name.
pub fn resolve_cookies_for_url(
    rules: &[database::CookieRule],
    url: &str,
) -> HashMap<String, String> {
    let mut result = HashMap::new();
    for rule in rules {
        let applies = match rule.url_pattern.as_deref() {
            None => true,
            Some(pattern) => match rule.pattern_kind.as_str() {
                "wildcard" => wildcard_matches(pattern, url),
                "regex" => regex::Regex::new(pattern)
                    .is_ok_and(|re| re.is_match(url)),
                _ => false,
            },
        };
        if applies {
            if let Ok(map) =
                serde_json::from_str::<HashMap<String, String>>(&rule.cookies_json)
            {
                result.extend(map);
            }
        }
    }
    result
}

/// Converts a wildcard pattern (`*` = any substring, `?` = any character) to a
/// regex and tests it against the URL.
///
/// If the pattern contains `://` it is matched against the full URL; otherwise
/// only against the hostname (via `reqwest::Url::parse`) so that patterns like
/// `*.youtube.com` work naturally without matching URL path components.
fn wildcard_matches(pattern: &str, url: &str) -> bool {
    use reqwest::Url as ReqwestUrl;
    let target: String = if pattern.contains("://") {
        url.to_string()
    } else {
        ReqwestUrl::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
            .unwrap_or_else(|| url.to_string())
    };
    let mut pat = String::with_capacity(pattern.len() * 2 + 2);
    pat.push('^');
    for ch in pattern.chars() {
        match ch {
            '*' => pat.push_str(".*"),
            '?' => pat.push('.'),
            c if "$.+[]{}()|^\\".contains(c) => { pat.push('\\'); pat.push(c); }
            c => pat.push(c),
        }
    }
    pat.push('$');
    regex::Regex::new(&pat).is_ok_and(|re| re.is_match(&target))
}

fn generate_entry_title(source: Source, meta: &PlatformMetadata) -> String {
    match source {
        Source::YouTubeVideo => meta.title.clone().unwrap_or_else(|| "YouTube Video".to_string()),
        Source::YouTubePlaylist => meta.title.clone().unwrap_or_else(|| "YouTube Playlist".to_string()),
        Source::YouTubeChannel => format!(
            "Archival of {}",
            meta.author.as_deref().unwrap_or("Unknown Channel")
        ),
        Source::YouTubeMusicTrack => {
            let title = meta.title.as_deref().unwrap_or("Unknown Track");
            match meta.author.as_deref() {
                Some(a) => format!("{title} \u{2014} {a}"),
                None => title.to_string(),
            }
        }
        Source::YouTubeMusicPlaylist => {
            meta.title.clone().unwrap_or_else(|| "YouTube Music Playlist".to_string())
        }
        Source::SpotifyTrack | Source::SpotifyAlbum | Source::SpotifyPlaylist => {
            meta.title.clone().unwrap_or_else(|| "Spotify Content".to_string())
        }
        Source::X => format!("X Media by {}", meta.author.as_deref().unwrap_or("unknown")),
        Source::Tweet => {
            let excerpt = meta.caption_excerpt().unwrap_or_else(|| "Tweet".to_string());
            format!("{} \u{2014} @{}", excerpt, meta.author.as_deref().unwrap_or("unknown"))
        }
        Source::TweetThread => format!("Thread by @{}", meta.author.as_deref().unwrap_or("unknown")),
        Source::Instagram => format!("Post by @{}", meta.author.as_deref().unwrap_or("unknown")),
        Source::Facebook => format!("Post by {}", meta.author.as_deref().unwrap_or("unknown")),
        Source::TikTok => format!("TikTok by @{}", meta.author.as_deref().unwrap_or("unknown")),
        Source::Reddit => format!(
            "{} \u{2014} r/{} (u/{})",
            meta.title.as_deref().unwrap_or("Reddit Post"),
            meta.subreddit.as_deref().unwrap_or("reddit"),
            meta.post_author.as_deref().unwrap_or("unknown")
        ),
        Source::Snapchat => format!("Snap by {}", meta.author.as_deref().unwrap_or("unknown")),
        Source::Local => meta.title.clone().unwrap_or_else(|| "Local File".to_string()),
        Source::Url => meta.title.clone().unwrap_or_else(|| "Downloaded File".to_string()),
        Source::WebPage => meta.title.clone().unwrap_or_else(|| "Archived Web Page".to_string()),
        Source::Other => "Archived Content".to_string(),
    }
}

fn expand_shorthand_to_url(path: &str, source: &Source) -> String {
    // YouTube shorthands: yt:video/ID, yt:playlist/ID, yt:@handle, yt:channel/ID, etc.
    if matches!(source, Source::YouTubeVideo | Source::YouTubePlaylist | Source::YouTubeChannel) {
        if let Some(after) = path.strip_prefix("yt:").or_else(|| path.strip_prefix("youtube:")) {
            if let Some(id) = after
                .strip_prefix("video/")
                .or_else(|| after.strip_prefix("short/"))
                .or_else(|| after.strip_prefix("shorts/"))
            {
                return format!("https://www.youtube.com/watch?v={id}");
            }
            if let Some(id) = after.strip_prefix("playlist/") {
                return format!("https://www.youtube.com/playlist?list={id}");
            }
            if let Some(id) = after.strip_prefix("channel/") {
                return format!("https://www.youtube.com/channel/{id}");
            }
            if let Some(id) = after.strip_prefix("c/") {
                return format!("https://www.youtube.com/c/{id}");
            }
            if let Some(id) = after.strip_prefix("user/") {
                return format!("https://www.youtube.com/user/{id}");
            }
            if let Some(handle) = after.strip_prefix("@") {
                return format!("https://www.youtube.com/@{handle}");
            }
        }
    }

    // YouTube Music shorthands: ytm:ID (track) or ytm:playlist/ID
    if matches!(source, Source::YouTubeMusicTrack | Source::YouTubeMusicPlaylist) {
        if let Some(after) = path.strip_prefix("ytm:") {
            if let Some(id) = after.strip_prefix("playlist/") {
                return format!("https://music.youtube.com/playlist?list={id}");
            }
            // bare ytm:ID → track
            return format!("https://music.youtube.com/watch?v={after}");
        }
    }

    // Spotify shorthands: spotify:track:ID, spotify:album:ID, spotify:playlist:ID
    if matches!(source, Source::SpotifyTrack | Source::SpotifyAlbum | Source::SpotifyPlaylist) {
        if let Some(after) = path.strip_prefix("spotify:") {
            if let Some(id) = after.strip_prefix("track:") {
                return format!("https://open.spotify.com/track/{id}");
            }
            if let Some(id) = after.strip_prefix("album:") {
                return format!("https://open.spotify.com/album/{id}");
            }
            if let Some(id) = after.strip_prefix("playlist:") {
                return format!("https://open.spotify.com/playlist/{id}");
            }
        }
    }

    if *source == Source::X && (path.starts_with("tweet:media:") || path.starts_with("x:media:")) {
        if let Some(tweet_id) = path.split(':').next_back().and_then(parse_tweet_id) {
            return format!("https://x.com/i/status/{tweet_id}");
        }
    }

    if let Some(path) = path.strip_prefix("instagram:") {
        if let Some(id) = path.strip_prefix("reel:") {
            return format!("https://www.instagram.com/reel/{id}");
        }
        return format!("https://www.instagram.com/{path}");
    }
    if let Some(path) = path.strip_prefix("facebook:") {
        return format!("https://www.facebook.com/{path}");
    }
    if let Some(path) = path.strip_prefix("tiktok:") {
        return format!("https://www.tiktok.com/{path}");
    }
    if let Some(path) = path.strip_prefix("reddit:") {
        return format!("https://www.reddit.com/{path}");
    }
    if let Some(path) = path.strip_prefix("snapchat:") {
        return format!("https://www.snapchat.com/{path}");
    }

    path.to_string()
}

// INFO: yt-dlp supports a lot of sites; so, when archiving (for example) a website, the user
// -> should be asked whether they want to archive the whole website or just the video(s) on it.
fn determine_source(path: &str) -> Source {
    // INFO: Extractor URLs can be found here:
    // -> https://github.com/yt-dlp/yt-dlp/tree/dfc0a84c192a7357dd1768cc345d590253a14fe5/yt_dlp/extractor
    // TEST: X posts can have multiple videos.

    // Shorthand schemes: yt: or youtube:
    if let Some(after_scheme) = path
        .strip_prefix("yt:")
        .or_else(|| path.strip_prefix("youtube:"))
    {
        // video/ID, short/ID, shorts/ID
        if after_scheme.starts_with("video/")
            || after_scheme.starts_with("short/")
            || after_scheme.starts_with("shorts/")
        {
            return Source::YouTubeVideo;
        }

        // playlist/ID
        if after_scheme.starts_with("playlist/") {
            return Source::YouTubePlaylist;
        }

        // channel/ID, c/ID, user/ID, @handle
        if after_scheme.starts_with("channel/")
            || after_scheme.starts_with("c/")
            || after_scheme.starts_with("user/")
            || after_scheme.starts_with("@")
        {
            return Source::YouTubeChannel;
        }
    }

    // Shorthand scheme: ytm:
    if let Some(after_scheme) = path.strip_prefix("ytm:") {
        if after_scheme.starts_with("playlist/") {
            return Source::YouTubeMusicPlaylist;
        }
        // Any other suffix is treated as a track ID
        return Source::YouTubeMusicTrack;
    }

    // Shorthand scheme: spotify:
    if let Some(after_scheme) = path.strip_prefix("spotify:") {
        if after_scheme.starts_with("album:") {
            return Source::SpotifyAlbum;
        }
        if after_scheme.starts_with("playlist:") {
            return Source::SpotifyPlaylist;
        }
        // track: or anything else maps to a track
        return Source::SpotifyTrack;
    }

    // Shorthand schemes: tweet:, x:, or twitter:
    if let Some(after_scheme) = path
        .strip_prefix("x:")
        .or_else(|| path.strip_prefix("twitter:"))
        .or_else(|| path.strip_prefix("tweet:"))
    {
        // For this scope, in comments, N is an alias for a string of type ('twitter' | 'x' | 'tweet').

        // N:media:id
        if after_scheme.starts_with("media:")
            && after_scheme
                .strip_prefix("media:")
                .and_then(parse_tweet_id)
                .is_some()
        {
            return Source::X;
        }

        // N:tweet:id or N:x:id
        if after_scheme
            .strip_prefix("tweet:")
            .or_else(|| after_scheme.strip_prefix("x:"))
            .and_then(parse_tweet_id)
            .is_some()
        {
            return Source::Tweet;
        }

        // N:thread:id
        if after_scheme
            .strip_prefix("thread:")
            .and_then(parse_tweet_id)
            .is_some()
        {
            return Source::TweetThread;
        }

        // N:id
        if parse_tweet_id(after_scheme).is_some() {
            return Source::Tweet;
        }

        // N:non-id
        return Source::Other;
    }

    // Shorthand schemes for other yt-dlp extractors
    if path.starts_with("instagram:") {
        return Source::Instagram;
    }
    if path.starts_with("facebook:") {
        return Source::Facebook;
    }
    if path.starts_with("tiktok:") {
        return Source::TikTok;
    }
    if path.starts_with("reddit:") {
        return Source::Reddit;
    }
    if path.starts_with("snapchat:") {
        return Source::Snapchat;
    }

    if path.starts_with("file://") {
        return Source::Local;
    } else if path.starts_with("http://") || path.starts_with("https://") {
        // Video URLs (watch, youtu.be, shorts)
        let video_re = regex::Regex::new(r"^https?://(?:www\.)?(?:youtu\.be/[0-9A-Za-z_-]+|youtube\.com/watch\?v=[0-9A-Za-z_-]+|youtube\.com/shorts/[0-9A-Za-z_-]+)")
            .expect("YouTube video URL regex literal must be valid");
        if video_re.is_match(path) {
            return Source::YouTubeVideo;
        }

        // Playlist URLs
        let playlist_re =
            regex::Regex::new(r"^https?://(?:www\.)?youtube\.com/playlist\?list=[0-9A-Za-z_-]+")
                .expect("YouTube playlist URL regex literal must be valid");
        if playlist_re.is_match(path) {
            return Source::YouTubePlaylist;
        }

        // Channel or user URLs (channel IDs, /c/, /user/, or @handles)
        let channel_re = regex::Regex::new(r"^https?://(?:www\.)?youtube\.com/(?:channel/[0-9A-Za-z_-]+|c/[0-9A-Za-z_-]+|user/[0-9A-Za-z_-]+|@[0-9A-Za-z_-]+)")
            .expect("YouTube channel URL regex literal must be valid");
        if channel_re.is_match(path) {
            return Source::YouTubeChannel;
        }

        // YouTube Music track URLs: music.youtube.com/watch?v=ID
        if path.starts_with("https://music.youtube.com/watch")
            || path.starts_with("http://music.youtube.com/watch")
        {
            return Source::YouTubeMusicTrack;
        }

        // YouTube Music playlist URLs: music.youtube.com/playlist?list=ID
        if path.starts_with("https://music.youtube.com/playlist")
            || path.starts_with("http://music.youtube.com/playlist")
        {
            return Source::YouTubeMusicPlaylist;
        }

        // Spotify URLs: open.spotify.com/{track,album,playlist}/ID
        if path.starts_with("https://open.spotify.com/track/")
            || path.starts_with("http://open.spotify.com/track/")
        {
            return Source::SpotifyTrack;
        }
        if path.starts_with("https://open.spotify.com/album/")
            || path.starts_with("http://open.spotify.com/album/")
        {
            return Source::SpotifyAlbum;
        }
        if path.starts_with("https://open.spotify.com/playlist/")
            || path.starts_with("http://open.spotify.com/playlist/")
        {
            return Source::SpotifyPlaylist;
        }

        if path.starts_with("https://x.com/") {
            return Source::X;
        }

        if path.starts_with("https://instagram.com/")
            || path.starts_with("https://www.instagram.com/")
            || path.starts_with("http://instagram.com/")
            || path.starts_with("http://www.instagram.com/")
        {
            return Source::Instagram;
        }

        if path.starts_with("https://facebook.com/")
            || path.starts_with("https://www.facebook.com/")
            || path.starts_with("http://facebook.com/")
            || path.starts_with("http://www.facebook.com/")
            || path.starts_with("https://fb.watch/")
            || path.starts_with("http://fb.watch/")
        {
            return Source::Facebook;
        }

        if path.starts_with("https://tiktok.com/")
            || path.starts_with("https://www.tiktok.com/")
            || path.starts_with("http://tiktok.com/")
            || path.starts_with("http://www.tiktok.com/")
        {
            return Source::TikTok;
        }

        if path.starts_with("https://reddit.com/")
            || path.starts_with("https://www.reddit.com/")
            || path.starts_with("http://reddit.com/")
            || path.starts_with("http://www.reddit.com/")
            || path.starts_with("https://redd.it/")
            || path.starts_with("http://redd.it/")
        {
            return Source::Reddit;
        }

        if path.starts_with("https://snapchat.com/")
            || path.starts_with("https://www.snapchat.com/")
            || path.starts_with("http://snapchat.com/")
            || path.starts_with("http://www.snapchat.com/")
        {
            return Source::Snapchat;
        }
        // No platform matched — treat as a generic file URL
        return Source::Url;
    }
    if Path::new(path).exists() {
        return Source::Local;
    }
    Source::Other
}

/// Resolves `locator` to the canonical URL that yt-dlp should receive, or
/// returns `None` if the locator does not map to a yt-dlp-downloadable source
/// (e.g. tweet/thread shorthands, web pages, local files, playlists/channels).
///
/// Use this to gate the probe endpoint: only call `fetch_metadata` when this
/// returns `Some`.
pub fn locator_to_ytdlp_url(locator: &str) -> Option<String> {
    let source = determine_source(locator);
    match source {
        Source::YouTubeVideo
        | Source::YouTubeMusicTrack
        | Source::X
        | Source::Instagram
        | Source::Facebook
        | Source::TikTok
        | Source::Reddit
        | Source::Snapchat => Some(expand_shorthand_to_url(locator, &source)),
        _ => None,
    }
}

fn hash_exists(hash: &str, file_extension: &str, store_path: &Path) -> Result<bool> {
    let path = store_path.join(raw_relative_path_from_hash(hash, file_extension)?);
    Ok(path.exists())
}

fn move_temp_to_raw(file: &Path, hash: &str, store_path: &Path) -> Result<()> {
    let file_extension = file
        .extension()
        .map_or(String::new(), |ext| format!(".{}", ext.to_string_lossy()));
    let raw_relpath = raw_relative_path_from_hash(hash, &file_extension)?;
    let destination = store_path.join(raw_relpath);

    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }

    fs::rename(file, destination)?;

    Ok(())
}

fn raw_relative_path_from_hash(hash: &str, file_extension: &str) -> Result<PathBuf> {
    let mut chars = hash.chars();
    let first_letter = chars.next().context("hash must not be empty")?;
    let second_letter = chars
        .next()
        .context("hash must be at least two characters")?;

    Ok(PathBuf::from("raw")
        .join(first_letter.to_string())
        .join(second_letter.to_string())
        .join(format!("{hash}{file_extension}")))
}

fn path_to_store_string(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn extension_without_dot(file_extension: &str) -> Option<String> {
    file_extension
        .strip_prefix('.')
        .filter(|extension| !extension.is_empty())
        .map(|extension| extension.to_string())
}

fn blob_record_for_raw_relpath(
    store_path: &Path,
    raw_relpath: &Path,
) -> Result<database::BlobRecord> {
    let absolute_path = store_path.join(raw_relpath);
    let file_name = raw_relpath
        .file_name()
        .and_then(|name| name.to_str())
        .context("raw artifact path must have a UTF-8 file name")?;
    let (sha256, extension) = match file_name.rsplit_once('.') {
        Some((hash, extension)) => (hash.to_string(), Some(extension.to_string())),
        None => (file_name.to_string(), None),
    };

    Ok(database::BlobRecord {
        sha256,
        byte_size: fs::metadata(&absolute_path)
            .with_context(|| format!("failed to stat raw artifact {}", absolute_path.display()))?
            .len() as i64,
        mime_type: None,
        extension,
        raw_relpath: path_to_store_string(raw_relpath),
    })
}

fn source_metadata(source: Source) -> (&'static str, &'static str, &'static str) {
    match source {
        Source::YouTubeVideo => ("youtube", "video", "video"),
        Source::YouTubePlaylist => ("youtube", "playlist", "container"),
        Source::YouTubeChannel => ("youtube", "channel", "container"),
        Source::YouTubeMusicTrack => ("youtube_music", "music", "audio"),
        Source::YouTubeMusicPlaylist => ("youtube_music", "playlist", "container"),
        Source::SpotifyTrack => ("spotify", "music", "audio"),
        Source::SpotifyAlbum => ("spotify", "album", "container"),
        Source::SpotifyPlaylist => ("spotify", "playlist", "container"),
        Source::X => ("x", "post", "video"),
        Source::Tweet => ("x", "tweet", "tweet_json"),
        Source::TweetThread => ("x", "tweet_thread", "tweet_json"),
        Source::Instagram => ("instagram", "post", "video"),
        Source::Facebook => ("facebook", "post", "video"),
        Source::TikTok => ("tiktok", "video", "video"),
        Source::Reddit => ("reddit", "post", "video"),
        Source::Snapchat => ("snapchat", "story", "video"),
        Source::Local => ("local", "file", "file"),
        Source::Url => ("web", "file", "file"),
        Source::WebPage => ("web", "page", "webpage"),
        Source::Other => ("other", "unknown", "unknown"),
    }
}

fn local_file_extension(path: &str) -> String {
    Path::new(path.trim_start_matches("file://"))
        .extension()
        .map_or(String::new(), |ext| format!(".{}", ext.to_string_lossy()))
}

fn tweet_id_from_archive_path(path: &str) -> Option<String> {
    path.split(':').next_back().and_then(parse_tweet_id)
}

fn create_structured_root(store_path: &Path, entry: &database::ArchivedEntry) -> Result<()> {
    debug_assert!(entry.entry_uid.starts_with("entry_"));
    fs::create_dir_all(store_path.join(&entry.structured_root_relpath))?;
    Ok(())
}

fn record_media_entry(
    conn: &rusqlite::Connection,
    store_path: &Path,
    user_id: i64,
    run: &database::ArchiveRun,
    item: &database::ArchiveRunItem,
    requested_locator: &str,
    canonical_locator: &str,
    source: Source,
    hash: &str,
    file_extension: &str,
    byte_size: i64,
    title: Option<String>,
) -> Result<database::ArchivedEntry> {
    debug_assert!(run.run_uid.starts_with("run_"));
    debug_assert!(item.item_uid.starts_with("item_"));
    let (source_kind, entity_kind, representation_kind) = source_metadata(source);
    let raw_relpath = raw_relative_path_from_hash(hash, file_extension)?;
    let blob = database::BlobRecord {
        sha256: hash.to_string(),
        byte_size,
        mime_type: None,
        extension: extension_without_dot(file_extension),
        raw_relpath: path_to_store_string(&raw_relpath),
    };
    let blob_id = database::upsert_blob(conn, &blob)?;
    let source_identity_id = database::upsert_source_identity(
        conn,
        source_kind,
        entity_kind,
        None,
        Some(canonical_locator),
        canonical_locator,
    )?;
    let entry = database::create_archived_entry(
        conn,
        &database::NewEntry {
            source_identity_id,
            archive_run_id: run.id,
            parent_entry_id: None,
            root_entry_id: None,
            created_by_user_id: user_id,
            owned_by_user_id: user_id,
            source_kind: source_kind.to_string(),
            entity_kind: entity_kind.to_string(),
            title,
            visibility: "private".to_string(),
            representation_kind: representation_kind.to_string(),
            source_metadata_json: json!({
                "requested_locator": requested_locator,
                "canonical_locator": canonical_locator
            })
            .to_string(),
            display_metadata_json: None,
        },
    )?;
    create_structured_root(store_path, &entry)?;
    database::add_entry_artifact(
        conn,
        &database::NewArtifact {
            entry_id: entry.id,
            artifact_role: "primary_media".to_string(),
            storage_area: "raw".to_string(),
            relpath: blob.raw_relpath,
            blob_id: Some(blob_id),
            logical_path: None,
            metadata_json: None,
        },
    )?;
    database::complete_archive_run_item(conn, item.id, entry.id)?;
    Ok(entry)
}

/// Extracts PlatformMetadata from a tweet JSON string.
/// Returns Default on any parse failure.
fn tweet_metadata_from_json(json_str: &str) -> PlatformMetadata {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json_str) else {
        return PlatformMetadata::default();
    };

    let screen_name = v
        .get("author")
        .and_then(|a| a.get("screen_name"))
        .and_then(|s| s.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let full_text = v
        .get("full_text")
        .and_then(|t| t.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    PlatformMetadata {
        author: screen_name,
        caption: full_text,
        ..Default::default()
    }
}

fn record_tweet_entry(
    conn: &rusqlite::Connection,
    store_path: &Path,
    user_id: i64,
    run: &database::ArchiveRun,
    item: &database::ArchiveRunItem,
    requested_locator: &str,
    source: Source,
    tweet_id: &str,
) -> Result<database::ArchivedEntry> {
    debug_assert!(run.run_uid.starts_with("run_"));
    debug_assert!(item.item_uid.starts_with("item_"));
    let (source_kind, entity_kind, representation_kind) = source_metadata(source);
    let canonical_locator = format!("https://x.com/i/status/{tweet_id}");
    let source_identity_id = database::upsert_source_identity(
        conn,
        source_kind,
        entity_kind,
        Some(tweet_id),
        Some(&canonical_locator),
        &canonical_locator,
    )?;
    // Read tweet JSON early to extract title before entry creation
    let tweet_json_relpath = PathBuf::from("raw_tweets").join(format!("tweet-{tweet_id}.json"));
    let tweet_json = fs::read_to_string(store_path.join(&tweet_json_relpath))?;
    let tweet_meta = tweet_metadata_from_json(&tweet_json);
    let tweet_title = generate_entry_title(source, &tweet_meta);

    let entry = database::create_archived_entry(
        conn,
        &database::NewEntry {
            source_identity_id,
            archive_run_id: run.id,
            parent_entry_id: None,
            root_entry_id: None,
            created_by_user_id: user_id,
            owned_by_user_id: user_id,
            source_kind: source_kind.to_string(),
            entity_kind: entity_kind.to_string(),
            title: Some(tweet_title),
            visibility: "private".to_string(),
            representation_kind: representation_kind.to_string(),
            source_metadata_json: json!({
                "tweet_id": tweet_id,
                "requested_locator": requested_locator
            })
            .to_string(),
            display_metadata_json: None,
        },
    )?;
    create_structured_root(store_path, &entry)?;

    database::add_entry_artifact(
        conn,
        &database::NewArtifact {
            entry_id: entry.id,
            artifact_role: "raw_tweet_json".to_string(),
            storage_area: "raw_tweets".to_string(),
            relpath: path_to_store_string(&tweet_json_relpath),
            blob_id: None,
            logical_path: None,
            metadata_json: None,
        },
    )?;

    for (role, raw_relpath) in tweet_raw_artifacts(&tweet_json)? {
        let raw_path = PathBuf::from(&raw_relpath);
        let blob = blob_record_for_raw_relpath(store_path, &raw_path)?;
        let blob_id = database::upsert_blob(conn, &blob)?;
        database::add_entry_artifact(
            conn,
            &database::NewArtifact {
                entry_id: entry.id,
                artifact_role: role,
                storage_area: "raw".to_string(),
                relpath: raw_relpath,
                blob_id: Some(blob_id),
                logical_path: None,
                metadata_json: None,
            },
        )?;
    }

    database::complete_archive_run_item(conn, item.id, entry.id)?;
    Ok(entry)
}

fn tweet_raw_artifacts(tweet_json: &str) -> Result<Vec<(String, String)>> {
    let regex = regex::Regex::new(r#""(avatar_local_path|local_path)": "([^"\n]+)""#)?;
    let mut seen = HashSet::new();
    let mut artifacts = Vec::new();

    for captures in regex.captures_iter(tweet_json) {
        let relpath = captures[2].to_string();
        if !relpath.starts_with("raw/") || !seen.insert(relpath.clone()) {
            continue;
        }

        let role = if &captures[1] == "avatar_local_path" {
            "avatar"
        } else {
            "media"
        };
        artifacts.push((role.to_string(), relpath));
    }

    Ok(artifacts)
}

/// Marks the run and item as failed in the database, returns the error.
/// Call sites: `return Err(fail_run(&conn, &run, &item, "message"));`
fn fail_run(
    conn: &rusqlite::Connection,
    run: &database::ArchiveRun,
    item: &database::ArchiveRunItem,
    message: &str,
) -> anyhow::Error {
    let _ = database::fail_archive_run_item(conn, item.id, message);
    let _ = database::fail_archive_run(conn, run.id, message);
    anyhow::anyhow!("{}", message)
}

pub fn perform_capture(
    archive_paths: &ArchivePaths,
    locator: &str,
    archive_id: Option<&str>,
    quality: Option<&str>,
    config: &CaptureConfig,
) -> Result<CaptureResult> {
    // Append a UUID so parallel captures starting in the same millisecond
    // never collide on the staging directory or file names.
    let timestamp = format!(
        "{}-{}",
        Local::now().format("%Y-%m-%dT%H-%M-%S%.3f"),
        Uuid::new_v4().simple(),
    );
    let store_path = &archive_paths.store_path;

    let conn = database::open_or_initialize(&archive_paths.archive_path)?;
    let user_id = database::ensure_default_user(&conn)?;

    let mut source = determine_source(locator);

    // Expand shorthands to the canonical URL for cookie matching.
    let canonical_url = expand_shorthand_to_url(locator, &source);
    let cookies = resolve_cookies_for_url(&config.cookie_rules, &canonical_url);

    // Create the run record before probing so every attempt — including
    // probe failures — is visible in /runs with a proper status and error.
    let run = database::create_archive_run(&conn, user_id, 1)?;

    // For generic http/https URLs, probe Content-Type to decide whether to
    // treat the URL as a raw file download or an HTML page for SingleFile.
    if source == Source::Url {
        match downloader::http::probe_url_kind(locator, &cookies) {
            Ok(downloader::http::UrlKind::Html) => source = Source::WebPage,
            Ok(downloader::http::UrlKind::File) => {}
            Err(e) => {
                // Record a failed item using the pre-probe source_kind so
                // failed_count increments and the item carries error_text.
                let (probe_sk, probe_ek, _) = source_metadata(Source::Url);
                let item = database::create_archive_run_item(
                    &conn, run.id, None, 0, locator, None, probe_sk, probe_ek,
                )?;
                let msg = format!("Failed to probe URL: {e}");
                return Err(fail_run(&conn, &run, &item, &msg));
            }
        }
    }

    let (source_kind, entity_kind, _) = source_metadata(source);

    let item = database::create_archive_run_item(
        &conn,
        run.id,
        None,
        0,
        locator,
        None,
        source_kind,
        entity_kind,
    )?;

    // Sources: Other (not yet implemented)
    if source == Source::Other {
        return Err(fail_run(
            &conn,
            &run,
            &item,
            "Archiving from this source is not yet implemented.",
        ));
    }

    // Sources: Spotify — not downloadable; Spotify audio is DRM-protected.
    if matches!(source, Source::SpotifyTrack | Source::SpotifyAlbum | Source::SpotifyPlaylist) {
        return Err(fail_run(
            &conn,
            &run,
            &item,
            "Spotify downloads are not supported: Spotify audio is DRM-protected and cannot \
             be downloaded by yt-dlp. Archive the equivalent YouTube Music track instead.",
        ));
    }

    // Sources: YouTube Music Playlist — container expansion not yet implemented.
    if source == Source::YouTubeMusicPlaylist {
        return Err(fail_run(
            &conn,
            &run,
            &item,
            "YouTube Music playlist archiving is not yet implemented.",
        ));
    }

    // Source: generic HTTP/S file URL
    if source == Source::Url {
        match downloader::http::download(locator, store_path, &timestamp, &cookies) {
            Ok((hash, file_extension, title_hint)) => {
                let temp_file = store_path
                    .join("temp")
                    .join(&timestamp)
                    .join(format!("{timestamp}{file_extension}"));
                let byte_size = fs::metadata(&temp_file)
                    .with_context(|| format!("failed to stat staged file {}", temp_file.display()))?
                    .len() as i64;

                let hash_exists = hash_exists(&hash, &file_extension, store_path)?;
                if hash_exists {
                    let _ = fs::remove_dir_all(store_path.join("temp").join(&timestamp));
                } else {
                    move_temp_to_raw(
                        &store_path
                            .join("temp")
                            .join(&timestamp)
                            .join(format!("{timestamp}{file_extension}")),
                        &hash,
                        store_path,
                    )?;
                    let _ = fs::remove_dir_all(store_path.join("temp").join(&timestamp));
                }

                let entry = record_media_entry(
                    &conn,
                    store_path,
                    user_id,
                    &run,
                    &item,
                    locator,
                    locator,
                    source,
                    &hash,
                    &file_extension,
                    byte_size,
                    title_hint,
                )?;
                database::refresh_entry_cached_bytes(&conn, entry.id)?;
                database::finish_archive_run(&conn, run.id)?;
                return Ok(CaptureResult {
                    run_uid: run.run_uid.clone(),
                    status: "completed".to_string(),
                });
            }
            Err(e) => {
                return Err(fail_run(
                    &conn,
                    &run,
                    &item,
                    &format!("Failed to download URL: {e}"),
                ));
            }
        }
    }

    // Source: web page — archive as a self-contained HTML snapshot via single-file-cli
    if source == Source::WebPage {
        match downloader::singlefile::save(locator, store_path, &timestamp, &cookies) {
            Ok(result) => {
                let file_extension = ".html".to_string();
                let temp_html = store_path
                    .join("temp")
                    .join(&timestamp)
                    .join(format!("{timestamp}{file_extension}"));

                // Font extraction: rewrite the HTML in-place before hashing.
                // Only runs when archive_id is known (server context). CLI passes
                // None and keeps fonts embedded — no behaviour change for CLI.
                let (html_hash, byte_size, extracted_fonts) =
                    if let Some(aid) = archive_id {
                        let content = fs::read_to_string(&temp_html)
                            .with_context(|| format!("failed to read {}", temp_html.display()))?;
                        let (rewritten, fonts) =
                            downloader::font_extractor::extract_and_rewrite(&content, store_path, aid)
                                .unwrap_or_else(|_| (content.clone(), vec![])); // non-fatal
                        fs::write(&temp_html, rewritten.as_bytes())
                            .with_context(|| "failed to write rewritten HTML")?;
                        let size = rewritten.len() as i64;
                        let new_hash = crate::hash::hash_bytes(rewritten.as_bytes());
                        (new_hash, size, fonts)
                    } else {
                        let size = fs::metadata(&temp_html)
                            .with_context(|| format!("failed to stat {}", temp_html.display()))?
                            .len() as i64;
                        (result.html_hash.clone(), size, vec![])
                    };

                // 1. Move HTML to raw store (if this hash hasn't been seen before).
                if !hash_exists(&html_hash, &file_extension, store_path)? {
                    move_temp_to_raw(&temp_html, &html_hash, store_path)?;
                }

                // 2. Process favicon while the temp dir still exists.
                //    Errors here are silenced — favicon is supplementary.
                let favicon_info: Option<(String, i64)> = (|| -> Option<(String, i64)> {
                    let fav_hash = result.favicon_hash.as_deref()?;
                    let fav_ext  = result.favicon_ext.as_deref()?;
                    let fav_temp = store_path
                        .join("temp").join(&timestamp)
                        .join(format!("{timestamp}.favicon{fav_ext}"));
                    if !fav_temp.exists() { return None; }
                    let fav_size = fs::metadata(&fav_temp).ok()?.len() as i64;
                    let fav_raw  = raw_relative_path_from_hash(fav_hash, fav_ext).ok()?;
                    if !hash_exists(fav_hash, fav_ext, store_path).ok()? {
                        move_temp_to_raw(&fav_temp, fav_hash, store_path).ok()?;
                    }
                    let fav_blob = database::BlobRecord {
                        sha256:      fav_hash.to_string(),
                        byte_size:   fav_size,
                        mime_type:   None,
                        extension:   extension_without_dot(fav_ext),
                        raw_relpath: path_to_store_string(&fav_raw),
                    };
                    let fav_blob_id = database::upsert_blob(&conn, &fav_blob).ok()?;
                    Some((fav_blob.raw_relpath, fav_blob_id))
                })();

                // 3. Remove the temp directory.
                let _ = fs::remove_dir_all(store_path.join("temp").join(&timestamp));

                // 4. Create the entry + primary_media artifact.
                let entry = record_media_entry(
                    &conn,
                    store_path,
                    user_id,
                    &run,
                    &item,
                    locator,
                    locator,
                    source,
                    &html_hash,
                    &file_extension,
                    byte_size,
                    result.title,
                )?;

                // 5. Add favicon artifact if we captured one.
                if let Some((fav_relpath, fav_blob_id)) = favicon_info {
                    let _ = database::add_entry_artifact(
                        &conn,
                        &database::NewArtifact {
                            entry_id:      entry.id,
                            artifact_role: "favicon".to_string(),
                            storage_area:  "raw".to_string(),
                            relpath:       fav_relpath,
                            blob_id:       Some(fav_blob_id),
                            logical_path:  None,
                            metadata_json: None,
                        },
                    );
                }

                // 6. Register each extracted font as a deduplicated blob + artifact.
                for font in extracted_fonts {
                    let font_blob = database::BlobRecord {
                        sha256:      font.sha256.clone(),
                        byte_size:   font.byte_size,
                        mime_type:   mime_for_font_ext(&font.ext),
                        extension:   font.ext.strip_prefix('.').map(|s| s.to_string()),
                        raw_relpath: font.raw_relpath.clone(),
                    };
                    if let Ok(blob_id) = database::upsert_blob(&conn, &font_blob) {
                        let _ = database::add_entry_artifact(
                            &conn,
                            &database::NewArtifact {
                                entry_id:      entry.id,
                                artifact_role: "font".to_string(),
                                storage_area:  "raw".to_string(),
                                relpath:       font.raw_relpath,
                                blob_id:       Some(blob_id),
                                logical_path:  None,
                                metadata_json: None,
                            },
                        );
                    }
                }

                // 7. Store how many bytes this entry gets "for free" from earlier entries.
                database::refresh_entry_cached_bytes(&conn, entry.id)?;

                database::finish_archive_run(&conn, run.id)?;
                return Ok(CaptureResult {
                    run_uid: run.run_uid.clone(),
                    status: "completed".to_string(),
                });
            }
            Err(e) => {
                return Err(fail_run(
                    &conn,
                    &run,
                    &item,
                    &format!("Failed to archive web page: {e}"),
                ));
            }
        }
    }

    // Sources: Tweets or Twitter Threads
    if matches!(source, Source::Tweet | Source::TweetThread) {
        let tweet_id = match tweet_id_from_archive_path(locator) {
            Some(tweet_id) => tweet_id,
            None => {
                return Err(fail_run(
                    &conn,
                    &run,
                    &item,
                    "Failed to archive tweet: invalid tweet ID",
                ));
            }
        };

        // Tweet shorthands (tweet:ID) don't expand to a URL, so `canonical_url`
        // won't match x.com patterns. Always resolve cookies against x.com so
        // that wildcard/global rules containing `ct0`+`auth_token` are picked up.
        let tweet_cookies = resolve_cookies_for_url(&config.cookie_rules, "https://x.com/");
        match downloader::tweets::archive(
            locator,
            source == Source::TweetThread,
            store_path,
            &timestamp,
            &tweet_cookies,
        ) {
            Ok(_) => {
                let tweet_entry = record_tweet_entry(
                    &conn,
                    store_path,
                    user_id,
                    &run,
                    &item,
                    locator,
                    source,
                    &tweet_id,
                )?;
                database::refresh_entry_cached_bytes(&conn, tweet_entry.id)?;
                database::finish_archive_run(&conn, run.id)?;
                return Ok(CaptureResult {
                    run_uid: run.run_uid.clone(),
                    status: "completed".to_string(),
                });
            }
            Err(e) => {
                return Err(fail_run(
                    &conn,
                    &run,
                    &item,
                    &format!("Failed to archive tweet: {e}"),
                ));
            }
        }
    }

    // Sources, for which yt-dlp is needed
    let requested_locator = locator.to_string();
    let path = expand_shorthand_to_url(locator, &source);
    // Fetch yt-dlp metadata before downloading — separate invocation
    // because --dump-json is a simulate flag that suppresses the download.
    let ytdlp_metadata_json: Option<String> = match source {
        Source::YouTubeVideo
        | Source::YouTubeMusicTrack
        | Source::X
        | Source::Instagram
        | Source::Facebook
        | Source::TikTok
        | Source::Reddit
        | Source::Snapchat => downloader::ytdlp::fetch_metadata(&path, &cookies),
        _ => None,
    };

    let local_filename_title: Option<String> = match source {
        Source::Local => {
            // path is a file:// URI; strip the scheme and take the last component.
            let file_path = path.trim_start_matches("file://");
            std::path::Path::new(file_path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
        }
        _ => None,
    };

    let (hash, file_extension) = match source {
        Source::YouTubeVideo
        | Source::X
        | Source::Instagram
        | Source::Facebook
        | Source::TikTok
        | Source::Reddit
        | Source::Snapchat => {
            match downloader::ytdlp::download(path.clone(), store_path, &timestamp, quality, &cookies) {
                Ok(result) => result,
                Err(e) => {
                    return Err(fail_run(
                        &conn,
                        &run,
                        &item,
                        &format!("Failed to download media: {e}"),
                    ));
                }
            }
        }
        Source::YouTubeMusicTrack => {
            // Music tracks are always audio-only regardless of the caller's quality hint.
            match downloader::ytdlp::download(path.clone(), store_path, &timestamp, Some("audio"), &cookies) {
                Ok(result) => result,
                Err(e) => {
                    return Err(fail_run(
                        &conn,
                        &run,
                        &item,
                        &format!("Failed to download audio: {e}"),
                    ));
                }
            }
        }
        Source::Local => {
            match downloader::local::save(path.clone(), store_path, &timestamp) {
                Ok(h) => (h, local_file_extension(&path)),
                Err(e) => {
                    return Err(fail_run(
                        &conn,
                        &run,
                        &item,
                        &format!("Failed to archive local file: {e}"),
                    ));
                }
            }
        }
        Source::YouTubePlaylist | Source::YouTubeChannel => {
            return Err(fail_run(
                &conn,
                &run,
                &item,
                "Playlist and channel container expansion are not yet implemented.",
            ));
        }
        _ => unreachable!(),
    };
    let temp_file = store_path
        .join("temp")
        .join(&timestamp)
        .join(format!("{timestamp}{file_extension}"));
    let byte_size = fs::metadata(&temp_file)
        .with_context(|| format!("failed to stat staged file {}", temp_file.display()))?
        .len() as i64;

    let hash_exists = hash_exists(&hash, &file_extension, store_path)?;

    if hash_exists {
        let _ = fs::remove_dir_all(store_path.join("temp").join(&timestamp));
    } else {
        move_temp_to_raw(
            &store_path
                .join("temp")
                .join(&timestamp)
                .join(format!("{timestamp}{file_extension}")),
            &hash,
            store_path,
        )?;
        let _ = fs::remove_dir_all(store_path.join("temp").join(&timestamp));
    }

    let entry_title: Option<String> = if let Some(json) = &ytdlp_metadata_json {
        let metadata = downloader::metadata::extract_from_ytdlp_json(json);
        Some(generate_entry_title(source, &metadata))
    } else if let Some(filename) = local_filename_title {
        let metadata = PlatformMetadata {
            title: Some(filename),
            ..Default::default()
        };
        Some(generate_entry_title(source, &metadata))
    } else {
        None
    };

    let media_entry = record_media_entry(
        &conn,
        store_path,
        user_id,
        &run,
        &item,
        &requested_locator,
        &path,
        source,
        &hash,
        &file_extension,
        byte_size,
        entry_title,
    )?;
    database::refresh_entry_cached_bytes(&conn, media_entry.id)?;
    database::finish_archive_run(&conn, run.id)?;

    Ok(CaptureResult {
        run_uid: run.run_uid.clone(),
        status: "completed".to_string(),
    })
}

fn mime_for_font_ext(ext: &str) -> Option<String> {
    match ext {
        ".woff2" => Some("font/woff2".to_string()),
        ".woff"  => Some("font/woff".to_string()),
        ".ttf"   => Some("font/ttf".to_string()),
        ".otf"   => Some("font/otf".to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{archive, database};
    use chrono::Local;
    use std::{env, fs};

    struct TestCase<'a> {
        url: &'a str,
        expected: Source,
    }

    #[test]
    fn test_tweet_sources() {
        let cases = [
            TestCase {
                url: "tweet:1234567890",
                expected: Source::Tweet,
            },
            TestCase {
                url: "x:tweet:1234567890",
                expected: Source::Tweet,
            },
            TestCase {
                url: "x:x:1234567890",
                expected: Source::Tweet,
            },
            TestCase {
                url: "twitter:x:1234567890",
                expected: Source::Tweet,
            },
            TestCase {
                url: "twitter:tweet:1234567890",
                expected: Source::Tweet,
            },
            TestCase {
                url: "tweet:media:1234567890",
                expected: Source::X,
            },
            TestCase {
                url: "x:media:1234567890",
                expected: Source::X,
            },
            TestCase {
                url: "x:thread:1234567890",
                expected: Source::TweetThread,
            },
            TestCase {
                url: "twitter:thread:1234567890",
                expected: Source::TweetThread,
            },
            TestCase {
                url: "tweet:thread:1234567890",
                expected: Source::TweetThread,
            },
            TestCase {
                url: "tweet:not-a-number",
                expected: Source::Other,
            },
            TestCase {
                url: "tweet:media:not-a-number",
                expected: Source::Other,
            },
            TestCase {
                url: "x:media:not-a-number",
                expected: Source::Other,
            },
        ];

        for case in &cases {
            assert_eq!(
                determine_source(case.url),
                case.expected,
                "Failed for URL: {}",
                case.url
            );
        }
    }

    #[test]
    fn test_resolve_source_path() {
        assert_eq!(
            expand_shorthand_to_url("tweet:media:1234567890", &Source::X),
            "https://x.com/i/status/1234567890"
        );
        assert_eq!(
            expand_shorthand_to_url("instagram:reel/ABC123", &Source::Instagram),
            "https://www.instagram.com/reel/ABC123"
        );
        assert_eq!(
            expand_shorthand_to_url("facebook:watch?v=123456", &Source::Facebook),
            "https://www.facebook.com/watch?v=123456"
        );
        assert_eq!(
            expand_shorthand_to_url("tiktok:@someone/video/123456789", &Source::TikTok),
            "https://www.tiktok.com/@someone/video/123456789"
        );
        assert_eq!(
            expand_shorthand_to_url("reddit:r/videos/comments/abc123/example", &Source::Reddit),
            "https://www.reddit.com/r/videos/comments/abc123/example"
        );
        assert_eq!(
            expand_shorthand_to_url("snapchat:discover/some-story/1234567890", &Source::Snapchat),
            "https://www.snapchat.com/discover/some-story/1234567890"
        );
        assert_eq!(
            expand_shorthand_to_url("tweet:1234567890", &Source::Tweet),
            "tweet:1234567890"
        );
        // YouTube shorthands must expand to full URLs before yt-dlp sees them
        assert_eq!(
            expand_shorthand_to_url("yt:video/MntbN1DdEP0", &Source::YouTubeVideo),
            "https://www.youtube.com/watch?v=MntbN1DdEP0"
        );
        assert_eq!(
            expand_shorthand_to_url("yt:shorts/EtC99eWiwRI", &Source::YouTubeVideo),
            "https://www.youtube.com/watch?v=EtC99eWiwRI"
        );
        assert_eq!(
            expand_shorthand_to_url("youtube:video/UHxw-L2WyyY", &Source::YouTubeVideo),
            "https://www.youtube.com/watch?v=UHxw-L2WyyY"
        );
        assert_eq!(
            expand_shorthand_to_url("yt:playlist/PL9vTTBa7QaQO", &Source::YouTubePlaylist),
            "https://www.youtube.com/playlist?list=PL9vTTBa7QaQO"
        );
        assert_eq!(
            expand_shorthand_to_url("yt:@CoreDumpped", &Source::YouTubeChannel),
            "https://www.youtube.com/@CoreDumpped"
        );
        assert_eq!(
            expand_shorthand_to_url("yt:channel/UCxyz123", &Source::YouTubeChannel),
            "https://www.youtube.com/channel/UCxyz123"
        );
        // Full YouTube URLs pass through unchanged
        assert_eq!(
            expand_shorthand_to_url("https://www.youtube.com/watch?v=UHxw-L2WyyY", &Source::YouTubeVideo),
            "https://www.youtube.com/watch?v=UHxw-L2WyyY"
        );
    }

    #[test]
    fn test_youtube_sources() {
        // --- YouTube Video URLs ---
        let video_cases = [
            TestCase {
                url: "https://www.youtube.com/watch?v=UHxw-L2WyyY",
                expected: Source::YouTubeVideo,
            },
            TestCase {
                url: "https://youtu.be/UHxw-L2WyyY",
                expected: Source::YouTubeVideo,
            },
            TestCase {
                url: "https://www.youtube.com/shorts/EtC99eWiwRI",
                expected: Source::YouTubeVideo,
            },
        ];

        for case in &video_cases {
            assert_eq!(
                determine_source(case.url),
                case.expected,
                "Failed for URL: {}",
                case.url
            );
        }

        // --- YouTube Playlist URLs ---
        let playlist_cases = [TestCase {
            url: "https://www.youtube.com/playlist?list=PL9vTTBa7QaQOoMfpP3ztvgyQkPWDPfJez",
            expected: Source::YouTubePlaylist,
        }];

        for case in &playlist_cases {
            assert_eq!(
                determine_source(case.url),
                case.expected,
                "Failed for URL: {}",
                case.url
            );
        }

        // --- YouTube Channel URLs ---
        let channel_cases = [
            TestCase {
                url: "https://www.youtube.com/channel/CoreDumpped",
                expected: Source::YouTubeChannel,
            },
            TestCase {
                url: "https://www.youtube.com/@CoreDumpped",
                expected: Source::YouTubeChannel,
            },
            TestCase {
                url: "https://www.youtube.com/c/YouTubeCreators",
                expected: Source::YouTubeChannel,
            },
            TestCase {
                url: "https://www.youtube.com/user/pewdiepie",
                expected: Source::YouTubeChannel,
            },
            TestCase {
                url: "https://youtube.com/@pewdiepie?si=KOcLN_KPYNpe5f_8",
                expected: Source::YouTubeChannel,
            },
        ];

        for case in &channel_cases {
            assert_eq!(
                determine_source(case.url),
                case.expected,
                "Failed for URL: {}",
                case.url
            );
        }

        // --- Shorthand scheme URLs ---
        let shorthand_cases = [
            // Videos
            TestCase {
                url: "yt:video/UHxw-L2WyyY",
                expected: Source::YouTubeVideo,
            },
            TestCase {
                url: "youtube:video/UHxw-L2WyyY",
                expected: Source::YouTubeVideo,
            },
            TestCase {
                url: "yt:short/EtC99eWiwRI",
                expected: Source::YouTubeVideo,
            },
            TestCase {
                url: "yt:shorts/EtC99eWiwRI",
                expected: Source::YouTubeVideo,
            },
            TestCase {
                url: "youtube:shorts/EtC99eWiwRI",
                expected: Source::YouTubeVideo,
            },
            // Playlists
            TestCase {
                url: "yt:playlist/PL9vTTBa7QaQOoMfpP3ztvgyQkPWDPfJez",
                expected: Source::YouTubePlaylist,
            },
            TestCase {
                url: "youtube:playlist/PL9vTTBa7QaQOoMfpP3ztvgyQkPWDPfJez",
                expected: Source::YouTubePlaylist,
            },
            // Channels
            TestCase {
                url: "yt:channel/UCxyz123",
                expected: Source::YouTubeChannel,
            },
            TestCase {
                url: "yt:c/YouTubeCreators",
                expected: Source::YouTubeChannel,
            },
            TestCase {
                url: "yt:user/pewdiepie",
                expected: Source::YouTubeChannel,
            },
            TestCase {
                url: "youtube:@CoreDumpped",
                expected: Source::YouTubeChannel,
            },
        ];

        for case in &shorthand_cases {
            assert_eq!(
                determine_source(case.url),
                case.expected,
                "Failed for URL: {}",
                case.url
            );
        }
    }

    #[test]
    fn test_youtube_music_sources() {
        // --- determine_source ---
        let cases = [
            TestCase {
                url: "https://music.youtube.com/watch?v=MntbN1DdEP0",
                expected: Source::YouTubeMusicTrack,
            },
            TestCase {
                url: "http://music.youtube.com/watch?v=MntbN1DdEP0",
                expected: Source::YouTubeMusicTrack,
            },
            TestCase {
                url: "https://music.youtube.com/playlist?list=PLtest123",
                expected: Source::YouTubeMusicPlaylist,
            },
            TestCase {
                url: "ytm:MntbN1DdEP0",
                expected: Source::YouTubeMusicTrack,
            },
            TestCase {
                url: "ytm:playlist/PLtest123",
                expected: Source::YouTubeMusicPlaylist,
            },
        ];
        for case in &cases {
            assert_eq!(
                determine_source(case.url),
                case.expected,
                "Failed for URL: {}",
                case.url
            );
        }

        // --- expand_shorthand_to_url ---
        assert_eq!(
            expand_shorthand_to_url("ytm:MntbN1DdEP0", &Source::YouTubeMusicTrack),
            "https://music.youtube.com/watch?v=MntbN1DdEP0"
        );
        assert_eq!(
            expand_shorthand_to_url("ytm:playlist/PLtest123", &Source::YouTubeMusicPlaylist),
            "https://music.youtube.com/playlist?list=PLtest123"
        );
        // Full URL passes through unchanged
        assert_eq!(
            expand_shorthand_to_url(
                "https://music.youtube.com/watch?v=MntbN1DdEP0",
                &Source::YouTubeMusicTrack
            ),
            "https://music.youtube.com/watch?v=MntbN1DdEP0"
        );

        // --- locator_to_ytdlp_url ---
        assert_eq!(
            locator_to_ytdlp_url("ytm:MntbN1DdEP0"),
            Some("https://music.youtube.com/watch?v=MntbN1DdEP0".to_string())
        );
        assert_eq!(
            locator_to_ytdlp_url("https://music.youtube.com/watch?v=MntbN1DdEP0"),
            Some("https://music.youtube.com/watch?v=MntbN1DdEP0".to_string())
        );
        // Playlist is not exposed to the probe endpoint
        assert_eq!(locator_to_ytdlp_url("ytm:playlist/PLtest123"), None);

        // --- source_metadata ---
        assert_eq!(
            source_metadata(Source::YouTubeMusicTrack),
            ("youtube_music", "music", "audio")
        );
        assert_eq!(
            source_metadata(Source::YouTubeMusicPlaylist),
            ("youtube_music", "playlist", "container")
        );
    }

    #[test]
    fn test_spotify_sources() {
        // --- determine_source ---
        let cases = [
            TestCase {
                url: "https://open.spotify.com/track/4iV5W9uYEdYUVa79Axb7Rh",
                expected: Source::SpotifyTrack,
            },
            TestCase {
                url: "https://open.spotify.com/album/1DFixLWuPkv3KT3TnV35m3",
                expected: Source::SpotifyAlbum,
            },
            TestCase {
                url: "https://open.spotify.com/playlist/37i9dQZF1DXcBWIGoYBM5M",
                expected: Source::SpotifyPlaylist,
            },
            TestCase {
                url: "spotify:track:4iV5W9uYEdYUVa79Axb7Rh",
                expected: Source::SpotifyTrack,
            },
            TestCase {
                url: "spotify:album:1DFixLWuPkv3KT3TnV35m3",
                expected: Source::SpotifyAlbum,
            },
            TestCase {
                url: "spotify:playlist:37i9dQZF1DXcBWIGoYBM5M",
                expected: Source::SpotifyPlaylist,
            },
        ];
        for case in &cases {
            assert_eq!(
                determine_source(case.url),
                case.expected,
                "Failed for URL: {}",
                case.url
            );
        }

        // --- expand_shorthand_to_url ---
        assert_eq!(
            expand_shorthand_to_url("spotify:track:4iV5W9uYEdYUVa79Axb7Rh", &Source::SpotifyTrack),
            "https://open.spotify.com/track/4iV5W9uYEdYUVa79Axb7Rh"
        );
        assert_eq!(
            expand_shorthand_to_url("spotify:album:1DFixLWuPkv3KT3TnV35m3", &Source::SpotifyAlbum),
            "https://open.spotify.com/album/1DFixLWuPkv3KT3TnV35m3"
        );
        assert_eq!(
            expand_shorthand_to_url(
                "spotify:playlist:37i9dQZF1DXcBWIGoYBM5M",
                &Source::SpotifyPlaylist
            ),
            "https://open.spotify.com/playlist/37i9dQZF1DXcBWIGoYBM5M"
        );

        // --- source_metadata ---
        assert_eq!(source_metadata(Source::SpotifyTrack), ("spotify", "music", "audio"));
        assert_eq!(source_metadata(Source::SpotifyAlbum), ("spotify", "album", "container"));
        assert_eq!(source_metadata(Source::SpotifyPlaylist), ("spotify", "playlist", "container"));
    }

    #[test]
    fn test_x_sources() {
        let x_cases = [
            TestCase {
                url: "https://x.com/some_post",
                expected: Source::X,
            },
            TestCase {
                url: "x:1234567890",
                expected: Source::Tweet,
            },
            TestCase {
                url: "twitter:1234567890",
                expected: Source::Tweet,
            },
        ];

        for case in &x_cases {
            assert_eq!(
                determine_source(case.url),
                case.expected,
                "Failed for URL: {}",
                case.url
            );
        }
    }

    #[test]
    fn test_other_social_sources() {
        let social_cases = [
            TestCase {
                url: "https://www.instagram.com/reel/ABC123/",
                expected: Source::Instagram,
            },
            TestCase {
                url: "instagram:reel/ABC123",
                expected: Source::Instagram,
            },
            TestCase {
                url: "https://www.facebook.com/watch/?v=123456",
                expected: Source::Facebook,
            },
            TestCase {
                url: "facebook:watch?v=123456",
                expected: Source::Facebook,
            },
            TestCase {
                url: "https://www.tiktok.com/@someone/video/123456789",
                expected: Source::TikTok,
            },
            TestCase {
                url: "tiktok:@someone/video/123456789",
                expected: Source::TikTok,
            },
            TestCase {
                url: "https://www.reddit.com/r/videos/comments/abc123/example/",
                expected: Source::Reddit,
            },
            TestCase {
                url: "reddit:r/videos/comments/abc123/example",
                expected: Source::Reddit,
            },
            TestCase {
                url: "https://www.snapchat.com/discover/some-story/1234567890",
                expected: Source::Snapchat,
            },
            TestCase {
                url: "snapchat:discover/some-story/1234567890",
                expected: Source::Snapchat,
            },
        ];

        for case in &social_cases {
            assert_eq!(
                determine_source(case.url),
                case.expected,
                "Failed for URL: {}",
                case.url
            );
        }
    }

    #[test]
    fn test_non_youtube_sources() {
        let other_cases = [
            TestCase {
                url: "file:///local/path/file.mp4",
                expected: Source::Local,
            },
            TestCase {
                url: "https://example.com/",
                expected: Source::Url,
            },
            TestCase {
                url: "https://example.com/?redirect=instagram.com/reel/ABC123",
                expected: Source::Url,
            },
            TestCase {
                url: "https://notfacebook.com/watch?v=123456",
                expected: Source::Url,
            },
        ];

        for case in &other_cases {
            assert_eq!(
                determine_source(case.url),
                case.expected,
                "Failed for URL: {}",
                case.url
            );
        }
    }

    #[test]
    fn test_existing_local_path_source() {
        let path = env::current_dir().unwrap().join("Cargo.toml");
        assert_eq!(
            determine_source(path.to_str().unwrap()),
            Source::Local,
            "existing filesystem paths should be archived as local files"
        );
    }

    #[test]
    fn test_initialize_store_directories() {
        let store_path = env::temp_dir().join(format!(
            "archivr-test-{}",
            Local::now().format("%Y%m%d%H%M%S%3f")
        ));

        archive::initialize_store_directories(&store_path).unwrap();

        assert!(store_path.join("raw").is_dir());
        assert!(store_path.join("raw_tweets").is_dir());
        assert!(store_path.join("structured").is_dir());
        assert!(store_path.join("temp").is_dir());
        assert!(!store_path.join("tmp").exists());

        fs::remove_dir_all(store_path).unwrap();
    }

    #[test]
    fn test_record_tweet_entry_links_json_and_raw_artifacts() {
        let store_path = env::temp_dir().join(format!(
            "archivr-tweet-db-test-{}",
            Local::now().format("%Y%m%d%H%M%S%3f")
        ));
        let _ = fs::remove_dir_all(&store_path);
        archive::initialize_store_directories(&store_path).unwrap();
        fs::create_dir_all(store_path.join("raw").join("a").join("b")).unwrap();
        fs::create_dir_all(store_path.join("raw").join("c").join("d")).unwrap();
        fs::write(
            store_path
                .join("raw")
                .join("a")
                .join("b")
                .join("abcdef.jpg"),
            b"avatar",
        )
        .unwrap();
        fs::write(
            store_path
                .join("raw")
                .join("c")
                .join("d")
                .join("cdef01.mp4"),
            b"media",
        )
        .unwrap();
        fs::write(
            store_path.join("raw_tweets").join("tweet-123.json"),
            r#"{
  "author": { "avatar_local_path": "raw/a/b/abcdef.jpg" },
  "entities": { "media": [{ "local_path": "raw/c/d/cdef01.mp4" }] }
}"#,
        )
        .unwrap();

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        database::initialize_schema(&conn).unwrap();
        let user_id = database::ensure_default_user(&conn).unwrap();
        let run = database::create_archive_run(&conn, user_id, 1).unwrap();
        let item = database::create_archive_run_item(
            &conn,
            run.id,
            None,
            0,
            "tweet:123",
            None,
            "x",
            "tweet",
        )
        .unwrap();

        let entry = record_tweet_entry(
            &conn,
            &store_path,
            user_id,
            &run,
            &item,
            "tweet:123",
            Source::Tweet,
            "123",
        )
        .unwrap();
        database::finish_archive_run(&conn, run.id).unwrap();

        let artifact_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM entry_artifacts WHERE entry_id = ?1",
                [entry.id],
                |row| row.get(0),
            )
            .unwrap();
        let blob_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM blobs", [], |row| row.get(0))
            .unwrap();
        let run_status: String = conn
            .query_row(
                "SELECT status FROM archive_runs WHERE id = ?1",
                [run.id],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(artifact_count, 3);
        assert_eq!(blob_count, 2);
        assert_eq!(run_status, "completed");
        assert!(store_path.join(&entry.structured_root_relpath).is_dir());

        let _ = fs::remove_dir_all(store_path);
    }

    mod title_tests {
        use super::*;

        fn meta(
            author: Option<&str>,
            title: Option<&str>,
            caption: Option<&str>,
            subreddit: Option<&str>,
            post_author: Option<&str>,
        ) -> PlatformMetadata {
            PlatformMetadata {
                author: author.map(str::to_string),
                title: title.map(str::to_string),
                caption: caption.map(str::to_string),
                subreddit: subreddit.map(str::to_string),
                post_author: post_author.map(str::to_string),
            }
        }

        #[test]
        fn youtube_video_uses_title() {
            let m = meta(None, Some("How to Rust"), None, None, None);
            assert_eq!(generate_entry_title(Source::YouTubeVideo, &m), "How to Rust");
        }

        #[test]
        fn youtube_video_fallback() {
            let m = meta(None, None, None, None, None);
            assert_eq!(generate_entry_title(Source::YouTubeVideo, &m), "YouTube Video");
        }

        #[test]
        fn youtube_playlist_uses_title() {
            let m = meta(None, Some("Rust Tutorial Series"), None, None, None);
            assert_eq!(generate_entry_title(Source::YouTubePlaylist, &m), "Rust Tutorial Series");
        }

        #[test]
        fn youtube_channel_uses_author() {
            let m = meta(Some("Rust By Example"), None, None, None, None);
            assert_eq!(generate_entry_title(Source::YouTubeChannel, &m), "Archival of Rust By Example");
        }

        #[test]
        fn x_media_uses_author() {
            let m = meta(Some("alice"), None, None, None, None);
            assert_eq!(generate_entry_title(Source::X, &m), "X Media by alice");
        }

        #[test]
        fn tweet_uses_excerpt_and_author() {
            let m = meta(Some("alice"), None, Some("Hello world"), None, None);
            assert_eq!(generate_entry_title(Source::Tweet, &m), "Hello world \u{2014} @alice");
        }

        #[test]
        fn tweet_truncates_long_caption() {
            let long = "a".repeat(150);
            let m = meta(Some("bob"), None, Some(&long), None, None);
            let title = generate_entry_title(Source::Tweet, &m);
            assert!(title.starts_with(&"a".repeat(100)));
            assert!(title.contains("..."));
            assert!(title.ends_with("\u{2014} @bob"));
        }

        #[test]
        fn tweet_thread_uses_author() {
            let m = meta(Some("bob"), None, None, None, None);
            assert_eq!(generate_entry_title(Source::TweetThread, &m), "Thread by @bob");
        }

        #[test]
        fn instagram_uses_author() {
            let m = meta(Some("photographer"), None, None, None, None);
            assert_eq!(generate_entry_title(Source::Instagram, &m), "Post by @photographer");
        }

        #[test]
        fn facebook_uses_author_no_at() {
            let m = meta(Some("John Doe"), None, None, None, None);
            assert_eq!(generate_entry_title(Source::Facebook, &m), "Post by John Doe");
        }

        #[test]
        fn tiktok_uses_author() {
            let m = meta(Some("dancemaster"), None, None, None, None);
            assert_eq!(generate_entry_title(Source::TikTok, &m), "TikTok by @dancemaster");
        }

        #[test]
        fn reddit_full_fields() {
            let m = meta(None, Some("My first Rust project"), None, Some("rust"), Some("newbie"));
            assert_eq!(
                generate_entry_title(Source::Reddit, &m),
                "My first Rust project \u{2014} r/rust (u/newbie)"
            );
        }

        #[test]
        fn snapchat_uses_author() {
            let m = meta(Some("snapuser"), None, None, None, None);
            assert_eq!(generate_entry_title(Source::Snapchat, &m), "Snap by snapuser");
        }

        #[test]
        fn local_uses_title_field() {
            let m = meta(None, Some("document.pdf"), None, None, None);
            assert_eq!(generate_entry_title(Source::Local, &m), "document.pdf");
        }

        #[test]
        fn all_none_falls_back() {
            let m = meta(None, None, None, None, None);
            assert!(!generate_entry_title(Source::Instagram, &m).is_empty());
        }

        #[test]
        fn tweet_title_extracted_from_json() {
            let json = r#"{
                "full_text": "Hello Rust world, this is a test tweet",
                "author": { "screen_name": "rustacean", "name": "The Rustacean" }
            }"#;
            let meta = tweet_metadata_from_json(json);
            assert_eq!(meta.author, Some("rustacean".to_string()));
            assert_eq!(meta.caption, Some("Hello Rust world, this is a test tweet".to_string()));
        }
    }

    #[test]
    fn source_metadata_webpage() {
        let (kind, entity, _) = source_metadata(Source::WebPage);
        assert_eq!(kind, "web");
        assert_eq!(entity, "page");
    }

    #[test]
    fn generate_entry_title_webpage_with_title() {
        let mut meta = PlatformMetadata::default();
        meta.title = Some("Paul Graham \u{2014} Great Work".to_string());
        assert_eq!(
            generate_entry_title(Source::WebPage, &meta),
            "Paul Graham \u{2014} Great Work"
        );
    }

    #[test]
    fn generate_entry_title_webpage_fallback() {
        let meta = PlatformMetadata::default();
        assert_eq!(
            generate_entry_title(Source::WebPage, &meta),
            "Archived Web Page"
        );
    }

}
