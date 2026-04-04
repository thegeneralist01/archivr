#!/usr/bin/env python3
"""
Extract tweet contents from given Tweet IDs and save them as JSON files.

This script uses the twitter-api-client library to fetch tweet data and saves
it in JSON format with optional media downloads and recursive extraction.
"""

import argparse
import json
import os
import sys
import time
import urllib.parse
import urllib.request
from datetime import datetime
from pathlib import Path
from typing import Any, Dict, List, Optional, Set, Tuple

from twitter.scraper import Scraper


def print_json(data):
    """Pretty print JSON data."""
    print(json.dumps(data, indent=2))


def is_rate_limit_error(error):
    """
    Check if an error is a rate limit error (429 Too Many Requests).

    Args:
        error: Exception object or error message

    Returns:
        True if it's a rate limit error, False otherwise
    """
    error_str = str(error).lower()
    rate_limit_indicators = [
        "429",
        "too many requests",
        "rate limit",
        "rate_limit",
        "exceeded",
        "quota",
        "limit exceeded",
    ]
    return any(indicator in error_str for indicator in rate_limit_indicators)


def handle_rate_limit_error(error, retry_count, base_wait_time=60):
    """
    Handle rate limit errors with exponential backoff.

    Args:
        error: The exception that occurred
        retry_count: Number of times we've retried
        base_wait_time: Base wait time in seconds (default 60s = 1 minute)

    Returns:
        Wait time in seconds before retrying
    """
    wait_time = base_wait_time * (2**retry_count)
    wait_time = min(wait_time, 900)  # Cap at 15 minutes

    print(f"\n  ⚠ Rate limit detected (attempt {retry_count + 1})")
    print(f"  ⏳ Waiting {wait_time}s ({wait_time / 60:.1f} minutes) before retry...")

    return wait_time


def parse_tweet_ids_from_args(
    tweet_ids_str: Optional[str], tweet_ids_files: Optional[str]
) -> Set[str]:
    """
    Parse tweet IDs from CLI arguments.

    Args:
        tweet_ids_str: Comma-separated tweet IDs string
        tweet_ids_files: Comma-separated file paths

    Returns:
        Set of tweet IDs (deduplicated)
    """
    all_tweet_ids = set()

    # Parse comma-separated tweet IDs
    if tweet_ids_str:
        ids = [tid.strip() for tid in tweet_ids_str.split(",") if tid.strip()]
        all_tweet_ids.update(ids)

    # Parse tweet IDs from files
    if tweet_ids_files:
        file_paths = [f.strip() for f in tweet_ids_files.split(",") if f.strip()]
        for file_path in file_paths:
            file_path = os.path.expanduser(file_path)
            if not os.path.isabs(file_path):
                file_path = os.path.join(os.getcwd(), file_path)

            if not os.path.exists(file_path):
                print(f"⚠ Warning: File not found: {file_path}")
                continue

            try:
                ids = parse_tweet_ids_from_file(file_path)
                all_tweet_ids.update(ids)
            except Exception as e:
                print(f"⚠ Warning: Error parsing file {file_path}: {e}")
                continue

    return all_tweet_ids


def parse_tweet_ids_from_file(file_path: str) -> List[str]:
    """
    Parse tweet IDs from a file.

    Supports:
    - Plain text file with one Tweet ID per line
    - JSON file containing a list (array) of Tweet IDs
    - Scrape summary JSON file (from scrape_user_tweet_ids.py)

    Args:
        file_path: Path to the file

    Returns:
        List of tweet IDs
    """
    tweet_ids = []

    # Check file extension
    _, ext = os.path.splitext(file_path.lower())

    if ext == ".json":
        # Try to parse as JSON
        with open(file_path, "r") as f:
            data = json.load(f)

        # Check if it's a scrape summary file
        if isinstance(data, dict) and "tweet_ids_file" in data:
            # It's a scrape summary file
            tweet_ids_file = data["tweet_ids_file"]
            if not os.path.isabs(tweet_ids_file):
                # Make relative to the summary file's directory
                summary_dir = os.path.dirname(file_path)
                tweet_ids_file = os.path.join(summary_dir, tweet_ids_file)

            # Recursively parse the tweet IDs file
            return parse_tweet_ids_from_file(tweet_ids_file)

        # Check if it's a list of tweet IDs
        elif isinstance(data, list):
            tweet_ids = [str(tid) for tid in data if tid]
        else:
            raise ValueError(f"Unexpected JSON structure in {file_path}")

    else:
        # Assume plain text file with one tweet ID per line
        with open(file_path, "r") as f:
            for line in f:
                line = line.strip()
                if line and not line.startswith("#"):
                    tweet_ids.append(line)

    return tweet_ids


