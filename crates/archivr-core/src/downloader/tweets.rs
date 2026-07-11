use anyhow::{Context, Result, bail};
use regex::Regex;
use std::{
    collections::HashMap,
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::OnceLock,
};

use crate::{downloader::store, twitter::parse_tweet_id};

/// Extracts a tweet ID from an archivr path like `"tweet:123"` by taking the
/// last colon-separated segment and validating it as a numeric ID.
fn tweet_id_from_path(path: &str) -> Option<String> {
    path.split(':').next_back().and_then(parse_tweet_id)
}

/// Resolves `path` relative to `cwd` if it is not already absolute.
fn absolutize_path_from_cwd(path: PathBuf, cwd: &Path) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    }
}

/// Builds the CLI argument list for the Python tweet scraper.
/// When `thread` is true, recursive flags are added to follow reply chains.
fn build_scraper_args(
    tweet_id: &str,
    thread: bool,
    output_dir: &Path,
    temp_dir: &Path,
    credentials_file: &Path,
) -> Vec<String> {
    let mut args = vec![
        "--tweet-ids".to_string(),
        tweet_id.to_string(),
        "--output-dir".to_string(),
        output_dir.display().to_string(),
        "--media-dir".to_string(),
        temp_dir.join("media").display().to_string(),
        "--download-media".to_string(),
        "--credentials-file".to_string(),
        credentials_file.display().to_string(),
    ];

    if thread {
        args.push("--recursive-replied-to-tweets".to_string());
        args.push("--recursive-replied-to-tweets-quotes-retweets".to_string());
        args.push("--download-replied-to-tweets-media".to_string());
    } else {
        args.push("--no-recursive".to_string());
    }

    args
}

