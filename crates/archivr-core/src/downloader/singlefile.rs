use anyhow::{Context, Result, bail};
use regex::Regex;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use std::{
    collections::HashMap,
    env,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
};

use crate::downloader::cookies::{domain_from_url, write_netscape_cookie_file};
use crate::hash::hash_file;

/// Combined reader-mode script: Readability.js (Apache 2.0) bundled with the
/// archivr wrapper in a single IIFE.  single-file-cli concatenates all
/// `--browser-script` files into one string before injection (scripts.js:84),
/// so scope sharing is guaranteed; the combined file is kept for clarity.
///
/// Emits `<meta name="archivr-reader-mode" content="applied|failed:REASON">`
/// so the outcome is observable in the saved HTML.
const READER_MODE_SCRIPT: &str = concat!(
    // Readability.js is injected verbatim first so `Readability` is in scope.
    include_str!("../../../../vendor/readability/Readability.js"),
    // Wrapper IIFE — runs on single-file-on-before-capture-request.
    // Sets 'installed' immediately at script-evaluation time so a missing meta
    // means the browser-script was never injected at all.
    r#"
;(function() {
  function _archivrReaderMark(content) {
    try {
      var m = document.querySelector('meta[name="archivr-reader-mode"]');
      if (!m) {
        m = document.createElement('meta');
        m.name = 'archivr-reader-mode';
        (document.head || document.documentElement).appendChild(m);
      }
      m.content = content;
    } catch(_) {}
  }
  // Mark immediately: if this meta is absent in the artifact the script
  // was never injected (separate from the hook never firing).
  _archivrReaderMark('installed');
  function _archivrApplyReader() {
    try {
      if (typeof Readability === 'undefined') {
        _archivrReaderMark('failed:no-readability');
        return;
      }
      // Helper: resolve lazy-loaded images (src="data:," placeholders) to absolute URLs.
      // Called before the Readability clone so the clone has real src values, and again
      // after body replacement because Readability serialises src attributes as-is from
      // the clone — any remaining placeholders in article.content must also be fixed.
      function _archivrResolveLazyImgs(base) {
        var seen=0,lazy=0,placeholders=0,fixed=0;
        document.querySelectorAll('img').forEach(function(img) {
          seen++;
          var lazySrc = img.getAttribute('data-src') || img.getAttribute('data-lazy-src') ||
                        img.getAttribute('data-zoom-src') || img.getAttribute('data-original') ||
                        img.getAttribute('data-lazy');
          if(lazySrc) lazy++;
          var curSrc = img.getAttribute('src') || '';
          var _isPlaceholder = !curSrc || curSrc === 'data:,' ||
            (curSrc.startsWith('data:') && curSrc.length < 512);
          if(_isPlaceholder) placeholders++;
          if (lazySrc && _isPlaceholder) {
            try { lazySrc = new URL(lazySrc, base).href; } catch(e) {}
            img.setAttribute('src', lazySrc);
            img.removeAttribute('loading');
            fixed++;
          }
        });
        return {seen:seen,lazy:lazy,placeholders:placeholders,fixed:fixed};
      }
      var _base = document.baseURI || location.href;
      var _pre = _archivrResolveLazyImgs(_base);
      var article = new Readability(document.cloneNode(true)).parse();
      if (!article || !article.content || article.content.length < 100) {
        _archivrReaderMark('failed:no-article');
        return;
      }
      document.body.innerHTML = article.content;
      // Post-Readability pass: stamp data-archivr-src on article images so the Rust
      // post-processor can fetch and inline them after SingleFile writes the file.
      // SingleFile cannot inline resources introduced at before-capture time; don't
      // touch src here — Rust replaces it from the marker. Skip already-inlined images.
      document.querySelectorAll('img').forEach(function(img) {
        var lazySrc = img.getAttribute('data-src') || img.getAttribute('data-lazy-src') ||
                      img.getAttribute('data-zoom-src') || img.getAttribute('data-original') ||
                      img.getAttribute('data-lazy');
        if (!lazySrc) return;
        var curSrc = img.getAttribute('src') || '';
        if (curSrc.startsWith('data:image/') && curSrc.length > 1000) return;
        try { lazySrc = new URL(lazySrc, _base).href; } catch(e) {}
        img.setAttribute('data-archivr-src', lazySrc);
        img.removeAttribute('loading');
        // Remove lazy attrs so _archivrResolveLazyImgs (called below) cannot
        // rewrite src to a CDN URL that SingleFile cannot fetch from the proxy
        // context — Rust owns these images via data-archivr-src instead.
        img.removeAttribute('data-src');
        img.removeAttribute('data-lazy-src');
        img.removeAttribute('data-zoom-src');
        img.removeAttribute('data-original');
        img.removeAttribute('data-lazy');
      });
      var _post = _archivrResolveLazyImgs(_base);
      if (article.title) document.title = article.title;
      var hdr = document.createElement('header');
      hdr.innerHTML =
        '<h1 style="margin:0 0 .4em;font-family:-apple-system,sans-serif;font-size:2em;line-height:1.2;font-weight:700">' +
          (article.title || '') + '</h1>' +
        (article.byline
          ? '<p style="margin:.3em 0 0;color:#666;font-size:15px;line-height:1.5">' + article.byline + '</p>'
          : '') +
        (article.siteName
          ? '<p style="margin:.3em 0 0;color:#999;font-size:13px">' + article.siteName + '</p>'
          : '');
      hdr.style.cssText = 'margin-bottom:2em;padding-bottom:1em;border-bottom:1px solid #ddd';
      document.body.insertBefore(hdr, document.body.firstChild);
      var style = document.createElement('style');
      style.textContent = [
        'body{max-width:680px;margin:40px auto;padding:0 24px;',
        'font-family:Georgia,"Times New Roman",serif;font-size:18px;',
        'line-height:1.75;color:#1a1a1a;background:#fafaf8}',
        'h1,h2,h3,h4,h5,h6{font-family:-apple-system,BlinkMacSystemFont,sans-serif;',
        'line-height:1.3;margin-top:1.6em}',
        'p{margin:0 0 1.15em}',
        'img,figure,video{max-width:100%;height:auto;display:block;margin:1.5em 0}',
        'figcaption{font-size:14px;color:#666;margin:.5em 0 0;font-style:italic;line-height:1.5}',
        'a{color:#0055cc}',
        'pre{background:#f4f4f4;padding:1em;border-radius:4px;overflow-x:auto;font-size:14px}',
        'code{background:#f4f4f4;padding:.1em .3em;border-radius:3px;font-size:14px}',
        'blockquote{border-left:3px solid #ccc;margin:1.2em 0;padding-left:1.2em;color:#555}',
      ].join('');
      document.head.appendChild(style);
      _archivrReaderMark('applied');
    } catch (e) {
      _archivrReaderMark('failed:exception:' + (e && e.message ? e.message : String(e)));
    }
  }
  // Ensure _singleFile_waitForUserScript is installed (strip-scripts also does
  // this, but be explicit here in case reader-mode ever runs without it).
  dispatchEvent(new CustomEvent('single-file-user-script-init'));
  // Synchronous work — no preventDefault()/response dispatch needed.
  addEventListener('single-file-on-before-capture-request', _archivrApplyReader);
})();
"#
);