def extract_tweet_from_response(response_data: Any, tweet_id: str) -> Optional[Dict]:
    """
    Extract tweet data from API response.

    Args:
        response_data: Response data from scraper
        tweet_id: The tweet ID we're looking for

    Returns:
        Tweet data dictionary or None if not found
    """
    try:
        # Handle list response
        if isinstance(response_data, list):
            if len(response_data) == 0:
                return None
            data = response_data[0]
        elif isinstance(response_data, dict):
            data = response_data
        else:
            return None

        # Navigate through the nested structure
        # Try different possible paths
        tweet_result = None

        # Path 1: TweetDetail GraphQL response structure
        # Check for threaded_conversation_with_injections_v2 structure
        if "data" in data:
            threaded_conversation = data.get("data", {}).get(
                "threaded_conversation_with_injections_v2", {}
            )
            instructions = threaded_conversation.get("instructions", [])

            for instruction in instructions:
                if instruction.get("type") == "TimelineAddEntries":
                    entries = instruction.get("entries", [])
                    for entry in entries:
                        content = entry.get("content", {})
                        if content.get("entryType") == "TimelineTimelineItem":
                            item_content = content.get("itemContent", {})
                            if item_content.get("itemType") == "TimelineTweet":
                                result = item_content.get("tweet_results", {}).get(
                                    "result", {}
                                )
                                if result.get("rest_id") == tweet_id:
                                    tweet_result = result
                                    break
                        if tweet_result:
                            break
                    if tweet_result:
                        break

        # Path 2: Timeline structure (for user tweets)
        if not tweet_result and "data" in data:
            timeline = (
                data.get("data", {})
                .get("user", {})
                .get("result", {})
                .get("timeline_v2", {})
                .get("timeline", {})
            )
            instructions = timeline.get("instructions", [])

            for instruction in instructions:
                if instruction.get("type") == "TimelineAddEntries":
                    entries = instruction.get("entries", [])
                    for entry in entries:
                        content = entry.get("content", {})
                        if content.get("entryType") == "TimelineTimelineItem":
                            item_content = content.get("itemContent", {})
                            if item_content.get("itemType") == "TimelineTweet":
                                result = item_content.get("tweet_results", {}).get(
                                    "result", {}
                                )
                                if result.get("rest_id") == tweet_id:
                                    tweet_result = result
                                    break
                        if tweet_result:
                            break
                    if tweet_result:
                        break

        # Path 3: Direct tweet lookup (recursive search)
        if not tweet_result:

            def find_tweet_recursive(obj, target_id):
                if isinstance(obj, dict):
                    # Check if this is a tweet result with matching ID
                    if (
                        obj.get("rest_id") == target_id
                        and obj.get("__typename") == "Tweet"
                    ):
                        return obj
                    # Also check legacy.id_str for older format
                    legacy = obj.get("legacy", {})
                    if legacy and legacy.get("id_str") == target_id:
                        return obj
                    # Recursively search
                    for value in obj.values():
                        result = find_tweet_recursive(value, target_id)
                        if result:
                            return result
                elif isinstance(obj, list):
                    for item in obj:
                        result = find_tweet_recursive(item, target_id)
                        if result:
                            return result
                return None

            tweet_result = find_tweet_recursive(data, tweet_id)

        return tweet_result

    except Exception as e:
        print(f"  ⚠ Warning: Error extracting tweet {tweet_id}: {e}")
        import traceback

        traceback.print_exc()
        return None


from typing import Any, Dict, List, Optional


def extract_article_data(tweet_result: Dict[str, Any]) -> Optional[Dict[str, Any]]:
    """
    Extract article data from a tweet result if the tweet contains an article.
    """
    article_result = (
        tweet_result.get("article", {}).get("article_results", {}).get("result", {})
    )

    if not article_result:
        return None

    content_state = article_result.get("content_state", {})
    blocks = content_state.get("blocks", [])
    entity_map_raw = content_state.get("entityMap", [])
    media_entities = article_result.get("media_entities", [])

    # Normalize entity map because X may return it as a list of
    # {"key": "...", "value": {...}} objects.
    entity_map: Dict[str, Dict[str, Any]] = {}
    if isinstance(entity_map_raw, list):
        for entry in entity_map_raw:
            key = str(entry.get("key"))
            value = entry.get("value", {})
            entity_map[key] = value
    elif isinstance(entity_map_raw, dict):
        entity_map = {str(k): v for k, v in entity_map_raw.items()}

    # Index article media by media_id so atomic MEDIA blocks can be resolved.
    media_by_id: Dict[str, Dict[str, Any]] = {}
    for media in media_entities:
        media_id = str(media.get("media_id"))
        media_by_id[media_id] = media

    structured_blocks: List[Dict[str, Any]] = []

    for block in blocks:
        block_type = block.get("type", "")
        block_text = block.get("text", "")
        block_data: Dict[str, Any] = {
            "type": block_type,
            "text": block_text,
            "key": block.get("key", ""),
            "inline_style_ranges": block.get("inlineStyleRanges", []),
            "entity_ranges": block.get("entityRanges", []),
            "data": block.get("data", {}),
        }

        # Resolve atomic blocks into something archivable/useful.
        if block_type == "atomic":
            resolved_entities: List[Dict[str, Any]] = []

            for entity_range in block.get("entityRanges", []):
                entity_key = str(entity_range.get("key"))
                entity = entity_map.get(entity_key, {})
                entity_type = entity.get("type", "")
                entity_data = entity.get("data", {})

                if entity_type == "MEDIA":
                    for media_item in entity_data.get("mediaItems", []):
                        media_id = str(media_item.get("mediaId"))
                        media = media_by_id.get(media_id, {})
                        media_info = media.get("media_info", {})

                        resolved_entities.append(
                            {
                                "type": "media",
                                "media_id": media_id,
                                "media_key": media.get("media_key", ""),
                                "url": media_info.get("original_img_url", ""),
                                "width": media_info.get("original_img_width", 0),
                                "height": media_info.get("original_img_height", 0),
                            }
                        )

                elif entity_type == "TWEET":
                    resolved_entities.append(
                        {
                            "type": "tweet",
                            "tweet_id": entity_data.get("tweetId", ""),
                        }
                    )

                elif entity_type == "DIVIDER":
                    resolved_entities.append({"type": "divider"})

                elif entity_type == "LINK":
                    resolved_entities.append(
                        {
                            "type": "link",
                            "url": entity_data.get("url", ""),
                        }
                    )

                elif entity_type == "TWEMOJI":
                    resolved_entities.append(
                        {
                            "type": "emoji",
                            "url": entity_data.get("url", ""),
                        }
                    )

                else:
                    resolved_entities.append(
                        {
                            "type": entity_type.lower() if entity_type else "",
                            "data": entity_data,
                        }
                    )

            block_data["resolved_entities"] = resolved_entities

        structured_blocks.append(block_data)

    # Pull article URL from the wrapper tweet URL entities if present.
    legacy = tweet_result.get("legacy", {})
    article_url = ""
    for url_obj in legacy.get("entities", {}).get("urls", []):
        expanded_url = url_obj.get("expanded_url", "")
        if "/i/article/" in expanded_url:
            article_url = expanded_url
            break

    # Author info: note this lives in user_result.core / avatar in your response,
    # not where your current code is reading it from.
    user_result = tweet_result.get("core", {}).get("user_results", {}).get("result", {})
    user_core = user_result.get("core", {})
    user_avatar = user_result.get("avatar", {})

    cover_media = article_result.get("cover_media", {})
    cover_media_info = cover_media.get("media_info", {})

    article_data = {
        "id": article_result.get("rest_id"),
        "tweet_id": tweet_result.get("rest_id"),
        "url": article_url,
        "title": article_result.get("title", ""),
        "preview_text": article_result.get("preview_text", ""),
        "summary_text": article_result.get("summary_text", ""),
        "plain_text": article_result.get("plain_text", ""),
        "is_grok_summary_eligible": article_result.get(
            "is_grok_summary_eligible", False
        ),
        "first_published_at_secs": article_result.get("metadata", {}).get(
            "first_published_at_secs"
        ),
        "modified_at_secs": article_result.get("lifecycle_state", {}).get(
            "modified_at_secs"
        ),
        "cover_media": {
            "media_id": cover_media.get("media_id"),
            "media_key": cover_media.get("media_key", ""),
            "url": cover_media_info.get("original_img_url", ""),
            "width": cover_media_info.get("original_img_width", 0),
            "height": cover_media_info.get("original_img_height", 0),
        },
        "author": {
            "id": user_result.get("rest_id"),
            "name": user_core.get("name", ""),
            "screen_name": user_core.get("screen_name", ""),
            "avatar_url": user_avatar.get("image_url", ""),
        },
        "blocks": structured_blocks,
        "media_entities": media_entities,
        "entity_map": entity_map,
    }

    return article_data