/// Runs the scraper into a staging dir, rewrites assets into raw/, moves tweet JSONs
/// to `raw_tweets/`, and returns their store-relative relpaths (e.g. `"raw_tweets/tweet-123.json"`).
/// The staging dir starts empty so all files found there are exactly the touched set.
fn run_scraper_staged(
    tweet_id: &str,
    thread: bool,
    store_path: &Path,
    timestamp: &str,
    cookies: &HashMap<String, String>,
) -> Result<Vec<String>> {
    let invocation_cwd = env::current_dir().context("Failed to read current working directory")?;

    // staging_dir = store_path/temp/{timestamp}/tweet_stage/
    // staging_tweets_dir = staging_dir/raw_tweets/  ← scraper writes JSONs here
    // Scraper media-dir = staging_dir/media/
    let staging_dir = store_path.join("temp").join(timestamp).join("tweet_stage");
    let staging_tweets_dir = staging_dir.join("raw_tweets");
    fs::create_dir_all(&staging_tweets_dir)?;

    // Final destination for tweet JSONs.
    let output_dir = store_path.join("raw_tweets");
    fs::create_dir_all(&output_dir)?;

    let python = env::var_os("ARCHIVR_TWEET_PYTHON").unwrap_or_else(|| OsString::from("python3"));
    let scraper_path = env::var_os("ARCHIVR_TWEET_SCRAPER")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("vendor/twitter/scrape_user_tweet_contents.py"));
    let scraper_path = absolutize_path_from_cwd(scraper_path, &invocation_cwd);
    let temp_creds_path: Option<PathBuf>;
    let credentials_file: PathBuf;
    let has_twitter_cookies = cookies.contains_key("ct0") && cookies.contains_key("auth_token");
    if has_twitter_cookies {
        let cf = store_path.join("temp").join(timestamp).join("twitter-creds.txt");
        let creds_str = cookies
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(";");
        {
            use std::io::Write;
            #[cfg(unix)]
            let mut f = {
                use std::os::unix::fs::OpenOptionsExt;
                std::fs::OpenOptions::new()
                    .write(true).create(true).truncate(true).mode(0o600)
                    .open(&cf)
                    .context("failed to write twitter credentials file")?
            };
            #[cfg(not(unix))]
            let mut f = std::fs::File::create(&cf)
                .context("failed to write twitter credentials file")?;
            f.write_all(creds_str.as_bytes())
                .context("failed to write twitter credentials file")?
        }
        temp_creds_path = Some(cf.clone());
        credentials_file = cf;
    } else if let Some(env_path) = env::var_os("ARCHIVR_TWITTER_CREDENTIALS_FILE") {
        credentials_file = absolutize_path_from_cwd(PathBuf::from(env_path), &invocation_cwd);
        temp_creds_path = None;
        if !credentials_file.is_file() {
            bail!(
                "Twitter credentials file not found: {}",
                credentials_file.display()
            );
        }
    } else {
        bail!(
            "Twitter scraping requires either cookie rules for x.com/twitter.com \
             or ARCHIVR_TWITTER_CREDENTIALS_FILE to be set."
        );
    }

    let mut cmd = Command::new(&python);
    // Run the scraper from staging_dir so relative paths in JSON resolve correctly.
    cmd.current_dir(&staging_dir).arg(&scraper_path);
    for arg in build_scraper_args(tweet_id, thread, &staging_tweets_dir, &staging_dir, &credentials_file) {
        cmd.arg(arg);
    }

    let spawn_result = cmd.output().with_context(|| {
        format!("Failed to spawn tweet scraper at {}", scraper_path.display())
    });
    if let Some(cf) = &temp_creds_path {
        let _ = fs::remove_file(cf);
    }
    let output = spawn_result?;
    if !output.status.success() {
        let _ = fs::remove_dir_all(&staging_dir);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        bail!(
            "Tweet scraper failed.\nstdout:\n{}\nstderr:\n{}",
            stdout.trim(),
            stderr.trim()
        );
    }

    let root_json = staging_tweets_dir.join(format!("tweet-{tweet_id}.json"));
    if !root_json.exists() {
        let _ = fs::remove_dir_all(&staging_dir);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        bail!(
            "Tweet scraper completed but did not create expected JSON file: {}\nstdout:\n{}\nstderr:\n{}",
            root_json.display(),
            stdout.trim(),
            stderr.trim()
        );
    }

    // Remove the scraping_summary.json if the scraper left one.
    cleanup_summary(&staging_tweets_dir)?;

    // Collect all tweet-*.json files from staging (this is the exact touched set).
    let mut staged_jsons: Vec<PathBuf> = fs::read_dir(&staging_tweets_dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("tweet-") && n.ends_with(".json"))
        })
        .collect();
    staged_jsons.sort();

    // Rewrite asset paths in staged JSONs (moves media blobs from staging into raw/).
    // staging_tweets_dir is base for avatar_local_path; staging_dir is base for local_path.
    rewrite_tweet_outputs(&staged_jsons, &staging_tweets_dir, &staging_dir, store_path)?;

    // Move each staged JSON to its final destination, collecting store-relative relpaths.
    let mut relpaths = Vec::with_capacity(staged_jsons.len());
    for staged_path in &staged_jsons {
        let filename = staged_path.file_name().context("tweet JSON path has no filename")?;
        let dest = output_dir.join(filename);
        fs::rename(staged_path, &dest)
            .or_else(|_| {
                fs::copy(staged_path, &dest).map(|_| ())?;
                fs::remove_file(staged_path)
            })
            .with_context(|| format!("failed to move staged tweet JSON to {}", dest.display()))?;
        let rel = dest
            .strip_prefix(store_path)
            .context("dest tweet JSON is not under store_path")?;
        relpaths.push(rel.to_string_lossy().replace('\\', "/"));
    }

    // Clean up the staging directory (media already moved to raw/ by rewrite_tweet_outputs).
    let _ = fs::remove_dir_all(&staging_dir);

    Ok(relpaths)
}

/// Archives a tweet (or full thread) identified by `path`.
///
/// Returns store-relative relpaths of all tweet JSON files registered for this capture.
/// For single tweets already on disk, returns the existing file path without re-downloading.
/// For new or thread captures, stages output in a temp dir then moves to `raw_tweets/`.
pub fn archive(
    path: &str,
    thread: bool,
    store_path: &Path,
    timestamp: &str,
    cookies: &HashMap<String, String>,
) -> Result<Vec<String>> {
    let tweet_id = tweet_id_from_path(path).context("Invalid tweet ID")?;
    let root_json = store_path.join("raw_tweets").join(format!("tweet-{tweet_id}.json"));
    if !thread && root_json.exists() {
        // Already present; skip re-download, return the existing file for artifact registration.
        return Ok(vec![format!("raw_tweets/tweet-{tweet_id}.json")]);
    }
    run_scraper_staged(&tweet_id, thread, store_path, timestamp, cookies)
}

