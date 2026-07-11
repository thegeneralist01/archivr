export default function IframePreview({ src, type }) {
  return (
    <div
      className="preview-iframe-wrap"
      style={{ height: '100%', display: 'flex', flexDirection: 'column' }}
    >
      {type === 'page' ? (
        <>
          <div
            className="preview-iframe-toolbar"
            style={{
              display: 'flex',
              alignItems: 'center',
              gap: '8px',
              padding: '6px 10px',
              borderBottom: '1px solid var(--line-soft)',
              background: 'var(--paper-2)',
              flexShrink: 0,
            }}
          >
            <span
              style={{
                flex: 1,
                overflow: 'hidden',
                textOverflow: 'ellipsis',
                whiteSpace: 'nowrap',
                fontSize: '0.8rem',
                color: 'var(--muted)',
                fontFamily: 'var(--sans)',
              }}
            >
              {src}
            </span>
            <a
              href={src}
              target="_blank"
              rel="noreferrer noopener"
              style={{
                fontSize: '0.8rem',
                color: 'var(--accent)',
                textDecoration: 'none',
                whiteSpace: 'nowrap',
                fontFamily: 'var(--sans)',
              }}
            >
              Open in new tab ↗
            </a>
          </div>
          <iframe
            src={src}
            sandbox="allow-same-origin allow-popups"
            allow="autoplay 'none'"
            referrerPolicy="no-referrer"
            style={{ flex: 1, border: 'none', width: '100%' }}
            title="Page preview"
          />
        </>
      ) : (
        <>
          <div
            style={{
              padding: '8px 12px',
              borderBottom: '1px solid var(--line-soft)',
              background: 'var(--paper-2)',
              flexShrink: 0,
              fontSize: '0.85rem',
              color: 'var(--muted)',
              fontFamily: 'var(--sans)',
            }}
          >
            PDF Document
          </div>
          <iframe
            src={src}
            allow="autoplay 'none'"
            style={{ flex: 1, border: 'none', width: '100%' }}
            title="PDF preview"
          />
        </>
      )}
    </div>
  );
}