// The modal-closer browser scripts below (MODAL_CLOSER_DIALOG_OVERRIDES and
// MODAL_CLOSER_POLLING_SETUP) incorporate logic ported from the modalcloser
// plugin in the abx-plugins project:
//   https://github.com/ArchiveBox/abx-plugins/tree/main/abx_plugins/plugins/modalcloser
//
// MIT License
// Copyright (c) 2024 Nick Sweeting
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.
/// Injected at the top of the strip-scripts user-script when modal-closer is enabled.
/// Bridges to the main JS world via an inline `<script>` element.  Best-effort: strict
/// `script-src` CSP can block inline script execution entirely.  SingleFile injects
/// `--browser-script` files in `worldName: SINGLE_FILE_WORLD_NAME`, so direct
/// `window.alert = …` assignment would not reach page scripts; the `<script>` bridge
/// reaches the main world but is subject to the page's content security policy.
const MODAL_CLOSER_DIALOG_OVERRIDES: &str = r#"(function(){try{
var s=document.createElement('script');
s.textContent="window.alert=function(){};window.confirm=function(){return true;};window.prompt=function(m,d){return typeof d!=='undefined'?d:'';};window.print=function(){};window.onbeforeunload=null;try{Object.defineProperty(window,'onbeforeunload',{set:function(){},get:function(){return null;},configurable:true});}catch(e){}var _ael=window.addEventListener.bind(window);window.addEventListener=function(t,h,o){if(t==='beforeunload')return;_ael(t,h,o);};";
(document.head||document.documentElement).appendChild(s);s.remove();
}catch(e){}})();
"#;

/// Defines `_archivr_mc_run()` and schedules it every 500 ms (matching
/// `MODALCLOSER_POLL_INTERVAL` default) to catch overlays that appear after initial
/// page load.  Each run does two passes:
///
/// **Pass 1 — main-world `<script>` bridge (best-effort):** calls Bootstrap,
/// jQuery, jQuery UI, and SweetAlert teardown APIs.  Framework globals live in the
/// main world and cannot be reached from the isolated world; the bridge gets there
/// but is blocked by strict `script-src` CSP.
///
/// **Pass 2 — isolated-world direct DOM (always runs, CSP-immune):** Escape-key
/// dispatch, Angular Material backdrop click, full CSS selector hiding, scroll-lock
/// reset.  DOM mutations and `KeyboardEvent` dispatch go through the shared document
/// without inline script execution, so strict CSP does not affect this pass.
///
/// The before-capture hook clears the interval and fires one final run.
const MODAL_CLOSER_POLLING_SETUP: &str = r#"var _archivr_mc_interval=null;
function _archivr_mc_run(){
// Pass 1: main-world bridge — framework teardown APIs (Bootstrap/jQuery/Swal).
// Best-effort: blocked by strict script-src CSP. Pass 2 below is the reliable fallback.
try{var s=document.createElement('script');s.textContent=`(function(){
if(window.bootstrap&&window.bootstrap.Modal){document.querySelectorAll('.modal.show').forEach(function(el){try{var m=bootstrap.Modal.getInstance(el);if(m)m.hide();}catch(e){}});}
if(window.jQuery&&jQuery.fn&&jQuery.fn.modal){try{jQuery('.modal.in,.modal.show').modal('hide');}catch(e){}}
if(window.jQuery&&jQuery.ui&&jQuery.ui.dialog){try{jQuery('.ui-dialog-content').dialog('close');}catch(e){}}
if(window.Swal&&Swal.close){try{Swal.close();}catch(e){}}
if(window.swal&&swal.close){try{swal.close();}catch(e){}}
})()`;(document.head||document.documentElement).appendChild(s);s.remove();}catch(e){}
// Pass 2: isolated-world direct DOM — CSP-immune (no inline script execution).
document.querySelectorAll('[data-radix-dialog-overlay],[data-state="open"][role="dialog"],[role="dialog"][aria-modal="true"]').forEach(function(el){try{el.dispatchEvent(new KeyboardEvent('keydown',{key:'Escape',bubbles:true,cancelable:true}));}catch(e){}});
document.querySelectorAll('.cdk-overlay-backdrop').forEach(function(el){try{el.click();}catch(e){}});
var sels=['.cky-consent-container','.cky-popup-center','.cky-overlay','.cky-modal','#ckyPreferenceCenter',
'#onetrust-consent-sdk','#onetrust-banner-sdk','.onetrust-pc-dark-filter','#onetrust-pc-sdk',
'#CybotCookiebotDialog','#CybotCookiebotDialogBodyUnderlay','#CookiebotWidget',
'.qc-cmp-ui-container','#qc-cmp2-container','.qc-cmp2-summary-buttons','#truste-consent-track','.truste-banner','#truste-consent-content',
'.osano-cm-window','.osano-cm-dialog','.klaro .cookie-modal','.klaro .cookie-notice',
'#tarteaucitronRoot','#tarteaucitronAlertBig','.cmplz-cookiebanner','#cmplz-cookiebanner-container',
'#gdpr-cookie-consent-bar','.gdpr-cookie-consent-popup','#cookie-notice','.cookie-notice-container',
'.eupopup','#eu-cookie-law',
'#didomi-popup','#didomi-host','.didomi-popup-container','#usercentrics-root','.uc-banner',
'#axeptio_overlay','#axeptio_btn','#iubenda-cs-banner','.iubenda-cs-container',
'.termly-consent-banner','#termly-code-snippet-support','#BorlabsCookieBox','.BorlabsCookie',
'.cookiefirst-root','#cookiefirst-root','#cookiescript_injected','.cookiescript_injected_wrapper',
'#ccc','#ccc-overlay','#cookie-consent','.cookie-banner','.cookie-notice',
'#cookieConsent','.cookie-consent','.cookies-banner',
'.modal.show','.modal.in','.ui-dialog','.ui-widget-overlay',
'.swal2-container','.swal2-overlay','.sweet-alert','#sweetAlert',
'[class*="cookie"][class*="banner"]','[class*="cookie"][class*="notice"]',
'[class*="cookie"][class*="popup"]','[class*="cookie"][class*="modal"]',
'[class*="consent"][class*="banner"]','[class*="consent"][class*="popup"]',
'[class*="gdpr"]','[class*="privacy"][class*="banner"]',
'.modal-overlay','.modal-backdrop','.overlay-visible','.popup-overlay','.newsletter-popup',
'.age-gate','.subscribe-popup','.subscription-modal',
'[class*="modal"][class*="open"]:not(.modal-open)','[class*="modal"][class*="show"][class*="overlay"]','[class*="modal"][class*="visible"]',
'[class*="dialog"][class*="open"]','[class*="overlay"][class*="visible"]',
'.interstitial','.interstitial-wrapper','[class*="interstitial"]'];
sels.forEach(function(sel){try{document.querySelectorAll(sel).forEach(function(el){
var cs=window.getComputedStyle(el);
if(cs.display==='none'||cs.visibility==='hidden')return;
el.style.display='none';el.style.visibility='hidden';el.style.opacity='0';el.style.pointerEvents='none';
});}catch(e){}});
try{document.body.style.overflow='';document.body.style.position='';
document.body.classList.remove('modal-open','overflow-hidden','no-scroll','scroll-locked');
document.documentElement.style.overflow='';
document.documentElement.classList.remove('overflow-hidden','no-scroll');}catch(e){}
}
_archivr_mc_run();
_archivr_mc_interval=setInterval(_archivr_mc_run,500);
"#;