def extract_tweet_data(
    tweet_result: Dict, bare_scrape: bool = False, advanced_info: bool = False
) -> Dict:
    """
    Extract tweet data from tweet result structure.

    Args:
        tweet_result: Tweet result dictionary from API
        bare_scrape: If True, only extract bare minimum fields
        advanced_info: If True, extract additional optional fields

    Returns:
        Dictionary with tweet data
    """
    tweet_data = {}

    # Extract tweet ID (bare)
    tweet_data["id"] = tweet_result.get("rest_id")

    # Extract legacy data (main tweet content)
    legacy = tweet_result.get("legacy", {})

    # Extract full text (bare)
    tweet_data["full_text"] = legacy.get("full_text", "")

    # Extract is_quote_status (bare)
    tweet_data["is_quote_status"] = legacy.get("is_quote_status", False)

    # Extract entities (always included)
    entities = legacy.get("entities", {})
    tweet_data["entities"] = {
        "hashtags": entities.get("hashtags", []),
        "urls": entities.get("urls", []),
        "user_mentions": entities.get("user_mentions", []),
        "symbols": entities.get("symbols", []),
        "media": entities.get("media", []) if not bare_scrape else [],
    }

    # Extract optional fields if not bare scrape
    if not bare_scrape:
        # Optional: creation date
        if advanced_info:
            tweet_data["created_at"] = legacy.get("created_at")

        # Optional: bookmark count
        if advanced_info:
            tweet_data["bookmark_count"] = legacy.get("bookmark_count", 0)

        # Optional: favorite count
        if advanced_info:
            tweet_data["favorite_count"] = legacy.get("favorite_count", 0)

        # Optional: quote count
        if advanced_info:
            tweet_data["quote_count"] = legacy.get("quote_count", 0)

        # Optional: reply count
        if advanced_info:
            tweet_data["reply_count"] = legacy.get("reply_count", 0)

        # Optional: retweet count
        if advanced_info:
            tweet_data["retweet_count"] = legacy.get("retweet_count", 0)

        # Optional: retweeted status
        if advanced_info:
            tweet_data["retweeted"] = legacy.get("retweeted", False)

        # Optional: edit_tweet_ids
        if advanced_info:
            edit_control = tweet_result.get("edit_control", {})
            edit_tweet_ids = edit_control.get("edit_tweet_ids", [])
            if edit_tweet_ids:
                tweet_data["edit_tweet_ids"] = edit_tweet_ids

    # Extract author information
    core = tweet_result.get("core", {})
    user_results = core.get("user_results", {})
    user_result = user_results.get("result", {})
    legacy_user = user_result.get("legacy", {})

    # Author ID (bare)
    tweet_data["author"] = {
        "id": user_result.get("rest_id"),
        "name": legacy_user.get("name", ""),
        "screen_name": legacy_user.get("screen_name", ""),
    }

    # Crutch-y way of fixing Author ID if broken
    if tweet_data["author"]["name"] == "" and tweet_data["author"]["screen_name"] == "":
        user_result = user_results.get("result", {})
        user_core = user_result.get("core", {})

        tweet_data["author"] = {
            "id": user_result.get("rest_id"),
            "name": user_core.get("name", ""),
            "screen_name": user_core.get("screen_name", ""),
        }

    tweet_data["is_article"] = False

    # Article data (bare)
    article_data = extract_article_data(tweet_result)
    if article_data:
        tweet_data["article"] = article_data
        tweet_data["is_article"] = True

    # Author optional fields
    if not bare_scrape:
        # Avatar URL (always included if downloading avatars)
        profile_image_url = legacy_user.get("profile_image_url_https", "")
        tweet_data["author"]["avatar_url"] = profile_image_url or user_result.get(
            "avatar", {}
        ).get("image_url", "")

        # Optional: verified status
        if advanced_info:
            tweet_data["author"]["is_verified"] = user_result.get(
                "is_blue_verified", False
            )

        # Optional: follower count
        if advanced_info:
            tweet_data["author"]["followers_count"] = legacy_user.get(
                "followers_count", 0
            )

    # Extract retweeted status if present
    # Check both top-level and legacy level
    retweeted_status_result = tweet_result.get("retweeted_status_result", {})
    if not retweeted_status_result:
        retweeted_status_result = legacy.get("retweeted_status_result", {})

    if retweeted_status_result:
        retweeted_result = retweeted_status_result.get("result", {})
        if retweeted_result:
            # Extract bare minimum for retweeted tweet
            tweet_data["retweeted_status"] = extract_tweet_data(
                retweeted_result,
                bare_scrape=True,  # Always bare for retweeted tweets
                advanced_info=False,
            )

    # Extract quoted status if present
    quoted_status_id_str = legacy.get("quoted_status_id_str")
    if quoted_status_id_str:
        tweet_data["quoted_status_id"] = quoted_status_id_str

    # Extract replied-to tweet ID if present
    in_reply_to_status_id_str = legacy.get("in_reply_to_status_id_str")
    if in_reply_to_status_id_str:
        tweet_data["in_reply_to_status_id"] = in_reply_to_status_id_str

    return tweet_data


