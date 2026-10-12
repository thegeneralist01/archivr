//! Branding constants: export stylesheet and "Preserved with archivr" credit.

/// Stylesheet embedded in every HTML export.
///
/// Palette comes from `docs/branding/style-guide.html`.  No web fonts are
/// loaded; the stacks fall back to Georgia.
pub(crate) const EXPORT_CSS: &str = r#":root{--ink:#20251f;--paper:#f5f0e7;--line:#d2c6b5;--line-soft:#e5dccd;--accent:#8d3f30;--link:#245f72;--muted:#6b6f66;--brand-text:#f4ead8;
--display:'Cormorant Garamond','Cormorant',Georgia,serif;--serif:'Spectral',Georgia,serif;--sans:'Helvetica Neue',Helvetica,Arial,sans-serif}
*{box-sizing:border-box}html{background:var(--paper);color:var(--ink)}
body{margin:0;font:17px/1.65 var(--serif);-webkit-font-smoothing:antialiased}
.ax-doc{max-width:760px;margin:0 auto;padding:64px 28px 32px}
a{color:var(--link);text-decoration-thickness:1px;text-underline-offset:2px}
.ax-eyebrow{margin:0 0 10px;font:500 10.5px/1 var(--sans);letter-spacing:.16em;text-transform:uppercase;color:var(--muted)}
.ax-title{margin:0;font:600 38px/1.12 var(--display);letter-spacing:.005em}
h2.ax-title{font-size:30px}h3.ax-title{font-size:23px}
.ax-meta{margin:12px 0 0;font:13px/1.5 var(--sans);color:var(--muted)}.ax-meta a{color:inherit}
.ax-masthead{padding-bottom:22px;margin-bottom:28px;border-bottom:1px solid var(--line)}
.ax-entry+.ax-entry{margin-top:72px;padding-top:56px;border-top:1px solid var(--line)}
.ax-collection{margin-bottom:56px}.ax-toc{margin:20px 0 0;padding-left:1.2em;font-size:16px}.ax-toc li{margin:4px 0}
.ax-muted{color:var(--muted);font:13px var(--sans)}
.ax-media{margin:0}.ax-media img,.ax-media video{display:block;max-width:100%;height:auto;border-radius:3px;background:var(--line-soft)}
.ax-media figcaption,.ax-aside{margin:10px 0 0;font:13px var(--sans);color:var(--muted)}
.ax-frame{display:block;width:100%;height:80vh;border:1px solid var(--line);border-radius:3px;background:#fff}
.ax-text{white-space:pre-wrap;overflow-wrap:anywhere}
.ax-placeholder{padding:28px;border:1px solid var(--line);border-radius:3px;font:13px var(--sans);color:var(--muted);text-align:center}
.ax-empty{color:var(--muted);font-style:italic}
.ax-children{list-style:none;margin:0;padding:0}.ax-entry--child{margin-top:40px}
.ax-post{padding:18px 0;border-bottom:1px solid var(--line-soft)}.ax-post:last-child{border-bottom:0}
.ax-post-head{display:flex;align-items:center;gap:10px;margin-bottom:8px}
.ax-avatar{width:36px;height:36px;border-radius:50%}.ax-post-name{font:600 15px var(--sans)}
.ax-post-text{white-space:pre-wrap;overflow-wrap:anywhere}
.ax-post-media{display:grid;grid-template-columns:repeat(auto-fit,minmax(220px,1fr));gap:6px;margin-top:12px}
.ax-post-media img,.ax-post-media video{width:100%;border-radius:3px}
.ax-quote{margin:14px 0 0;padding:4px 16px;border-left:2px solid var(--line)}
.ax-post-link{display:inline-block;margin-top:10px;font:12px var(--sans);color:var(--muted)}
.ax-article-title{font:600 26px/1.2 var(--display);margin:6px 0 12px}.ax-article-cover{max-width:100%;border-radius:3px}
.ax-colophon{max-width:760px;margin:48px auto 0;padding:22px 28px 56px;border-top:1px solid var(--line);text-align:center}
.ax-credit{display:inline-flex;align-items:center;gap:8px;font:11px var(--sans);letter-spacing:.08em;color:var(--muted);text-decoration:none}
.ax-mark{display:inline-grid;place-items:center;width:16px;height:16px;border-radius:2.5px;background:var(--accent);color:var(--brand-text);font:600 12px/1 var(--display)}
.ax-wordmark{font:600 15px var(--display);letter-spacing:.01em;color:var(--ink)}
.ax-credit:hover .ax-wordmark{color:var(--accent)}
@media print{body{font-size:11.5pt}.ax-doc{padding:0;max-width:none}.ax-entry+.ax-entry{break-before:page;border-top:0;margin-top:0;padding-top:0}
.ax-post,.ax-media,.ax-quote{break-inside:avoid}.ax-colophon{margin-top:32px}}
@page{margin:18mm 16mm}"#;

/// The "Preserved with archivr" footer element.  Included verbatim when
/// `include_credit` is set; omitted entirely otherwise (no Archivr mention).
pub(crate) const CREDIT_HTML: &str = r#"<footer class="ax-colophon"><a class="ax-credit" href="https://github.com/thegeneralist01/archivr" rel="noopener">
<span class="ax-mark" aria-hidden="true">A</span><span>Preserved with <span class="ax-wordmark">archivr</span></span></a></footer>"#;