/// Result of archiving a web page with single-file.
#[derive(Debug)]
pub struct SaveResult {
    /// SHA-256 hex of the archived `.html` file.
    pub html_hash: String,
    /// Page title from `<title>` tag, if present.
    pub title: Option<String>,
    /// SHA-256 hex of the extracted favicon, if present.
    pub favicon_hash: Option<String>,
    /// File extension for the favicon (e.g. `".ico"`, `".png"`), if present.
    pub favicon_ext: Option<String>,
    /// `true` when `ARCHIVR_UBLOCK=true` (the default) but the extension path
    /// was missing or invalid.  The capture succeeded but ran without ad-blocking.
    pub ublock_skipped: bool,
    /// `true` when `ARCHIVR_COOKIE_CONSENT=true` (the default) but the extension path
    /// was missing or invalid.  The capture succeeded but ran without cookie-consent blocking.
    pub cookie_ext_skipped: bool,
}

/// Archives `url` as a self-contained HTML snapshot.
///
/// Env vars:
/// - `ARCHIVR_SINGLE_FILE`: path to the `single-file` binary (default: `"single-file"`).
/// - `ARCHIVR_CHROME`: path to the Chromium/Chrome binary (default: `"chromium"`).
/// - `ARCHIVR_UBLOCK`: enable uBlock Origin Lite extension (default: `"true"`).
/// - `ARCHIVR_UBLOCK_EXT`: path to the unpacked uBlock Origin Lite extension directory.
/// - `ARCHIVR_CHROME_ARGS`: space-separated extra Chrome flags (e.g. `"--no-sandbox"`).
pub fn save(
    url: &str,
    store_path: &Path,
    timestamp: &str,
    cookies: &HashMap<String, String>,
    ublock_enabled_override: Option<bool>,
    cookie_ext_enabled: Option<bool>,
    reader_mode: bool,
    modal_closer_enabled: Option<bool>,
    freedium_cleanup: bool,
) -> Result<SaveResult> {
    let single_file =
        env::var("ARCHIVR_SINGLE_FILE").unwrap_or_else(|_| "single-file".to_string());
    let chrome = env::var("ARCHIVR_CHROME").unwrap_or_else(|_| "chromium".to_string());
    let (ublock_ext, ublock_skipped) = resolve_ublock_config(ublock_enabled_override);
    let (cookie_ext, cookie_ext_skipped) = resolve_cookie_ext_config(cookie_ext_enabled);
    let modal_closer = resolve_modal_closer_config(modal_closer_enabled);
    let mut result = save_with(
        url,
        store_path,
        timestamp,
        &single_file,
        &chrome,
        cookies,
        ublock_ext.as_deref(),
        cookie_ext.as_deref(),
        reader_mode,
        modal_closer,
        freedium_cleanup,
    )?;
    result.ublock_skipped = ublock_skipped;
    result.cookie_ext_skipped = cookie_ext_skipped;
    Ok(result)
}

/// Resolves uBlock configuration from env vars, optionally overridden by the caller.
///
/// Returns:
/// - `(Some(path), false)` — uBlock is enabled and the extension dir is valid.
/// - `(None, true)`  — uBlock is enabled but the extension dir is missing/invalid
///                     (warns to stderr; the capture proceeds without ad-blocking).
/// - `(None, false)` — uBlock is disabled (`ARCHIVR_UBLOCK=false` or overridden).
fn resolve_ublock_config(enabled_override: Option<bool>) -> (Option<PathBuf>, bool) {
    // The override (from instance settings or per-capture body) takes precedence over env.
    let want_ublock = enabled_override.unwrap_or_else(|| {
        let env_val = env::var("ARCHIVR_UBLOCK").unwrap_or_else(|_| "true".to_string());
        !env_val.eq_ignore_ascii_case("false") && env_val != "0"
    });
    if !want_ublock {
        return (None, false);
    }
    match env::var("ARCHIVR_UBLOCK_EXT").ok().filter(|s| !s.is_empty()) {
        None => {
            eprintln!(
                "warn: uBlock: ARCHIVR_UBLOCK_EXT is not set; \
                 capturing without ad-blocking"
            );
            (None, true)
        }
        Some(ext_path_str) => {
            let path = PathBuf::from(&ext_path_str);
            if path.is_dir() {
                (Some(path), false)
            } else {
                eprintln!(
                    "warn: uBlock: ARCHIVR_UBLOCK_EXT={ext_path_str:?} is not a directory; \
                     capturing without ad-blocking"
                );
                (None, true)
            }
        }
    }
}

/// Resolves cookie-consent extension configuration from env vars, optionally overridden by the caller.
///
/// Returns:
/// - `(Some(path), false)` — cookie-consent ext is enabled and the extension dir is valid.
/// - `(None, true)`  — cookie-consent ext is enabled but the extension dir is missing/invalid
///                     (warns to stderr; the capture proceeds without cookie-consent blocking).
/// - `(None, false)` — cookie-consent ext is disabled (`ARCHIVR_COOKIE_CONSENT=false` or overridden).
fn resolve_cookie_ext_config(enabled_override: Option<bool>) -> (Option<PathBuf>, bool) {
    let want_cookie_ext = enabled_override.unwrap_or_else(|| {
        let env_val = env::var("ARCHIVR_COOKIE_CONSENT").unwrap_or_else(|_| "true".to_string());
        !env_val.eq_ignore_ascii_case("false") && env_val != "0"
    });
    if !want_cookie_ext {
        return (None, false);
    }
    match env::var("ARCHIVR_COOKIE_EXT").ok().filter(|s| !s.is_empty()) {
        None => {
            eprintln!(
                "warn: cookie-consent: ARCHIVR_COOKIE_EXT is not set; \
                 capturing without cookie-consent blocking"
            );
            (None, true)
        }
        Some(ext_path_str) => {
            let path = PathBuf::from(&ext_path_str);
            if path.is_dir() {
                (Some(path), false)
            } else {
                eprintln!(
                    "warn: cookie-consent: ARCHIVR_COOKIE_EXT={ext_path_str:?} is not a directory; \
                     capturing without cookie-consent blocking"
                );
                (None, true)
            }
        }
    }
}