def download_file(url: str, output_path: str, retry_count: int = 0) -> bool:
    """
    Download a file from URL to output path.

    Args:
        url: URL to download from
        output_path: Path to save the file
        retry_count: Number of retries attempted

    Returns:
        True if successful, False otherwise
    """
    try:
        os.makedirs(os.path.dirname(output_path), exist_ok=True)

        # Create request with user agent
        req = urllib.request.Request(url)
        req.add_header(
            "User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36"
        )

        with urllib.request.urlopen(req, timeout=30) as response:
            with open(output_path, "wb") as f:
                f.write(response.read())

        return True
    except Exception as e:
        if retry_count < 2:
            time.sleep(2)
            return download_file(url, output_path, retry_count + 1)
        print(f"  ⚠ Warning: Failed to download {url}: {e}")
        return False


def download_tweet_media(tweet_data: Dict, tweet_id: str, media_dir: str) -> List[str]:
    """
    Download media files for a tweet.

    Args:
        tweet_data: Tweet data dictionary
        media_dir: Directory to save media files

    Returns:
        List of local file paths for downloaded media
    """
    media_paths = []
    entities = tweet_data.get("entities", {})
    media_list = entities.get("media", [])

    if not media_list:
        return media_paths

    tweet_media_dir = os.path.join(media_dir, tweet_id)

    for idx, media_item in enumerate(media_list):
        media_url = media_item.get("media_url_https") or media_item.get("media_url")
        if not media_url:
            continue

        # Determine file extension
        ext = "jpg"  # Default
        if "type" in media_item:
            media_type = media_item["type"]
            if media_type == "video":
                # Try to get video URL
                video_info = media_item.get("video_info", {})
                variants = video_info.get("variants", [])
                if variants:
                    # Get the highest bitrate variant
                    best_variant = max(variants, key=lambda v: v.get("bitrate", 0))
                    media_url = best_variant.get("url", media_url)
                    ext = "mp4"
            elif media_type == "animated_gif":
                ext = "gif"

        # Extract extension from URL if possible
        parsed_url = urllib.parse.urlparse(media_url)
        path_ext = os.path.splitext(parsed_url.path)[1]
        if path_ext:
            ext = path_ext.lstrip(".")

        filename = f"media_{idx + 1}.{ext}"
        output_path = os.path.join(tweet_media_dir, filename)

        if download_file(media_url, output_path):
            media_paths.append(output_path)
            # Update tweet data with local path
            media_item["local_path"] = os.path.relpath(
                output_path, os.path.dirname(media_dir)
            )

    return media_paths


def download_article_media(
    article_data: Dict, tweet_id: str, media_dir: str, output_dir: str
) -> None:
    """
    Download images embedded in an article: the cover image and any inline
    media blocks in the article body. Sets ``local_path`` in-place on each
    media item so the Rust archiver can rewrite paths into the content store.

    Args:
        article_data: Article dict produced by extract_article_data()
        tweet_id: ID of the wrapper tweet (used as the media subdirectory name)
        media_dir: Root media directory (e.g. ``{temp_dir}/media``)
        output_dir: Directory where tweet JSON files are written; used to
                    compute relative paths consistent with the rest of the scraper
    """
    article_media_dir = os.path.join(media_dir, tweet_id)
    # Paths are stored relative to the parent of media_dir (i.e. temp_dir),
    # matching the convention used by download_tweet_media.
    rel_base = os.path.dirname(media_dir)

    def _ext_from_url(url: str) -> str:
        parsed = urllib.parse.urlparse(url)
        ext = os.path.splitext(parsed.path)[1].lstrip(".")
        return ext if ext else "jpg"

    # --- Cover image ---
    cover = article_data.get("cover_media", {})
    cover_url = cover.get("url", "")
    if cover_url and not cover.get("local_path"):
        ext = _ext_from_url(cover_url)
        output_path = os.path.join(article_media_dir, f"cover.{ext}")
        if download_file(cover_url, output_path):
            cover["local_path"] = os.path.relpath(output_path, rel_base)

    # --- Inline block images ---
    for block in article_data.get("blocks", []):
        for entity in block.get("resolved_entities", []):
            if entity.get("type") != "media":
                continue
            url = entity.get("url", "")
            if not url or entity.get("local_path"):
                continue
            media_id = entity.get("media_id", "")
            ext = _ext_from_url(url)
            filename = f"article_{media_id}.{ext}" if media_id else f"article_img.{ext}"
            output_path = os.path.join(article_media_dir, filename)
            if download_file(url, output_path):
                entity["local_path"] = os.path.relpath(output_path, rel_base)


def download_avatar(avatar_url: str, author_id: str, avatars_dir: str) -> Optional[str]:
    """
    Download avatar image for an author.

    Args:
        avatar_url: URL of the avatar image
        author_id: Author's user ID
        avatars_dir: Directory to save avatars

    Returns:
        Local file path if successful, None otherwise
    """
    if not avatar_url:
        return None

    # Determine file extension
    ext = "jpg"  # Default
    parsed_url = urllib.parse.urlparse(avatar_url)
    path_ext = os.path.splitext(parsed_url.path)[1]
    if path_ext:
        ext = path_ext.lstrip(".")

    # Remove '_normal' from filename to get higher resolution if available
    avatar_url_hq = avatar_url.replace("_normal", "")

    filename = f"{author_id}.{ext}"
    output_path = os.path.join(avatars_dir, filename)

    # Try high quality first, fallback to normal
    if download_file(avatar_url_hq, output_path):
        return output_path
    elif download_file(avatar_url, output_path):
        return output_path

    return None


