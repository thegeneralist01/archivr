//! Markdown rendering for entry exports.

use std::path::PathBuf;

use crate::export::model::{Asset, ExportBody, ExportEntry, Post, Span, ext_of_s};
use crate::export::html::{entry_title, source_label};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn fmt_date(archived_at: &str) -> String {
    use chrono::DateTime;
    if let Ok(dt) = DateTime::parse_from_rfc3339(archived_at) {
        dt.format("%-d %B %Y").to_string()
    } else {
        archived_at.to_string()
    }
}

fn fmt_timestamp(secs: i64) -> String {
    use chrono::{DateTime, Utc};
    let dt = DateTime::<Utc>::from_timestamp(secs, 0)
        .unwrap_or_else(|| DateTime::<Utc>::from_timestamp(0, 0).unwrap());
    dt.format("%-d %B %Y").to_string()
}

/// Escape `[` and `]` in link text.
fn esc_link(s: &str) -> String {
    s.replace('[', "\\[").replace(']', "\\]")
}

/// Normalise a title for headings (no newlines).
fn normalise_title(s: &str) -> String {
    s.replace('\n', " ").replace('\r', "")
}

/// URL hostname (no www.).
fn url_host(url: &str) -> Option<String> {
    let after = url.strip_prefix("http://").or_else(|| url.strip_prefix("https://"))?;
    let host = after.split('/').next()?;
    Some(host.strip_prefix("www.").unwrap_or(host).to_string())
}

/// Asset src: `assets/<name>`.
fn asset_ref(asset: &Asset) -> String {
    format!("assets/{}", asset.name)
}

/// Poster src.
fn poster_ref(poster: &std::path::Path) -> String {
    poster
        .file_name()
        .map(|n| format!("assets/{}", n.to_string_lossy()))
        .unwrap_or_default()
}