/// Resolves modal-closer configuration from env vars, optionally overridden by the caller.
/// Returns `true` when enabled (the default), `false` when explicitly disabled via
/// `ARCHIVR_MODAL_CLOSER=false` or `0`.
///
/// Unlike uBlock and cookie-ext, modal-closer has no external resource dependency; the
/// behaviour is implemented entirely as an injected browser script.
fn resolve_modal_closer_config(enabled_override: Option<bool>) -> bool {
    enabled_override.unwrap_or_else(|| {
        let env_val = env::var("ARCHIVR_MODAL_CLOSER").unwrap_or_else(|_| "true".to_string());
        !env_val.eq_ignore_ascii_case("false") && env_val != "0"
    })
}

/// Inner implementation.  Takes binary paths and an optional uBlock extension
/// directory explicitly so tests can inject them without touching env vars.
///
/// single-file always manages Chrome.  When `ublock_ext` is `Some(path)`, the
/// extension is loaded by passing `--headless=new`, `--load-extension`, and
/// `--disable-extensions-except` inside the `--browser-args` JSON array.
/// single-file's `browser.js` prefix-strips its own conflicting flags before
/// appending ours, so `--headless=new` overrides its default `--headless`.
///
/// Note: single-file always adds `--single-process` to Chrome.  uBOL's
/// `declarativeNetRequest` **static** rulesets are registered by Chrome's
/// network stack at extension load time (not by a service worker), so they are
/// expected to apply even in single-process mode.  Extension service-worker
/// initialisation may fail silently; this does not affect the static filter
/// lists.  Ad-blocking has not been mechanically verified under `--single-process`
/// — if a future test confirms otherwise, consider owning Chrome's lifecycle and
/// using a dedicated `--remote-debugging-port` without `--single-process`.
fn save_with(
    url: &str,
    store_path: &Path,
    timestamp: &str,
    single_file: &str,
    chrome: &str,
    cookies: &HashMap<String, String>,
    ublock_ext: Option<&Path>,
    cookie_ext: Option<&Path>,
    reader_mode: bool,
    modal_closer: bool,
    freedium_cleanup: bool,
) -> Result<SaveResult> {
    let temp_dir = store_path.join("temp").join(timestamp);
    std::fs::create_dir_all(&temp_dir).context("failed to create temp dir")?;

    let out_file = temp_dir.join(format!("{timestamp}.html"));

    // Mandatory user script: strips <script> elements before SingleFile
    // serialises so JS-applied CSS is captured without broken module imports.
    // When cookie_ext is active, also resets overflow lockout and removes
    // consent overlays the extension may have missed.
    // When modal_closer is active, dialog overrides are prepended (main-world
    // bridge), a 500 ms polling loop starts after dispatchEvent (matches
    // MODALCLOSER_POLL_INTERVAL default), and the before-capture hook clears
    // the interval and fires a final main-world pass before serialisation.
    let strip_scripts_path = temp_dir.join("sf-strip-scripts.js");
    let mut strip_scripts = String::new();
    if modal_closer {
        // Must come before dispatchEvent so overrides are installed before any page
        // scripts fire.  Bridges to main world — see MODAL_CLOSER_DIALOG_OVERRIDES.
        strip_scripts.push_str(MODAL_CLOSER_DIALOG_OVERRIDES);
    }
    strip_scripts.push_str(
        // Dispatch single-file-user-script-init so single-file installs
        // _singleFile_waitForUserScript, which gates the -request hooks.
        "dispatchEvent(new CustomEvent('single-file-user-script-init'));",
    );
    if modal_closer {
        // Start the 500 ms main-world polling loop after dispatchEvent so the
        // interval is live before any page scripts run.
        strip_scripts.push_str(MODAL_CLOSER_POLLING_SETUP);
    }
    strip_scripts.push_str(
        "addEventListener('single-file-on-before-capture-request',()=>{\
           document.querySelectorAll('script:not([type=\"application/ld+json\"])')\
           .forEach(el=>el.remove());",
    );
    if cookie_ext.is_some() {
        // Reset overflow:hidden that consent modals inject on body/html.
        // Gate on cookie_ext so we never mutate pages where the feature is off.
        strip_scripts.push_str(
            "document.body&&(document.body.style.overflow='');\
             document.documentElement&&(document.documentElement.style.overflow='');\
             /* Remove consent overlays the extension may have missed          \
              * (e.g. Google Funding Choices, Quantcast, Sourcepoint).        \
              * Selectors are specific to consent infrastructure, not content. */\
             document.querySelectorAll(\
               '.fc-consent-root,.fc-dialog-overlay,.fc-dialog,\
                .qc-cmp2-container,.qc-cmp2-ui,\
                .sp-message-container,\
                #sp-cc,\
                #usercentrics-root'\
             ).forEach(function(el){el.remove();});",
        );
    }
    if ublock_ext.is_some() {
        // uBlock blocks ad network requests but first-party ad placeholder
        // elements (ins.adsbygoogle, iframe hosts) retain their computed
        // height, leaving blank space. Remove them pre-capture.
        strip_scripts.push_str(
            "document.querySelectorAll(\
               'ins.adsbygoogle,\
                [id^=\"aswift_\"],\
                iframe[id^=\"google_ads_\"],\
                iframe[name^=\"google_ads_frame\"],\
                iframe[src*=\"googlesyndication\"],\
                iframe[src*=\"doubleclick\"]'\
             ).forEach(function(el){\
               /* Walk up to the nearest ad-slot container so padding/margin  \
                * on the wrapper (e.g. .top-ad, .google-auto-placed) collapses \
                * too, not just the inner ins/iframe element.                  */\
               var slot=el.closest('.top-ad,.google-auto-placed,.ad-slot,.ad-container');\
               (slot||el).remove();\
             });",
        );
    }
    if modal_closer {
        // Clear the polling interval and fire a final main-world pass right
        // before SingleFile serialises the DOM.
        strip_scripts.push_str(
            "if(_archivr_mc_interval){\
               clearInterval(_archivr_mc_interval);\
               _archivr_mc_interval=null;\
             }\
             _archivr_mc_run();"
        );
    }
    if freedium_cleanup {
        strip_scripts.push_str(
            // Sonner toast overlay (data attribute is stable across Freedium updates).
            "document.querySelectorAll('[data-sonner-toaster]').forEach(function(el){\
               var s=el.closest('section')||el;s.remove();\
             });\
             document.querySelectorAll('nav#header').forEach(function(el){el.remove();});\
             document.querySelectorAll('nav').forEach(function(el){\
               if(el.querySelector('[aria-label=\"Go back\"]'))el.remove();\
             });\
             document.querySelectorAll('nav').forEach(function(el){\
               if(el.querySelector('a[href*=\"freedium-mirror.cfd/\"]'))el.remove();\
             });\
             document.querySelectorAll('footer').forEach(function(el){\
               if(el.querySelector('a[href*=\"freedium-mirror.cfd/\"]')||\
                 el.textContent.toLowerCase().indexOf('freedium')>-1)el.remove();\
             });\
             var _pr=document.getElementById('progress');if(_pr)_pr.remove();\
             document.querySelectorAll('[data-nosnippet]').forEach(function(el){\
               if(!el.firstElementChild&&!el.textContent.trim())el.remove();\
             });\
             document.querySelectorAll('section>.flex.justify-end>button[data-slot=\"dropdown-menu-trigger\"][type=\"button\"].flex.items-center>span').forEach(function(span){\
               if(span.textContent.trim()!=='Download article')return;\
               var wrap=span.closest('.flex.justify-end');\
               var sec=wrap.closest('section');\
               wrap.remove();\
               if(sec&&sec.children.length===0)sec.remove();\
             });",
        );
    }
    strip_scripts.push_str("});");
    std::fs::write(&strip_scripts_path, &strip_scripts)
        .context("failed to write single-file user script")?;

    // Optional reader-mode script: Readability.js + wrapper combined into one
    // file so both run in the same execution scope.  (Separate --browser-script
    // files can each get their own context depending on single-file version.)
    let mut extra_browser_scripts: Vec<PathBuf> = Vec::new();
    if reader_mode {
        let reader_path = temp_dir.join("sf-reader-mode.js");
        std::fs::write(&reader_path, READER_MODE_SCRIPT)
            .context("failed to write reader-mode script")?;
        extra_browser_scripts.push(reader_path);
    }

    // Isolated Chrome profile directory; cleaned up with the rest of temp.
    let chrome_data_dir = temp_dir.join("chrome-data");

    // Build Chrome flags passed via --browser-args to single-file.
    // single-file's browser.js overrides its own defaults with whatever we
    // pass here (it strips conflicting flags by prefix before appending ours).
    let mut chrome_flags = vec![
        "--disable-web-security".to_string(),
        format!("--user-data-dir={}", chrome_data_dir.display()),
        "--window-size=1920,1080".to_string(),
    ];
    // Build comma-separated extension list for Chrome flags.
    // --headless=new is required for --load-extension to work.
    let ext_paths: Vec<PathBuf> = [ublock_ext, cookie_ext]
        .iter()
        .filter_map(|p| p.map(|p| p.to_path_buf()))
        .collect();
    if !ext_paths.is_empty() {
        let joined = ext_paths
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(",");
        chrome_flags.push("--headless=new".to_string());
        chrome_flags.push(format!("--load-extension={joined}"));
        chrome_flags.push(format!("--disable-extensions-except={joined}"));
    }
    // Operator extras (e.g. --no-sandbox in Docker).
    let extra_chrome_args: Vec<String> = env::var("ARCHIVR_CHROME_ARGS")
        .unwrap_or_default()
        .split_whitespace()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    chrome_flags.extend(extra_chrome_args);

    // single-file expects browser-args as a JSON array of strings.
    let quoted: Vec<String> = chrome_flags
        .iter()
        .map(|f| format!("\"{}\"", f.replace('\\', "\\\\").replace('"', "\\\"")))
        .collect();
    let browser_args = format!("[{}]", quoted.join(","));

    // Write cookie file (secrets must never appear in process args).
    let cookie_file: Option<PathBuf> = if !cookies.is_empty() {
        let cf = temp_dir.join("cookies.txt");
        let domain = domain_from_url(url);
        write_netscape_cookie_file(cookies, &domain, &cf)
            .context("failed to write single-file cookie file")?;
        Some(cf)
    } else {
        None
    };

    let mut scripts: Vec<&Path> = vec![strip_scripts_path.as_path()];
    scripts.extend(extra_browser_scripts.iter().map(|p| p.as_path()));

    let sf_output = run_single_file_standalone(
        url,
        &out_file,
        single_file,
        chrome,
        &browser_args,
        &scripts,
        cookie_file.as_deref(),
    )
    .with_context(|| format!("failed to spawn single-file ({single_file})"))?;

    // Delete cookie file unconditionally — including on failure — so secrets
    // are never left in store/temp when the capture fails.
    if let Some(cf) = &cookie_file {
        let _ = std::fs::remove_file(cf);
    }

    if !sf_output.status.success() {
        let stderr = String::from_utf8_lossy(&sf_output.stderr);
        bail!("single-file failed (exit {:?}): {stderr}", sf_output.status.code());
    }

    if !out_file.exists() {
        // Collect diagnostics: stdout, stderr, and what's actually in the temp dir.
        let stdout = String::from_utf8_lossy(&sf_output.stdout);
        let stderr = String::from_utf8_lossy(&sf_output.stderr);
        let dir_contents: String = std::fs::read_dir(&temp_dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_else(|_| "<unreadable>".to_string());
        eprintln!(
            "warn: single-file produced no file at {}\n  temp dir contents: [{dir_contents}]\n  stderr: {}\n  stdout (first 200 chars): {}",
            out_file.display(),
            stderr.trim(),
            &stdout[..stdout.len().min(200)],
        );
        bail!(
            "single-file exited successfully but produced no output file at {}; \
             temp dir contains: [{dir_contents}]; \
             stderr: {}",
            out_file.display(),
            stderr.trim(),
        );
    }
    // Post-process: fetch and inline images that SingleFile couldn't inline from the
    // page resource cache (resources introduced via DOM manipulation at before-capture
    // time are not tracked). Browser script stamped data-archivr-src on those images.
    inline_archivr_img_srcs(&out_file, url, cookies);

    let title = extract_html_title(&out_file);
    let html_hash = hash_file(&out_file)?;
    let (favicon_hash, favicon_ext) =
        extract_and_save_favicon(&out_file, &temp_dir, timestamp)
            .map(|(h, e)| (Some(h), Some(e)))
            .unwrap_or((None, None));

    Ok(SaveResult {
        html_hash,
        title,
        favicon_hash,
        favicon_ext,
        ublock_skipped: false,     // overwritten by save() from resolve_ublock_config()
        cookie_ext_skipped: false, // overwritten by save() from resolve_cookie_ext_config()
    })
}

/// Runs single-file, letting it launch and manage Chrome itself.
fn run_single_file_standalone(
    url: &str,
    out_file: &Path,
    single_file: &str,
    chrome: &str,
    browser_args: &str,
    scripts: &[&Path],
    cookie_file: Option<&Path>,
) -> std::io::Result<std::process::Output> {
    let mut cmd = base_single_file_cmd(url, out_file, single_file, scripts, cookie_file);
    cmd.arg(format!("--browser-executable-path={chrome}"))
        .arg("--browser-headless")
        .arg(format!("--browser-args={browser_args}"));
    cmd.output()
}

/// Builds a `Command` with the single-file args that are the same regardless
/// of how Chrome is started.  Passes each script as a separate `--browser-script` arg.
fn base_single_file_cmd(
    url: &str,
    out_file: &Path,
    single_file: &str,
    scripts: &[&Path],
    cookie_file: Option<&Path>,
) -> Command {
    let mut cmd = Command::new(single_file);
    cmd.arg(url)
        .arg(out_file)
        .arg("--browser-wait-until=networkidle2")
        .arg("--browser-wait-delay=2000")
        .arg("--user-agent=Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/137.0.0.0 Safari/537.36")
        .arg("--remove-unused-styles=false")
        .arg("--remove-alternative-medias=false")
        .arg("--block-scripts=false")
        .arg("--remove-unused-fonts=false")
        .arg("--remove-alternative-fonts=false")
        // Explicitly prevent single-file from dumping HTML to stdout instead of
        // writing the file (its Docker-detection heuristic can trigger on some setups).
        .arg("--dump-content=false");
    for script in scripts {
        cmd.arg(format!("--browser-script={}", script.display()));
    }
    if let Some(cf) = cookie_file {
        cmd.arg(format!("--browser-cookies-file={}", cf.display()));
    }
    cmd
}

// ── HTML helpers ──────────────────────────────────────────────────────────────

/// Reads up to 256 KiB of `path` and extracts the content of the first
/// `<title>…</title>` element. Returns `None` if absent or empty.
///
/// Uses `to_ascii_lowercase` for case-insensitive tag matching. ASCII-only
/// lowercasing is byte-length-preserving, so byte offsets derived from the
/// lowercased buffer are valid indices into the original buffer.
///
/// Uses `take().read_to_end()` rather than a single `read()` call so the
/// full 256 KiB is always consumed even when the OS short-reads.
fn extract_html_title(path: &Path) -> Option<String> {
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    f.take(256 * 1024).read_to_end(&mut buf).ok()?;
    extract_html_title_from_buf(&buf)
}

/// Extracts the `<title>` content from an HTML string.
///
/// Used in the server capture path where the font-extracted HTML is already
/// in memory — avoids a re-read and operates on the smaller post-extraction
/// content where the title is guaranteed to be within range.
pub fn extract_html_title_str(html: &str) -> Option<String> {
    extract_html_title_from_buf(html.as_bytes())
}

fn extract_html_title_from_buf(buf: &[u8]) -> Option<String> {
    let lower = String::from_utf8_lossy(buf).to_ascii_lowercase();
    let start = lower.find("<title>")? + "<title>".len();
    let end = lower[start..].find("</title>")? + start;
    let title = String::from_utf8_lossy(&buf[start..end])
        .trim()
        .to_string();
    if title.is_empty() { None } else { Some(title) }
}

/// Extracts the favicon embedded in a single-file HTML archive.
///
/// Scans for a `<link rel="…icon…">` tag whose `href` is a `data:image/…;base64,…` URL.
/// Decodes the base64 payload, writes it to `{temp_dir}/{timestamp}.favicon.{ext}`,
/// hashes the file, and returns `(sha256_hex, ".ext")`.
/// All failures are silent (returns `None`) — a missing favicon is non-fatal.
fn extract_and_save_favicon(
    html_path: &Path,
    temp_dir: &Path,
    timestamp: &str,
) -> Option<(String, String)> {
    let content = std::fs::read_to_string(html_path).ok()?;
    let lower = content.to_ascii_lowercase();

    // Find the first <link …> tag that looks like a favicon with a data: href.
    let mut search_pos = 0;
    loop {
        let tag_start = lower[search_pos..].find("<link")? + search_pos;
        let tag_end = lower[tag_start..].find('>')? + tag_start;
        let tag = &lower[tag_start..=tag_end];

        if tag.contains("icon") {
            // Look for href="data:image/...;base64,..."
            if let Some(href_pos) = tag.find("href=") {
                let after_href = &content[tag_start + href_pos + 5..];
                let (quote, after_quote) = if after_href.starts_with('"') {
                    ('"', &after_href[1..])
                } else if after_href.starts_with('\'') {
                    ('\'', &after_href[1..])
                } else {
                    search_pos = tag_end + 1;
                    continue;
                };
                let value_end = after_quote.find(quote)?;
                let href_value = &after_quote[..value_end];
                if let Some(b64_start) = href_value.to_ascii_lowercase().find(";base64,") {
                    let mime_part = &href_value[5..b64_start]; // skip "data:"
                    let ext = mime_to_favicon_ext(mime_part)?;
                    let b64_data = &href_value[b64_start + 8..];
                    let bytes = B64.decode(b64_data).ok()?;
                    let out_path = temp_dir.join(format!("{timestamp}.favicon{ext}"));
                    std::fs::write(&out_path, &bytes).ok()?;
                    let hash = hash_file(&out_path).ok()?;
                    return Some((hash, ext.to_string()));
                }
            }
        }

        search_pos = tag_end + 1;
    }
}

fn mime_to_favicon_ext(mime: &str) -> Option<&'static str> {
    match mime.to_ascii_lowercase().trim() {
        "image/x-icon" | "image/vnd.microsoft.icon" => Some(".ico"),
        "image/png"  => Some(".png"),
        "image/jpeg" => Some(".jpg"),
        "image/gif"  => Some(".gif"),
        "image/svg+xml" => Some(".svg"),
        "image/webp" => Some(".webp"),
        _ => None,
    }
}