def fetch_tweet_by_id(
    scraper: Scraper,
    tweet_id: str,
    retry_count: int = 0,
    delay_between_requests: float = 2.0,
) -> Optional[Dict]:
    """
    Fetch a single tweet by ID with rate limit handling.

    Uses the twitter-api-client library's methods to fetch tweet details.
    Tries multiple approaches to handle different library versions.

    Args:
        scraper: Scraper instance
        tweet_id: Tweet ID to fetch
        retry_count: Current retry count
        delay_between_requests: Delay between requests

    Returns:
        Tweet result dictionary or None if not found
    """
    try:
        response_data = None
        last_error = None

        # Method 4: Try using the scraper's session directly to make a GraphQL request
        if hasattr(scraper, "session"):
            try:
                # Use the TweetDetail GraphQL endpoint
                # The endpoint hash might vary, but this is a common one
                url = "https://twitter.com/i/api/graphql/rU08O-YiXdr0IZfE7qaUMg/TweetDetail"
                variables = {
                    "focalTweetId": tweet_id,
                    "with_rux_injections": False,
                    "rankingMode": "Relevance",
                    "includePromotedContent": True,
                    "withCommunity": True,
                    "withQuickPromoteEligibilityTweetFields": True,
                    "withBirdwatchNotes": True,
                    "withVoice": True,
                }

                features = {
                    "rweb_video_screen_enabled": False,
                    "profile_label_improvements_pcf_label_in_post_enabled": True,
                    "responsive_web_profile_redirect_enabled": False,
                    "rweb_tipjar_consumption_enabled": False,
                    "verified_phone_label_enabled": False,
                    "creator_subscriptions_tweet_preview_api_enabled": True,
                    "responsive_web_graphql_timeline_navigation_enabled": True,
                    "responsive_web_graphql_skip_user_profile_image_extensions_enabled": False,
                    "premium_content_api_read_enabled": False,
                    "communities_web_enable_tweet_community_results_fetch": True,
                    "c9s_tweet_anatomy_moderator_badge_enabled": True,
                    "responsive_web_grok_analyze_button_fetch_trends_enabled": False,
                    "responsive_web_grok_analyze_post_followups_enabled": True,
                    "responsive_web_jetfuel_frame": True,
                    "responsive_web_grok_share_attachment_enabled": True,
                    "responsive_web_grok_annotations_enabled": True,
                    "articles_preview_enabled": True,
                    "responsive_web_edit_tweet_api_enabled": True,
                    "graphql_is_translatable_rweb_tweet_is_translatable_enabled": True,
                    "view_counts_everywhere_api_enabled": True,
                    "longform_notetweets_consumption_enabled": True,
                    "responsive_web_twitter_article_tweet_consumption_enabled": True,
                    "content_disclosure_indicator_enabled": True,
                    "content_disclosure_ai_generated_indicator_enabled": True,
                    "responsive_web_grok_show_grok_translated_post": False,
                    "responsive_web_grok_analysis_button_from_backend": True,
                    "post_ctas_fetch_enabled": True,
                    "freedom_of_speech_not_reach_fetch_enabled": True,
                    "standardized_nudges_misinfo": True,
                    "tweet_with_visibility_results_prefer_gql_limited_actions_policy_enabled": True,
                    "longform_notetweets_rich_text_read_enabled": True,
                    "longform_notetweets_inline_media_enabled": False,
                    "responsive_web_grok_image_annotation_enabled": True,
                    "responsive_web_grok_imagine_annotation_enabled": True,
                    "responsive_web_grok_community_note_auto_translation_is_enabled": False,
                    "responsive_web_enhance_cards_enabled": False,
                }

                field_toggles = {
                    "withArticleRichContentState": True,
                    "withArticlePlainText": True,
                    "withArticleSummaryText": True,
                    "withArticleVoiceOver": True,
                    "withGrokAnalyze": False,
                    "withDisallowedReplyControls": False,
                }
                params = {
                    "variables": json.dumps(variables),
                    "features": json.dumps(features),
                    "fieldToggles": json.dumps(field_toggles),
                }
                response = scraper.session.get(url, params=params)
                if response.status_code == 200:
                    response_data = response.json()
                    if response_data:
                        print(f"  ✓ Fetched using direct GraphQL request")
                else:
                    error_text = (
                        response.text[:200]
                        if hasattr(response, "text") and response.text
                        else str(response.status_code)
                    )
                    last_error = Exception(
                        f"GraphQL request failed with status {response.status_code}: {error_text}"
                    )
                    if retry_count == 0:
                        print(f"  ⚠ Debug: Direct GraphQL request failed: {last_error}")
            except Exception as e:
                last_error = e
                pass

        # Try different methods based on what's available in the library
        # Method 1: Try tweets_details() if available (note: plural "tweets")
        if response_data is None and hasattr(scraper, "tweets_details"):
            try:
                response_data = scraper.tweets_details([tweet_id])
                if response_data:
                    print(f"  ✓ Fetched using tweets_details()")
            except Exception as e:
                last_error = e
                if retry_count == 0:
                    print(f"  ⚠ tweets_details() failed: {e}")
                pass

        if response_data is None:
            # Debug: print available methods
            available_methods = [
                m
                for m in dir(scraper)
                if not m.startswith("_") and callable(getattr(scraper, m, None))
            ]
            print(
                f"  ⚠ Debug: Available scraper methods: {', '.join(available_methods[:10])}..."
            )
            if last_error:
                print(f"  ⚠ Debug: Last error: {last_error}")
            error_msg = f"Could not fetch tweet {tweet_id} using any available method. "
            error_msg += (
                f"Tried: tweets_details, tweet, graphql, direct GraphQL request. "
            )
            if last_error:
                error_msg += f"Last error: {last_error}"
            raise Exception(error_msg)

        # Extract tweet from response
        tweet_result = extract_tweet_from_response(response_data, tweet_id)

        if tweet_result:
            return tweet_result
        else:
            # Debug: print response structure
            print(
                f"  ⚠ Debug: Response structure keys: {list(response_data.keys()) if isinstance(response_data, dict) else 'Not a dict'}"
            )
            if isinstance(response_data, list) and len(response_data) > 0:
                print(
                    f"  ⚠ Debug: Response is list, first item keys: {list(response_data[0].keys()) if isinstance(response_data[0], dict) else 'Not a dict'}"
                )
            print(f"  ⚠ Warning: Tweet {tweet_id} not found in response")
            return None

    except Exception as e:
        error_msg = str(e)

        # Check if it's a rate limit error
        if is_rate_limit_error(e):
            wait_time = handle_rate_limit_error(e, retry_count)
            time.sleep(wait_time)
            if retry_count < 5:  # Max 5 retries for rate limits
                return fetch_tweet_by_id(
                    scraper, tweet_id, retry_count + 1, delay_between_requests
                )
            else:
                print(f"  ❌ Max retries reached for tweet {tweet_id}")
                return None
        else:
            # For other errors, retry once
            if retry_count < 1:
                time.sleep(delay_between_requests * 3)
                return fetch_tweet_by_id(
                    scraper, tweet_id, retry_count + 1, delay_between_requests
                )
            else:
                print(f"  ⚠ Warning: Error fetching tweet {tweet_id}: {error_msg}")
                return None


