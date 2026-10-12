//! Entry export: HTML ZIP, PDF, Markdown ZIP.
//!
//! The public entry point is [`export_entries`].

mod brand;
mod bundle;
mod html;
mod markdown;
mod model;
mod pdf;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::Utc;
use tempfile::TempDir;

use crate::archive::{self, ArchivePaths, EntrySummary};
use crate::database;
use crate::env_config::env_or;
use crate::process;


// ---------------------------------------------------------------------------
// Public API types
// ---------------------------------------------------------------------------

/// The supported output formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Html,
    Pdf,
    Markdown,
}

impl ExportFormat {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "html" => Some(Self::Html),
            "pdf" => Some(Self::Pdf),
            "markdown" => Some(Self::Markdown),
            _ => None,
        }
    }
}

/// Whether multiple entries are combined into one document or each gets its own file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportLayout {
    Together,
    Separate,
}

impl ExportLayout {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "together" => Some(Self::Together),
            "separate" => Some(Self::Separate),
            _ => None,
        }
    }
}

/// Options controlling the export.
#[derive(Debug, Clone)]
pub struct ExportOptions {
    pub format: ExportFormat,
    pub layout: ExportLayout,
    pub include_credit: bool,
    /// Visibility bits for child (container) enumeration; CLI passes `u32::MAX`.
    pub caller_bits: u32,
}

/// Maximum number of entries per export request.
pub const MAX_EXPORT_ENTRIES: usize = 100;

/// A completed export file inside a temporary work directory.
///
/// Call [`ExportFile::open`] to get a readable file handle or
/// [`ExportFile::persist_into`] to move the file to a permanent location.
#[derive(Debug)]
pub struct ExportFile {
    /// Absolute path to the output file inside `work_dir`.
    pub path: PathBuf,
    /// ASCII file name suitable for `Content-Disposition`.
    pub filename: String,
    pub content_type: &'static str,
    work_dir: TempDir,
}

impl ExportFile {
    /// Open the file, then drop the work dir (unix unlink keeps the handle
    /// alive for reading).  Returns `(file, len, filename, content_type)`.
    pub fn open(self) -> Result<(fs::File, u64, String, &'static str)> {
        let file = fs::File::open(&self.path)
            .with_context(|| format!("failed to open export file: {}", self.path.display()))?;
        let len = file.metadata().map(|m| m.len()).unwrap_or(0);
        let filename = self.filename.clone();
        let ct = self.content_type;
        drop(self.work_dir); // unlinks temp dir; the open handle survives on unix
        Ok((file, len, filename, ct))
    }

    /// Move the output file to `dest_dir/<filename>`.  Errors if the
    /// destination already exists.
    pub fn persist_into(self, dest_dir: &Path) -> Result<PathBuf> {
        let dest = dest_dir.join(&self.filename);
        if dest.exists() {
            bail!("{} already exists", dest.display());
        }
        // Try rename first (same device); fall back to copy+remove.
        if fs::rename(&self.path, &dest).is_err() {
            fs::copy(&self.path, &dest)
                .with_context(|| format!("failed to copy export to {}", dest.display()))?;
            let _ = fs::remove_file(&self.path);
        }
        drop(self.work_dir);
        Ok(dest)
    }
}

// ---------------------------------------------------------------------------
// Main entry point
// ---------------------------------------------------------------------------

