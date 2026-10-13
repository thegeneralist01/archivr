//! HTML rendering for entry exports.

use std::path::PathBuf;

use crate::export::brand;
use crate::export::model::{Asset, ExportBody, ExportEntry, Post, Span, ext_of_s};

// ---------------------------------------------------------------------------
// Asset reference mode
// ---------------------------------------------------------------------------

/// How asset `src` / `href` values are built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AssetMode {
    /// `assets/<name>` — for ZIP bundles.
    Bundle,
}

pub(crate) fn asset_src(mode: AssetMode, asset: &Asset) -> String {
    match mode {
        AssetMode::Bundle => format!("assets/{}", asset.name),
    }
}



// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// HTML-escape `& < > " '`.
pub(crate) fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// Returns `Some(s)` only for `http://` and `https://` URLs.
fn safe_href(s: &str) -> Option<&str> {
    if s.starts_with("http://") || s.starts_with("https://") {
        Some(s)
    } else {
        None
    }
}

/// Format `archived_at` (RFC 3339) as `"%-d %B %Y"`.
fn fmt_date(archived_at: &str) -> String {
    use chrono::DateTime;
    if let Ok(dt) = DateTime::parse_from_rfc3339(archived_at) {
        dt.format("%-d %B %Y").to_string()
    } else {
        archived_at.to_string()
    }
}

/// Format Unix seconds as `"%-d %B %Y"`.
fn fmt_timestamp(secs: i64) -> String {
    use chrono::{DateTime, Utc};
    let dt = DateTime::<Utc>::from_timestamp(secs, 0)
        .unwrap_or_else(|| DateTime::<Utc>::from_timestamp(0, 0).unwrap());
    dt.format("%-d %B %Y").to_string()
}

/// `"{Source} · {Kind}"`, or just `"{Source}"` when the kind is empty/unknown.
pub(crate) fn source_label(source_kind: &str, entity_kind: &str) -> String {
    let src = match source_kind {
        "youtube" => "YouTube",
        "youtube_music" => "YouTube Music",
        "spotify" => "Spotify",
        "x" | "twitter" => "X",
        "instagram" => "Instagram",
        "facebook" => "Facebook",
        "tiktok" => "TikTok",
        "reddit" => "Reddit",
        "snapchat" => "Snapchat",
        "web" => "Web",
        "local" => "File",
        "text" => "Note",
        other => {
            // Capitalize first letter.
            let mut c = other.chars();
            return match c.next() {
                None => String::new(),
                Some(f) => {
                    let mut s = f.to_uppercase().to_string();
                    s.push_str(c.as_str());
                    let kind_part = kind_label(entity_kind);
                    if kind_part.is_empty() {
                        s
                    } else {
                        format!("{s} · {kind_part}")
                    }
                }
            };
        }
    };
    let kind_part = kind_label(entity_kind);
    if kind_part.is_empty() {
        src.to_string()
    } else {
        format!("{src} · {kind_part}")
    }
}

fn kind_label(entity_kind: &str) -> &str {
    match entity_kind {
        "video" => "Video",
        "playlist" => "Playlist",
        "channel" => "Channel",
        "music" => "Track",
        "album" => "Album",
        "post" | "tweet" => "Post",
        "tweet_thread" => "Thread",
        "story" => "Story",
        "page" => "Page",
        "file" => "File",
        "document" | "unknown" | "" => "",
        other => other,
    }
}

/// Display title: `summary.title`, else `original_url`, else `"Untitled entry"`.
pub(crate) fn entry_title(entry: &ExportEntry) -> String {
    entry
        .summary
        .title
        .as_deref()
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .or_else(|| entry.summary.original_url.clone())
        .unwrap_or_else(|| "Untitled entry".to_string())
}

/// Strip a leading `www.` from a hostname.
fn strip_www(host: &str) -> &str {
    host.strip_prefix("www.").unwrap_or(host)
}

/// Return the host of an HTTP(S) URL, or `None`.
fn url_host(url: &str) -> Option<String> {
    // Very minimal: find `://` then take up to the next `/`.
    let after = url.strip_prefix("http://").or_else(|| url.strip_prefix("https://"))?;
    let host = after.split('/').next()?;
    Some(strip_www(host).to_string())
}

/// Return the file size formatted as B / KB / MB / GB (1024-based, 1 decimal).
fn fmt_size(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let kb = bytes as f64 / 1024.0;
    if kb < 1024.0 {
        return format!("{kb:.1} KB");
    }
    let mb = kb / 1024.0;
    if mb < 1024.0 {
        return format!("{mb:.1} MB");
    }
    format!("{:.1} GB", mb / 1024.0)
}

