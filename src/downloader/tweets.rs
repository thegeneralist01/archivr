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

use super::local;

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

fn resolve_from_cwd(path: PathBuf, cwd: &Path) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    }
}

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

pub fn archive(path: &str, thread: bool, store_path: &Path, timestamp: &str) -> Result<bool> {
    let invocation_cwd = env::current_dir().context("Failed to read current working directory")?;
    let output_dir = store_path.join("raw_tweets");
    let temp_dir = store_path.join("temp").join(timestamp).join("tweets");
    let tweet_id = tweet_id_from_path(path).context("Invalid tweet ID")?;

    fs::create_dir_all(&output_dir)?;
    fs::create_dir_all(&temp_dir)?;

    let root_toml = output_dir.join(format!("tweet-{tweet_id}.toml"));
    if !thread && root_toml.exists() {
        return Ok(false);
    }

    let before = tweet_toml_files(&output_dir)?;

    let python = env::var_os("ARCHIVR_TWEET_PYTHON").unwrap_or_else(|| OsString::from("python3"));
    let scraper_path = env::var_os("ARCHIVR_TWEET_SCRAPER")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("vendor/twitter/scrape_user_tweet_contents.py"));
    let scraper_path = resolve_from_cwd(scraper_path, &invocation_cwd);

    let credentials_file = if let Some(credentials_file) =
        env::var_os("ARCHIVR_TWITTER_CREDENTIALS_FILE")
    {
        resolve_from_cwd(PathBuf::from(credentials_file), &invocation_cwd)
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

    if !root_toml.exists() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        bail!(
            "Tweet scraper completed but did not create expected TOML file: {}\nstdout:\n{}\nstderr:\n{}",
            root_toml.display(),
            stdout.trim(),
            stderr.trim()
        );
    }

    cleanup_summary(&output_dir)?;
    let after = tweet_toml_files(&output_dir)?;
    let new_tomls = new_tweet_tomls(&before, &after);
    rewrite_tweet_outputs(&new_tomls, &output_dir, &temp_dir, store_path)?;
    let _ = fs::remove_dir_all(store_path.join("temp").join(timestamp));

    Ok(true)
}

fn cleanup_summary(output_dir: &Path) -> Result<()> {
    let summary_path = output_dir.join("scraping_summary.toml");
    if summary_path.exists() {
        fs::remove_file(summary_path)?;
    }
    Ok(())
}

fn tweet_toml_files(output_dir: &Path) -> Result<HashSet<PathBuf>> {
    let mut files = HashSet::new();

    for entry in fs::read_dir(output_dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_file()
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("tweet-") && name.ends_with(".toml"))
        {
            files.insert(path);
        }
    }

    Ok(files)
}

fn new_tweet_tomls(before: &HashSet<PathBuf>, after: &HashSet<PathBuf>) -> Vec<PathBuf> {
    let mut files = after.difference(before).cloned().collect::<Vec<_>>();
    files.sort();
    files
}

fn avatar_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r#"avatar_local_path = "([^"\n]+)""#).unwrap())
}

fn media_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r#"(?m)\blocal_path = "([^"\n]+)""#).unwrap())
}

fn rewrite_tweet_outputs(
    tweet_tomls: &[PathBuf],
    output_dir: &Path,
    temp_dir: &Path,
    store_path: &Path,
) -> Result<()> {
    let mut archived_assets = HashMap::new();

    for path in tweet_tomls {
        let contents = fs::read_to_string(path)?;
        let rewritten = rewrite_toml_asset_paths(
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

fn rewrite_toml_asset_paths(
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
            &format!(r#"avatar_local_path = "{old_path}""#),
            &format!(r#"avatar_local_path = "{new_path}""#),
        );
    }

    for captures in media_regex().captures_iter(contents) {
        let old_path = captures[1].to_string();
        let new_path =
            archive_asset_reference(&old_path, temp_dir, store_path, "media", archived_assets)?;
        rewritten = rewritten.replace(
            &format!(r#"local_path = "{old_path}""#),
            &format!(r#"local_path = "{new_path}""#),
        );
    }

    Ok(rewritten)
}

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

    let relative_path = local::archive_staged_file(&absolute_path, store_path)?;
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
        fs::write(output_dir.join("scraping_summary.toml"), "summary").unwrap();
        fs::write(output_dir.join("tweet-1.toml"), "tweet").unwrap();

        cleanup_summary(&output_dir).unwrap();

        assert!(!output_dir.join("scraping_summary.toml").exists());
        assert!(output_dir.join("tweet-1.toml").exists());

        let _ = fs::remove_dir_all(output_dir);
    }

    #[test]
    fn test_rewrite_toml_asset_paths_rearchives_assets() {
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

        let contents = r#"
[entities]
media = [{ local_path = "media/123/media_1.jpg" }]

[author]
avatar_local_path = "../temp/ts/tweets/media/avatars/avatar.jpg"
"#;

        let rewritten = rewrite_toml_asset_paths(
            contents,
            &output_dir,
            &temp_dir,
            &store_path,
            &mut HashMap::new(),
        )
        .unwrap();

        assert!(rewritten.contains(r#"avatar_local_path = "raw/"#));
        assert!(rewritten.contains(r#"local_path = "raw/"#));
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
        let path = resolve_from_cwd(PathBuf::from("/tmp/creds.txt"), Path::new("/work"));
        assert_eq!(path, PathBuf::from("/tmp/creds.txt"));
    }

    #[test]
    fn test_resolve_from_cwd_expands_relative_paths() {
        let path = resolve_from_cwd(PathBuf::from("creds.txt"), Path::new("/work"));
        assert_eq!(path, PathBuf::from("/work/creds.txt"));
    }

    #[test]
    fn test_archive_skips_existing_flat_tweet() {
        let _guard = env_lock();
        let store_path = unique_path("archivr-tweet-skip");
        let output_dir = store_path.join("raw_tweets");
        fs::create_dir_all(&output_dir).unwrap();
        fs::create_dir_all(store_path.join("temp")).unwrap();
        fs::write(output_dir.join("tweet-123.toml"), "id = \"123\"").unwrap();

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
printf 'summary = true\n' > "$output_dir/scraping_summary.toml"
cat > "$output_dir/tweet-$tweet_id.toml" <<EOF
id = "$tweet_id"

[entities]
media = [{ local_path = "media/$tweet_id/media_1.jpg" }]

[author]
avatar_local_path = "../temp/ts/tweets/media/avatars/author.jpg"
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
        let tweet_file = output_dir.join("tweet-123.toml");
        let contents = fs::read_to_string(&tweet_file).unwrap();

        assert!(archived);
        assert!(tweet_file.exists());
        assert!(!output_dir.join("scraping_summary.toml").exists());
        assert!(contents.contains(r#"avatar_local_path = "raw/"#));
        assert!(contents.contains(r#"local_path = "raw/"#));
        assert!(!store_path.join("temp").join("ts").exists());

        remove_test_env("ARCHIVR_TWITTER_CREDENTIALS_FILE");
        remove_test_env("ARCHIVR_TWEET_SCRAPER");
        remove_test_env("ARCHIVR_TWEET_PYTHON");
        let _ = fs::remove_dir_all(store_path);
    }
}