/// Export the given entries.
///
/// Returns an [`ExportFile`] inside a temporary directory; call
/// `open()` or `persist_into()` to retrieve the result.
pub fn export_entries(
    paths: &ArchivePaths,
    entry_uids: &[String],
    options: &ExportOptions,
) -> Result<ExportFile> {
    // 1. Validate
    if entry_uids.is_empty() {
        bail!("entries must not be empty");
    }
    if entry_uids.len() > MAX_EXPORT_ENTRIES {
        bail!("select at most 100 entries");
    }

    // Deduplicate UIDs while preserving first-occurrence order.
    let mut seen = std::collections::HashSet::new();
    let uids: Vec<String> = entry_uids
        .iter()
        .filter(|uid| seen.insert((*uid).clone()))
        .cloned()
        .collect();

    // 2. Open DB and load entries.
    let conn = database::open_or_initialize(&paths.archive_path)?;
    let mut summaries: Vec<EntrySummary> = Vec::new();
    for uid in &uids {
        let detail = archive::get_entry_detail(&conn, uid)?;
        match detail {
            Some(d) => summaries.push(d.summary),
            None => bail!("entry not found: {uid}"),
        }
    }

    // 3. Remove entries whose parent is also in the set.
    let uid_set: std::collections::HashSet<String> = uids.iter().cloned().collect();
    let summaries: Vec<EntrySummary> = summaries
        .into_iter()
        .filter(|s| {
            s.parent_entry_uid
                .as_ref()
                .map_or(true, |p| !uid_set.contains(p))
        })
        .collect();

    if summaries.is_empty() {
        bail!("no entries to export after filtering");
    }

    // 4. Create work directory.
    let exports_temp = paths.store_path.join("temp").join("exports");
    fs::create_dir_all(&exports_temp)?;
    let work_dir = tempfile::Builder::new()
        .prefix("export-")
        .tempdir_in(&exports_temp)
        .context("failed to create export work dir")?;
    let wpath = work_dir.path().to_path_buf();

    // 5. Load all entries.
    let entries: Vec<model::ExportEntry> = summaries
        .into_iter()
        .map(|s| model::load_entry(&conn, &paths.store_path, s, options.caller_bits, 0))
        .collect::<Result<Vec<_>>>()?;

    // 6. Generate posters when format requires them.
    let posters: HashMap<String, PathBuf> = if options.format != ExportFormat::Markdown {
        generate_posters(&entries, &wpath)
    } else {
        // Still generate for markdown (poster images as video links).
        generate_posters(&entries, &wpath)
    };

    // 7. Dispatch.
    let (out_path, filename, content_type) =
        assemble(&entries, options, &wpath, &posters, &paths.store_path)?;

    Ok(ExportFile {
        path: out_path,
        filename,
        content_type,
        work_dir,
    })
}

// ---------------------------------------------------------------------------
// Poster generation
// ---------------------------------------------------------------------------

fn collect_video_names(entries: &[model::ExportEntry]) -> Vec<(PathBuf, String)> {
    let mut out: Vec<(PathBuf, String)> = Vec::new();
    for entry in entries {
        collect_videos_from_body(&entry.body, &mut out);
    }
    out
}

fn collect_videos_from_body(body: &model::ExportBody, out: &mut Vec<(PathBuf, String)>) {
    match body {
        model::ExportBody::Video(a) => out.push((a.source.clone(), a.name.clone())),
        model::ExportBody::Container(children) => {
            for child in children {
                collect_videos_from_body(&child.body, out);
            }
        }
        model::ExportBody::Posts(posts) => {
            for post in posts {
                collect_post_videos(post, out);
            }
        }
        _ => {}
    }
}

fn collect_post_videos(post: &model::Post, out: &mut Vec<(PathBuf, String)>) {
    for (video, _) in &post.videos {
        out.push((video.source.clone(), video.name.clone()));
    }
    if let Some(quote) = &post.quote {
        collect_post_videos(quote, out);
    }
}

/// Run ffmpeg to generate poster images for all video assets.
/// Returns a map of video name → poster path.
fn generate_posters(entries: &[model::ExportEntry], work_dir: &Path) -> HashMap<String, PathBuf> {
    let ffmpeg_path = PathBuf::from(env_or("ARCHIVR_FFMPEG", "ffmpeg"));
    let videos = collect_video_names(entries);
    if videos.is_empty() {
        return HashMap::new();
    }

    let poster_dir = work_dir.join("posters");
    if let Err(e) = fs::create_dir_all(&poster_dir) {
        eprintln!("warn: export: failed to create poster dir: {e:#}");
        return HashMap::new();
    }

    let mut posters: HashMap<String, PathBuf> = HashMap::new();
    let mut ffmpeg_missing = false;

    for (src, name) in &videos {
        if ffmpeg_missing {
            break;
        }
        let stem = Path::new(name)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| name.clone());
        let poster_path = poster_dir.join(format!("{stem}-poster.jpg"));

        if let Some(p) = try_poster(&ffmpeg_path, src, &poster_path, &mut ffmpeg_missing) {
            posters.insert(name.clone(), p);
        }
    }
    posters
}

