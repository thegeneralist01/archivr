import { useState, useEffect } from 'react';

// ── Date helper ────────────────────────────────────────────────────────────────

function fmtDate(secs) {
  if (!secs) return '';
  return new Intl.DateTimeFormat('en-US', {
    month: 'short',
    day: 'numeric',
    year: 'numeric',
  }).format(new Date(secs * 1000));
}

// ── Inline style definitions ───────────────────────────────────────────────────

const S = {
  // ── Tweet card ──
  card: {
    background: 'var(--paper)',
    border: '1px solid var(--line)',
    borderRadius: '12px',
    overflow: 'hidden',
    maxWidth: '598px',
    margin: '0 auto',
    fontFamily: 'var(--sans, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif)',
  },
  threadOuter: {
    border: '1px solid var(--line)',
    borderRadius: '12px',
    overflow: 'hidden',
    maxWidth: '598px',
    margin: '0 auto',
    background: 'var(--paper)',
    fontFamily: 'var(--sans, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif)',
  },
  tweetRow: {
    display: 'flex',
    gap: '12px',
    padding: '16px',
  },
  tweetRowThread: {
    display: 'flex',
    gap: '12px',
    padding: '16px 16px 0',
  },
  leftCol: {
    display: 'flex',
    flexDirection: 'column',
    alignItems: 'center',
    flexShrink: 0,
    width: '40px',
  },
  rightCol: {
    flex: 1,
    minWidth: 0,
    paddingBottom: '16px',
  },
  avatar: {
    width: '40px',
    height: '40px',
    borderRadius: '50%',
    objectFit: 'cover',
    flexShrink: 0,
    display: 'block',
  },
  avatarPh: {
    width: '40px',
    height: '40px',
    borderRadius: '50%',
    background: 'var(--line)',
    flexShrink: 0,
  },
  threadLine: {
    flex: 1,
    width: '2px',
    background: 'var(--line-soft, var(--line))',
    margin: '4px 0',
    minHeight: '16px',
    borderRadius: '1px',
  },
  authorRow: {
    display: 'flex',
    alignItems: 'baseline',
    gap: '4px',
    flexWrap: 'wrap',
    marginBottom: '6px',
    lineHeight: '1.3',
  },
  authorName: {
    fontWeight: '700',
    fontSize: '15px',
    color: 'var(--ink)',
  },
  authorHandle: {
    fontSize: '14px',
    color: 'var(--muted)',
  },
  datePart: {
    fontSize: '14px',
    color: 'var(--muted)',
  },
  tweetText: {
    fontSize: '15px',
    lineHeight: '1.6',
    color: 'var(--ink)',
    whiteSpace: 'pre-line',
    marginBottom: '10px',
    wordBreak: 'break-word',
  },
  stats: {
    display: 'flex',
    gap: '16px',
    fontSize: '14px',
    color: 'var(--muted)',
  },
  link: {
    color: 'var(--accent)',
    textDecoration: 'none',
  },
  loading: {
    padding: '32px 16px',
    textAlign: 'center',
    color: 'var(--muted)',
    fontSize: '15px',
  },
  error: {
    padding: '32px 16px',
    textAlign: 'center',
    color: 'var(--alert)',
    fontSize: '15px',
  },
  // ── Article ──
  article: {
    maxWidth: '598px',
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
    maxHeight: '420px',
  },
  aMeta: {
    padding: '20px 16px 0',
  },
  aTweetTitle: {
    fontSize: '23px',
    fontWeight: '800',
    letterSpacing: '-0.3px',
    color: 'var(--ink)',
    lineHeight: '1.3',
    marginBottom: '16px',
  },
  aAuthorRow: {
    display: 'flex',
    alignItems: 'center',
    gap: '12px',
    marginBottom: '16px',
  },
  aAvatar: {
    width: '40px',
    height: '40px',
    borderRadius: '50%',
    objectFit: 'cover',
    flexShrink: 0,
    display: 'block',
  },
  aAvatarPh: {
    width: '40px',
    height: '40px',
    borderRadius: '50%',
    background: 'var(--line)',
    flexShrink: 0,
  },
  aAuthorName: {
    fontSize: '15px',
    fontWeight: '700',
    color: 'var(--ink)',
    lineHeight: '1.3',
  },
  aAuthorSub: {
    fontSize: '15px',
    color: 'var(--muted)',
    lineHeight: '1.3',
  },
  aDivider: {
    border: 'none',
    borderTop: '1px solid var(--line)',
    margin: '0',
  },
  aBody: {
    padding: '4px 16px 24px',
  },
  // ── Article blocks ──
  bH1: {
    fontSize: '26px',
    fontWeight: '800',
    letterSpacing: '-0.4px',
    color: 'var(--ink)',
    lineHeight: '1.25',
    margin: '24px 0 10px',
  },
  bH2: {
    fontSize: '20px',
    fontWeight: '700',
    letterSpacing: '-0.2px',
    color: 'var(--ink)',
    lineHeight: '1.3',
    margin: '22px 0 8px',
  },
  bP: {
    fontSize: '17px',
    color: 'var(--ink)',
    lineHeight: '1.7',
    marginBottom: '16px',
    marginTop: '0',
  },
  bSpacer: {
    height: '6px',
    display: 'block',
  },
  bQuote: {
    borderLeft: '3px solid var(--line)',
    padding: '2px 14px',
    margin: '14px 0',
    color: 'var(--muted)',
    fontSize: '17px',
    lineHeight: '1.65',
  },
  bHr: {
    border: 'none',
    borderTop: '1px solid var(--line)',
    margin: '28px 0',
  },
  bImg: {
    width: '100%',
    display: 'block',
    borderRadius: '12px',
    margin: '16px 0',
  },
  bUl: {
    margin: '10px 0 16px',
    paddingLeft: '28px',
  },
  bOl: {
    margin: '10px 0 16px',
    paddingLeft: '28px',
  },
  bLi: {
    fontSize: '17px',
    color: 'var(--ink)',
    lineHeight: '1.65',
    marginBottom: '6px',
  },
  bTweet: {
    display: 'flex',
    alignItems: 'center',
    gap: '12px',
    border: '1px solid var(--line)',
    borderRadius: '12px',
    padding: '14px 16px',
    margin: '14px 0',
    color: 'var(--muted)',
    fontSize: '15px',
    textDecoration: 'none',
  },
  bMdPre: {
    borderRadius: '8px',
    margin: '12px 0',
    overflow: 'auto',
    background: 'var(--paper-3, var(--field))',
    padding: '14px 16px',
  },
  bMdCode: {
    fontFamily: "ui-monospace, 'Cascadia Code', 'Fira Code', Menlo, Consolas, monospace",
    fontSize: '13px',
    lineHeight: '1.65',
    color: 'var(--ink)',
    background: 'transparent',
    display: 'block',
  },
  iCode: {
    fontFamily: "ui-monospace, 'Cascadia Code', Menlo, Consolas, monospace",
    fontSize: '0.875em',
    background: 'var(--paper-3, var(--field))',
    padding: '2px 6px',
    borderRadius: '4px',
    color: 'var(--ink)',
  },
};

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