def extract_related_tweet_ids(tweet_data: Dict) -> List[str]:
    """
    Extract related tweet IDs (quoted, retweeted, replied-to) from tweet data.

    Args:
        tweet_data: Tweet data dictionary

    Returns:
        List of related tweet IDs
    """
    related_ids = []

    # Check for quoted status
    quoted_status_id = tweet_data.get("quoted_status_id")
    if quoted_status_id:
        related_ids.append(quoted_status_id)

    # Check for retweeted status
    retweeted_status = tweet_data.get("retweeted_status")
    if retweeted_status:
        retweet_id = retweeted_status.get("id")
        if retweet_id:
            related_ids.append(retweet_id)

    # Check for replied-to status
    in_reply_to_status_id = tweet_data.get("in_reply_to_status_id")
    if in_reply_to_status_id:
        related_ids.append(in_reply_to_status_id)

    return related_ids


def scrape_tweets_recursive(
    scraper: Scraper,
    tweet_id: str,
    scraped_tweets: Dict[str, Dict],
    output_dir: str,
    media_dir: str,
    avatars_dir: str,
    depth: int,
    max_depth: int,
    bare_scrape: bool,
    advanced_info: bool,
    download_media: bool,
    download_avatars: bool,
    recursive: bool,
    scrape_replied_to_tweet: bool,
    recursive_replied_to_tweets: bool,
    recursive_replied_to_tweets_quotes_retweets: bool,
    download_replied_to_tweets_media: bool,
    max_replied_to_tweets_recursion_depth: int,
    delay_between_requests: float,
    replied_to_depth: int = 0,
) -> None:
    """
    Recursively scrape tweets (quoted, retweeted, replied-to).

    Args:
        scraper: Scraper instance
        tweet_id: Tweet ID to scrape
        scraped_tweets: Dictionary of already scraped tweets
        output_dir: Output directory for JSON files
        media_dir: Media directory
        avatars_dir: Avatars directory
        depth: Current recursion depth
        max_depth: Maximum recursion depth
        bare_scrape: Whether to do bare scraping
        advanced_info: Whether to include advanced info
        download_media: Whether to download media
        download_avatars: Whether to download avatars
        recursive: Whether to recursively scrape quotes/retweets
        scrape_replied_to_tweet: Whether to scrape replied-to tweets
        recursive_replied_to_tweets: Whether to recursively scrape replied-to tweets
        recursive_replied_to_tweets_quotes_retweets: Whether to scrape quotes/retweets of replied-to tweets
        download_replied_to_tweets_media: Whether to download media for replied-to tweets
        max_replied_to_tweets_recursion_depth: Max depth for replied-to tweets
        delay_between_requests: Delay between requests
        replied_to_depth: Current replied-to recursion depth
    """
    # Skip if already scraped
    if tweet_id in scraped_tweets:
        return

    # Check depth limits
    if depth >= max_depth:
        return

    if replied_to_depth >= max_replied_to_tweets_recursion_depth:
        return

    # Fetch tweet
    print(f"  {'  ' * depth}→ Fetching tweet {tweet_id}...")
    tweet_result = fetch_tweet_by_id(
        scraper, tweet_id, delay_between_requests=delay_between_requests
    )

    if not tweet_result:
        print(
            f"  {'  ' * depth}⚠ Warning: Could not fetch tweet {tweet_id} (deleted or private?)"
        )
        return

    # Extract tweet data
    is_replied_to_tweet = replied_to_depth > 0
    current_bare_scrape = bare_scrape and not is_replied_to_tweet
    current_advanced_info = advanced_info and not is_replied_to_tweet

    tweet_data = extract_tweet_data(
        tweet_result,
        bare_scrape=current_bare_scrape,
        advanced_info=current_advanced_info,
    )

    # Download avatar if enabled
    if download_avatars and not is_replied_to_tweet:
        author_id = tweet_data.get("author", {}).get("id")
        avatar_url = tweet_data.get("author", {}).get("avatar_url", "")
        if author_id and avatar_url:
            avatar_path = download_avatar(avatar_url, author_id, avatars_dir)
            if avatar_path:
                tweet_data["author"]["avatar_local_path"] = os.path.relpath(
                    avatar_path, output_dir
                )

    # Download media if enabled
    should_download_media = download_media and not is_replied_to_tweet
    if not should_download_media and is_replied_to_tweet:
        should_download_media = download_replied_to_tweets_media

    if should_download_media:
        download_tweet_media(tweet_data, tweet_id, media_dir)
        if tweet_data.get("is_article") and tweet_data.get("article"):
            download_article_media(tweet_data["article"], tweet_id, media_dir, output_dir)

    # Save tweet to JSON file
    json_file = os.path.join(output_dir, f"tweet-{tweet_id}.json")
    try:
        with open(json_file, "w") as f:
            json.dump(tweet_data, f, indent=2)
    except Exception as e:
        print(
            f"  {'  ' * depth}⚠ Warning: Failed to save JSON file for tweet {tweet_id}: {e}"
        )
        return

    # Mark as scraped
    scraped_tweets[tweet_id] = tweet_data

    # Rate limiting
    if delay_between_requests > 0:
        time.sleep(delay_between_requests)

    # Recursively scrape related tweets
    if recursive and depth < max_depth - 1:
        related_ids = extract_related_tweet_ids(tweet_data)

        for related_id in related_ids:
            if related_id not in scraped_tweets:
                scrape_tweets_recursive(
                    scraper,
                    related_id,
                    scraped_tweets,
                    output_dir,
                    media_dir,
                    avatars_dir,
                    depth + 1,
                    max_depth,
                    bare_scrape,
                    advanced_info,
                    download_media,
                    download_avatars,
                    recursive,
                    scrape_replied_to_tweet,
                    recursive_replied_to_tweets,
                    recursive_replied_to_tweets_quotes_retweets,
                    download_replied_to_tweets_media,
                    max_replied_to_tweets_recursion_depth,
                    delay_between_requests,
                    replied_to_depth,
                )

    # Handle replied-to tweets
    if scrape_replied_to_tweet or recursive_replied_to_tweets:
        in_reply_to_status_id = tweet_data.get("in_reply_to_status_id")
        if in_reply_to_status_id and in_reply_to_status_id not in scraped_tweets:
            new_replied_to_depth = (
                replied_to_depth + 1
                if recursive_replied_to_tweets
                else replied_to_depth
            )

            # Determine if we should recursively scrape quotes/retweets of replied-to tweets
            should_recurse_quotes_retweets = (
                recursive_replied_to_tweets_quotes_retweets
                and new_replied_to_depth < max_replied_to_tweets_recursion_depth
            )

            scrape_tweets_recursive(
                scraper,
                in_reply_to_status_id,
                scraped_tweets,
                output_dir,
                media_dir,
                avatars_dir,
                depth,
                max_depth,
                bare_scrape,
                advanced_info,
                download_media,
                download_avatars,
                should_recurse_quotes_retweets,
                scrape_replied_to_tweet,
                recursive_replied_to_tweets,
                recursive_replied_to_tweets_quotes_retweets,
                download_replied_to_tweets_media,
                max_replied_to_tweets_recursion_depth,
                delay_between_requests,
                new_replied_to_depth,
            )


