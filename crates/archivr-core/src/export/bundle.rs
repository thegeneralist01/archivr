//! ZIP bundle assembly for HTML and Markdown exports.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::export::model::Asset;

// Extensions that should be compressed (text-like formats).
const COMPRESSED_EXTS: &[&str] = &[
    "html", "htm", "md", "txt", "css", "svg", "vtt", "srt", "json",
];

/// Build a ZIP at `zip_path` containing multiple entry folders.
///
/// `folders` is a list of `(folder_name, html_or_md_content, assets)`.
/// Each folder becomes `<folder_name>/index.{ext}` plus
/// `<folder_name>/assets/<name>` per asset.
/// `ext` is `"html"` or `"md"`.
pub(crate) fn write_zip_multi(
    zip_path: &Path,
    folders: &[(String, String, Vec<Asset>)],
    ext: &str,
) -> Result<()> {
    let file = std::fs::File::create(zip_path)
        .with_context(|| format!("failed to create zip: {}", zip_path.display()))?;
    let mut zip = ZipWriter::new(file);
    let mut seen_assets: HashSet<String> = HashSet::new();

    for (folder, content, assets) in folders {
        let index_path = format!("{folder}/index.{ext}");
        zip.start_file(&index_path, text_options(ext))
            .with_context(|| format!("failed to start {index_path}"))?;
        io::copy(&mut content.as_bytes(), &mut zip)?;

        for asset in assets {
            let entry_path = format!("{folder}/assets/{}", asset.name);
            // Deduplicate: same name in same folder is skipped.
            if !seen_assets.insert(entry_path.clone()) {
                continue;
            }
            let opts = asset_options(&asset.name, asset.byte_size);
            zip.start_file(&entry_path, opts)
                .with_context(|| format!("failed to start {entry_path}"))?;
            let mut f = std::fs::File::open(&asset.source)
                .with_context(|| format!("failed to open asset: {}", asset.source.display()))?;
            io::copy(&mut f, &mut zip)?;
        }
    }

    zip.finish().context("failed to finish zip")?;
    Ok(())
}

/// Build a ZIP for a single entry.
///
/// `(content, assets)` — `assets` may include poster files.
pub(crate) fn write_zip_single(
    zip_path: &Path,
    slug: &str,
    content: &str,
    ext: &str,
    assets: &[Asset],
    extra_assets: &[PathBuf], // poster paths etc. (not an Asset with source+name)
) -> Result<()> {
    let folders = vec![(slug.to_string(), content.to_string(), assets.to_vec())];
    write_zip_multi_with_extra(zip_path, &folders, ext, extra_assets)
}

/// Variant that also writes caller-supplied extra paths (e.g. poster JPEGs
/// generated in the work dir) into `<folder>/assets/`.
pub(crate) fn write_zip_multi_with_extra(
    zip_path: &Path,
    folders: &[(String, String, Vec<Asset>)],
    ext: &str,
    extra_assets: &[PathBuf],
) -> Result<()> {
    let file = std::fs::File::create(zip_path)
        .with_context(|| format!("failed to create zip: {}", zip_path.display()))?;
    let mut zip = ZipWriter::new(file);
    let mut seen_assets: HashSet<String> = HashSet::new();

    for (folder, content, assets) in folders {
        let index_path = format!("{folder}/index.{ext}");
        zip.start_file(&index_path, text_options(ext))
            .with_context(|| format!("failed to start {index_path}"))?;
        io::copy(&mut content.as_bytes(), &mut zip)?;

        for asset in assets {
            let entry_path = format!("{folder}/assets/{}", asset.name);
            if !seen_assets.insert(entry_path.clone()) {
                continue;
            }
            let opts = asset_options(&asset.name, asset.byte_size);
            zip.start_file(&entry_path, opts)
                .with_context(|| format!("failed to start {entry_path}"))?;
            let mut f = std::fs::File::open(&asset.source)
                .with_context(|| format!("failed to open asset: {}", asset.source.display()))?;
            io::copy(&mut f, &mut zip)?;
        }

        // Extra assets (posters, etc.) for the first folder only — or the folder
        // that owns the extra assets is determined by the caller.
        // Here we write them under the last folder; callers that need per-folder
        // placement should call write_zip_multi instead.
        for extra in extra_assets {
            let name = extra
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let entry_path = format!("{folder}/assets/{name}");
            if !seen_assets.insert(entry_path.clone()) {
                continue;
            }
            let size = extra.metadata().map(|m| m.len()).unwrap_or(0);
            let opts = asset_options(&name, size);
            zip.start_file(&entry_path, opts)
                .with_context(|| format!("failed to start extra {entry_path}"))?;
            let mut f = std::fs::File::open(extra)
                .with_context(|| format!("failed to open extra asset: {}", extra.display()))?;
            io::copy(&mut f, &mut zip)?;
        }
    }

    zip.finish().context("failed to finish zip")?;
    Ok(())
}

/// Build a ZIP that holds multiple PDFs: `<folder>/<NN>-<slug>.pdf`.
pub(crate) fn write_pdf_zip(
    zip_path: &Path,
    folder: &str,
    pdfs: &[(String, PathBuf)], // (name_in_zip, source_path)
) -> Result<()> {
    let file = std::fs::File::create(zip_path)
        .with_context(|| format!("failed to create zip: {}", zip_path.display()))?;
    let mut zip = ZipWriter::new(file);

    for (name, source) in pdfs {
        let entry_path = format!("{folder}/{name}");
        let size = source.metadata().map(|m| m.len()).unwrap_or(0);
        let opts = asset_options(name, size);
        zip.start_file(&entry_path, opts)
            .with_context(|| format!("failed to start {entry_path}"))?;
        let mut f = std::fs::File::open(source)
            .with_context(|| format!("failed to open pdf: {}", source.display()))?;
        io::copy(&mut f, &mut zip)?;
    }

    zip.finish().context("failed to finish pdf zip")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn text_options(_ext: &str) -> SimpleFileOptions {
    SimpleFileOptions::default().compression_method(CompressionMethod::Deflated)
    // Deflated for text content
    // (ext check is implicit for index.html / index.md)
    .compression_level(Some(6))
}

fn asset_options(name: &str, byte_size: u64) -> SimpleFileOptions {
    let ext = std::path::Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default();

    let method = if COMPRESSED_EXTS.contains(&ext.as_str()) {
        CompressionMethod::Deflated
    } else {
        CompressionMethod::Stored
    };

    let mut opts = SimpleFileOptions::default().compression_method(method);
    if COMPRESSED_EXTS.contains(&ext.as_str()) {
        opts = opts.compression_level(Some(6));
    }
    if byte_size >= 4 * 1024 * 1024 * 1024 {
        opts = opts.large_file(true);
    }
    opts
}