/// Attempt to generate a poster for one video. Returns `Some(path)` on success.
fn try_poster(
    ffmpeg: &Path,
    src: &Path,
    out: &Path,
    ffmpeg_missing: &mut bool,
) -> Option<PathBuf> {
    let args: Vec<std::ffi::OsString> = vec![
        "-nostdin".into(),
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-ss".into(),
        "1".into(),
        "-i".into(),
        src.into(),
        "-frames:v".into(),
        "1".into(),
        "-vf".into(),
        "scale='min(1280,iw)':-2".into(),
        "-q:v".into(),
        "3".into(),
        "-y".into(),
        out.into(),
    ];

    let run = process::run_with_timeout(ffmpeg, &args, None, Duration::from_secs(60));
    match run {
        Ok(_) if out.exists() && out.metadata().map(|m| m.len()).unwrap_or(0) > 0 => {
            return Some(out.to_path_buf());
        }
        Err(e) if is_not_found(&e) => {
            eprintln!("warn: export: ffmpeg not found; skipping posters");
            *ffmpeg_missing = true;
            return None;
        }
        _ => {}
    }

    // Retry with -ss 0.
    let args_retry: Vec<std::ffi::OsString> = vec![
        "-nostdin".into(),
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-ss".into(),
        "0".into(),
        "-i".into(),
        src.into(),
        "-frames:v".into(),
        "1".into(),
        "-vf".into(),
        "scale='min(1280,iw)':-2".into(),
        "-q:v".into(),
        "3".into(),
        "-y".into(),
        out.into(),
    ];
    match process::run_with_timeout(ffmpeg, &args_retry, None, Duration::from_secs(60)) {
        Ok(_) if out.exists() && out.metadata().map(|m| m.len()).unwrap_or(0) > 0 => {
            Some(out.to_path_buf())
        }
        Err(e) => {
            let src_name = src.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            eprintln!("warn: export: poster for {src_name}: {e:#}");
            None
        }
        Ok(_) => None,
    }
}

fn is_not_found(err: &anyhow::Error) -> bool {
    err.chain()
        .any(|e| {
            e.downcast_ref::<std::io::Error>()
                .map_or(false, |io| io.kind() == std::io::ErrorKind::NotFound)
        })
}

// ---------------------------------------------------------------------------
// Assembly dispatch
// ---------------------------------------------------------------------------

/// Returns `(out_path, filename, content_type)`.
fn assemble(
    entries: &[model::ExportEntry],
    options: &ExportOptions,
    work_dir: &Path,
    posters: &HashMap<String, PathBuf>,
    store_path: &Path,
) -> Result<(PathBuf, String, &'static str)> {
    let date = Utc::now().format("%Y-%m-%d").to_string();
    let n = entries.len();

    match options.format {
        ExportFormat::Html => assemble_html(entries, options, work_dir, posters, &date, n),
        ExportFormat::Markdown => {
            assemble_markdown(entries, options, work_dir, posters, &date, n)
        }
        ExportFormat::Pdf => assemble_pdf(entries, options, work_dir, posters, store_path, &date, n),
    }
}

// ---------------------------------------------------------------------------
// HTML assembly
// ---------------------------------------------------------------------------

fn assemble_html(
    entries: &[model::ExportEntry],
    options: &ExportOptions,
    work_dir: &Path,
    posters: &HashMap<String, PathBuf>,
    date: &str,
    n: usize,
) -> Result<(PathBuf, String, &'static str)> {
    let all_assets = collect_all_assets(entries, posters);

    if n == 1 || options.layout == ExportLayout::Together {
        // Single document: one folder.
        let slug = if n == 1 {
            entry_slug(entries[0].summary.title.as_deref().unwrap_or(""))
        } else {
            format!("export-{date}")
        };
        let filename = if n == 1 {
            format!("{slug}.zip")
        } else {
            format!("export-{n}-entries-{date}.zip")
        };
        let html = html::render_document(entries, html::AssetMode::Bundle, posters, options.include_credit);
        let zip_path = work_dir.join(&filename);
        bundle::write_zip_single(&zip_path, &slug, &html, "html", &all_assets, &[])?;
        Ok((zip_path, filename, "application/zip"))
    } else {
        // Separate: one folder per entry.
        let outer = format!("export-{date}");
        let filename = format!("export-{n}-entries-{date}.zip");
        let zip_path = work_dir.join(&filename);

        let folders: Vec<(String, String, Vec<model::Asset>)> = entries
            .iter()
            .enumerate()
            .map(|(i, entry)| {
                let nn = format!("{:02}", i + 1);
                let slug = entry_slug(entry.summary.title.as_deref().unwrap_or(""));
                let folder = format!("{outer}/{nn}-{slug}");
                let html = html::render_document(
                    std::slice::from_ref(entry),
                    html::AssetMode::Bundle,
                    posters,
                    options.include_credit,
                );
                let assets = collect_entry_assets(entry, posters);
                (folder, html, assets)
            })
            .collect();

        bundle::write_zip_multi(&zip_path, &folders, "html")?;
        Ok((zip_path, filename, "application/zip"))
    }
}

