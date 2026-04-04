use anyhow::{Context, Result, bail};
use regex::Regex;
use std::{
    collections::{HashMap, HashSet},
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::OnceLock,
};

use crate::twitter::parse_tweet_id;

use super::store;

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

/// Archives a tweet (or full thread) identified by `path` (e.g. `"tweet:123"`).
///
/// Invokes the Python scraper, then moves all produced media assets into the
/// content-addressed raw store and rewrites the JSON output to use the new
/// store-relative paths. Returns `true` if new content was archived, `false`
/// if the tweet was already present and `thread` is `false`.
///
/// Requires `ARCHIVR_TWITTER_CREDENTIALS_FILE` to be set. The scraper binary
/// can be overridden via `ARCHIVR_TWEET_SCRAPER` and `ARCHIVR_TWEET_PYTHON`.
pub fn archive(path: &str, thread: bool, store_path: &Path, timestamp: &str) -> Result<bool> {
    let invocation_cwd = env::current_dir().context("Failed to read current working directory")?;
    // Output directory for Tweet JSON files.
    let output_dir = store_path.join("raw_tweets");
    // Temporary directory for media assets downloaded by the scraper in `temp/...`.
    let temp_dir = store_path.join("temp").join(timestamp).join("tweets");
    let tweet_id = tweet_id_from_path(path).context("Invalid tweet ID")?;

    fs::create_dir_all(&output_dir)?;
    fs::create_dir_all(&temp_dir)?;

    // Path to the root - the to-be-archived tweet's JSON file.
    let root_json = output_dir.join(format!("tweet-{tweet_id}.json"));
    if !thread && root_json.exists() {
        return Ok(false);
    }

    let before = tweet_json_files(&output_dir)?;

    let python = env::var_os("ARCHIVR_TWEET_PYTHON").unwrap_or_else(|| OsString::from("python3"));
    let scraper_path = env::var_os("ARCHIVR_TWEET_SCRAPER")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("vendor/twitter/scrape_user_tweet_contents.py"));
    let scraper_path = absolutize_path_from_cwd(scraper_path, &invocation_cwd);

    let credentials_file = if let Some(credentials_file) =
        env::var_os("ARCHIVR_TWITTER_CREDENTIALS_FILE")
    {
        absolutize_path_from_cwd(PathBuf::from(credentials_file), &invocation_cwd)
    } else {
        bail!(
            "Twitter scraping requires ARCHIVR_TWITTER_CREDENTIALS_FILE to point to a cookies file."
        );
    };

    if !credentials_file.is_file() {
        bail!(
            "Twitter credentials file not found: {}",
            credentials_file.display()
        );
    }

    let mut cmd = Command::new(&python);
    cmd.current_dir(&temp_dir).arg(&scraper_path);
    for arg in build_scraper_args(&tweet_id, thread, &output_dir, &temp_dir, &credentials_file) {
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

    if !root_json.exists() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        bail!(
            "Tweet scraper completed but did not create expected JSON file: {}\nstdout:\n{}\nstderr:\n{}",
            root_json.display(),
            stdout.trim(),
            stderr.trim()
        );
    }

    cleanup_summary(&output_dir)?;
    let after = tweet_json_files(&output_dir)?;
    let new_jsons = new_tweet_jsons(&before, &after);
    rewrite_tweet_outputs(&new_jsons, &output_dir, &temp_dir, store_path)?;
    let _ = fs::remove_dir_all(store_path.join("temp").join(timestamp));

    Ok(true)
}

/// Removes the `scraping_summary.json` file left by the scraper, if present.
fn cleanup_summary(output_dir: &Path) -> Result<()> {
    let summary_path = output_dir.join("scraping_summary.json");
    if summary_path.exists() {
        fs::remove_file(summary_path)?;
    }
    Ok(())
}

/// Returns the set of `tweet-*.json` files present in `output_dir`.
fn tweet_json_files(output_dir: &Path) -> Result<HashSet<PathBuf>> {
    let mut files = HashSet::new();

    for entry in fs::read_dir(output_dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_file()
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("tweet-") && name.ends_with(".json"))
        {
            files.insert(path);
        }
    }

    Ok(files)
}

/// Returns the sorted list of JSON files present in `after` but not in `before`.
fn new_tweet_jsons(before: &HashSet<PathBuf>, after: &HashSet<PathBuf>) -> Vec<PathBuf> {
    let mut files = after.difference(before).cloned().collect::<Vec<_>>();
    files.sort();
    files
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

        let archived = archive("tweet:123", false, &store_path, "ts").unwrap();

        assert!(!archived);

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
  "author": { "avatar_local_path": "../temp/ts/tweets/media/avatars/author.jpg" }
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

        let archived = archive("tweet:123", false, &store_path, "ts").unwrap();
        let tweet_file = output_dir.join("tweet-123.json");
        let contents = fs::read_to_string(&tweet_file).unwrap();

        assert!(archived);
        assert!(tweet_file.exists());
        assert!(!output_dir.join("scraping_summary.json").exists());
        assert!(contents.contains(r#""avatar_local_path": "raw/"#));
        assert!(contents.contains(r#""local_path": "raw/"#));
        assert!(!store_path.join("temp").join("ts").exists());

        remove_test_env("ARCHIVR_TWITTER_CREDENTIALS_FILE");
        remove_test_env("ARCHIVR_TWEET_SCRAPER");
        remove_test_env("ARCHIVR_TWEET_PYTHON");
        let _ = fs::remove_dir_all(store_path);
    }
}