/// Re-archives a tweet or thread, replacing files in `raw_tweets/` with fresh content.
/// Always runs the scraper; output is staged before replacing existing files.
/// Returns store-relative relpaths of all produced tweet JSON files.
/// If the scraper fails (tweet deleted/private), returns an error; existing files are untouched.
pub fn rearchive(
    path: &str,
    thread: bool,
    store_path: &Path,
    timestamp: &str,
    cookies: &HashMap<String, String>,
) -> Result<Vec<String>> {
    let tweet_id = tweet_id_from_path(path).context("Invalid tweet ID")?;
    run_scraper_staged(&tweet_id, thread, store_path, timestamp, cookies)
}

/// Removes the `scraping_summary.json` file left by the scraper, if present.
fn cleanup_summary(output_dir: &Path) -> Result<()> {
    let summary_path = output_dir.join("scraping_summary.json");
    if summary_path.exists() {
        fs::remove_file(summary_path)?;
    }
    Ok(())
}


/// Returns a lazily-compiled regex matching `"avatar_local_path": "..."` in JSON.
fn avatar_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r#""avatar_local_path": "([^"\n]+)""#).unwrap())
}

/// Returns a lazily-compiled regex matching `"local_path": "..."` in JSON.
fn media_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r#"(?m)"local_path": "([^"\n]+)""#).unwrap())
}

/// Rewrites asset paths in each newly-created JSON file, moving assets into
/// the content-addressed store. Files are written back only if content changed.
fn rewrite_tweet_outputs(
    tweet_jsons: &[PathBuf],
    output_dir: &Path,
    temp_dir: &Path,
    store_path: &Path,
) -> Result<()> {
    let mut archived_assets = HashMap::new();

    for path in tweet_jsons {
        let contents = fs::read_to_string(path)?;
        let rewritten = rewrite_json_asset_paths(
            &contents,
            output_dir,
            temp_dir,
            store_path,
            &mut archived_assets,
        )?;

        if rewritten != contents {
            fs::write(path, rewritten)?;
        }
    }

    Ok(())
}