// ---------------------------------------------------------------------------
// Markdown assembly
// ---------------------------------------------------------------------------

fn assemble_markdown(
    entries: &[model::ExportEntry],
    options: &ExportOptions,
    work_dir: &Path,
    posters: &HashMap<String, PathBuf>,
    date: &str,
    n: usize,
) -> Result<(PathBuf, String, &'static str)> {
    let all_assets = collect_all_assets(entries, posters);

    if n == 1 || options.layout == ExportLayout::Together {
        let slug = if n == 1 {
            entry_slug(entries[0].summary.title.as_deref().unwrap_or(""))
        } else {
            format!("export-{date}")
        };
        let filename = if n == 1 {
            format!("{slug}.zip")
        } else {
            format!("export-{n}-entries-{date}.zip")
        };
        let md = markdown::render_markdown(entries, posters, options.include_credit);
        let zip_path = work_dir.join(&filename);
        bundle::write_zip_single(&zip_path, &slug, &md, "md", &all_assets, &[])?;
        Ok((zip_path, filename, "application/zip"))
    } else {
        let outer = format!("export-{date}");
        let filename = format!("export-{n}-entries-{date}.zip");
        let zip_path = work_dir.join(&filename);

        let folders: Vec<(String, String, Vec<model::Asset>)> = entries
            .iter()
            .enumerate()
            .map(|(i, entry)| {
                let nn = format!("{:02}", i + 1);
                let slug = entry_slug(entry.summary.title.as_deref().unwrap_or(""));
                let folder = format!("{outer}/{nn}-{slug}");
                let md = markdown::render_markdown(
                    std::slice::from_ref(entry),
                    posters,
                    options.include_credit,
                );
                let assets = collect_entry_assets(entry, posters);
                (folder, md, assets)
            })
            .collect();

        bundle::write_zip_multi(&zip_path, &folders, "md")?;
        Ok((zip_path, filename, "application/zip"))
    }
}

// ---------------------------------------------------------------------------
// PDF assembly
// ---------------------------------------------------------------------------

fn assemble_pdf(
    entries: &[model::ExportEntry],
    options: &ExportOptions,
    work_dir: &Path,
    posters: &HashMap<String, PathBuf>,
    _store_path: &Path,
    date: &str,
    n: usize,
) -> Result<(PathBuf, String, &'static str)> {
    if n == 1 {
        let slug = entry_slug(entries[0].summary.title.as_deref().unwrap_or(""));
        let filename = format!("{slug}.pdf");
        let pdf_path = work_dir.join(&filename);
        let entry_path =
            pdf::entry_pdf(&entries[0], 0, options.include_credit, work_dir, posters)?;
        fs::rename(&entry_path, &pdf_path)
            .or_else(|_| fs::copy(&entry_path, &pdf_path).map(|_| ()))?;
        return Ok((pdf_path, filename, "application/pdf"));
    }

    match options.layout {
        ExportLayout::Together => {
            let filename = format!("export-{n}-entries-{date}.pdf");
            let out = work_dir.join(&filename);

            // Cover page.
            let cover_html = html::render_cover(entries, options.include_credit);
            let cover_html_path = work_dir.join("cover.html");
            fs::write(&cover_html_path, &cover_html)?;
            let cover_pdf_path = work_dir.join("cover.pdf");
            let profile_dir = work_dir.join("chrome-profile");
            fs::create_dir_all(&profile_dir)?;
            pdf::print_pdf(&cover_html_path, &cover_pdf_path, &profile_dir)?;

            // Per-entry PDFs (no credit: it's on the cover).
            let mut pdf_paths: Vec<PathBuf> = vec![cover_pdf_path];
            for (i, entry) in entries.iter().enumerate() {
                let p = pdf::entry_pdf(entry, i, false, work_dir, posters)?;
                pdf_paths.push(p);
            }

            pdf::merge_pdf_files(&pdf_paths, &out)?;
            Ok((out, filename, "application/pdf"))
        }
        ExportLayout::Separate => {
            // A ZIP of individual PDFs.
            let outer = format!("export-{date}");
            let filename = format!("export-{n}-entries-{date}.zip");
            let zip_path = work_dir.join(&filename);

            let mut pdf_names: Vec<(String, PathBuf)> = Vec::new();
            for (i, entry) in entries.iter().enumerate() {
                let nn = format!("{:02}", i + 1);
                let slug = entry_slug(entry.summary.title.as_deref().unwrap_or(""));
                let pdf_path = pdf::entry_pdf(entry, i, options.include_credit, work_dir, posters)?;
                let name_in_zip = format!("{nn}-{slug}.pdf");
                pdf_names.push((name_in_zip, pdf_path));
            }

            bundle::write_pdf_zip(&zip_path, &outer, &pdf_names)?;
            Ok((zip_path, filename, "application/zip"))
        }
    }
}

