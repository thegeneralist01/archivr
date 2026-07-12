export default function IframePreview({ src, type, title, originalUrl }) {
  const displayUrl = originalUrl || src;
  const displayTitle = title || null;

  return (
    <div
      className="preview-iframe-wrap"
      style={{ flex: 1, minHeight: 0, display: 'flex', flexDirection: 'column' }}
    >
      <div
        className="preview-iframe-toolbar"
        style={{
          display: 'flex',
          flexDirection: 'column',
          gap: '2px',
          padding: '6px 12px',
          borderBottom: '1px solid var(--line-soft)',
          background: 'var(--paper-2)',
          flexShrink: 0,
        }}
      >
        {displayTitle && (
          <span style={{
            fontSize: '0.85rem',
            fontWeight: 600,
            color: 'var(--ink)',
            fontFamily: 'var(--sans)',
            overflow: 'hidden',
            textOverflow: 'ellipsis',
            whiteSpace: 'nowrap',
          }}>
            {displayTitle}
          </span>
        )}
        <div style={{ display: 'flex', alignItems: 'center', gap: '8px' }}>
          <span style={{
            flex: 1,
            overflow: 'hidden',
            textOverflow: 'ellipsis',
            whiteSpace: 'nowrap',
            fontSize: '0.78rem',
            color: 'var(--muted)',
            fontFamily: 'var(--sans)',
          }}>
            {displayUrl}
          </span>
          <a
            href={src}
            target="_blank"
            rel="noreferrer noopener"
            style={{
              fontSize: '0.78rem',
              color: 'var(--accent)',
              textDecoration: 'none',
              whiteSpace: 'nowrap',
              fontFamily: 'var(--sans)',
              flexShrink: 0,
            }}
          >
            {type === 'pdf' ? 'Open PDF ↗' : 'Open in new tab ↗'}
          </a>
        </div>
      </div>
      <iframe
        src={src}
        sandbox="allow-same-origin allow-popups"
        allow="autoplay 'none'"
        referrerPolicy="no-referrer"
        style={{ flex: 1, border: 'none', width: '100%', minHeight: 0 }}
        title={displayTitle || (type === 'pdf' ? 'PDF preview' : 'Page preview')}
      />
    </div>
  );
}
