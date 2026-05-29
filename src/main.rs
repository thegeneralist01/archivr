use anyhow::{Context, Result};
use chrono::Local;
use clap::{Parser, Subcommand};
use serde_json::json;
use std::{
    collections::HashSet,
    env, fs,
    path::{Path, PathBuf},
    process,
};

mod database;
mod downloader;
mod hash;
mod twitter;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Archive the specified file or directory
    Archive {
        /// URL or Path to archive
        path: String,
    },
    Init {
        /// Path to initialize the archive in
        #[arg(default_value = ".")]
        path: String,

        /// Store path - path to store the archived files in.
        /// Structure will be:
        /// store_path/
        ///   temp/
        ///     ...
        ///   raw/
        ///     ...
        ///   raw_tweets/
        ///     ...
        ///   structured/
        ///     ...
        #[arg(default_value = "./.archivr/store")]
        store_path: String,

        /// Name of the archive
        #[arg(short, long)]
        name: String,

        /// Wipe existing .archivr repository data
        #[arg(long = "force-with-info-removal")]
        force_with_info_removal: bool,
    },
}

fn get_archive_path() -> Option<PathBuf> {
    let mut dir = env::current_dir().unwrap();
    loop {
        if dir.join(".archivr").is_dir() {
            return Some(dir.join(".archivr"));
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Source {
    YouTubeVideo,
    YouTubePlaylist,
    YouTubeChannel,
    X,
    Tweet,
    TweetThread,
    Instagram,
    Facebook,
    TikTok,
    Reddit,
    Snapchat,
    Local,
    Other,
}

use crate::twitter::parse_tweet_id;

fn expand_shorthand_to_url(path: &str, source: &Source) -> String {
    if *source == Source::X && (path.starts_with("tweet:media:") || path.starts_with("x:media:")) {
        return format!(
            "https://x.com/i/status/{}",
            path.split(':')
                .next_back()
                .and_then(parse_tweet_id)
                .unwrap()
        );
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
        let video_re = regex::Regex::new(r"^https?://(?:www\.)?(?:youtu\.be/[0-9A-Za-z_-]+|youtube\.com/watch\?v=[0-9A-Za-z_-]+|youtube\.com/shorts/[0-9A-Za-z_-]+)").unwrap();
        if video_re.is_match(path) {
            return Source::YouTubeVideo;
        }

        // Playlist URLs
        let playlist_re =
            regex::Regex::new(r"^https?://(?:www\.)?youtube\.com/playlist\?list=[0-9A-Za-z_-]+")
                .unwrap();
        if playlist_re.is_match(path) {
            return Source::YouTubePlaylist;
        }

        // Channel or user URLs (channel IDs, /c/, /user/, or @handles)
        let channel_re = regex::Regex::new(r"^https?://(?:www\.)?youtube\.com/(?:channel/[0-9A-Za-z_-]+|c/[0-9A-Za-z_-]+|user/[0-9A-Za-z_-]+|@[0-9A-Za-z_-]+)").unwrap();
        if channel_re.is_match(path) {
            return Source::YouTubeChannel;
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
    }
    Source::Other
}

fn hash_exists(filename: String, store_path: &Path) -> bool {
    let mut chars = filename.chars();
    let first_letter = chars.next().unwrap();
    let second_letter = chars.next().unwrap();

    let path = store_path
        .join("raw")
        .join(first_letter.to_string())
        .join(second_letter.to_string())
        .join(filename);

    println!("Checking {}", path.display());

    path.exists()
}

fn move_temp_to_raw(file: &Path, hash: &String, store_path: &Path) -> Result<()> {
    let mut chars = hash.chars();
    let first_letter = chars.next().unwrap().to_string();
    let second_letter = chars.next().unwrap().to_string();
    let file_extension = file
        .extension()
        .map_or(String::new(), |ext| format!(".{}", ext.to_string_lossy()));

    fs::create_dir_all(
        store_path
            .join("raw")
            .join(&first_letter)
            .join(&second_letter),
    )?;

    fs::rename(
        file,
        store_path
            .join("raw")
            .join(&first_letter)
            .join(&second_letter)
            .join(format!(
                "{hash}{}",
                if file_extension.is_empty() {
                    ""
                } else {
                    &file_extension
                }
            )),
    )?;

    Ok(())
}

fn initialize_store_directories(store_path: &Path) -> Result<()> {
    fs::create_dir_all(store_path.join("raw"))?;
    fs::create_dir_all(store_path.join("raw_tweets"))?;
    fs::create_dir_all(store_path.join("structured"))?;
    fs::create_dir_all(store_path.join("temp"))?;
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
        Source::X => ("x", "post", "video"),
        Source::Tweet => ("x", "tweet", "tweet_json"),
        Source::TweetThread => ("x", "tweet_thread", "tweet_json"),
        Source::Instagram => ("instagram", "post", "video"),
        Source::Facebook => ("facebook", "post", "video"),
        Source::TikTok => ("tiktok", "video", "video"),
        Source::Reddit => ("reddit", "post", "video"),
        Source::Snapchat => ("snapchat", "story", "video"),
        Source::Local => ("local", "file", "file"),
        Source::Other => ("other", "unknown", "unknown"),
    }
}

fn local_file_extension(path: &str) -> String {
    Path::new(path.trim_start_matches("file://"))
        .extension()
        .map_or(String::new(), |ext| format!(".{}", ext.to_string_lossy()))
}

fn media_file_extension(source: Source, path: &str) -> String {
    match source {
        Source::YouTubeVideo
        | Source::X
        | Source::Instagram
        | Source::Facebook
        | Source::TikTok
        | Source::Reddit
        | Source::Snapchat => ".mp4".to_string(),
        Source::Local => local_file_extension(path),
        _ => String::new(),
    }
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
            title: None,
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
    database::finish_archive_run(conn, run.id)?;
    Ok(entry)
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
            title: None,
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

    let tweet_json_relpath = PathBuf::from("raw_tweets").join(format!("tweet-{tweet_id}.json"));
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

    let tweet_json = fs::read_to_string(store_path.join(&tweet_json_relpath))?;
    for (role, raw_relpath) in tweet_raw_artifacts(&tweet_json) {
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
    database::finish_archive_run(conn, run.id)?;
    Ok(entry)
}

fn tweet_raw_artifacts(tweet_json: &str) -> Vec<(String, String)> {
    let regex = regex::Regex::new(r#""(avatar_local_path|local_path)": "([^"\n]+)""#).unwrap();
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

    artifacts
}

fn fail_archive_and_exit(
    conn: &rusqlite::Connection,
    run: &database::ArchiveRun,
    item: &database::ArchiveRunItem,
    message: &str,
) -> ! {
    let _ = database::fail_archive_run_item(conn, item.id, message);
    let _ = database::fail_archive_run(conn, run.id, message);
    eprintln!("{message}");
    process::exit(1);
}

fn main() -> Result<()> {
    let args = Args::parse();

    match args.command {
        Command::Archive { ref path } => {
            let archive_path = match get_archive_path() {
                Some(path) => path,
                None => {
                    eprintln!("Not in an archive. Use 'archivr init' to create one.");
                    process::exit(1);
                }
            };

            // let download_id = uuid::Uuid::new_v4();
            let timestamp = Local::now().format("%Y-%m-%dT%H-%M-%S%.3f").to_string();

            let store_path_string_file = archive_path.join("store_path");
            let store_path = match fs::read_to_string(store_path_string_file) {
                Ok(p) => PathBuf::from(p.trim()),
                Err(e) => {
                    eprintln!("Failed to read store path: {e}");
                    process::exit(1);
                }
            };

            let source = determine_source(path);
            let (source_kind, entity_kind, _) = source_metadata(source);
            let conn = database::open_or_initialize(&archive_path)?;
            let user_id = database::ensure_default_user(&conn)?;
            let run = database::create_archive_run(&conn, user_id, 1)?;
            let item = database::create_archive_run_item(
                &conn,
                run.id,
                None,
                0,
                path,
                None,
                source_kind,
                entity_kind,
            )?;

            // Sources: Tweets or Twitter Threads
            match source {
                Source::Other => {
                    fail_archive_and_exit(
                        &conn,
                        &run,
                        &item,
                        "Archiving from this source is not yet implemented.",
                    );
                }
                Source::Tweet | Source::TweetThread => {
                    let tweet_id = match tweet_id_from_archive_path(path) {
                        Some(tweet_id) => tweet_id,
                        None => fail_archive_and_exit(
                            &conn,
                            &run,
                            &item,
                            "Failed to archive tweet: invalid tweet ID",
                        ),
                    };

                    match downloader::tweets::archive(
                        path,
                        source == Source::TweetThread,
                        &store_path,
                        &timestamp,
                    ) {
                        Ok(true) => {
                            record_tweet_entry(
                                &conn,
                                &store_path,
                                user_id,
                                &run,
                                &item,
                                path,
                                source,
                                &tweet_id,
                            )?;
                            println!(
                                "Tweet archived successfully to {}",
                                store_path.join("raw_tweets").display()
                            );
                            return Ok(());
                        }
                        Ok(false) => {
                            record_tweet_entry(
                                &conn,
                                &store_path,
                                user_id,
                                &run,
                                &item,
                                path,
                                source,
                                &tweet_id,
                            )?;
                            println!(
                                "Tweet already archived in {}",
                                store_path.join("raw_tweets").display()
                            );
                            return Ok(());
                        }
                        Err(e) => {
                            fail_archive_and_exit(
                                &conn,
                                &run,
                                &item,
                                &format!("Failed to archive tweet: {e}"),
                            );
                        }
                    }
                }
                _ => {}
            }

            // Sources, for which yt-dlp is needed
            let requested_path = path.to_string();
            let path = expand_shorthand_to_url(path, &source);
            let hash = match source {
                Source::YouTubeVideo
                | Source::X
                | Source::Instagram
                | Source::Facebook
                | Source::TikTok
                | Source::Reddit
                | Source::Snapchat => {
                    match downloader::ytdlp::download(path.clone(), &store_path, &timestamp) {
                        Ok(h) => h,
                        Err(e) => {
                            fail_archive_and_exit(
                                &conn,
                                &run,
                                &item,
                                &format!("Failed to download media: {e}"),
                            );
                        }
                    }
                }
                Source::Local => {
                    match downloader::local::save(path.clone(), &store_path, &timestamp) {
                        Ok(h) => h,
                        Err(e) => {
                            fail_archive_and_exit(
                                &conn,
                                &run,
                                &item,
                                &format!("Failed to archive local file: {e}"),
                            );
                        }
                    }
                }
                Source::YouTubePlaylist | Source::YouTubeChannel => {
                    fail_archive_and_exit(
                        &conn,
                        &run,
                        &item,
                        "Playlist and channel container expansion are not yet implemented.",
                    );
                }
                _ => unreachable!(),
            };

            let file_extension = media_file_extension(source, &path);
            let temp_file = store_path
                .join("temp")
                .join(&timestamp)
                .join(format!("{timestamp}{file_extension}"));
            let byte_size = fs::metadata(&temp_file)
                .with_context(|| format!("failed to stat staged file {}", temp_file.display()))?
                .len() as i64;

            let hash_exists = hash_exists(format!("{hash}{file_extension}"), &store_path);

            // TODO: check for repeated archives?
            // There could be one of the following:
            // - We are literally archiving the same path over again.
            // - We are archiving a different path, which had this file. E.g.: we archived a
            // website before which had this YouTube video, and while recursively archiving
            // everything, we also archived the YouTube video although it wasn't our main
            // target. This means that we should archive again; whereas with the first case...
            // Not sure. Need to think about this.
            // ----
            // Thinking about it a day later...
            // If we are specifically archiving a YouTube video, it could also be two of the
            // above. So yeah, just create a new DB entry and symlink the Raw to the Structured
            // Dir or whatever. it's midnight and my brain ain't wording/braining.
            if hash_exists {
                println!("File already archived.");
                let _ = fs::remove_dir_all(store_path.join("temp").join(&timestamp));
            } else {
                move_temp_to_raw(
                    &store_path
                        .join("temp")
                        .join(&timestamp)
                        .join(format!("{timestamp}{file_extension}")),
                    &hash,
                    &store_path,
                )?;
                let _ = fs::remove_dir_all(store_path.join("temp").join(&timestamp));

                println!("File archived successfully.");
            }

            record_media_entry(
                &conn,
                &store_path,
                user_id,
                &run,
                &item,
                &requested_path,
                &path,
                source,
                &hash,
                &file_extension,
                byte_size,
            )?;

            Ok(())
        }

        Command::Init {
            path: ref archive_path_string,
            store_path: ref store_path_string,
            name: ref archive_name,
            force_with_info_removal,
        } => {
            let archive_path = Path::new(&archive_path_string).join(".archivr");
            let store_path = if Path::new(&store_path_string).is_relative() {
                env::current_dir().unwrap().join(store_path_string)
            } else {
                Path::new(store_path_string).to_path_buf()
            };

            if archive_path.exists() {
                if !archive_path.is_dir() {
                    eprintln!(
                        "Archive path exists and is not a directory: {}",
                        archive_path.display()
                    );
                    process::exit(1);
                }

                if force_with_info_removal {
                    fs::remove_dir_all(&archive_path)?;
                } else if fs::read_dir(&archive_path)?.next().is_some() {
                    eprintln!(
                        "Archive already exists at {} and is not empty. Use --force-with-info-removal to reinitialize.",
                        archive_path.display()
                    );
                    process::exit(1);
                }
            }

            if store_path.exists() && !force_with_info_removal {
                eprintln!("Store path already exists at {}", store_path.display());
                process::exit(1);
            }

            fs::create_dir_all(&archive_path).unwrap();
            fs::create_dir_all(&store_path).unwrap();
            fs::write(archive_path.join("name"), archive_name).unwrap();
            let _ = fs::write(
                archive_path.join("store_path"),
                store_path.canonicalize().unwrap().to_str().unwrap(),
            );
            initialize_store_directories(&store_path).unwrap();
            let conn = database::open_or_initialize(&archive_path)?;
            let _ = database::ensure_default_user(&conn)?;

            println!("Initialized empty archive in {}", archive_path.display());

            Ok(())
        } // _ => eprintln!("Unknown command: {:?}", args.command),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

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
                expected: Source::Other,
            },
            TestCase {
                url: "https://example.com/?redirect=instagram.com/reel/ABC123",
                expected: Source::Other,
            },
            TestCase {
                url: "https://notfacebook.com/watch?v=123456",
                expected: Source::Other,
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
    fn test_initialize_store_directories() {
        let store_path = env::temp_dir().join(format!(
            "archivr-test-{}",
            Local::now().format("%Y%m%d%H%M%S%3f")
        ));

        initialize_store_directories(&store_path).unwrap();

        assert!(store_path.join("raw").is_dir());
        assert!(store_path.join("raw_tweets").is_dir());
        assert!(store_path.join("structured").is_dir());
        assert!(store_path.join("temp").is_dir());
        assert!(!store_path.join("tmp").exists());

        fs::remove_dir_all(store_path).unwrap();
    }
}
