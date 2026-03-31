use anyhow::{Context, Result, bail};
use std::{
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TweetArchiveMode {
    Tweet,
    Thread,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TweetArchiveRequest {
    pub tweet_id: String,
    pub mode: TweetArchiveMode,
}

fn build_scraper_args(
    request: &TweetArchiveRequest,
    output_dir: &Path,
    credentials_file: &Path,
) -> Vec<String> {
    let mut args = vec![
        "--tweet-ids".to_string(),
        request.tweet_id.clone(),
        "--output-dir".to_string(),
        output_dir.display().to_string(),
        "--media-dir".to_string(),
        output_dir.join("media").display().to_string(),
        "--no-download-avatars".to_string(),
        "--credentials-file".to_string(),
        credentials_file.display().to_string(),
    ];

    match request.mode {
        TweetArchiveMode::Tweet => {
            args.push("--no-recursive".to_string());
        }
        TweetArchiveMode::Thread => {
            args.push("--recursive-replied-to-tweets".to_string());
            args.push("--recursive-replied-to-tweets-quotes-retweets".to_string());
        }
    }

    args
}

pub fn archive(
    request: &TweetArchiveRequest,
    store_path: &Path,
    timestamp: &str,
) -> Result<PathBuf> {
    let output_dir = store_path.join("raw_tweets").join(timestamp);
    let temp_dir = store_path.join("temp").join(timestamp);
    fs::create_dir_all(&output_dir)?;
    fs::create_dir_all(&temp_dir)?;

    let python = env::var_os("ARCHIVR_TWEET_PYTHON").unwrap_or_else(|| OsString::from("python3"));
    let scraper_path = env::var_os("ARCHIVR_TWEET_SCRAPER")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("vendor/twitter/scrape_user_tweet_contents.py"));

    let credentials_file = if let Some(credentials_file) =
        env::var_os("ARCHIVR_TWITTER_CREDENTIALS_FILE")
    {
        PathBuf::from(credentials_file)
    } else {
        bail!(
            "Twitter scraping requires ARCHIVR_TWITTER_CREDENTIALS_FILE to point to a cookies file."
        );
    };

    let mut cmd = Command::new(&python);
    cmd.current_dir(&temp_dir).arg(&scraper_path);
    for arg in build_scraper_args(request, &output_dir, &credentials_file) {
        cmd.arg(arg);
    }

    let output = cmd.output().with_context(|| {
        format!(
            "Failed to spawn tweet scraper at {}",
            scraper_path.display()
        )
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        bail!(
            "Tweet scraper failed.\nstdout:\n{}\nstderr:\n{}",
            stdout.trim(),
            stderr.trim()
        );
    }

    let root_toml = output_dir.join(format!("tweet-{}.toml", request.tweet_id));
    if !root_toml.exists() {
        bail!(
            "Tweet scraper completed but did not create expected TOML file: {}",
            root_toml.display()
        );
    }

    let _ = fs::remove_dir_all(&temp_dir);

    Ok(output_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_scraper_args_for_single_tweet() {
        let args = build_scraper_args(
            &TweetArchiveRequest {
                tweet_id: "1234567890".to_string(),
                mode: TweetArchiveMode::Tweet,
            },
            Path::new("/tmp/raw_tweets/test"),
            Path::new("/tmp/twitter-creds.txt"),
        );

        assert!(args.contains(&"--tweet-ids".to_string()));
        assert!(args.contains(&"1234567890".to_string()));
        assert!(args.contains(&"--output-dir".to_string()));
        assert!(args.contains(&"--credentials-file".to_string()));
        assert!(args.contains(&"--no-recursive".to_string()));
        assert!(!args.contains(&"--recursive-replied-to-tweets".to_string()));
        assert!(!args.contains(&"--recursive-replied-to-tweets-quotes-retweets".to_string()));
    }

    #[test]
    fn test_build_scraper_args_for_thread() {
        let args = build_scraper_args(
            &TweetArchiveRequest {
                tweet_id: "1234567890".to_string(),
                mode: TweetArchiveMode::Thread,
            },
            Path::new("/tmp/raw_tweets/test"),
            Path::new("/tmp/twitter-creds.txt"),
        );

        assert!(args.contains(&"--recursive-replied-to-tweets".to_string()));
        assert!(args.contains(&"--recursive-replied-to-tweets-quotes-retweets".to_string()));
        assert!(!args.contains(&"--no-recursive".to_string()));
    }
}
