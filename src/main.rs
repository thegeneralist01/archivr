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

#[derive(Debug, PartialEq)]
enum Source {
    YouTubeVideo,
    YouTubePlaylist,
    YouTubeChannel,
    X,
    Local,
    Other,
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

    // Shorthand schemes: x: or twitter:
    if path.starts_with("x:") || path.starts_with("twitter:") {
        return Source::X;
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

fn main() -> Result<()> {
    let args = Args::parse();

    match args.command {
        Command::Archive { ref path } => {
            let archive_path = get_archive_path();
            if get_archive_path().is_none() {
                eprintln!("Not in an archive. Use 'archivr init' to create one.");
                process::exit(1);
            }

            // let download_id = uuid::Uuid::new_v4();
            let timestamp = Local::now().format("%Y-%m-%dT%H-%M-%S%.3f").to_string();

            let source = determine_source(path);
            if let Source::Other = source {
                eprintln!("Archiving from this source is not yet implemented.");
                process::exit(1);
            }

            let store_path_string_file = archive_path.unwrap().join("store_path");
            let store_path = match fs::read_to_string(store_path_string_file) {
                Ok(p) => PathBuf::from(p.trim()),
                Err(e) => {
                    eprintln!("Failed to read store path: {e}");
                    process::exit(1);
                }
            };

            let hash = match source {
                Source::YouTubeVideo | Source::X => {
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
                Source::YouTubeVideo | Source::X => ".mp4",
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
                let _ = fs::remove_file(store_path.join("temp").join(&timestamp));
            } else {
                move_temp_to_raw(
                    &store_path
                        .join("temp")
                        .join(&timestamp)
                        .join(format!("{timestamp}{file_extension}")),
                    &hash,
                    &store_path,
                )?;
                let _ = fs::remove_file(store_path.join("temp").join(&timestamp));

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
            fs::create_dir_all(store_path.join("raw")).unwrap();
            fs::create_dir_all(store_path.join("structured")).unwrap();
            fs::create_dir_all(store_path.join("tmp")).unwrap();

            println!("Initialized empty archive in {}", archive_path.display());

            Ok(())
        } // _ => eprintln!("Unknown command: {:?}", args.command),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestCase<'a> {
        url: &'a str,
        expected: Source,
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
}