def load_scraped_tweets(output_dir: str) -> Dict[str, Dict]:
    """
    Load already scraped tweets from JSON files (for resume capability).

    Args:
        output_dir: Output directory

    Returns:
        Dictionary mapping tweet IDs to tweet data
    """
    scraped_tweets = {}

    if not os.path.exists(output_dir):
        return scraped_tweets

    for filename in os.listdir(output_dir):
        if filename.startswith("tweet-") and filename.endswith(".json"):
            tweet_id = filename[6:-5]  # Remove 'tweet-' prefix and '.json' suffix
            scraped_tweets[tweet_id] = {"id": tweet_id}  # Mark as scraped

    return scraped_tweets


def main():
    """Main function."""
    parser = argparse.ArgumentParser(
        description="Extract tweet contents from Tweet IDs and save as JSON files."
    )

    # Tweet ID inputs
    parser.add_argument(
        "--tweet-ids",
        type=str,
        help='Comma-separated Tweet IDs, e.g. "12345,67890,13579"',
    )
    parser.add_argument(
        "--tweet-ids-file",
        type=str,
        help="Path(s) to file(s) containing Tweet IDs (comma-separated), "
        'e.g. "path/to/tweet_ids.txt,path/to/second/file.json"',
    )

    # Output directories
    parser.add_argument(
        "--output-dir",
        type=str,
        default="scraped-tweets",
        help="Directory to save tweet JSON files (default: scraped-tweets)",
    )
    parser.add_argument(
        "--media-dir",
        type=str,
        help="Directory to save media files (default: <output-dir>/media)",
    )

    # Media and avatar downloads
    parser.add_argument(
        "--download-media",
        action="store_true",
        help="Download media files (images, videos, GIFs) attached to tweets",
    )
    avatar_group = parser.add_mutually_exclusive_group()
    avatar_group.add_argument(
        "--download-avatars",
        action="store_true",
        default=True,
        help="Download avatars of tweet authors (default: True)",
    )
    avatar_group.add_argument(
        "--no-download-avatars",
        dest="download_avatars",
        action="store_false",
        help="Do not download avatars",
    )

    # Recursion settings
    recursion_group = parser.add_mutually_exclusive_group()
    recursion_group.add_argument(
        "--recursive",
        action="store_true",
        default=True,
        help="Recursively extract quoted or retweeted tweets (default: True)",
    )
    recursion_group.add_argument(
        "--no-recursive",
        dest="recursive",
        action="store_false",
        help="Do not recursively extract quoted or retweeted tweets",
    )
    parser.add_argument(
        "--max-recursion-depth",
        type=int,
        default=10,
        help="Maximum recursion depth for quoted/retweeted tweets (default: 10)",
    )

    # Replied-to tweet settings
    parser.add_argument(
        "--scrape-replied-to-tweet",
        action="store_true",
        help="Also extract the tweet that the author replied to",
    )
    parser.add_argument(
        "--recursive-replied-to-tweets",
        action="store_true",
        help="Recursively extract replied-to tweets",
    )
    parser.add_argument(
        "--recursive-replied-to-tweets-quotes-retweets",
        action="store_true",
        help="Recursively extract quoted or retweeted tweets of replied-to tweets",
    )
    parser.add_argument(
        "--download-replied-to-tweets-media",
        action="store_true",
        help="Download media for replied-to tweets as well",
    )
    parser.add_argument(
        "--max-replied-to-tweets-recursion-depth",
        type=int,
        default=5,
        help="Maximum depth for replied-to tweets recursion (default: 5)",
    )

    # Scraping modes
    parser.add_argument(
        "--advanced-info",
        action="store_true",
        help="Extract additional optional information about tweets",
    )
    parser.add_argument(
        "--bare-scrape",
        action="store_true",
        help="Only extract bare minimum information about tweets",
    )

    # Rate limiting
    parser.add_argument(
        "--delay-between-requests",
        type=float,
        default=2.0,
        help="Delay in seconds between requests (default: 2.0)",
    )

    # Credentials
    parser.add_argument(
        "--credentials-file",
        type=str,
        help="Path to credentials file (default: creds.txt in current directory)",
    )
    parser.add_argument(
        "--credentials-string",
        type=str,
        help="Credentials string directly (cannot be used with --credentials-file)",
    )

    args = parser.parse_args()

    # Validate arguments
    if not args.tweet_ids and not args.tweet_ids_file:
        parser.error("Either --tweet-ids or --tweet-ids-file must be provided")

    if args.bare_scrape and args.advanced_info:
        parser.error("--bare-scrape and --advanced-info are mutually exclusive")

    if args.credentials_file and args.credentials_string:
        parser.error(
            "--credentials-file and --credentials-string cannot be specified at the same time"
        )

    # Parse tweet IDs
    print("Parsing tweet IDs...")
    tweet_ids = parse_tweet_ids_from_args(args.tweet_ids, args.tweet_ids_file)

    if not tweet_ids:
        print("❌ No tweet IDs found. Exiting.")
        return

    print(f"✓ Found {len(tweet_ids)} unique tweet ID(s)")

    # Set up directories
    output_dir = os.path.abspath(args.output_dir)
    os.makedirs(output_dir, exist_ok=True)

    if args.media_dir:
        media_dir = os.path.abspath(args.media_dir)
    else:
        media_dir = os.path.join(output_dir, "media")

    avatars_dir = os.path.join(media_dir, "avatars")
    os.makedirs(avatars_dir, exist_ok=True)

    # Load cookies
    if args.credentials_string:
        # Use credentials string directly
        cookie_str = args.credentials_string.strip()
    elif args.credentials_file:
        # Use specified credentials file
        creds_file = os.path.abspath(args.credentials_file)
        if not os.path.exists(creds_file):
            print(f"❌ Error: Credentials file not found: {creds_file}")
            return
        with open(creds_file, "r") as f:
            cookie_str = f.read().strip()
    else:
        # Default: look for creds.txt in current directory
        creds_file = os.path.join(os.getcwd(), "creds.txt")
        if not os.path.exists(creds_file):
            print(
                f"❌ Error: creds.txt not found in current directory ({os.getcwd()}). "
                f"Please create it with your Twitter cookies, or use --credentials-file or --credentials-string."
            )
            return
        with open(creds_file, "r") as f:
            cookie_str = f.read().strip()

    # Parse cookie string into dictionary
    cookie_dict = dict(item.split("=", 1) for item in cookie_str.split(";"))

    # Initialize scraper
    scraper = Scraper(cookies=cookie_dict, save=False)

    # Load already scraped tweets (for resume)
    scraped_tweets = load_scraped_tweets(output_dir)
    initial_count = len(scraped_tweets)

    if initial_count > 0:
        print(f"✓ Found {initial_count} already scraped tweet(s), resuming...")

    # Filter out already scraped tweets
    remaining_tweet_ids = [tid for tid in tweet_ids if tid not in scraped_tweets]

    if not remaining_tweet_ids:
        print("✓ All tweets already scraped!")
        return

    print(f"→ Scraping {len(remaining_tweet_ids)} new tweet(s)...")
    print("-" * 80)

    # Track statistics
    stats = {
        "total_requested": len(tweet_ids),
        "already_scraped": initial_count,
        "newly_scraped": 0,
        "failed": 0,
        "start_time": datetime.now(),
    }

    # Scrape tweets
    for idx, tweet_id in enumerate(remaining_tweet_ids, 1):
        print(f"\n[{idx}/{len(remaining_tweet_ids)}] Processing tweet {tweet_id}...")

        try:
            scrape_tweets_recursive(
                scraper,
                tweet_id,
                scraped_tweets,
                output_dir,
                media_dir,
                avatars_dir,
                depth=0,
                max_depth=args.max_recursion_depth,
                bare_scrape=args.bare_scrape,
                advanced_info=args.advanced_info,
                download_media=args.download_media,
                download_avatars=args.download_avatars,
                recursive=args.recursive,
                scrape_replied_to_tweet=args.scrape_replied_to_tweet,
                recursive_replied_to_tweets=args.recursive_replied_to_tweets,
                recursive_replied_to_tweets_quotes_retweets=args.recursive_replied_to_tweets_quotes_retweets,
                download_replied_to_tweets_media=args.download_replied_to_tweets_media,
                max_replied_to_tweets_recursion_depth=args.max_replied_to_tweets_recursion_depth,
                delay_between_requests=args.delay_between_requests,
            )
            stats["newly_scraped"] += 1
        except Exception as e:
            print(f"  ❌ Error processing tweet {tweet_id}: {e}")
            stats["failed"] += 1

    # Calculate final statistics
    stats["end_time"] = datetime.now()
    stats["duration"] = (stats["end_time"] - stats["start_time"]).total_seconds()
    stats["total_scraped"] = len(scraped_tweets)

    # Save summary
    summary = {
        "scraping_summary": {
            "total_requested": stats["total_requested"],
            "already_scraped": stats["already_scraped"],
            "newly_scraped": stats["newly_scraped"],
            "failed": stats["failed"],
            "total_scraped": stats["total_scraped"],
            "start_time": stats["start_time"].isoformat(),
            "end_time": stats["end_time"].isoformat(),
            "duration_seconds": stats["duration"],
            "output_directory": output_dir,
            "media_directory": media_dir,
            "settings": {
                "recursive": args.recursive,
                "max_recursion_depth": args.max_recursion_depth,
                "bare_scrape": args.bare_scrape,
                "advanced_info": args.advanced_info,
                "download_media": args.download_media,
                "download_avatars": args.download_avatars,
                "scrape_replied_to_tweet": args.scrape_replied_to_tweet,
                "recursive_replied_to_tweets": args.recursive_replied_to_tweets,
                "max_replied_to_tweets_recursion_depth": args.max_replied_to_tweets_recursion_depth,
            },
        }
    }

    summary_file = os.path.join(output_dir, "scraping_summary.json")
    with open(summary_file, "w") as f:
        json.dump(summary, f, indent=2)

    # Print final summary
    print(f"\n{'=' * 80}")
    print("Scraping complete!")
    print(f"  Total requested: {stats['total_requested']}")
    print(f"  Already scraped: {stats['already_scraped']}")
    print(f"  Newly scraped: {stats['newly_scraped']}")
    print(f"  Failed: {stats['failed']}")
    print(f"  Total scraped: {stats['total_scraped']}")
    print(
        f"  Duration: {stats['duration']:.1f}s ({stats['duration'] / 60:.1f} minutes)"
    )
    print(f"  Output directory: {output_dir}")
    print(f"  Summary saved to: {summary_file}")
    print(f"{'=' * 80}\n")


if __name__ == "__main__":
    main()