// ---------------------------------------------------------------------------
// Document rendering
// ---------------------------------------------------------------------------

/// Render a complete HTML document for one or more entries.
///
/// `mode` controls whether asset `src` values use bundle-relative paths
/// (`assets/<name>`) or absolute `file://` URLs for PDF printing.
pub(crate) fn render_document(
    entries: &[ExportEntry],
    mode: AssetMode,
    posters: &std::collections::HashMap<String, PathBuf>,
    include_credit: bool,
) -> String {
    let together = entries.len() > 1;
    let doc_title = if together {
        format!("{} archived entries", entries.len())
    } else {
        entry_title(&entries[0])
    };

    let generator_meta = if include_credit {
        r#"<meta name="generator" content="Archivr">"#
    } else {
        ""
    };

    let mut html = format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="referrer" content="no-referrer">
{generator_meta}
<title>{title}</title><style>{css}</style></head>
<body><main class="ax-doc">
"#,
        title = esc(&doc_title),
        css = brand::EXPORT_CSS,
        generator_meta = generator_meta,
    );

    if together {
        html.push_str(&render_collection_header(entries));
    }

    for (i, entry) in entries.iter().enumerate() {
        html.push_str(&render_entry(entry, i, together, mode, posters));
    }

    html.push_str("</main>");
    if include_credit {
        html.push('\n');
        html.push_str(brand::CREDIT_HTML);
        html.push('\n');
    }
    html.push_str("</body></html>\n");
    html
}


fn render_collection_header(entries: &[ExportEntry]) -> String {
    let mut s = format!(
        r#"<header class="ax-collection"><p class="ax-eyebrow">Collection</p><h1 class="ax-title">{n} archived entries</h1>
<ol class="ax-toc">"#,
        n = entries.len(),
    );
    for (i, entry) in entries.iter().enumerate() {
        let title = entry_title(entry);
        let label = source_label(&entry.summary.source_kind, &entry.summary.entity_kind);
        s.push_str(&format!(
            "<li><a href=\"#entry-{i}\">{title}</a> <span class=\"ax-muted\">{label}</span></li>",
            title = esc(&title),
            label = esc(&label),
        ));
    }
    s.push_str("</ol></header>\n");
    s
}