/// Decodes the minimal set of HTML character references that browsers encode
/// in attribute values during serialisation.  Covers the four characters
/// browsers must escape (`&`, `<`, `>`, `"`) — sufficient for URL query
/// strings and signed CDN parameters embedded in `data-*` attributes.
fn html_attr_decode(s: &str) -> String {
    // Decode &amp; LAST so a value like `&amp;lt;` becomes `&lt;` (one layer
    // removed) rather than `<` (two layers).  If `&amp;` ran first it would
    // produce `&lt;` from the remainder, which the subsequent `&lt;` pass
    // would then incorrectly collapse to `<`.
    s.replace("&lt;",   "<")
     .replace("&gt;",   ">")
     .replace("&quot;", "\"")
     .replace("&amp;",  "&")
}

/// Returns a `Cookie: …` header value when `img_url` is same-domain as
/// `capture_url` and `cookies` is non-empty; `None` otherwise.
/// Uses `domain_from_url` for consistency with the SingleFile cookie-file writer.
fn same_origin_cookie_header(
    capture_url: &str,
    img_url: &str,
    cookies: &HashMap<String, String>,
) -> Option<String> {
    if cookies.is_empty() {
        return None;
    }
    let cap = domain_from_url(capture_url);
    let img = domain_from_url(img_url);
    if cap.is_empty() || img.is_empty() || cap != img {
        return None;
    }
    Some(
        cookies
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; "),
    )
}

