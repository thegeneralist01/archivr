/// Returns the tweet ID if `id` is non-empty and contains only ASCII digits.
pub fn parse_tweet_id(id: &str) -> Option<String> {
    if !id.is_empty() && id.chars().all(|char| char.is_ascii_digit()) {
        Some(id.to_string())
    } else {
        None
    }
}