function renderTweetTextJSX(fullText, entities) {
  if (!fullText) return null;

  const urls = (entities.urls || []).filter(
    u => u.fromIndex != null && u.toIndex != null
  );
  const mentions = (entities.user_mentions || []).filter(
    m => m.fromIndex != null && m.toIndex != null
  );

  if (urls.length === 0 && mentions.length === 0) return fullText;

  const anns = [
    ...urls.map(u => ({
      s: u.fromIndex,
      e: u.toIndex,
      kind: 'url',
      href: u.expanded_url,
      display: u.display_url,
    })),
    ...mentions.map(m => ({
      s: m.fromIndex,
      e: m.toIndex,
      kind: 'mention',
      screen_name: m.screen_name,
    })),
  ];

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

    return <span key={i}>{seg}</span>;
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
  for (const u of urls)
    anns.push({ s: u.fromIndex, e: u.toIndex, kind: 'url', href: u.text });
  for (const m of mentions)
    anns.push({ s: m.fromIndex, e: m.toIndex, kind: 'mention', name: m.text });

  if (anns.length === 0) return text;

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
      content = (
        <a href={url.href} target="_blank" rel="noopener noreferrer" style={S.link}>
          {content}
        </a>
      );
    }

    const mention = active.find(a => a.kind === 'mention');
    if (mention) {
      content = (
        <a
          href={`https://x.com/${mention.name}`}
          target="_blank"
          rel="noopener noreferrer"
          style={S.link}
        >
          {content}
        </a>
      );
    }

    return <span key={i}>{content}</span>;
  });
}

// ── Article atomic block renderer ──────────────────────────────────────────────
// Port of renderAtomic() from x-article-renderer.