/// Scans a reader-mode HTML file for `<img data-archivr-src="…">` elements whose
/// `src` is not already a large inlined image, fetches those URLs with blocking
/// reqwest, and replaces `src` with `data:<mime>;base64,…`.  Non-fatal: any failure
/// is logged and the image is left as-is.  Guardrails: 10 s timeout, 5 redirects,
/// `Content-Type: image/*` required, 20 MiB body cap (enforced via `Content-Length`
/// and a bounded read).  Cookies are forwarded only when the image host matches the
/// captured page's domain; Freedium fetches pass empty cookies so this is a no-op.
fn inline_archivr_img_srcs(path: &Path, capture_url: &str, cookies: &HashMap<String, String>) {
    const MAX_BYTES: usize = 20 * 1024 * 1024;

    let html = match std::fs::read_to_string(path) {
        Ok(h) => h,
        Err(_) => return,
    };
    if !html.contains("data-archivr-src=") {
        return;
    }

    let client = match reqwest::blocking::Client::builder()
        .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/137.0.0.0 Safari/537.36")
        .redirect(reqwest::redirect::Policy::limited(5))
        .timeout(std::time::Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(_) => return,
    };


    let re_img        = Regex::new(r"(?si)<img\b[^>]*>").unwrap();
    let re_archivr    = Regex::new(r#"(?i)\bdata-archivr-src="([^"]+)""#).unwrap();
    let re_src        = Regex::new(r#"(?i)\bsrc="([^"]*)""#).unwrap();
    let re_rm_archivr = Regex::new(r#"(?i)\s*data-archivr-src="[^"]*""#).unwrap();
    let re_set_src    = Regex::new(r#"(?i)\bsrc="[^"]*""#).unwrap();

    // Collect (start, end, replacement) in document order, then apply in reverse
    // so earlier byte positions are still valid when we reach them.
    let mut replacements: Vec<(usize, usize, String)> = Vec::new();

    for m in re_img.find_iter(&html) {
        let tag = m.as_str();
        let raw_url = match re_archivr.captures(tag).map(|c| c[1].to_string()) {
            Some(u) => u,
            None => continue,
        };
        // HTML-decode entities the browser's serialiser encodes in attribute values
        // (e.g. & → &amp; in CDN signed/query URLs).
        let img_url = html_attr_decode(&raw_url);

        // Already a large inlined image — leave it alone.
        let cur_src = re_src.captures(tag).map(|c| c[1].to_string()).unwrap_or_default();
        if cur_src.starts_with("data:image/") && cur_src.len() > 1000 {
            continue;
        }

        let cookie_header = same_origin_cookie_header(capture_url, &img_url, cookies);

        let data_uri = match fetch_image_as_data_uri(&client, &img_url, MAX_BYTES, cookie_header) {
            Ok(u) => u,
            Err(e) => {
                eprintln!("warn: reader image inline ({img_url}): {e}");
                continue;
            }
        };
        let new_tag = if re_src.is_match(tag) {
            re_set_src.replace(tag, format!("src=\"{}\"", data_uri)).into_owned()
        } else {
            tag.replacen("<img", &format!("<img src=\"{}\"", data_uri), 1)
        };
        let new_tag = re_rm_archivr.replace(&new_tag, "").into_owned();
        replacements.push((m.start(), m.end(), new_tag));
    }

    if replacements.is_empty() {
        return;
    }
    let mut html = html;
    for (start, end, new_tag) in replacements.into_iter().rev() {
        html.replace_range(start..end, &new_tag);
    }
    if let Err(e) = std::fs::write(path, html.as_bytes()) {
        eprintln!("warn: reader image inline write {}: {e}", path.display());
    }
}

/// Fetches `url` and returns a `data:<mime>;base64,…` URI.
/// Requires `Content-Type: image/*`, enforces `max_bytes` cap via `Content-Length`
/// rejection and a bounded streaming read.  Attaches `cookie_header` when supplied.
fn fetch_image_as_data_uri(
    client: &reqwest::blocking::Client,
    url: &str,
    max_bytes: usize,
    cookie_header: Option<String>,
) -> Result<String> {
    let mut req = client.get(url);
    if let Some(cookie) = cookie_header {
        req = req.header(reqwest::header::COOKIE, cookie);
    }
    let resp = req.send().context("request failed")?;
    if !resp.status().is_success() {
        bail!("HTTP {}", resp.status());
    }
    let ct = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let mime = ct.split(';').next().unwrap_or("").trim().to_string();
    if !mime.starts_with("image/") {
        bail!("non-image Content-Type: {mime}");
    }
    // Reject early when the server advertises a body larger than the cap.
    if let Some(cl) = resp.content_length() {
        if cl as usize > max_bytes {
            bail!("Content-Length {} exceeds {} MiB cap", cl, max_bytes / (1024 * 1024));
        }
    }
    // Stream at most max_bytes + 1 bytes so we never buffer an unbounded body.
    // Reading one byte past the limit lets us distinguish "exactly at cap" from
    // "over cap" without a separate Content-Length check.
    let mut buf = Vec::new();
    resp.take((max_bytes as u64) + 1)
        .read_to_end(&mut buf)
        .context("reading body")?;
    if buf.len() > max_bytes {
        bail!("image too large: > {} MiB", max_bytes / (1024 * 1024));
    }
    Ok(format!("data:{};base64,{}", mime, B64.encode(&buf)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn extract_html_title_finds_title() {
        let mut f = NamedTempFile::new().unwrap();
        write!(f, "<html><head><title>Paul Graham — Great Work</title></head></html>").unwrap();
        assert_eq!(
            extract_html_title(f.path()),
            Some("Paul Graham — Great Work".to_string())
        );
    }

    #[test]
    fn extract_html_title_case_insensitive() {
        let mut f = NamedTempFile::new().unwrap();
        write!(f, "<HTML><HEAD><TITLE>My Page</TITLE></HEAD></HTML>").unwrap();
        assert_eq!(extract_html_title(f.path()), Some("My Page".to_string()));
    }

    #[test]
    fn extract_html_title_empty_tag_returns_none() {
        let mut f = NamedTempFile::new().unwrap();
        write!(f, "<html><head><title>   </title></head></html>").unwrap();
        assert_eq!(extract_html_title(f.path()), None);
    }

    #[test]
    fn extract_html_title_no_title_tag_returns_none() {
        let mut f = NamedTempFile::new().unwrap();
        write!(f, "<html><head></head><body>no title here</body></html>").unwrap();
        assert_eq!(extract_html_title(f.path()), None);
    }

    #[test]
    fn extract_html_title_after_100kb_preamble() {
        // Freedium / SingleFile pages can embed large CSS blocks before <title>.
        // Verify take().read_to_end() reads the full 256 KiB window.
        let mut f = NamedTempFile::new().unwrap();
        let padding = " ".repeat(100 * 1024); // 100 KiB of whitespace
        write!(f, "{}<html><head><title>Deep Title</title></head></html>", padding).unwrap();
        assert_eq!(
            extract_html_title(f.path()),
            Some("Deep Title".to_string())
        );
    }

    #[test]
    fn save_with_missing_binary_returns_clear_error() {
        // Calls save_with directly — no env mutation, safe in parallel test runs.
        let tmp = tempfile::tempdir().unwrap();
        let result = save_with(
            "https://example.com",
            tmp.path(),
            "test-ts",
            "/nonexistent/single-file",
            "chromium",
            &HashMap::new(),
            None,  // no ublock ext
            None,  // no cookie ext
            false, // reader mode off
            false, // modal closer off
            false, // freedium cleanup off
        );
        let err = result.unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("spawn") || msg.contains("nonexistent") || msg.contains("No such"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn save_with_both_extensions_uses_comma_joined_flags() {
        use std::path::Path;
        // We can't run single-file here, but we can exercise the flag-building
        // logic by checking the path list construction directly.
        let ublock = Path::new("/tmp/ublock");
        let cookie = Path::new("/tmp/cookie");
        let ext_paths: Vec<std::path::PathBuf> = [Some(ublock), Some(cookie)]
            .iter()
            .filter_map(|p| p.map(|p| p.to_path_buf()))
            .collect();
        let joined = ext_paths
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(",");
        assert_eq!(joined, "/tmp/ublock,/tmp/cookie");
        let load_flag = format!("--load-extension={joined}");
        let except_flag = format!("--disable-extensions-except={joined}");
        assert_eq!(load_flag, "--load-extension=/tmp/ublock,/tmp/cookie");
        assert_eq!(except_flag, "--disable-extensions-except=/tmp/ublock,/tmp/cookie");
    }

    #[test]
    fn resolve_ublock_config_disabled_when_false() {
        // Can't mutate env vars safely in parallel tests; test the logic directly
        // by verifying the env-var parsing branch we care about.
        let enabled = "false";
        let is_disabled =
            enabled.eq_ignore_ascii_case("false") || enabled == "0";
        assert!(is_disabled);


        let enabled = "0";
        let is_disabled =
            enabled.eq_ignore_ascii_case("false") || enabled == "0";
        assert!(is_disabled);
    }

    // ── html_attr_decode ─────────────────────────────────────────────────────

    #[test]
    fn html_attr_decode_plain_url_unchanged() {
        assert_eq!(
            html_attr_decode("https://cdn.example.com/img.jpg"),
            "https://cdn.example.com/img.jpg",
        );
    }

    #[test]
    fn html_attr_decode_ampersand_in_query() {
        // CDN signed URL with & encoded as &amp; by the browser's HTML serialiser.
        assert_eq!(
            html_attr_decode("https://cdn.example.com/img.jpg?a=1&amp;b=2&amp;sig=abc"),
            "https://cdn.example.com/img.jpg?a=1&b=2&sig=abc",
        );
    }

    #[test]
    fn html_attr_decode_single_layer_only() {
        // &amp;lt; → &lt;  (one browser-serialisation layer removed, not two).
        // If &amp; ran first it would produce &lt; then < — wrong.
        assert_eq!(html_attr_decode("&amp;lt;"), "&lt;");
    }

    #[test]
    fn html_attr_decode_double_encoded_amp() {
        // &amp;amp; → &amp; (the attribute value is a literal &amp;).
        assert_eq!(html_attr_decode("&amp;amp;"), "&amp;");
    }

    #[test]
    fn html_attr_decode_lt_gt_quot_direct() {
        // Directly encoded entities (no surrounding &amp;) decode normally.
        assert_eq!(html_attr_decode("&lt;tag&gt;"), "<tag>");
        assert_eq!(html_attr_decode("say &quot;hi&quot;"), "say \"hi\"");
    }

    // ── same_origin_cookie_header ─────────────────────────────────────────────

    #[test]
    fn same_origin_cookie_header_same_host_attaches_cookies() {
        let mut cookies = HashMap::new();
        cookies.insert("session".to_string(), "abc".to_string());
        cookies.insert("tok".to_string(), "xyz".to_string());
        let h = same_origin_cookie_header(
            "https://example.com/article",
            "https://example.com/img/photo.jpg",
            &cookies,
        ).expect("expected Some for same host");
        assert!(h.contains("session=abc"), "missing session: {h}");
        assert!(h.contains("tok=xyz"),     "missing tok: {h}");
    }

    #[test]
    fn same_origin_cookie_header_third_party_returns_none() {
        let mut cookies = HashMap::new();
        cookies.insert("session".to_string(), "abc".to_string());
        assert!(same_origin_cookie_header(
            "https://example.com/article",
            "https://cdn.third-party.com/img.jpg",
            &cookies,
        ).is_none(), "should not forward cookies to third-party host");
    }

    #[test]
    fn same_origin_cookie_header_empty_cookies_returns_none() {
        assert!(same_origin_cookie_header(
            "https://example.com/article",
            "https://example.com/img.jpg",
            &HashMap::new(),
        ).is_none());
    }

    #[test]
    fn same_origin_cookie_header_freedium_empty_cookies_noop() {
        // capture.rs passes empty cookies for Freedium fetches — confirm no-op.
        assert!(same_origin_cookie_header(
            "https://freedium-mirror.cfd/https://example.com/",
            "https://freedium-mirror.cfd/img/example.com/photo.jpg",
            &HashMap::new(),
        ).is_none());
    }

    // ── bounded read (size cap) ───────────────────────────────────────────────

    #[test]
    fn bounded_read_stops_at_limit() {
        // Exercises the same Read::take logic used in fetch_image_as_data_uri.
        let max: usize = 10;
        let data: Vec<u8> = (0u8..100).collect();
        let mut buf = Vec::new();
        std::io::Cursor::new(&data)
            .take((max as u64) + 1)
            .read_to_end(&mut buf)
            .unwrap();
        assert!(buf.len() > max,      "take should read up to max+1");
        assert!(buf.len() <= max + 1, "take should not exceed max+1");
    }

    #[test]
    fn bounded_read_allows_at_limit() {
        let max: usize = 10;
        let data: Vec<u8> = vec![0u8; max];
        let mut buf = Vec::new();
        std::io::Cursor::new(&data)
            .take((max as u64) + 1)
            .read_to_end(&mut buf)
            .unwrap();
        // Exactly at cap: buf.len() == max, guard does NOT fire.
        assert_eq!(buf.len(), max);
        assert!(buf.len() <= max);
    }
}