// ---------------------------------------------------------------------------
// Asset collection
// ---------------------------------------------------------------------------

/// Collect all assets referenced across all entries (and posters) without duplicates.
fn collect_all_assets(
    entries: &[model::ExportEntry],
    posters: &HashMap<String, PathBuf>,
) -> Vec<model::Asset> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out: Vec<model::Asset> = Vec::new();
    for entry in entries {
        for asset in collect_entry_assets(entry, posters) {
            if seen.insert(asset.name.clone()) {
                out.push(asset);
            }
        }
    }
    out
}

fn collect_entry_assets(
    entry: &model::ExportEntry,
    posters: &HashMap<String, PathBuf>,
) -> Vec<model::Asset> {
    let mut out: Vec<model::Asset> = Vec::new();
    collect_body_assets(&entry.body, posters, &mut out);
    for sub in &entry.subtitles {
        out.push(sub.clone());
    }
    out
}

fn collect_body_assets(
    body: &model::ExportBody,
    posters: &HashMap<String, PathBuf>,
    out: &mut Vec<model::Asset>,
) {
    match body {
        model::ExportBody::Video(a) => {
            out.push(a.clone());
            if let Some(poster) = posters.get(&a.name) {
                if let Ok(sz) = poster.metadata().map(|m| m.len()) {
                    out.push(model::Asset {
                        source: poster.clone(),
                        name: poster
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                        byte_size: sz,
                    });
                }
            }
        }
        model::ExportBody::Audio(a)
        | model::ExportBody::Image(a)
        | model::ExportBody::Pdf(a)
        | model::ExportBody::Page(a)
        | model::ExportBody::TextTooLarge(a)
        | model::ExportBody::File(a) => out.push(a.clone()),
        model::ExportBody::Text { .. } | model::ExportBody::Empty => {}
        model::ExportBody::Posts(posts) => {
            for post in posts {
                collect_post_assets(post, posters, out);
            }
        }
        model::ExportBody::Container(children) => {
            for child in children {
                collect_entry_assets_into(child, posters, out);
            }
        }
    }
}

fn collect_post_assets(
    post: &model::Post,
    posters: &HashMap<String, PathBuf>,
    out: &mut Vec<model::Asset>,
) {
    if let Some(a) = &post.avatar {
        out.push(a.clone());
    }
    if let Some((_, _, cover)) = &post.article {
        if let Some(a) = cover {
            out.push(a.clone());
        }
    }
    for photo in &post.photos {
        out.push(photo.clone());
    }
    for (video, _) in &post.videos {
        out.push(video.clone());
        if let Some(poster) = posters.get(&video.name) {
            if let Ok(sz) = poster.metadata().map(|m| m.len()) {
                out.push(model::Asset {
                    source: poster.clone(),
                    name: poster
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    byte_size: sz,
                });
            }
        }
    }
    if let Some(quote) = &post.quote {
        collect_post_assets(quote, posters, out);
    }
}

fn collect_entry_assets_into(
    entry: &model::ExportEntry,
    posters: &HashMap<String, PathBuf>,
    out: &mut Vec<model::Asset>,
) {
    collect_body_assets(&entry.body, posters, out);
    for sub in &entry.subtitles {
        out.push(sub.clone());
    }
}

// ---------------------------------------------------------------------------
// Naming helpers
// ---------------------------------------------------------------------------

/// Convert a title to a URL-safe slug (lowercase ASCII-alphanumeric, `-`
/// for other runs, trimmed and capped at 60 bytes).
pub fn entry_slug(title: &str) -> String {
    let mut slug = String::new();
    let mut in_sep = true; // start as true to trim leading separators
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            in_sep = false;
        } else if !in_sep {
            slug.push('-');
            in_sep = true;
        }
    }
    // Trim trailing `-`.
    while slug.ends_with('-') {
        slug.pop();
    }
    // Cap at 60 bytes.
    let mut end = 60.min(slug.len());
    while end > 0 && !slug.is_char_boundary(end) {
        end -= 1;
    }
    slug.truncate(end);
    if slug.is_empty() { "entry".to_string() } else { slug }
}

// ---------------------------------------------------------------------------
// Stale export cleanup
// ---------------------------------------------------------------------------

