import { useState, useEffect } from 'react';
import { fetchEntryArtifacts } from '../api';

// ── Date helper ────────────────────────────────────────────────────────────────

function fmtDate(secs) {
  if (!secs) return '';
  return new Intl.DateTimeFormat('en-US', {
    month: 'short',
    day: 'numeric',
    year: 'numeric',
  }).format(new Date(secs * 1000));
}

// Decode common HTML entities Twitter stores in full_text.
// MUST be called on already-sliced segments, not on the raw string before
// entity-offset arithmetic, because entity offsets are into the stored text.
function decodeEnt(str) {
  if (!str || !str.includes('&')) return str;
  // &amp; first so &amp;gt; → &gt; → > (handles double-encoded entities)
  return str
    .replace(/&amp;/g, '&')
    .replace(/&lt;/g, '<')
    .replace(/&gt;/g, '>')
    .replace(/&quot;/g, '"')
    .replace(/&#39;/g, "'")
    .replace(/&apos;/g, "'");
}

// ── Inline style definitions ───────────────────────────────────────────────────

const S = {
  // ── Tweet card ──
  card: {
    background: 'var(--paper)',
    border: '1px solid var(--line)',
    borderRadius: '12px',
    overflow: 'hidden',
    maxWidth: '560px',
    margin: '0 auto',
    fontFamily: 'var(--sans, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif)',
  },
  threadOuter: {
    border: '1px solid var(--line)',
    borderRadius: '12px',
    overflow: 'hidden',
    maxWidth: '560px',
    margin: '0 auto',
    background: 'var(--paper)',
    fontFamily: 'var(--sans, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif)',
  },
  tweetRow: {
    display: 'flex',
    gap: '10px',
    padding: '10px 12px',
  },
  tweetRowThread: {
    display: 'flex',
    gap: '10px',
    padding: '10px 12px 0',
  },
  leftCol: {
    display: 'flex',
    flexDirection: 'column',
    alignItems: 'center',
    flexShrink: 0,
    width: '36px',
  },
  rightCol: {
    flex: 1,
    minWidth: 0,
    paddingBottom: '8px',
  },
  avatar: {
    width: '36px',
    height: '36px',
    borderRadius: '50%',
    objectFit: 'cover',
    flexShrink: 0,
    display: 'block',
  },
  avatarPh: {
    width: '36px',
    height: '36px',
    borderRadius: '50%',
    background: 'var(--line)',
    flexShrink: 0,
  },
  threadLine: {
    flex: 1,
    width: '2px',
    background: 'var(--line-soft, var(--line))',
    margin: '4px 0',
    minHeight: '12px',
    borderRadius: '1px',
  },
  authorRow: {
    display: 'flex',
    alignItems: 'baseline',
    gap: '4px',
    flexWrap: 'wrap',
    marginBottom: '4px',
    lineHeight: '1.3',
  },
  authorName: {
    fontWeight: '700',
    fontSize: '14px',
    color: 'var(--ink)',
  },
  authorHandle: {
    fontSize: '13px',
    color: 'var(--muted)',
  },
  datePart: {
    fontSize: '13px',
    color: 'var(--muted)',
  },
  tweetText: {
    fontSize: '14px',
    lineHeight: '1.5',
    color: 'var(--ink)',
    whiteSpace: 'pre-line',
    marginBottom: '8px',
    wordBreak: 'break-word',
  },
  stats: {
    display: 'flex',
    gap: '12px',
    fontSize: '13px',
    color: 'var(--muted)',
  },
  link: {
    color: 'var(--accent)',
    textDecoration: 'none',
  },
  loading: {
    padding: '24px 16px',
    textAlign: 'center',
    color: 'var(--muted)',
    fontSize: '14px',
  },
  error: {
    padding: '24px 16px',
    textAlign: 'center',
    color: 'var(--alert)',
    fontSize: '14px',
  },
  mediaGrid: {
    marginBottom: '8px',
    borderRadius: '10px',
    overflow: 'hidden',
    border: '1px solid var(--line)',
  },
  mediaImg: {
    display: 'block',
    width: '100%',
    objectFit: 'cover',
    maxHeight: '260px',
  },
  mediaVideo: {
    display: 'block',
    width: '100%',
    maxHeight: '260px',
    background: '#000',
  },
  // ── Article ──
  article: {
    maxWidth: '560px',
    margin: '0 auto',
    border: '1px solid var(--line)',
    borderRadius: '12px',
    overflow: 'hidden',
    background: 'var(--paper)',
    fontFamily: 'var(--sans, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif)',
  },
  aCover: {
    width: '100%',
    display: 'block',
    objectFit: 'cover',
    maxHeight: '280px',
  },
  aMeta: {
    padding: '10px 14px 0',
  },
  aTweetTitle: {
    fontSize: '20px',
    fontWeight: '800',
    letterSpacing: '-0.3px',
    color: 'var(--ink)',
    lineHeight: '1.3',
    marginBottom: '8px',
  },
  aAuthorRow: {
    display: 'flex',
    alignItems: 'center',
    gap: '8px',
    marginBottom: '8px',
  },
  aAvatar: {
    width: '36px',
    height: '36px',
    borderRadius: '50%',
    objectFit: 'cover',
    flexShrink: 0,
    display: 'block',
  },
  aAvatarPh: {
    width: '36px',
    height: '36px',
    borderRadius: '50%',
    background: 'var(--line)',
    flexShrink: 0,
  },
  aAuthorName: {
    fontSize: '14px',
    fontWeight: '700',
    color: 'var(--ink)',
    lineHeight: '1.3',
  },
  aAuthorSub: {
    fontSize: '13px',
    color: 'var(--muted)',
    lineHeight: '1.3',
  },
  aDivider: {
    border: 'none',
    borderTop: '1px solid var(--line)',
    margin: '0',
  },
  aBody: {
    padding: '4px 14px 16px',
  },
  // ── Article blocks ──
  bH1: {
    fontSize: '22px',
    fontWeight: '800',
    letterSpacing: '-0.4px',
    color: 'var(--ink)',
    lineHeight: '1.25',
    margin: '16px 0 6px',
  },
  bH2: {
    fontSize: '18px',
    fontWeight: '700',
    letterSpacing: '-0.2px',
    color: 'var(--ink)',
    lineHeight: '1.3',
    margin: '14px 0 4px',
  },
  bP: {
    fontSize: '15px',
    color: 'var(--ink)',
    lineHeight: '1.65',
    marginBottom: '12px',
    marginTop: '0',
  },
  bSpacer: {
    height: '4px',
    display: 'block',
  },
  bQuote: {
    borderLeft: '3px solid var(--line)',
    padding: '2px 12px',
    margin: '12px 0',
    color: 'var(--muted)',
    fontSize: '15px',
    lineHeight: '1.6',
  },
  bHr: {
    border: 'none',
    borderTop: '1px solid var(--line)',
    margin: '14px 0',
  },
  bImg: {
    width: '100%',
    display: 'block',
    borderRadius: '8px',
    margin: '12px 0',
  },
  bUl: {
    margin: '8px 0 12px',
    paddingLeft: '24px',
  },
  bOl: {
    margin: '8px 0 12px',
    paddingLeft: '24px',
  },
  bLi: {
    fontSize: '15px',
    color: 'var(--ink)',
    lineHeight: '1.6',
    marginBottom: '4px',
  },
  bTweet: {
    display: 'flex',
    alignItems: 'center',
    gap: '10px',
    border: '1px solid var(--line)',
    borderRadius: '10px',
    padding: '10px 14px',
    margin: '12px 0',
    color: 'var(--muted)',
    fontSize: '14px',
    textDecoration: 'none',
  },
  bMdPre: {
    borderRadius: '8px',
    margin: '10px 0',
    overflow: 'auto',
    background: 'var(--paper-3, var(--field))',
    padding: '12px 14px',
  },
  bMdCode: {
    fontFamily: "ui-monospace, 'Cascadia Code', 'Fira Code', Menlo, Consolas, monospace",
    fontSize: '12px',
    lineHeight: '1.6',
    color: 'var(--ink)',
    background: 'transparent',
    display: 'block',
  },
  iCode: {
    fontFamily: "ui-monospace, 'Cascadia Code', Menlo, Consolas, monospace",
    fontSize: '0.875em',
    background: 'var(--paper-3, var(--field))',
    padding: '1px 5px',
    borderRadius: '4px',
    color: 'var(--ink)',
  },
  // ── QT badge ──
  qtBadge: {
    fontSize: '11px',
    color: 'var(--muted)',
    display: 'inline-flex',
    alignItems: 'center',
    gap: '2px',
    marginLeft: '4px',
    letterSpacing: '0.03em',
    flexShrink: 0,
  },
  // ── Lightbox ──
  lightboxBackdrop: {
    position: 'fixed',
    inset: 0,
    zIndex: 500,
    background: 'rgba(0,0,0,0.92)',
    display: 'flex',
    alignItems: 'center',
    justifyContent: 'center',
    padding: '20px',
  },
  lightboxContent: {
    display: 'flex',
    flexDirection: 'column',
    alignItems: 'center',
    gap: '10px',
    maxWidth: '90vw',
    maxHeight: '90vh',
  },
  lightboxToolbar: {
    display: 'flex',
    alignItems: 'center',
    gap: '10px',
    alignSelf: 'flex-end',
  },
  lightboxImg: {
    maxWidth: '88vw',
    maxHeight: '80vh',
    objectFit: 'contain',
    borderRadius: '6px',
    display: 'block',
  },
  lightboxNav: {
    display: 'flex',
    gap: '12px',
    alignItems: 'center',
  },
  lightboxNavBtn: {
    background: 'rgba(255,255,255,0.18)',
    border: 'none',
    color: '#fff',
    fontSize: '20px',
    cursor: 'pointer',
    borderRadius: '50%',
    width: '36px',
    height: '36px',
    display: 'flex',
    alignItems: 'center',
    justifyContent: 'center',
    lineHeight: 1,
    flexShrink: 0,
  },
  lightboxBtn: {
    background: 'rgba(255,255,255,0.18)',
    border: 'none',
    color: '#fff',
    fontSize: '16px',
    cursor: 'pointer',
    borderRadius: '50%',
    width: '30px',
    height: '30px',
    display: 'flex',
    alignItems: 'center',
    justifyContent: 'center',
    lineHeight: 1,
    flexShrink: 0,
  },
  lightboxLink: {
    color: 'rgba(255,255,255,0.7)',
    textDecoration: 'none',
    fontSize: '14px',
  },
  lightboxCounter: {
    color: 'rgba(255,255,255,0.6)',
    fontSize: '13px',
  },
};

// ── Artifact URL helpers ────────────────────────────────────────────────────────
// Build relpath → artifact API URL map from the artifacts array so archived
// copies are preferred over CDN URLs (avatars, media, cover images).
function buildArtifactMap(archiveId, entryUid, artifacts) {
  const map = {};
  if (!artifacts) return map;
  artifacts.forEach((a, idx) => {
    if (a.relpath) {
      map[a.relpath] = `/api/archives/${archiveId}/entries/${entryUid}/artifacts/${idx}`;
    }
  });
  return map;
}

// Return the archived artifact URL if available, otherwise fall back to cdnUrl.
function resolveUrl(localPath, cdnUrl, artifactMap) {
  if (localPath && artifactMap[localPath]) return artifactMap[localPath];
  return cdnUrl || null;
}


// ── SVG X logo ─────────────────────────────────────────────────────────────────

function XLogo() {
  return (
    <svg
      width="20"
      height="20"
      viewBox="0 0 24 24"
      fill="currentColor"
      aria-hidden="true"
      style={{ flexShrink: 0, color: 'var(--ink)' }}
    >
      <path d="M18.244 2.25h3.308l-7.227 8.26 8.502 11.24H16.17l-4.714-6.231-5.401 6.231H2.748l7.73-8.835L1.254 2.25H8.08l4.259 5.63L18.244 2.25zm-1.161 17.52h1.833L7.084 4.126H5.117L17.083 19.77z" />
    </svg>
  );
}

// ── Tweet text renderer ────────────────────────────────────────────────────────
// Splits full_text at entity boundaries, replacing t.co URLs with display_url
// links and @mentions with x.com links. Returns an array of React nodes.
// white-space: pre-line on the container preserves newlines in plain segments.

// Resolve start/end offsets for a URL or mention entity, handling three
// storage formats: camelCase fromIndex/toIndex (article inline), Twitter
// native indices:[s,e] array, and fallback exact-string search in fullText.
function resolveEntityBounds(ent, fullText, ...candidates) {
  if (ent.fromIndex != null && ent.toIndex != null)
    return { s: ent.fromIndex, e: ent.toIndex };
  if (ent.indices?.length === 2)
    return { s: ent.indices[0], e: ent.indices[1] };
  if (fullText) {
    for (const c of candidates) {
      if (!c) continue;
      const idx = fullText.indexOf(c);
      if (idx !== -1) return { s: idx, e: idx + c.length };
    }
  }
  return null;
}

// Build a canonical URL annotation from any entity format.
// Search candidates: short url first (appears in fullText), then expanded.
function normalizeUrlAnn(u, fullText) {
  const href    = u.expanded_url || u.url || u.text || '';
  const display = u.display_url  || u.expanded_url || u.url || u.text || '';
  const bounds  = resolveEntityBounds(u, fullText, u.url, u.text, u.display_url, u.expanded_url);
  if (!bounds || !href) return null;
  return { ...bounds, kind: 'url', href, display };
}

// Build a canonical mention annotation from any entity format.
function normalizeMentionAnn(m, fullText) {
  const screen_name = m.screen_name || m.name || m.text || '';
  const matchStr = screen_name ? `@${screen_name}` : null;
  const bounds = resolveEntityBounds(m, fullText, matchStr);
  if (!bounds || !screen_name) return null;
  return { ...bounds, kind: 'mention', screen_name };
}

// Linkify bare http(s) URLs in a plain-text string that have no entity coverage.
// Returns an array of strings and <a> nodes, or the original string if no URLs.
const URL_RE = /https?:\/\/[^\s<>"'\]）]+/g;
// Characters that commonly trail a URL but aren't part of it
const TRAIL_PUNCT = /[.,;:!?)（）]+$/;
function linkifyText(text, linkStyle) {
  const parts = [];
  let last = 0;
  let m;
  URL_RE.lastIndex = 0;
  while ((m = URL_RE.exec(text)) !== null) {
    if (m.index > last) parts.push(text.slice(last, m.index));
    let href = m[0].replace(TRAIL_PUNCT, '');
    parts.push(
      <a key={m.index} href={href} target="_blank" rel="noopener noreferrer" style={linkStyle}>
        {href}
      </a>
    );
    // Put back any trimmed trailing chars as plain text
    const trail = m[0].slice(href.length);
    if (trail) parts.push(trail);
    last = m.index + m[0].length;
  }
  if (last === 0) return text; // no URLs — return string directly (no array alloc)
  if (last < text.length) parts.push(text.slice(last));
  return parts;
}

function renderTweetTextJSX(fullText, entities) {
  if (!fullText) return null;

  const anns = [
    ...(entities.urls || []).map(u => normalizeUrlAnn(u, fullText)).filter(Boolean),
    ...(entities.user_mentions || []).map(m => normalizeMentionAnn(m, fullText)).filter(Boolean),
  ];
  if (anns.length === 0) return linkifyText(decodeEnt(fullText), S.link);


  const pts = new Set([0, fullText.length]);
  for (const a of anns) {
    if (a.s >= 0 && a.s <= fullText.length) pts.add(a.s);
    if (a.e >= 0 && a.e <= fullText.length) pts.add(a.e);
  }
  const sorted = [...pts].sort((a, b) => a - b);

  return sorted.slice(0, -1).map((s, i) => {
    const e = sorted[i + 1];
    const seg = fullText.slice(s, e);
    const active = anns.filter(a => a.s <= s && a.e >= e);

    const url = active.find(a => a.kind === 'url');
    if (url) {
      return (
        <a key={i} href={url.href} target="_blank" rel="noopener noreferrer" style={S.link}>
          {url.display || seg}
        </a>
      );
    }

    const mention = active.find(a => a.kind === 'mention');
    if (mention) {
      return (
        <a
          key={i}
          href={`https://x.com/${mention.screen_name}`}
          target="_blank"
          rel="noopener noreferrer"
          style={S.link}
        >
          {seg}
        </a>
      );
    }

    return <span key={i}>{linkifyText(decodeEnt(seg), S.link)}</span>;
  });
}

// ── Article inline text renderer ───────────────────────────────────────────────
// Port of renderInline() from x-article-renderer.
// Splits block text at style-range, URL, and mention boundaries, returning
// an array of React nodes with the appropriate wrappers applied.

function renderInlineJSX(text, styleRanges, urls, mentions) {
  if (!text) return null;
  styleRanges = styleRanges || [];
  urls = urls || [];
  mentions = mentions || [];

  const anns = [];
  for (const r of styleRanges) {
    if (r.length > 0)
      anns.push({ s: r.offset, e: r.offset + r.length, kind: 'style', style: r.style });
  }
  for (const u of urls) {
    const ann = normalizeUrlAnn(u, text);
    if (ann) anns.push(ann);
  }
  for (const m of mentions) {
    const ann = normalizeMentionAnn(m, text);
    if (ann) anns.push(ann);
  }

  if (anns.length === 0) return linkifyText(text, S.link);

  const pts = new Set([0, text.length]);
  for (const a of anns) {
    if (a.s >= 0 && a.s <= text.length) pts.add(a.s);
    if (a.e >= 0 && a.e <= text.length) pts.add(a.e);
  }
  const sorted = [...pts].sort((a, b) => a - b);

  return sorted.slice(0, -1).map((s, i) => {
    const e = sorted[i + 1];
    const active = anns.filter(a => a.s <= s && a.e >= e);
    const seg = text.slice(s, e);

    // Handle newlines within the segment by inserting <br /> elements.
    let content;
    if (seg.includes('\n')) {
      const parts = seg.split('\n');
      content = parts.flatMap((p, pi) =>
        pi < parts.length - 1 ? [p, <br key={pi} />] : [p]
      );
    } else {
      content = seg;
    }

    // Apply inline styles innermost first (matching Draft.js precedence).
    if (active.some(a => a.kind === 'style' && a.style === 'Code'))
      content = <code style={S.iCode}>{content}</code>;
    if (active.some(a => a.kind === 'style' && a.style === 'Bold'))
      content = <strong>{content}</strong>;
    if (active.some(a => a.kind === 'style' && a.style === 'Italic'))
      content = <em>{content}</em>;
    if (active.some(a => a.kind === 'style' && a.style === 'Underline'))
      content = <u>{content}</u>;
    if (active.some(a => a.kind === 'style' && a.style === 'Strikethrough'))
      content = <s>{content}</s>;

    // Links wrap outermost.
    const url = active.find(a => a.kind === 'url');
    if (url) {
      // If the visible segment is a raw t.co short URL, replace it with the
      // human-readable display URL; otherwise keep the styled anchor text.
      const isTco = /^https?:\/\/t\.co\//i.test(content);
      content = (
        <a href={url.href} target="_blank" rel="noopener noreferrer" style={S.link}>
          {isTco ? url.display : content}
        </a>
      );
    }

    const mention = active.find(a => a.kind === 'mention');
    if (mention) {
      content = (
        <a
          href={`https://x.com/${mention.screen_name}`}
          target="_blank"
          rel="noopener noreferrer"
          style={S.link}
        >
          {content}
        </a>
      );
    }

    return <span key={i}>{typeof content === 'string' ? linkifyText(content, S.link) : content}</span>;
  });
}

// ── Article atomic block renderer ──────────────────────────────────────────────
// Port of renderAtomic() from x-article-renderer.

function renderAtomicJSX(block, artifactMap, opts) {
  const entities = block.resolved_entities || [];
  if (entities.length === 0) return null;

  return entities.map((e, i) => {
    switch (e.type) {
      case 'divider':
        return <hr key={i} style={S.bHr} />;

      case 'media': {
        const src = resolveUrl(e.local_path, e.url, artifactMap);
        if (!src) return null;
        return (
          <a
            key={i}
            href={src}
            target="_blank"
            rel="noopener noreferrer"
            style={{ display: 'block', cursor: 'zoom-in' }}
            onClick={ev => {
              if (!ev.metaKey && !ev.ctrlKey) {
                ev.preventDefault();
                opts?.onImgClick?.(src);
              }
            }}
          >
            <img src={src} style={S.bImg} loading="lazy" alt="" />
          </a>
        );
      }

      case 'tweet':
        return e.tweet_id ? (
          <a
            key={i}
            href={`https://x.com/i/status/${e.tweet_id}`}
            target="_blank"
            rel="noopener noreferrer"
            style={S.bTweet}
          >
            <XLogo />
            View post on X
          </a>
        ) : null;

      case 'link':
        return e.url ? (
          <p key={i} style={S.bP}>
            <a href={e.url} target="_blank" rel="noopener noreferrer" style={S.link}>
              {e.url}
            </a>
          </p>
        ) : null;

      case 'markdown': {
        const md = e.markdown ?? e.data?.markdown ?? '';
        return (
          <pre key={i} style={S.bMdPre}>
            <code style={S.bMdCode}>{md}</code>
          </pre>
        );
      }

      case 'emoji':
        return e.url ? (
          <img
            key={i}
            src={e.url}
            alt=""
            style={{ height: '1.2em', verticalAlign: 'middle', margin: '0 1px' }}
          />
        ) : null;

      default:
        return null;
    }
  });
}

// ── Article single block renderer ──────────────────────────────────────────────
// Port of renderBlock() from x-article-renderer.

function renderBlockJSX(block, key, artifactMap, opts) {
  const type = block.type || '';
  const text = block.text || '';
  const styleRanges = block.inline_style_ranges || [];
  const data = block.data || {};
  const inner = renderInlineJSX(text, styleRanges, data.urls || [], data.mentions || []);

  switch (type) {
    case 'header-one':
      return <h1 key={key} style={S.bH1}>{inner}</h1>;

    case 'header-two': {
      const m = text.match(/(?:x\.com|twitter\.com)\/i\/status\/(\d+)/);
      if (m) {
        return (
          <a
            key={key}
            href={`https://x.com/i/status/${m[1]}`}
            target="_blank"
            rel="noopener noreferrer"
            style={S.bTweet}
          >
            <XLogo />
            View post on X
          </a>
        );
      }
      return <h2 key={key} style={S.bH2}>{inner}</h2>;
    }

    case 'unstyled':
      if (!text.trim()) return <span key={key} style={S.bSpacer} />;
      return <p key={key} style={S.bP}>{inner}</p>;

    case 'blockquote':
      return <blockquote key={key} style={S.bQuote}>{inner}</blockquote>;

    case 'unordered-list-item':
    case 'ordered-list-item':
      return <li key={key} style={S.bLi}>{inner}</li>;

    case 'atomic':
      return <span key={key}>{renderAtomicJSX(block, artifactMap, opts)}</span>;

    default:
      return text ? <p key={key} style={S.bP}>{inner}</p> : null;
  }
}

// ── Article block list renderer ────────────────────────────────────────────────
// Port of renderBlocks() from x-article-renderer.
// Groups consecutive same-type list items into a single ul/ol.

function renderBlocksJSX(blocks, artifactMap, opts) {
  const items = [];
  let i = 0;

  while (i < blocks.length) {
    const b = blocks[i];

    if (b.type === 'unordered-list-item') {
      const startIdx = i;
      const listItems = [];
      while (i < blocks.length && blocks[i].type === 'unordered-list-item') {
        listItems.push(renderBlockJSX(blocks[i], i, artifactMap, opts));
        i++;
      }
      items.push(<ul key={`ul-${startIdx}`} style={S.bUl}>{listItems}</ul>);
    } else if (b.type === 'ordered-list-item') {
      const startIdx = i;
      const listItems = [];
      while (i < blocks.length && blocks[i].type === 'ordered-list-item') {
        listItems.push(renderBlockJSX(blocks[i], i, artifactMap, opts));
        i++;
      }
      items.push(<ol key={`ol-${startIdx}`} style={S.bOl}>{listItems}</ol>);
    } else {
      items.push(renderBlockJSX(b, i, artifactMap, opts));
      i++;
    }
  }

  return items;
}

// ── Article renderer ───────────────────────────────────────────────────────────

function ArticleRenderer({ article, tweetAuthor, artifactMap }) {
  const [lightboxSrc, setLightboxSrc] = useState(null);

  const cover = article.cover_media || {};
  const author = article.author || tweetAuthor || {};
  const date = article.first_published_at_secs
    ? fmtDate(article.first_published_at_secs)
    : '';
  const handlePart = author.screen_name ? `@${author.screen_name}` : '';
  const subLine = [handlePart, date].filter(Boolean).join(' · ');

  const coverSrc = resolveUrl(cover.local_path, cover.url, artifactMap);
  const avatarSrc = resolveUrl(author.avatar_local_path, author.avatar_url, artifactMap);

  return (
    <div style={S.article}>
      {lightboxSrc && (
        <MediaLightbox
          items={[{ src: lightboxSrc, alt: '' }]}
          startIndex={0}
          onClose={() => setLightboxSrc(null)}
        />
      )}
      {coverSrc && (
        <a
          href={coverSrc}
          target="_blank"
          rel="noopener noreferrer"
          style={{ display: 'block', cursor: 'zoom-in' }}
          onClick={e => { if (!e.metaKey && !e.ctrlKey) { e.preventDefault(); setLightboxSrc(coverSrc); } }}
        >
          <img src={coverSrc} style={S.aCover} alt="Article cover" />
        </a>
      )}
      <div style={S.aMeta}>
        {article.title && (
          <div style={S.aTweetTitle}>{article.title}</div>
        )}
        <div style={S.aAuthorRow}>
          {avatarSrc
            ? <img src={avatarSrc} style={S.aAvatar} alt={author.name || ''} />
            : <div style={S.aAvatarPh} />
          }
          <div>
            <div style={S.aAuthorName}>
              {author.name || author.screen_name || 'Unknown'}
            </div>
            {subLine && (
              <div style={S.aAuthorSub}>{subLine}</div>
            )}
          </div>
        </div>
      </div>
      <hr style={S.aDivider} />
      <div style={S.aBody}>
        {renderBlocksJSX(article.blocks || [], artifactMap, { onImgClick: setLightboxSrc })}
      </div>
    </div>
  );
}

// ── Tweet card ─────────────────────────────────────────────────────────────────
// Renders one tweet. When isInThread, omits bottom padding from the row and
// shows a thread connector line below the avatar (except on the last card).


// ── Photo grid ─────────────────────────────────────────────────────────────────
// Renders 1–4 photos in Twitter-style grid. Regular click = lightbox;
// Cmd/Ctrl+click follows the <a> href to open the image in a new tab.
function PhotoGrid({ photos, onOpen }) {
  const n = photos.length;
  if (n === 0) return null;

  if (n === 1) {
    return (
      <a
        href={photos[0].src}
        target="_blank"
        rel="noopener noreferrer"
        style={{ display: 'block' }}
        onClick={e => { if (!e.metaKey && !e.ctrlKey) { e.preventDefault(); onOpen(0); } }}
      >
        <img src={photos[0].src} alt={photos[0].alt || ''} style={S.mediaImg} loading="lazy" />
      </a>
    );
  }

  // 2 → two columns, 1 row; 3 → left spans 2 rows; 4 → 2×2
  const rowH = n <= 2 ? '180px' : '140px';
  return (
    <div style={{
      display: 'grid',
      gridTemplateColumns: '1fr 1fr',
      gridTemplateRows: n === 2 ? rowH : `${rowH} ${rowH}`,
      gap: '2px',
    }}>
      {photos.map((ph, i) => (
        <a
          key={i}
          href={ph.src}
          target="_blank"
          rel="noopener noreferrer"
          style={{
            display: 'block',
            overflow: 'hidden',
            gridRow: (n === 3 && i === 0) ? 'span 2' : undefined,
          }}
          onClick={e => { if (!e.metaKey && !e.ctrlKey) { e.preventDefault(); onOpen(i); } }}
        >
          <img
            src={ph.src}
            alt={ph.alt || ''}
            loading="lazy"
            style={{ width: '100%', height: '100%', objectFit: 'cover', display: 'block' }}
          />
        </a>
      ))}
    </div>
  );
}

// ── Media lightbox ─────────────────────────────────────────────────────────────
// position:fixed escapes any overflow/stacking context.
function MediaLightbox({ items, startIndex, onClose }) {
  const [idx, setIdx] = useState(startIndex);

  useEffect(() => {
    const h = e => {
      if (e.key === 'Escape' || e.key === 'ArrowLeft' || e.key === 'ArrowRight') {
        e.stopPropagation();
        e.preventDefault();
      }
      if (e.key === 'Escape') onClose();
      if (e.key === 'ArrowRight') setIdx(i => Math.min(i + 1, items.length - 1));
      if (e.key === 'ArrowLeft') setIdx(i => Math.max(i - 1, 0));
    };
    document.addEventListener('keydown', h);
    return () => document.removeEventListener('keydown', h);
  }, [onClose, items.length]);

  const item = items[idx];
  return (
    <div style={S.lightboxBackdrop} onClick={onClose}>
      <div style={S.lightboxContent} onClick={e => e.stopPropagation()}>
        <div style={S.lightboxToolbar}>
          {items.length > 1 && (
            <span style={S.lightboxCounter}>{idx + 1} / {items.length}</span>
          )}
          <a
            href={item.src}
            target="_blank"
            rel="noopener noreferrer"
            style={S.lightboxLink}
            title="Open in new tab"
          >↗</a>
          <button style={S.lightboxBtn} onClick={onClose} aria-label="Close">×</button>
        </div>
        <img src={item.src} alt={item.alt || ''} style={S.lightboxImg} />
        {items.length > 1 && (
          <div style={S.lightboxNav}>
            <button
              style={S.lightboxNavBtn}
              onClick={() => setIdx(i => Math.max(i - 1, 0))}
              disabled={idx === 0}
            >‹</button>
            <button
              style={S.lightboxNavBtn}
              onClick={() => setIdx(i => Math.min(i + 1, items.length - 1))}
              disabled={idx === items.length - 1}
            >›</button>
          </div>
        )}
      </div>
    </div>
  );
}

function TweetCard({ tweet, isInThread, isLast, artifactMap }) {
  const [lightboxIdx, setLightboxIdx] = useState(null);

  const author = tweet.author || {};
  const date = tweet.created_at_secs ? fmtDate(tweet.created_at_secs) : '';
  const entities = tweet.entities || {};
  const avatarSrc = resolveUrl(author.avatar_local_path, author.avatar_url, artifactMap);
  const showConnector = isInThread && !isLast;
  const isQT = tweet.is_quote_status === true;
  const rowStyle = isInThread ? S.tweetRowThread : S.tweetRow;

  // Build resolved media lists first; skip the grid entirely if every item resolves to null.
  const rawMedia = (tweet.extended_entities?.media?.length
    ? tweet.extended_entities.media
    : entities.media) || [];

  const photos = [];
  const videoItems = [];
  for (const m of rawMedia) {
    if (m.type === 'photo') {
      const src = resolveUrl(m.local_path, m.media_url_https, artifactMap);
      if (src) photos.push({ kind: 'photo', src, alt: m.alt_text || '' });
    } else if (m.type === 'video' || m.type === 'animated_gif') {
      const src = (m.local_path && artifactMap[m.local_path])
        || (() => {
          const variants = m.video_info?.variants || [];
          return variants
            .filter(v => v.content_type === 'video/mp4')
            .sort((a, b) => (b.bitrate || 0) - (a.bitrate || 0))[0]?.url;
        })();
      if (src) videoItems.push({ kind: m.type === 'animated_gif' ? 'gif' : 'video', src });
    }
  }

  return (
    <>
      {lightboxIdx !== null && (
        <MediaLightbox
          items={photos}
          startIndex={lightboxIdx}
          onClose={() => setLightboxIdx(null)}
        />
      )}
      <div style={rowStyle}>
        <div style={S.leftCol}>
          {avatarSrc
            ? <img src={avatarSrc} style={S.avatar} alt={author.name || ''} />
            : <div style={S.avatarPh} />
          }
          {showConnector && <div style={S.threadLine} />}
        </div>

        <div style={S.rightCol}>
          <div style={S.authorRow}>
            <span style={S.authorName}>{author.name || author.screen_name || 'Unknown'}</span>
            {author.screen_name && (
              <span style={S.authorHandle}>@{author.screen_name}</span>
            )}
            {date && <span style={S.datePart}>· {date}</span>}
            {isQT && (
              <span style={S.qtBadge} title="Quote tweet">↻ QT</span>
            )}
          </div>

          <div style={S.tweetText}>
            {renderTweetTextJSX(tweet.full_text || '', entities)}
          </div>

          {photos.length > 0 && (
            <div style={S.mediaGrid}>
              <PhotoGrid photos={photos} onOpen={i => setLightboxIdx(i)} />
            </div>
          )}

          {videoItems.map((v, i) => (
            <div key={i} style={S.mediaGrid}>
              <video
                src={v.src}
                style={S.mediaVideo}
                controls
                loop={v.kind === 'gif'}
                muted={v.kind === 'gif'}
                autoPlay={v.kind === 'gif'}
              />
            </div>
          ))}

          {(tweet.retweet_count > 0 || tweet.favorite_count > 0) && (
            <div style={S.stats}>
              {tweet.favorite_count > 0 && (
                <span>❤️ {tweet.favorite_count.toLocaleString()}</span>
              )}
              {tweet.retweet_count > 0 && (
                <span>🔁 {tweet.retweet_count.toLocaleString()}</span>
              )}
            </div>
          )}
        </div>
      </div>
    </>
  );
}

// ── TweetPreview ───────────────────────────────────────────────────────────────

export default function TweetPreview({ archiveId, entryUid, artifacts, entityKind }) {
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState(null);
  const [tweets, setTweets] = useState([]);

  useEffect(() => {
    setLoading(true);
    setError(null);
    setTweets([]);

    if (!artifacts || !archiveId || !entryUid) {
      setLoading(false);
      return;
    }

    const tweetArtifacts = artifacts
      .map((a, index) => ({ ...a, index }))
      .filter(a => a.artifact_role === 'raw_tweet_json');

    if (tweetArtifacts.length === 0) {
      setError('No tweet data found.');
      setLoading(false);
      return;
    }

    let cancelled = false;

    fetchEntryArtifacts(archiveId, entryUid, tweetArtifacts.map(a => a.index))
      .then(data => { if (!cancelled) setTweets(data); })
      .catch(e => { if (!cancelled) setError(e.message || 'Failed to load tweet.'); })
      .finally(() => { if (!cancelled) setLoading(false); });

    return () => { cancelled = true; };
  }, [archiveId, entryUid, artifacts]);

  if (loading) return <div style={S.loading}>Loading…</div>;
  if (error) return <div style={S.error}>Error: {error}</div>;
  if (tweets.length === 0) return null;

  // Build relpath → artifact URL map once for this entry
  const artifactMap = buildArtifactMap(archiveId, entryUid, artifacts);

  if (entityKind === 'tweet_thread') {
    return (
      <div style={S.threadOuter}>
        {tweets.map((tweet, i) => (
          <TweetCard
            key={tweet.id || i}
            tweet={tweet}
            isInThread
            isLast={i === tweets.length - 1}
            artifactMap={artifactMap}
          />
        ))}
      </div>
    );
  }

  const tweet = tweets[0];
  if (tweet.is_article && tweet.article) {
    return <ArticleRenderer article={tweet.article} tweetAuthor={tweet.author} artifactMap={artifactMap} />;
  }

  return (
    <div style={S.card}>
      <TweetCard tweet={tweet} isInThread={false} isLast artifactMap={artifactMap} />
    </div>
  );
}