fn fenced_plain_text(text: &str) -> String {
    let mut longest_run = 0;
    let mut run = 0;
    for byte in text.bytes() {
        if byte == b'`' {
            run += 1;
            longest_run = longest_run.max(run);
        } else {
            run = 0;
        }
    }
    let fence = "`".repeat((longest_run + 1).max(3));
    let trailing_newline = if text.ends_with('\n') { "" } else { "\n" };
    format!("{fence}\n{text}{trailing_newline}{fence}\n")
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Render a Markdown document for one or more entries.
pub(crate) fn render_markdown(
    entries: &[ExportEntry],
    posters: &std::collections::HashMap<String, PathBuf>,
    include_credit: bool,
) -> String {
    let together = entries.len() > 1;
    let mut out = String::new();

    if together {
        out.push_str(&format!("# {} archived entries\n\n", entries.len()));
        for (i, entry) in entries.iter().enumerate() {
            let title = normalise_title(&entry_title(entry));
            out.push_str(&format!("- [{title}](#{anchor})\n", anchor = entry_anchor(i)));
        }
        out.push('\n');
        for (i, entry) in entries.iter().enumerate() {
            out.push_str(&format!(
                "<a id=\"{}\"></a>\n\n",
                entry_anchor(i)
            ));
            out.push_str(&render_entry_body(entry, 2, posters));
        }
    } else {
        out.push_str(&render_entry_body(&entries[0], 1, posters));
    }

    if include_credit {
        out.push_str("\n\n---\n\n<sub>Preserved with [archivr](https://github.com/thegeneralist01/archivr)</sub>\n");
    }

    out
}

fn entry_anchor(index: usize) -> String {
    format!("entry-{index}")
}

/// Render a single entry as Markdown, starting with a heading at `level`.
fn render_entry_body(
    entry: &ExportEntry,
    level: usize,
    posters: &std::collections::HashMap<String, PathBuf>,
) -> String {
    let title = normalise_title(&entry_title(entry));
    let label = source_label(&entry.summary.source_kind, &entry.summary.entity_kind);
    let date = fmt_date(&entry.summary.archived_at);

    let heading = "#".repeat(level);

    let url_part = entry
        .summary
        .original_url
        .as_deref()
        .filter(|u| u.starts_with("http://") || u.starts_with("https://"))
        .map(|url| {
            let host = url_host(url).unwrap_or_else(|| url.to_string());
            format!(" · [{host}]({url})")
        })
        .unwrap_or_default();

    let mut out = format!(
        "{heading} {title}\n\n*{label} · Archived {date}{url_part}*\n\n",
        heading = heading,
        title = esc_link(&title),
        label = esc_link(&label),
        url_part = url_part,
    );

    out.push_str(&render_body_md(&entry.body, level + 1, posters, &title));
    out.push('\n');
    out
}

fn render_body_md(
    body: &ExportBody,
    child_level: usize,
    posters: &std::collections::HashMap<String, PathBuf>,
    entry_title: &str,
) -> String {
    match body {
        ExportBody::Video(asset) => {
            let video_ref = asset_ref(asset);
            if let Some(poster) = posters.get(&asset.name) {
                let p = poster_ref(poster);
                format!("[![Video]({p})]({video_ref})\n")
            } else {
                format!("[Video file]({video_ref})\n")
            }
        }
        ExportBody::Audio(asset) => format!("[Audio file]({})\n", asset_ref(asset)),
        ExportBody::Image(asset) => {
            format!("![{title}]({src})\n", title = esc_link(entry_title), src = asset_ref(asset))
        }
        ExportBody::Pdf(asset) => format!("[PDF document]({})\n", asset_ref(asset)),
        ExportBody::Page(asset) => format!("[Archived page snapshot]({})\n", asset_ref(asset)),
        ExportBody::Text { mime_markdown, text } => {
            if *mime_markdown {
                format!("{text}\n")
            } else {
                fenced_plain_text(text)
            }
        }
        ExportBody::TextTooLarge(asset) | ExportBody::File(asset) => {
            let ext = ext_of_s(&asset.name).to_uppercase();
            format!("[Download {ext} file]({})\n", asset_ref(asset))
        }
        ExportBody::Posts(posts) => render_posts_md(posts, posters),
        ExportBody::Container(children) => {
            render_container_md(children, child_level, posters)
        }
        ExportBody::Empty => "*No archived content is available for this entry.*\n".to_string(),
    }
}

fn render_posts_md(
    posts: &[Post],
    posters: &std::collections::HashMap<String, PathBuf>,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    for post in posts {
        parts.push(render_post_md(post, posters));
    }
    parts.join("\n\n---\n\n")
}

fn render_post_md(
    post: &Post,
    posters: &std::collections::HashMap<String, PathBuf>,
) -> String {
    let mut out = String::new();

    let date = post
        .created_at_secs
        .map(fmt_timestamp)
        .unwrap_or_default();

    let handle_part = post
        .handle
        .as_deref()
        .map(|h| format!(" @{h}"))
        .unwrap_or_default();
    let date_part = if date.is_empty() { String::new() } else { format!(" · {date}") };

    out.push_str(&format!(
        "**{name}**{handle_part}{date_part}\n\n",
        name = post.name,
    ));

    // Body.
    if let Some((title, body, cover)) = &post.article {
        if let Some(t) = title {
            out.push_str(&format!("**{t}**\n\n"));
        }
        if let Some(c) = cover {
            out.push_str(&format!("![]({})\n\n", asset_ref(c)));
        }
        out.push_str(body);
        out.push('\n');
    } else if !post.spans.is_empty() {
        for span in &post.spans {
            match span {
                Span::Text(t) => out.push_str(t),
                Span::Link { text, href }
                    if href.starts_with("http://") || href.starts_with("https://") =>
                {
                    out.push_str(&format!("[{text}]({href})"));
                }
                Span::Link { text, .. } => out.push_str(text),
            }
        }
        out.push('\n');
    }

    // Photos.
    for photo in &post.photos {
        out.push_str(&format!("\n![]({})", asset_ref(photo)));
    }

    // Videos.
    for (video, _) in &post.videos {
        let video_ref = asset_ref(video);
        if let Some(poster) = posters.get(&video.name) {
            let p = poster_ref(poster);
            out.push_str(&format!("\n[![Video]({p})]({video_ref})"));
        } else {
            out.push_str(&format!("\n[Video]({video_ref})"));
        }
    }

    // Quote.
    if let Some(quote) = &post.quote {
        let quoted = render_post_md(quote, posters);
        for line in quoted.lines() {
            out.push_str(&format!("\n> {line}"));
        }
        out.push('\n');
    }

    // Status link.
    if let Some(url) = &post.status_url {
        out.push_str(&format!("\n[View on X]({url})\n"));
    }

    out
}

fn render_container_md(
    children: &[ExportEntry],
    level: usize,
    posters: &std::collections::HashMap<String, PathBuf>,
) -> String {
    let mut out = String::new();
    for child in children {
        let title = normalise_title(&entry_title(child));
        let date = fmt_date(&child.summary.archived_at);
        let heading = "#".repeat(level);
        out.push_str(&format!("{heading} {title}\n\n*Archived {date}*\n\n"));
        out.push_str(&render_body_md(&child.body, level + 1, posters, &title));
        out.push('\n');
    }
    out
}