/// Remove export temp dirs older than `older_than` from
/// `<store_path>/temp/exports`.
pub fn prune_stale_export_dirs(store_path: &Path, older_than: Duration) {
    let exports_dir = store_path.join("temp").join("exports");
    let dir = match fs::read_dir(&exports_dir) {
        Ok(d) => d,
        Err(_) => return,
    };
    let cutoff = std::time::SystemTime::now() - older_than;
    for entry in dir.flatten() {
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let mtime = match meta.modified() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if mtime < cutoff {
            let path = entry.path();
            if let Err(e) = fs::remove_dir_all(&path) {
                eprintln!("warn: export cleanup: {}: {e:#}", path.display());
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use super::pdf::merge_documents;
    use crate::archive::initialize_archive;
    use crate::database;

    struct TestCtx {
        _tmp: tempfile::TempDir,
        paths: ArchivePaths,
        user_id: i64,
        run_id: i64,
        si_counter: std::cell::Cell<u32>,
    }

    impl TestCtx {
        fn new() -> Self {
            let tmp = tempfile::TempDir::new().unwrap();
            let archive_dir = tmp.path().join(".archivr");
            let store_dir = tmp.path().join("store");
            let paths = initialize_archive(&archive_dir, &store_dir, "Test", false).unwrap();
            let conn = database::open_or_initialize(&paths.archive_path).unwrap();
            let user_id = database::ensure_default_user(&conn).unwrap();
            let run = database::create_archive_run(&conn, user_id, 10).unwrap();
            TestCtx { _tmp: tmp, paths, user_id, run_id: run.id, si_counter: std::cell::Cell::new(0) }
        }

        fn conn(&self) -> rusqlite::Connection {
            database::open_or_initialize(&self.paths.archive_path).unwrap()
        }

        /// Create a text entry with a .txt primary_media artifact. Returns entry_uid.
        fn add_text_entry(&self, title: &str, content: &str) -> String {
            let conn = self.conn();
            let n = self.si_counter.get();
            self.si_counter.set(n + 1);
            let si_id = database::upsert_source_identity(
                &conn, "text", "document", Some(&format!("t-{n}")), None, &format!("text:{n}"),
            ).unwrap();
            let entry = database::create_archived_entry(
                &conn,
                &database::NewEntry {
                    source_identity_id: si_id,
                    archive_run_id: self.run_id,
                    parent_entry_id: None,
                    root_entry_id: None,
                    created_by_user_id: self.user_id,
                    owned_by_user_id: self.user_id,
                    source_kind: "text".to_string(),
                    entity_kind: "document".to_string(),
                    title: Some(title.to_string()),
                    visibility: "public".to_string(),
                    representation_kind: "text".to_string(),
                    source_metadata_json: "{}".to_string(),
                    display_metadata_json: None,
                },
            ).unwrap();
            // Write the file on disk.
            let hash = format!("{n:064x}");
            let sub1 = &hash[0..1];
            let sub2 = &hash[1..2];
            let relpath = format!("raw/{sub1}/{sub2}/{hash}.txt");
            let file_path = self.paths.store_path.join(&relpath);
            std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
            std::fs::write(&file_path, content).unwrap();
            database::add_entry_artifact(
                &conn,
                &database::NewArtifact {
                    entry_id: entry.id,
                    artifact_role: "primary_media".to_string(),
                    storage_area: "raw".to_string(),
                    relpath,
                    blob_id: None,
                    logical_path: None,
                    metadata_json: None,
                },
            ).unwrap();
            entry.entry_uid
        }

        /// Create a parent + child entry pair. Returns (parent_uid, child_uid).
        fn add_container_entry(&self, parent_title: &str, child_title: &str) -> (String, String) {
            let conn = self.conn();
            let n = self.si_counter.get();
            self.si_counter.set(n + 2);
            let si_parent = database::upsert_source_identity(
                &conn, "youtube", "playlist", Some(&format!("p-{n}")), None, &format!("yt:p:{n}"),
            ).unwrap();
            let parent = database::create_archived_entry(
                &conn,
                &database::NewEntry {
                    source_identity_id: si_parent,
                    archive_run_id: self.run_id,
                    parent_entry_id: None,
                    root_entry_id: None,
                    created_by_user_id: self.user_id,
                    owned_by_user_id: self.user_id,
                    source_kind: "youtube".to_string(),
                    entity_kind: "playlist".to_string(),
                    title: Some(parent_title.to_string()),
                    visibility: "public".to_string(),
                    representation_kind: "json".to_string(),
                    source_metadata_json: "{}".to_string(),
                    display_metadata_json: None,
                },
            ).unwrap();
            let si_child = database::upsert_source_identity(
                &conn, "youtube", "video", Some(&format!("c-{n}")), None, &format!("yt:c:{n}"),
            ).unwrap();
            let child = database::create_archived_entry(
                &conn,
                &database::NewEntry {
                    source_identity_id: si_child,
                    archive_run_id: self.run_id,
                    parent_entry_id: Some(parent.id),
                    root_entry_id: Some(parent.id),
                    created_by_user_id: self.user_id,
                    owned_by_user_id: self.user_id,
                    source_kind: "youtube".to_string(),
                    entity_kind: "video".to_string(),
                    title: Some(child_title.to_string()),
                    visibility: "public".to_string(),
                    representation_kind: "video".to_string(),
                    source_metadata_json: "{}".to_string(),
                    display_metadata_json: None,
                },
            ).unwrap();
            (parent.entry_uid, child.entry_uid)
        }
    }

    fn read_zip_entry(file: std::fs::File, suffix: &str) -> String {
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let names: Vec<String> = archive.file_names().map(str::to_string).collect();
        let name = names
            .iter()
            .find(|n| n.ends_with(suffix))
            .cloned()
            .unwrap_or_else(|| panic!("no entry ending with {suffix}; names: {names:?}"));
        let mut entry = archive.by_name(&name).unwrap();
        let mut content = String::new();
        std::io::Read::read_to_string(&mut entry, &mut content).unwrap();
        content
    }

    #[test]
    fn test_html_credit_on() {
        let ctx = TestCtx::new();
        let uid = ctx.add_text_entry("<b>&test", "hello world");
        let opts = ExportOptions {
            format: ExportFormat::Html,
            layout: ExportLayout::Together,
            include_credit: true,
            caller_bits: u32::MAX,
        };
        let export = export_entries(&ctx.paths, &[uid.clone()], &opts).unwrap();
        let (file, _, filename, _) = export.open().unwrap();
        assert!(filename.ends_with(".zip"), "filename: {filename}");
        let html = read_zip_entry(file, "index.html");
        assert!(html.contains("&lt;b&gt;&amp;test"), "title should be HTML-escaped");
        assert!(html.contains("Preserved with"), "credit present");
        assert!(html.contains("github.com/thegeneralist01/archivr"));
        assert!(!html.contains("/api/"), "no /api/ paths");
        assert!(!html.contains(&uid), "uid not in output");
    }

    #[test]
    fn test_html_credit_off() {
        let ctx = TestCtx::new();
        let uid = ctx.add_text_entry("My Entry", "body");
        let opts = ExportOptions {
            format: ExportFormat::Html,
            layout: ExportLayout::Together,
            include_credit: false,
            caller_bits: u32::MAX,
        };
        let export = export_entries(&ctx.paths, &[uid], &opts).unwrap();
        let (file, _, _, _) = export.open().unwrap();
        let html = read_zip_entry(file, "index.html");
        assert!(!html.to_lowercase().contains("archivr"), "no archivr mention when credit off");
    }

    #[test]
    fn test_separate_layout() {
        let ctx = TestCtx::new();
        let u1 = ctx.add_text_entry("Alpha", "a");
        let u2 = ctx.add_text_entry("Beta", "b");
        let opts = ExportOptions {
            format: ExportFormat::Html,
            layout: ExportLayout::Separate,
            include_credit: false,
            caller_bits: u32::MAX,
        };
        let export = export_entries(&ctx.paths, &[u1, u2], &opts).unwrap();
        let (file, _, filename, _) = export.open().unwrap();
        assert!(filename.ends_with(".zip"));
        let archive = zip::ZipArchive::new(file).unwrap();
        let names: Vec<String> = archive.file_names().map(str::to_string).collect();
        assert!(names.iter().any(|n| n.contains("01-") && n.ends_with("index.html")), "01 missing; {names:?}");
        assert!(names.iter().any(|n| n.contains("02-") && n.ends_with("index.html")), "02 missing; {names:?}");
    }

    #[test]
    fn test_markdown_entry() {
        let ctx = TestCtx::new();
        let uid = ctx.add_text_entry("My Note", "# heading\n\nbody text");
        let opts = ExportOptions {
            format: ExportFormat::Markdown,
            layout: ExportLayout::Together,
            include_credit: false,
            caller_bits: u32::MAX,
        };
        let export = export_entries(&ctx.paths, &[uid], &opts).unwrap();
        let (file, _, _, _) = export.open().unwrap();
        let md = read_zip_entry(file, "index.md");
        assert!(md.starts_with("# My Note"), "heading: {md}");
        assert!(md.contains("# heading\n\nbody text"), "body verbatim");
    }

    #[test]
    fn test_parent_child_dedupe() {
        let ctx = TestCtx::new();
        let (parent_uid, child_uid) = ctx.add_container_entry("Parent Playlist", "Child Video");
        let opts = ExportOptions {
            format: ExportFormat::Html,
            layout: ExportLayout::Together,
            include_credit: false,
            caller_bits: u32::MAX,
        };
        // Both UIDs requested; child should be dropped since parent is also selected.
        let export = export_entries(&ctx.paths, &[parent_uid, child_uid], &opts).unwrap();
        let (file, _, _, _) = export.open().unwrap();
        let html = read_zip_entry(file, "index.html");
        let count = html.matches("Child Video").count();
        assert!(count <= 1, "child title at most once; found {count}");
    }

    #[test]
    fn test_validation_empty() {
        let ctx = TestCtx::new();
        let opts = ExportOptions {
            format: ExportFormat::Html,
            layout: ExportLayout::Together,
            include_credit: true,
            caller_bits: u32::MAX,
        };
        let err = export_entries(&ctx.paths, &[], &opts).unwrap_err();
        assert!(err.to_string().contains("must not be empty"));
    }

    #[test]
    fn test_validation_too_many() {
        let ctx = TestCtx::new();
        let uids: Vec<String> = (0..101).map(|i| format!("uid-{i}")).collect();
        let opts = ExportOptions {
            format: ExportFormat::Html,
            layout: ExportLayout::Together,
            include_credit: true,
            caller_bits: u32::MAX,
        };
        let err = export_entries(&ctx.paths, &uids, &opts).unwrap_err();
        assert!(err.to_string().contains("100 entries"));
    }

    #[test]
    fn test_slug() {
        assert_eq!(entry_slug("Hello World!"), "hello-world");
        assert_eq!(entry_slug("  "), "entry");
        assert_eq!(entry_slug("日本語"), "entry");
        assert_eq!(entry_slug("my-entry"), "my-entry");
    }

    #[test]
    fn test_merge_pdfs_page_count() {
        use lopdf::{Dictionary, Document, Object, Stream};
        let make_doc = |content: &[u8]| -> Document {
            let mut doc = Document::with_version("1.4");
            let stream_id = doc.add_object(Object::Stream(Stream::new(Dictionary::new(), content.to_vec())));
            let mut page_dict = Dictionary::new();
            page_dict.set("Type", Object::Name(b"Page".to_vec()));
            page_dict.set("Contents", Object::Reference(stream_id));
            page_dict.set("MediaBox", Object::Array(vec![
                Object::Integer(0), Object::Integer(0),
                Object::Integer(612), Object::Integer(792),
            ]));
            let page_id = doc.add_object(Object::Dictionary(page_dict));
            let mut pages_dict = Dictionary::new();
            pages_dict.set("Type", Object::Name(b"Pages".to_vec()));
            pages_dict.set("Kids", Object::Array(vec![Object::Reference(page_id)]));
            pages_dict.set("Count", Object::Integer(1));
            let pages_id = doc.add_object(Object::Dictionary(pages_dict));
            if let Some(Object::Dictionary(pd)) = doc.objects.get_mut(&page_id) {
                pd.set("Parent", Object::Reference(pages_id));
            }
            let mut catalog = Dictionary::new();
            catalog.set("Type", Object::Name(b"Catalog".to_vec()));
            catalog.set("Pages", Object::Reference(pages_id));
            let catalog_id = doc.add_object(Object::Dictionary(catalog));
            doc.trailer.set("Root", Object::Reference(catalog_id));
            doc
        };

        let doc1 = make_doc(b"BT /F1 12 Tf 100 700 Td (Page one) Tj ET");
        let doc2_p1 = make_doc(b"BT /F1 12 Tf 100 700 Td (Page two) Tj ET");
        let doc2_p2 = make_doc(b"BT /F1 12 Tf 100 700 Td (Page three) Tj ET");

        let doc2 = merge_documents(vec![doc2_p1, doc2_p2]).unwrap();
        let mut merged = merge_documents(vec![doc1, doc2]).unwrap();
        let mut bytes: Vec<u8> = Vec::new();
        merged.save_to(&mut bytes).unwrap();
        let reloaded = lopdf::Document::load_mem(&bytes).unwrap();
        assert_eq!(reloaded.get_pages().len(), 3, "merged PDF should have 3 pages");
    }
}
