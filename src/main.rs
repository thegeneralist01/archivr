use anyhow::Result;
use chrono::Local;
use clap::{Parser, Subcommand};
use std::{
    env, fs,
    path::{Path, PathBuf},
    process,
};

mod downloader;
mod hash;

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

fn parse_tweet_id(id: &str) -> Option<String> {
    if !id.is_empty() && id.chars().all(|char| char.is_ascii_digit()) {
        Some(id.to_string())
    } else {
        None
    }
}

fn tweet_id_from_path(path: &str) -> Option<String> {
    path.split(':').next_back().and_then(parse_tweet_id)
}

fn resolve_source_path(path: &str, source: &Source) -> String {
    if *source == Source::X && path.starts_with("tweet:media:") {
        format!(
            "https://x.com/i/status/{}",
            tweet_id_from_path(path).unwrap()
        )
    } else {
        path.to_string()
    }
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
    if let Some(after_scheme) = path.strip_prefix("tweet:") {
        if after_scheme.starts_with("media:")
            && after_scheme
                .strip_prefix("media:")
                .and_then(parse_tweet_id)
                .is_some()
        {
            return Source::X;
        }

        if parse_tweet_id(after_scheme).is_some() {
            return Source::Tweet;
        }
    }

    if let Some(after_scheme) = path
        .strip_prefix("x:")
        .or_else(|| path.strip_prefix("twitter:"))
    {
        if after_scheme
            .strip_prefix("thread:")
            .and_then(parse_tweet_id)
            .is_some()
        {
            return Source::TweetThread;
        }

        if after_scheme
            .strip_prefix("tweet:")
            .or_else(|| after_scheme.strip_prefix("x:"))
            .and_then(parse_tweet_id)
            .is_some()
        {
            return Source::Tweet;
        }

        return Source::X;
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

            // Sources: Tweets or Twitter Threads
            match source {
                Source::Other => {
                    eprintln!("Archiving from this source is not yet implemented.");
                    process::exit(1);
                }
                Source::Tweet | Source::TweetThread => {
                    match downloader::tweets::archive(
                        path,
                        source == Source::TweetThread,
                        &store_path,
                        &timestamp,
                    ) {
                        Ok(true) => {
                            println!(
                                "Tweet archived successfully to {}",
                                store_path.join("raw_tweets").display()
                            );
                            return Ok(());
                        }
                        Ok(false) => {
                            println!(
                                "Tweet already archived in {}",
                                store_path.join("raw_tweets").display()
                            );
                            return Ok(());
                        }
                        Err(e) => {
                            eprintln!("Failed to archive tweet: {e}");
                            process::exit(1);
                        }
                    }
                }
                _ => {}
            }

            // Sources, for which yt-dlp is needed
            let path = resolve_source_path(path, &source);
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
                            eprintln!("Failed to download from YouTube: {e}");
                            process::exit(1);
                        }
                    }
                }
                Source::Local => {
                    match downloader::local::save(path.clone(), &store_path, &timestamp) {
                        Ok(h) => h,
                        Err(e) => {
                            eprintln!("Failed to archive local file: {e}");
                            process::exit(1);
                        }
                    }
                }
                _ => unreachable!(),
            };

            let file_extension = match source {
                Source::YouTubeVideo
                | Source::X
                | Source::Instagram
                | Source::Facebook
                | Source::TikTok
                | Source::Reddit
                | Source::Snapchat => ".mp4",
                Source::Local => {
                    let p = Path::new(path.trim_start_matches("file://"));
                    &p.extension()
                        .map_or(String::new(), |ext| format!(".{}", ext.to_string_lossy()))
                }
                _ => "",
            };

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

            // TODO: DB INSERT, inserting a record
            // https://github.com/rusqlite/rusqlite
            // Think of the DB schema

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
                url: "x:thread:1234567890",
                expected: Source::TweetThread,
            },
            TestCase {
                url: "twitter:thread:1234567890",
                expected: Source::TweetThread,
            },
            TestCase {
                url: "tweet:thread:1234567890",
                expected: Source::Other,
            },
            TestCase {
                url: "tweet:not-a-number",
                expected: Source::Other,
            },
            TestCase {
                url: "tweet:media:not-a-number",
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
    fn test_tweet_id_from_path() {
        assert_eq!(
            tweet_id_from_path("tweet:1234567890"),
            Some("1234567890".to_string())
        );
        assert_eq!(
            tweet_id_from_path("tweet:media:1234567890"),
            Some("1234567890".to_string())
        );
        assert_eq!(
            tweet_id_from_path("x:thread:1234567890"),
            Some("1234567890".to_string())
        );
        assert_eq!(tweet_id_from_path("tweet:not-a-number"), None);
    }

    #[test]
    fn test_resolve_source_path() {
        assert_eq!(
            resolve_source_path("tweet:media:1234567890", &Source::X),
            "https://x.com/i/status/1234567890"
        );
        assert_eq!(
            resolve_source_path("tweet:1234567890", &Source::Tweet),
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
                expected: Source::X,
            },
            TestCase {
                url: "twitter:1234567890",
                expected: Source::X,
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
