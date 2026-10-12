//! PDF generation via headless Chrome and lopdf merging.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream};

use crate::env_config::{env_or, env_timeout};
use crate::export::brand;
use crate::export::html::{AssetMode, entry_title, render_document, source_label};
use crate::export::model::{Asset, ExportBody, ExportEntry};
use crate::process;

// ---------------------------------------------------------------------------
// Chrome PDF printing
// ---------------------------------------------------------------------------

/// Print an HTML file to PDF using headless Chrome.
pub(crate) fn print_pdf(html_path: &Path, out: &Path, profile_dir: &Path) -> Result<()> {
    let chrome = env_or("ARCHIVR_CHROME", "chromium");
    let chrome_path = PathBuf::from(&chrome);

    let timeout_secs = env_timeout("ARCHIVR_EXPORT_PDF_TIMEOUT", 120);

    let extra_args: Vec<String> = std::env::var("ARCHIVR_CHROME_ARGS")
        .unwrap_or_default()
        .split_whitespace()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();

    let html_url = percent_encode_path(html_path);

    let mut args: Vec<OsString> = vec![
        "--headless=new".into(),
        "--disable-gpu".into(),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "--hide-scrollbars".into(),
        format!("--user-data-dir={}", profile_dir.display()).into(),
        "--no-pdf-header-footer".into(),
        "--run-all-compositor-stages-before-draw".into(),
        "--virtual-time-budget=15000".into(),
        format!("--print-to-pdf={}", out.display()).into(),
    ];
    for a in &extra_args {
        args.push(a.into());
    }
    args.push(html_url.into());

    process::run_with_timeout(&chrome_path, &args, None, Duration::from_secs(timeout_secs))
        .with_context(|| "PDF export needs Chromium: set ARCHIVR_CHROME")?;

    if !out.exists() || out.metadata().map(|m| m.len()).unwrap_or(0) == 0 {
        bail!(
            "Chromium produced no PDF (set ARCHIVR_CHROME to a Chromium/Chrome binary); \
             tried: {chrome}"
        );
    }
    Ok(())
}