fn render_entry(
    entry: &ExportEntry,
    index: usize,
    together: bool,
    mode: AssetMode,
    posters: &std::collections::HashMap<String, PathBuf>,
) -> String {
    let title = entry_title(entry);
    let label = source_label(&entry.summary.source_kind, &entry.summary.entity_kind);
    let date = fmt_date(&entry.summary.archived_at);

    let id_attr = format!(r#" id="entry-{index}""#);
    let heading = if together { "h2" } else { "h1" };

    let original_url_part = match &entry.summary.original_url {
        Some(url) if safe_href(url).is_some() => {
            let host = url_host(url).unwrap_or_else(|| url.clone());
            format!(
                r#" · <a href="{url}" rel="noopener noreferrer">{host}</a>"#,
                url = esc(url),
                host = esc(&host),
            )
        }
        _ => String::new(),
    };

    let mut s = format!(
        r#"<article class="ax-entry"{id_attr}>
<header class="ax-masthead"><p class="ax-eyebrow">{label}</p>
<{heading} class="ax-title">{title}</{heading}>
<p class="ax-meta">Archived <time datetime="{dt}">{date}</time>{url_part}</p></header>
<div class="ax-body">"#,
        label = esc(&label),
        title = esc(&title),
        dt = esc(&entry.summary.archived_at),
        url_part = original_url_part,
    );

    s.push_str(&render_body(&entry.body, mode, posters, &entry.subtitles, &title, 0));
    s.push_str("</div></article>\n");
    s
}

fn render_body(
    body: &ExportBody,
    mode: AssetMode,
    posters: &std::collections::HashMap<String, PathBuf>,
    subtitles: &[Asset],
    entry_title: &str,
    depth: usize,
) -> String {
    match body {
        ExportBody::Video(asset) => render_video(asset, mode, posters, subtitles),
        ExportBody::Audio(asset) => render_audio(asset, mode),
        ExportBody::Image(asset) => render_image(asset, mode, entry_title),
        ExportBody::Pdf(asset) => render_pdf(asset, mode, entry_title),
        ExportBody::Page(asset) => render_page(asset, mode, entry_title),
        ExportBody::Text { text, .. } => {
            format!(r#"<div class="ax-text">{}</div>"#, esc(text))
        }
        ExportBody::TextTooLarge(asset) => render_download_link(asset, mode),
        ExportBody::File(asset) => render_download_link(asset, mode),
        ExportBody::Posts(posts) => render_posts(posts, mode, posters),
        ExportBody::Container(children) => render_container(children, mode, posters, depth),
        ExportBody::Empty => {
            r#"<p class="ax-empty">No archived content is available for this entry.</p>"#
                .to_string()
        }
    }
}

fn render_video(
    asset: &Asset,
    mode: AssetMode,
    posters: &std::collections::HashMap<String, PathBuf>,
    subtitles: &[Asset],
) -> String {
    let src = asset_src(mode, asset);
    let poster = posters.get(&asset.name);
    let poster_attr = poster
        .map(|p| {
            let pname = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            format!(r#" poster="assets/{pname}""#)
        })
        .unwrap_or_default();
    let tracks: String = subtitles
        .iter()
        // SRT files stay in the ZIP, but HTML video tracks require WebVTT.
        .filter(|sub| ext_of_s(&sub.name) == "vtt")
        .enumerate()
        .map(|(i, sub)| {
            let sub_src = asset_src(mode, sub);
            let label = sub.name.clone();
            let srclang_attr = label
                .split('-')
                .next()
                .map(|l| format!(r#" srclang="{l}""#))
                .unwrap_or_default();
            let default_attr = if i == 0 { r#" default"# } else { "" };
            format!(
                r#"<track kind="subtitles" src="{sub_src}" label="{label}"{srclang_attr}{default_attr}>"#,
                label = esc(&label),
            )
        })
        .collect();
    format!(
        r#"<figure class="ax-media"><video controls preload="metadata" playsinline{poster_attr} src="{src}">{tracks}</video></figure>"#,
        src = esc(&src),
    )
}

fn render_audio(asset: &Asset, mode: AssetMode) -> String {
    let src = asset_src(mode, asset);
    format!(r#"<audio controls preload="metadata" src="{}"></audio>"#, esc(&src))
}

fn render_image(asset: &Asset, mode: AssetMode, title: &str) -> String {
    let src = asset_src(mode, asset);
    format!(
        r#"<figure class="ax-media"><img src="{src}" alt="{alt}" loading="lazy"></figure>"#,
        src = esc(&src),
        alt = esc(title),
    )
}

fn render_pdf(asset: &Asset, mode: AssetMode, _title: &str) -> String {
    let src = asset_src(mode, asset);
    format!(
        r#"<object class="ax-frame" data="{src}" type="application/pdf"></object>
<p class="ax-aside"><a href="{src}">Open the PDF</a></p>"#,
        src = esc(&src),
    )
}

fn render_page(asset: &Asset, mode: AssetMode, title: &str) -> String {
    let src = asset_src(mode, asset);
    format!(
        r#"<iframe class="ax-frame" src="{src}" title="{title}" sandbox="allow-same-origin allow-popups" referrerpolicy="no-referrer" loading="lazy"></iframe>
<p class="ax-aside"><a href="{src}">Open the archived page on its own</a></p>"#,
        src = esc(&src),
        title = esc(title),
    )
}

fn render_download_link(asset: &Asset, mode: AssetMode) -> String {
    let src = asset_src(mode, asset);
    let ext = ext_of_s(&asset.name).to_uppercase();
    let size = fmt_size(asset.byte_size);
    format!(
        r#"<p class="ax-aside"><a href="{src}" download>Download {ext} file</a> · {size}</p>"#,
        src = esc(&src),
        ext = esc(&ext),
    )
}

fn render_posts(
    posts: &[Post],
    mode: AssetMode,
    posters: &std::collections::HashMap<String, PathBuf>,
) -> String {
    let mut s = String::from(r#"<div class="ax-thread">"#);
    for post in posts {
        s.push_str(&render_post(post, mode, posters, false));
    }
    s.push_str("</div>");
    s
}

fn render_post(
    post: &Post,
    mode: AssetMode,
    posters: &std::collections::HashMap<String, PathBuf>,
    is_quote: bool,
) -> String {
    let tag = if is_quote { "blockquote" } else { "section" };
    let class = if is_quote { "ax-quote" } else { "ax-post" };

    let date = post
        .created_at_secs
        .map(fmt_timestamp)
        .unwrap_or_default();

    let avatar_html = post
        .avatar
        .as_ref()
        .map(|a| {
            let src = asset_src(mode, a);
            format!(r#"<img class="ax-avatar" src="{src}" alt="">"#, src = esc(&src))
        })
        .unwrap_or_default();

    let handle_html = post
        .handle
        .as_deref()
        .map(|h| format!(" @{h} · {date}", h = esc(h)))
        .unwrap_or_else(|| if date.is_empty() { String::new() } else { format!(" · {date}") });

    let mut s = format!(
        r#"<{tag} class="{class}">
<header class="ax-post-head">{avatar_html}<span class="ax-post-name">{name}</span> <span class="ax-muted">{handle_html}</span></header>
"#,
        name = esc(&post.name),
    );

    // Body: article or text.
    if let Some((title, body, cover)) = &post.article {
        if let Some(title) = title {
            s.push_str(&format!(
                r#"<h3 class="ax-article-title">{}</h3>"#,
                esc(title)
            ));
        }
        if let Some(cover) = cover {
            let src = asset_src(mode, cover);
            s.push_str(&format!(
                r#"<img class="ax-article-cover" src="{}" alt="">"#,
                esc(&src)
            ));
        }
        for para in body.split("\n\n") {
            let trimmed = para.trim();
            if !trimmed.is_empty() {
                s.push_str(&format!("<p>{}</p>", esc(trimmed)));
            }
        }
    } else if !post.spans.is_empty() {
        s.push_str(r#"<div class="ax-post-text">"#);
        for span in &post.spans {
            match span {
                Span::Text(t) => s.push_str(&esc(t)),
                Span::Link { text, href } => {
                    if let Some(h) = safe_href(href) {
                        s.push_str(&format!(
                            r#"<a href="{h}" rel="noopener noreferrer">{text}</a>"#,
                            h = esc(h),
                            text = esc(text),
                        ));
                    } else {
                        s.push_str(&esc(text));
                    }
                }
            }
        }
        s.push_str("</div>");
    }

    // Photos and videos.
    if !post.photos.is_empty() || !post.videos.is_empty() {
        s.push_str(r#"<div class="ax-post-media">"#);
        for photo in &post.photos {
            let src = asset_src(mode, photo);
            s.push_str(&format!(
                r#"<img src="{src}" alt="" loading="lazy">"#,
                src = esc(&src)
            ));
        }
        for (video, is_gif) in &post.videos {
            let src = asset_src(mode, video);
            if *is_gif {
                s.push_str(&format!(
                    r#"<video autoplay loop muted playsinline src="{src}"></video>"#,
                    src = esc(&src)
                ));
            } else {
                s.push_str(&format!(
                    r#"<video controls src="{src}"></video>"#,
                    src = esc(&src)
                ));
            }
        }
        s.push_str("</div>");
    }

    // Quote.
    if let Some(quote) = &post.quote {
        s.push_str(&render_post(quote, mode, posters, true));
    }

    // Status link.
    if let Some(url) = &post.status_url {
        s.push_str(&format!(
            r#"<a class="ax-post-link" href="{url}" rel="noopener noreferrer">View on X</a>"#,
            url = esc(url)
        ));
    }

    s.push_str(&format!("</{tag}>"));
    s
}

fn render_container(
    children: &[ExportEntry],
    mode: AssetMode,
    posters: &std::collections::HashMap<String, PathBuf>,
    depth: usize,
) -> String {
    let mut s = String::from(r#"<ol class="ax-children">"#);
    for child in children {
        let title = entry_title(child);
        let date = fmt_date(&child.summary.archived_at);
        let child_body = render_body(&child.body, mode, posters, &child.subtitles, &title, depth + 1);
        s.push_str(&format!(
            r#"<li><article class="ax-entry ax-entry--child">
<header class="ax-masthead"><h3 class="ax-title">{title}</h3>
<p class="ax-meta"><time datetime="{dt}">{date}</time></p></header>
<div class="ax-body">{child_body}</div>
</article></li>"#,
            title = esc(&title),
            dt = esc(&child.summary.archived_at),
        ));
    }
    s.push_str("</ol>");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_tracks_use_vtt_only_even_when_srt_precedes_it() {
        let asset = |name: &str| Asset {
            source: PathBuf::new(),
            name: name.to_string(),
            byte_size: 0,
        };
        let html = render_video(
            &asset("video.mp4"),
            AssetMode::Bundle,
            &std::collections::HashMap::new(),
            &[asset("captions.srt"), asset("captions.vtt")],
        );

        assert_eq!(html.matches("<track ").count(), 1);
        assert!(html.contains("src=\"assets/captions.vtt\""));
        assert!(html.contains(" default>"));
        assert!(!html.contains("captions.srt"));
    }
}