function renderAtomicJSX(block) {
  const entities = block.resolved_entities || [];
  if (entities.length === 0) return null;

  return entities.map((e, i) => {
    switch (e.type) {
      case 'divider':
        return <hr key={i} style={S.bHr} />;

      case 'media':
        return e.url
          ? <img key={i} src={e.url} style={S.bImg} loading="lazy" alt="" />
          : null;

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

function renderBlockJSX(block, key) {
  const type = block.type || '';
  const text = block.text || '';
  const styleRanges = block.inline_style_ranges || [];
  const data = block.data || {};
  const inner = renderInlineJSX(text, styleRanges, data.urls || [], data.mentions || []);

  switch (type) {
    case 'header-one':
      return <h1 key={key} style={S.bH1}>{inner}</h1>;

    case 'header-two': {
      // X's editor sometimes stores a tweet URL as the sole content of an H2.
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

    // List items — the wrapper ul/ol is added by renderBlocksJSX.
    case 'unordered-list-item':
    case 'ordered-list-item':
      return <li key={key} style={S.bLi}>{inner}</li>;

    case 'atomic':
      return <span key={key}>{renderAtomicJSX(block)}</span>;

    default:
      return text ? <p key={key} style={S.bP}>{inner}</p> : null;
  }
}

// ── Article block list renderer ────────────────────────────────────────────────
// Port of renderBlocks() from x-article-renderer.
// Groups consecutive same-type list items into a single ul/ol.

function renderBlocksJSX(blocks) {
  const items = [];
  let i = 0;

  while (i < blocks.length) {
    const b = blocks[i];

    if (b.type === 'unordered-list-item') {
      const startIdx = i;
      const listItems = [];
      while (i < blocks.length && blocks[i].type === 'unordered-list-item') {
        listItems.push(renderBlockJSX(blocks[i], i));
        i++;
      }
      items.push(<ul key={`ul-${startIdx}`} style={S.bUl}>{listItems}</ul>);
    } else if (b.type === 'ordered-list-item') {
      const startIdx = i;
      const listItems = [];
      while (i < blocks.length && blocks[i].type === 'ordered-list-item') {
        listItems.push(renderBlockJSX(blocks[i], i));
        i++;
      }
      items.push(<ol key={`ol-${startIdx}`} style={S.bOl}>{listItems}</ol>);
    } else {
      items.push(renderBlockJSX(b, i));
      i++;
    }
  }

  return items;
}

// ── Article renderer ───────────────────────────────────────────────────────────

function ArticleRenderer({ article, tweetAuthor }) {
  const cover = article.cover_media || {};
  // article.author is preferred; fall back to the tweet-level author.
  const author = article.author || tweetAuthor || {};
  const date = article.first_published_at_secs
    ? fmtDate(article.first_published_at_secs)
    : '';
  const handlePart = author.screen_name ? `@${author.screen_name}` : '';
  const subLine = [handlePart, date].filter(Boolean).join(' · ');

  return (
    <div style={S.article}>
      {cover.url && (
        <img src={cover.url} style={S.aCover} alt="Article cover" />
      )}
      <div style={S.aMeta}>
        {article.title && (
          <div style={S.aTweetTitle}>{article.title}</div>
        )}
        <div style={S.aAuthorRow}>
          {author.avatar_url
            ? <img src={author.avatar_url} style={S.aAvatar} alt={author.name || ''} />
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
        {renderBlocksJSX(article.blocks || [])}
      </div>
    </div>
  );
}

// ── Tweet card ─────────────────────────────────────────────────────────────────
// Renders one tweet. When isInThread, omits bottom padding from the row and
// shows a thread connector line below the avatar (except on the last card).

function TweetCard({ tweet, isInThread, isLast }) {
  const author = tweet.author || {};
  const date = tweet.created_at_secs ? fmtDate(tweet.created_at_secs) : '';
  const entities = tweet.entities || {};
  const avatarUrl = author.avatar_url;
  const showConnector = isInThread && !isLast;

  const rowStyle = isInThread ? S.tweetRowThread : S.tweetRow;

  return (
    <div style={rowStyle}>
      {/* Left column: avatar + optional thread connector line */}
      <div style={S.leftCol}>
        {avatarUrl
          ? <img src={avatarUrl} style={S.avatar} alt={author.name || ''} />
          : <div style={S.avatarPh} />
        }
        {showConnector && <div style={S.threadLine} />}
      </div>

      {/* Right column: author row, tweet text, stats */}
      <div style={S.rightCol}>
        <div style={S.authorRow}>
          <span style={S.authorName}>
            {author.name || author.screen_name || 'Unknown'}
          </span>
          {author.screen_name && (
            <span style={S.authorHandle}>@{author.screen_name}</span>
          )}
          {date && (
            <span style={S.datePart}>· {date}</span>
          )}
        </div>

        <div style={S.tweetText}>
          {renderTweetTextJSX(tweet.full_text || '', entities)}
        </div>

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

    Promise.all(
      tweetArtifacts.map(a =>
        fetch(`/api/archives/${archiveId}/entries/${entryUid}/artifacts/${a.index}`)
          .then(r => {
            if (!r.ok) throw new Error(`HTTP ${r.status}`);
            return r.json();
          })
      )
    )
      .then(data => {
        if (!cancelled) setTweets(data);
      })
      .catch(e => {
        if (!cancelled) setError(e.message || 'Failed to load tweet.');
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });

    return () => { cancelled = true; };
  }, [archiveId, entryUid, artifacts]);

  if (loading) {
    return <div style={S.loading}>Loading…</div>;
  }

  if (error) {
    return <div style={S.error}>Error: {error}</div>;
  }

  if (tweets.length === 0) return null;

  // Thread: multiple tweet cards joined by a connector line.
  if (entityKind === 'tweet_thread') {
    return (
      <div style={S.threadOuter}>
        {tweets.map((tweet, i) => (
          <TweetCard
            key={tweet.id || i}
            tweet={tweet}
            isInThread
            isLast={i === tweets.length - 1}
          />
        ))}
      </div>
    );
  }

  // Single tweet — check for X Article first.
  const tweet = tweets[0];

  if (tweet.is_article && tweet.article) {
    return <ArticleRenderer article={tweet.article} tweetAuthor={tweet.author} />;
  }

  return (
    <div style={S.card}>
      <TweetCard tweet={tweet} isInThread={false} isLast />
    </div>
  );
}