fn percent_encode_path(path: &Path) -> String {
    let s = path.to_string_lossy();
    let mut out = String::with_capacity(s.len() + 7);
    out.push_str("file://");
    for byte in s.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'.' | b'_' | b'-' | b'~' => {
                out.push(*byte as char)
            }
            b => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// lopdf merging
// ---------------------------------------------------------------------------

/// Merge PDF files on disk into a single output PDF.
pub(crate) fn merge_pdf_files(inputs: &[PathBuf], output: &Path) -> Result<()> {
    if inputs.is_empty() {
        bail!("merge_pdf_files: no inputs");
    }
    if inputs.len() == 1 {
        std::fs::copy(&inputs[0], output)
            .with_context(|| format!("failed to copy PDF to {}", output.display()))?;
        return Ok(());
    }

    let docs: Vec<Document> = inputs
        .iter()
        .map(|p| Document::load(p).with_context(|| format!("failed to load PDF: {}", p.display())))
        .collect::<Result<Vec<_>>>()?;

    let mut merged = merge_documents(docs)?;
    merged
        .save(output)
        .with_context(|| format!("failed to save merged PDF to {}", output.display()))?;
    Ok(())
}

/// Merge multiple `Document` values into one, appending pages in order.
///
/// Follows the lopdf `examples/merge.rs` pattern: each source document's
/// object IDs are shifted by an offset so they do not collide, then all
/// objects are copied into a fresh document with a new Pages tree and Catalog.
pub(crate) fn merge_documents(docs: Vec<Document>) -> Result<Document> {
    if docs.is_empty() {
        bail!("merge_documents: no documents provided");
    }
    if docs.len() == 1 {
        return Ok(docs.into_iter().next().unwrap());
    }

    let mut merged = Document::with_version("1.5");
    let mut all_page_ids: Vec<ObjectId> = Vec::new();
    // next_id tracks the next free object number so we can assign the Pages
    // and Catalog objects after all source objects have been copied in.
    let mut next_id: u32 = 0;

    for doc in docs {
        let offset = next_id;

        // Collect pages in page-number order before we consume the doc.
        let pages = doc.get_pages(); // BTreeMap<u32, ObjectId>
        for (_page_num, page_oid) in &pages {
            all_page_ids.push((page_oid.0 + offset, page_oid.1));
        }

        let doc_max = doc.objects.keys().map(|(n, _)| *n).max().unwrap_or(0);

        for (oid, obj) in &doc.objects {
            let new_oid: ObjectId = (oid.0 + offset, oid.1);
            let remapped = remap_refs(obj, offset);
            merged.objects.insert(new_oid, remapped);
        }

        next_id += doc_max + 1;
    }

    // Build the Pages dictionary.
    let pages_oid: ObjectId = (next_id, 0);
    next_id += 1;

    // Point every collected page at the new Pages parent.
    for page_id in &all_page_ids {
        if let Some(Object::Dictionary(dict)) = merged.objects.get_mut(page_id) {
            dict.set("Parent", Object::Reference(pages_oid));
        }
    }

    let mut pages_dict = Dictionary::new();
    pages_dict.set("Type", Object::Name(b"Pages".to_vec()));
    pages_dict.set(
        "Kids",
        Object::Array(all_page_ids.iter().map(|id| Object::Reference(*id)).collect()),
    );
    pages_dict.set("Count", Object::Integer(all_page_ids.len() as i64));
    merged.objects.insert(pages_oid, Object::Dictionary(pages_dict));

    // Build the Catalog.
    let catalog_oid: ObjectId = (next_id, 0);
    let mut catalog = Dictionary::new();
    catalog.set("Type", Object::Name(b"Catalog".to_vec()));
    catalog.set("Pages", Object::Reference(pages_oid));
    merged.objects.insert(catalog_oid, Object::Dictionary(catalog));

    merged.trailer.set("Root", Object::Reference(catalog_oid));
    merged.trailer.set("Size", Object::Integer((next_id + 1) as i64));
    merged.max_id = next_id;

    Ok(merged)
}

/// Recursively remap all `Reference` object IDs by adding `offset` to the
/// object number component.
fn remap_refs(obj: &Object, offset: u32) -> Object {
    match obj {
        Object::Reference(id) => Object::Reference((id.0 + offset, id.1)),
        Object::Array(arr) => Object::Array(arr.iter().map(|o| remap_refs(o, offset)).collect()),
        Object::Dictionary(dict) => {
            let mut new_dict = Dictionary::new();
            for (k, v) in dict.iter() {
                new_dict.set(k.clone(), remap_refs(v, offset));
            }
            Object::Dictionary(new_dict)
        }
        Object::Stream(stream) => {
            let mut new_dict = Dictionary::new();
            for (k, v) in stream.dict.iter() {
                new_dict.set(k.clone(), remap_refs(v, offset));
            }
            Object::Stream(Stream::new(new_dict, stream.content.clone()))
        }
        other => other.clone(),
    }
}

// ---------------------------------------------------------------------------
// Per-entry PDF generation
// ---------------------------------------------------------------------------

/// Generate a PDF for a single entry.  Returns the path inside `work_dir`.
pub(crate) fn entry_pdf(
    entry: &ExportEntry,
    index: usize,
    with_credit: bool,
    work_dir: &Path,
    posters: &HashMap<String, PathBuf>,
) -> Result<PathBuf> {
    let profile_dir = work_dir.join("chrome-profile");
    std::fs::create_dir_all(&profile_dir)?;

    match &entry.body {
        ExportBody::Page(asset) => {
            entry_pdf_from_page(entry, index, with_credit, work_dir, &profile_dir, asset)
        }
        ExportBody::Pdf(asset) => {
            entry_pdf_from_existing_pdf(entry, index, with_credit, work_dir, &profile_dir, asset)
        }
        _ => entry_pdf_from_html(entry, index, with_credit, work_dir, &profile_dir, posters),
    }
}

fn entry_pdf_from_html(
    entry: &ExportEntry,
    index: usize,
    with_credit: bool,
    work_dir: &Path,
    profile_dir: &Path,
    posters: &HashMap<String, PathBuf>,
) -> Result<PathBuf> {
    let html = render_document(std::slice::from_ref(entry), AssetMode::Print, posters, with_credit);
    let html_path = work_dir.join(format!("print-{index}.html"));
    std::fs::write(&html_path, &html)
        .with_context(|| format!("failed to write print HTML: {}", html_path.display()))?;
    let out = work_dir.join(format!("entry-{index}.pdf"));
    print_pdf(&html_path, &out, profile_dir)?;
    Ok(out)
}

fn entry_pdf_from_page(
    entry: &ExportEntry,
    index: usize,
    with_credit: bool,
    work_dir: &Path,
    profile_dir: &Path,
    asset: &Asset,
) -> Result<PathBuf> {
    let snapshot_bytes = std::fs::read(&asset.source)
        .with_context(|| format!("failed to read snapshot: {}", asset.source.display()))?;
    let snapshot = String::from_utf8_lossy(&snapshot_bytes);

    let title = entry_title(entry);
    let label = source_label(&entry.summary.source_kind, &entry.summary.entity_kind);
    let masthead = build_masthead_div(&title, &label, &entry.summary.archived_at);

    let injected = inject_after_body_open(&snapshot, &masthead);
    let injected = if with_credit {
        inject_before_body_close(&injected, brand::CREDIT_INLINE_HTML)
    } else {
        injected
    };

    let html_path = work_dir.join(format!("print-page-{index}.html"));
    std::fs::write(&html_path, injected.as_bytes())
        .with_context(|| format!("failed to write page HTML: {}", html_path.display()))?;
    let out = work_dir.join(format!("entry-{index}.pdf"));
    print_pdf(&html_path, &out, profile_dir)?;
    Ok(out)
}

fn entry_pdf_from_existing_pdf(
    entry: &ExportEntry,
    index: usize,
    with_credit: bool,
    work_dir: &Path,
    profile_dir: &Path,
    asset: &Asset,
) -> Result<PathBuf> {
    // Print the masthead as its own page.
    let title = entry_title(entry);
    let label = source_label(&entry.summary.source_kind, &entry.summary.entity_kind);
    let mhtml = build_masthead_page(&title, &label, &entry.summary.archived_at, with_credit);
    let mhtml_path = work_dir.join(format!("masthead-{index}.html"));
    std::fs::write(&mhtml_path, mhtml.as_bytes()).context("failed to write masthead HTML")?;
    let mast_pdf_path = work_dir.join(format!("masthead-{index}.pdf"));
    print_pdf(&mhtml_path, &mast_pdf_path, profile_dir)?;

    let out = work_dir.join(format!("entry-{index}.pdf"));

    match Document::load(&asset.source) {
        Ok(orig_doc) => {
            let mast_doc = Document::load(&mast_pdf_path).context("failed to reload masthead PDF")?;
            let mut merged = merge_documents(vec![mast_doc, orig_doc])?;
            merged.save(&out).context("failed to save merged PDF")?;
            Ok(out)
        }
        Err(e) => {
            eprintln!("warn: export: failed to load PDF artifact: {e:#}");
            // Fallback: render a plain HTML page for this entry.
            let html = render_document(
                std::slice::from_ref(entry),
                AssetMode::Print,
                &HashMap::new(),
                with_credit,
            );
            let html_path = work_dir.join(format!("print-{index}.html"));
            std::fs::write(&html_path, &html)?;
            print_pdf(&html_path, &out, profile_dir)?;
            Ok(out)
        }
    }
}

// ---------------------------------------------------------------------------
// Masthead injection helpers
// ---------------------------------------------------------------------------

fn build_masthead_div(title: &str, label: &str, archived_at: &str) -> String {
    use chrono::DateTime;
    let date = DateTime::parse_from_rfc3339(archived_at)
        .map(|dt| dt.format("%-d %B %Y").to_string())
        .unwrap_or_else(|_| archived_at.to_string());

    format!(
        r#"<div style="all:initial;display:block;max-width:760px;margin:0 auto 28px;padding:0 0 22px;border-bottom:1px solid #d2c6b5;font-family:'Helvetica Neue',Helvetica,Arial,sans-serif">
<p style="all:initial;display:block;margin:0 0 10px;font-size:10.5px;letter-spacing:.16em;text-transform:uppercase;color:#6b6f66">{label}</p>
<h1 style="all:initial;display:block;margin:0;font:600 38px/1.12 'Cormorant Garamond','Cormorant',Georgia,serif;letter-spacing:.005em;color:#20251f">{title}</h1>
<p style="all:initial;display:block;margin:12px 0 0;font-size:13px;color:#6b6f66">Archived {date}</p>
</div>"#,
        label = html_esc(label),
        title = html_esc(title),
        date = html_esc(&date),
    )
}

fn build_masthead_page(title: &str, label: &str, archived_at: &str, with_credit: bool) -> String {
    let masthead = build_masthead_div(title, label, archived_at);
    let credit = if with_credit { brand::CREDIT_HTML } else { "" };
    format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><style>body{{margin:0;font:17px/1.65 Georgia,serif;background:#f5f0e7;color:#20251f}}.ax-doc{{max-width:760px;margin:0 auto;padding:64px 28px 32px}}</style></head>
<body><main class="ax-doc">{masthead}</main>{credit}</body></html>"#,
    )
}

fn inject_after_body_open(html: &str, to_inject: &str) -> String {
    let lower = html.to_lowercase();
    if let Some(body_start) = lower.find("<body") {
        if let Some(rel_end) = lower[body_start..].find('>') {
            let insert_at = body_start + rel_end + 1;
            let mut out = String::with_capacity(html.len() + to_inject.len() + 1);
            out.push_str(&html[..insert_at]);
            out.push('\n');
            out.push_str(to_inject);
            out.push_str(&html[insert_at..]);
            return out;
        }
    }
    let mut out = String::with_capacity(html.len() + to_inject.len() + 1);
    out.push_str(to_inject);
    out.push('\n');
    out.push_str(html);
    out
}

fn inject_before_body_close(html: &str, to_inject: &str) -> String {
    let lower = html.to_lowercase();
    if let Some(close_pos) = lower.rfind("</body>") {
        let mut out = String::with_capacity(html.len() + to_inject.len() + 1);
        out.push_str(&html[..close_pos]);
        out.push('\n');
        out.push_str(to_inject);
        out.push_str(&html[close_pos..]);
        return out;
    }
    let mut out = html.to_string();
    out.push('\n');
    out.push_str(to_inject);
    out
}

fn html_esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