/// Rewrites all `avatar_local_path` and `local_path` references in `contents`,
/// archiving each referenced file into the raw store and returning the updated
/// JSON string. `archived_assets` is a cache to avoid re-archiving the same
/// file when it is referenced by multiple tweets.
fn rewrite_json_asset_paths(
    contents: &str,
    output_dir: &Path,
    temp_dir: &Path,
    store_path: &Path,
    archived_assets: &mut HashMap<String, String>,
) -> Result<String> {
    let mut rewritten = contents.to_string();

    for captures in avatar_regex().captures_iter(contents) {
        let old_path = captures[1].to_string();
        let new_path =
            archive_asset_reference(&old_path, output_dir, store_path, "avatar", archived_assets)?;
        rewritten = rewritten.replace(
            &format!(r#""avatar_local_path": "{old_path}""#),
            &format!(r#""avatar_local_path": "{new_path}""#),
        );
    }

    for captures in media_regex().captures_iter(contents) {
        let old_path = captures[1].to_string();
        let new_path =
            archive_asset_reference(&old_path, temp_dir, store_path, "media", archived_assets)?;
        rewritten = rewritten.replace(
            &format!(r#""local_path": "{old_path}""#),
            &format!(r#""local_path": "{new_path}""#),
        );
    }

    Ok(rewritten)
}

/// Archives the asset at `old_path` (relative to `base_dir`) into the raw store
/// and returns its new store-relative path. Already-archived paths (starting
/// with `"raw/"`) are returned unchanged. Results are cached in `archived_assets`
/// by `"<kind>:<old_path>"` key to deduplicate work across TOML files.
fn archive_asset_reference(
    old_path: &str,
    base_dir: &Path,
    store_path: &Path,
    kind: &str,
    archived_assets: &mut HashMap<String, String>,
) -> Result<String> {
    if old_path.starts_with("raw/") {
        return Ok(old_path.to_string());
    }

    let key = format!("{kind}:{old_path}");
    if let Some(existing) = archived_assets.get(&key) {
        return Ok(existing.clone());
    }

    let absolute_path = base_dir.join(old_path);
    if !absolute_path.exists() {
        bail!(
            "Referenced tweet asset not found: {}",
            absolute_path.display()
        );
    }

    let relative_path = store::archive_staged_file(&absolute_path, store_path)?;
    let relative_path = relative_path.to_string_lossy().replace('\\', "/");
    archived_assets.insert(key, relative_path.clone());

    Ok(relative_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{Mutex, MutexGuard},
        time::{SystemTime, UNIX_EPOCH},
    };

    fn env_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    fn unique_path(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        env::temp_dir().join(format!("{prefix}-{nanos}-{}", std::process::id()))
    }

    fn set_test_env(key: &str, value: impl AsRef<std::ffi::OsStr>) {
        unsafe {
            env::set_var(key, value);
        }
    }

    fn remove_test_env(key: &str) {
        unsafe {
            env::remove_var(key);
        }
    }

    #[test]
    fn test_build_scraper_args_for_single_tweet() {
        let args = build_scraper_args(
            "1234567890",
            false,
            Path::new("/tmp/raw_tweets"),
            Path::new("/tmp/temp/tweets"),
            Path::new("/tmp/twitter-creds.txt"),
        );

        assert!(args.contains(&"--tweet-ids".to_string()));
        assert!(args.contains(&"1234567890".to_string()));
        assert!(args.contains(&"--output-dir".to_string()));
        assert!(args.contains(&"--download-media".to_string()));
        assert!(args.contains(&"--credentials-file".to_string()));
        assert!(args.contains(&"--no-recursive".to_string()));
        assert!(!args.contains(&"--recursive-replied-to-tweets".to_string()));
        assert!(!args.contains(&"--recursive-replied-to-tweets-quotes-retweets".to_string()));
        assert!(!args.contains(&"--download-replied-to-tweets-media".to_string()));
    }

    #[test]
    fn test_build_scraper_args_for_thread() {
        let args = build_scraper_args(
            "1234567890",
            true,
            Path::new("/tmp/raw_tweets"),
            Path::new("/tmp/temp/tweets"),
            Path::new("/tmp/twitter-creds.txt"),
        );

        assert!(args.contains(&"--recursive-replied-to-tweets".to_string()));
        assert!(args.contains(&"--recursive-replied-to-tweets-quotes-retweets".to_string()));
        assert!(args.contains(&"--download-replied-to-tweets-media".to_string()));
        assert!(!args.contains(&"--no-recursive".to_string()));
    }

    #[test]
    fn test_cleanup_summary_removes_summary_only() {
        let output_dir = unique_path("archivr-tweet-summary");
        fs::create_dir_all(&output_dir).unwrap();
        fs::write(output_dir.join("scraping_summary.json"), "summary").unwrap();
        fs::write(output_dir.join("tweet-1.json"), "tweet").unwrap();

        cleanup_summary(&output_dir).unwrap();

        assert!(!output_dir.join("scraping_summary.json").exists());
        assert!(output_dir.join("tweet-1.json").exists());

        let _ = fs::remove_dir_all(output_dir);
    }

    #[test]
    fn test_rewrite_json_asset_paths_rearchives_assets() {
        let store_path = unique_path("archivr-tweet-store");
        let output_dir = store_path.join("raw_tweets");
        let temp_dir = store_path.join("temp").join("ts").join("tweets");
        fs::create_dir_all(&output_dir).unwrap();
        fs::create_dir_all(temp_dir.join("media").join("avatars")).unwrap();
        fs::create_dir_all(temp_dir.join("media").join("123")).unwrap();

        fs::write(
            temp_dir.join("media").join("avatars").join("avatar.jpg"),
            b"avatar",
        )
        .unwrap();
        fs::write(
            temp_dir.join("media").join("123").join("media_1.jpg"),
            b"media",
        )
        .unwrap();

        let contents = r#"{
  "entities": { "media": [{ "local_path": "media/123/media_1.jpg" }] },
  "author": { "avatar_local_path": "../temp/ts/tweets/media/avatars/avatar.jpg" }
}"#;

        let rewritten = rewrite_json_asset_paths(
            contents,
            &output_dir,
            &temp_dir,
            &store_path,
            &mut HashMap::new(),
        )
        .unwrap();

        assert!(rewritten.contains(r#""avatar_local_path": "raw/"#));
        assert!(rewritten.contains(r#""local_path": "raw/"#));
        assert!(
            !temp_dir
                .join("media")
                .join("avatars")
                .join("avatar.jpg")
                .exists()
        );
        assert!(
            !temp_dir
                .join("media")
                .join("123")
                .join("media_1.jpg")
                .exists()
        );

        let _ = fs::remove_dir_all(store_path);
    }

    #[test]
    fn test_resolve_from_cwd_keeps_absolute_paths() {
        let path = absolutize_path_from_cwd(PathBuf::from("/tmp/creds.txt"), Path::new("/work"));
        assert_eq!(path, PathBuf::from("/tmp/creds.txt"));
    }

    #[test]
    fn test_resolve_from_cwd_expands_relative_paths() {
        let path = absolutize_path_from_cwd(PathBuf::from("creds.txt"), Path::new("/work"));
        assert_eq!(path, PathBuf::from("/work/creds.txt"));
    }

    #[test]
    fn test_archive_skips_existing_flat_tweet() {
        let _guard = env_lock();
        let store_path = unique_path("archivr-tweet-skip");
        let output_dir = store_path.join("raw_tweets");
        fs::create_dir_all(&output_dir).unwrap();
        fs::create_dir_all(store_path.join("temp")).unwrap();
        fs::write(output_dir.join("tweet-123.json"), r#"{"id":"123"}"#).unwrap();

        let credentials = store_path.join("creds.txt");
        fs::write(&credentials, "ct0=test;auth_token=test").unwrap();
        set_test_env("ARCHIVR_TWITTER_CREDENTIALS_FILE", &credentials);

        let relpaths = archive("tweet:123", false, &store_path, "ts", &HashMap::new()).unwrap();

        assert_eq!(relpaths, vec!["raw_tweets/tweet-123.json"]);

        remove_test_env("ARCHIVR_TWITTER_CREDENTIALS_FILE");
        let _ = fs::remove_dir_all(store_path);
    }

    #[test]
    fn test_archive_flattens_tweets_and_rewrites_assets_with_stub_scraper() {
        let _guard = env_lock();
        let store_path = unique_path("archivr-tweet-integration");
        let output_dir = store_path.join("raw_tweets");
        fs::create_dir_all(&output_dir).unwrap();
        fs::create_dir_all(store_path.join("temp")).unwrap();

        let credentials = store_path.join("creds.txt");
        fs::write(&credentials, "ct0=test;auth_token=test").unwrap();

        let script = store_path.join("stub_scraper.sh");
        fs::write(
            &script,
            r#"#!/bin/sh
set -eu

tweet_id=""
output_dir=""
media_dir=""

while [ "$#" -gt 0 ]; do
  case "$1" in
    --tweet-ids)
      tweet_id="$2"
      shift 2
      ;;
    --output-dir)
      output_dir="$2"
      shift 2
      ;;
    --media-dir)
      media_dir="$2"
      shift 2
      ;;
    *)
      shift
      ;;
  esac
done

mkdir -p "$output_dir" "$media_dir/avatars" "$media_dir/$tweet_id"
printf 'avatar' > "$media_dir/avatars/author.jpg"
printf 'media' > "$media_dir/$tweet_id/media_1.jpg"
printf '{"summary":true}\n' > "$output_dir/scraping_summary.json"
cat > "$output_dir/tweet-$tweet_id.json" <<EOF
{
  "id": "$tweet_id",
  "entities": { "media": [{ "local_path": "media/$tweet_id/media_1.jpg" }] },
  "author": { "avatar_local_path": "$media_dir/avatars/author.jpg" }
}
EOF
"#,
        )
        .unwrap();
        Command::new("chmod")
            .arg("+x")
            .arg(&script)
            .status()
            .unwrap();

        set_test_env("ARCHIVR_TWITTER_CREDENTIALS_FILE", &credentials);
        set_test_env("ARCHIVR_TWEET_SCRAPER", &script);
        set_test_env("ARCHIVR_TWEET_PYTHON", "/bin/sh");

        let relpaths = archive("tweet:123", false, &store_path, "ts", &HashMap::new()).unwrap();
        let tweet_file = output_dir.join("tweet-123.json");
        let contents = fs::read_to_string(&tweet_file).unwrap();

        assert!(!relpaths.is_empty());
        assert!(tweet_file.exists());
        assert!(!output_dir.join("scraping_summary.json").exists());
        assert!(contents.contains(r#""avatar_local_path": "raw/"#));
        assert!(contents.contains(r#""local_path": "raw/"#));
        assert!(!store_path.join("temp").join("ts").join("tweet_stage").exists());

        remove_test_env("ARCHIVR_TWITTER_CREDENTIALS_FILE");
        remove_test_env("ARCHIVR_TWEET_SCRAPER");
        remove_test_env("ARCHIVR_TWEET_PYTHON");
        let _ = fs::remove_dir_all(store_path);
    }
}
