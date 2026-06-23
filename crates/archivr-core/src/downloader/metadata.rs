use regex::Regex;
use serde_json::Value;
use crate::capture::PlatformMetadata;

/// Parses a yt-dlp `--dump-json` output string into a PlatformMetadata.
/// Returns a zeroed-out PlatformMetadata on any parse failure — never errors.
pub fn extract_from_ytdlp_json(json_str: &str) -> PlatformMetadata {
    let Ok(v): Result<Value, _> = serde_json::from_str(json_str) else {
        return PlatformMetadata::default();
    };

    let str_field = |key: &str| -> Option<String> {
        v.get(key)
            .and_then(|f| f.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };

    let webpage_url = str_field("webpage_url").unwrap_or_default();
    let uploader = str_field("uploader");

    // For Reddit, yt-dlp's "uploader" is the post author; subreddit comes from the URL.
    // For all other platforms, uploader maps to author.
    let subreddit = subreddit_from_url(&webpage_url);
    let (author, post_author) = if subreddit.is_some() {
        (None, uploader)
    } else {
        (uploader, None)
    };

    PlatformMetadata {
        author,
        title: str_field("title"),
        caption: str_field("description"),
        subreddit,
        post_author,
    }
}

fn subreddit_from_url(url: &str) -> Option<String> {
    let re = Regex::new(r"reddit\.com/r/([A-Za-z0-9_]+)").expect("static regex is valid");
    re.captures(url)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_youtube_fields() {
        let json = r#"{
            "uploader": "Tech Channel",
            "title": "How Rust Works",
            "description": "A deep dive into Rust"
        }"#;
        let m = extract_from_ytdlp_json(json);
        assert_eq!(m.author, Some("Tech Channel".to_string()));
        assert_eq!(m.title, Some("How Rust Works".to_string()));
        assert_eq!(m.caption, Some("A deep dive into Rust".to_string()));
        assert_eq!(m.subreddit, None);
    }

    #[test]
    fn parses_reddit_subreddit_from_url() {
        let json = r#"{
            "uploader": "some_user",
            "title": "Cool Post",
            "description": "",
            "webpage_url": "https://www.reddit.com/r/rust/comments/abc123/cool_post/"
        }"#;
        let m = extract_from_ytdlp_json(json);
        assert_eq!(m.subreddit, Some("rust".to_string()));
        assert_eq!(m.post_author, Some("some_user".to_string()));
        assert_eq!(m.title, Some("Cool Post".to_string()));
    }

    #[test]
    fn missing_fields_produce_none() {
        let json = r#"{}"#;
        let m = extract_from_ytdlp_json(json);
        assert!(m.author.is_none());
        assert!(m.title.is_none());
        assert!(m.caption.is_none());
    }

    #[test]
    fn invalid_json_returns_default() {
        let m = extract_from_ytdlp_json("not json at all");
        assert!(m.author.is_none());
    }

    #[test]
    fn empty_string_fields_produce_none() {
        let json = r#"{"uploader": "", "title": "  ", "description": ""}"#;
        let m = extract_from_ytdlp_json(json);
        assert_eq!(m.author, None);
        assert_eq!(m.title, None);
    }

    #[test]
    fn subreddit_regex_extracts_name() {
        assert_eq!(
            subreddit_from_url("https://www.reddit.com/r/rust/comments/abc/"),
            Some("rust".to_string())
        );
        assert_eq!(subreddit_from_url("https://youtube.com/watch?v=abc"), None);
    }
}
