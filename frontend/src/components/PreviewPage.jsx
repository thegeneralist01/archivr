import { useState, useEffect } from 'react'
import { fetchEntryDetail } from '../api'
import PreviewPanel from './PreviewPanel'

// Standalone full-page preview — rendered when the URL is /preview/:archiveId/:entryUid.
// Auth uses the existing session cookie; no login flow needed here since the
// main app will have set one.
export default function PreviewPage({ archiveId, entryUid }) {
  const [detail, setDetail] = useState(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState(null)

  useEffect(() => {
    fetchEntryDetail(archiveId, entryUid)
      .then(d => { setDetail(d); setLoading(false) })
      .catch(e => { setError(e?.message || 'Failed to load entry'); setLoading(false) })
  }, [archiveId, entryUid])

  const title = detail?.summary?.title || entryUid
  const originalUrl = detail?.summary?.original_url

  return (
    <div style={{
      minHeight: '100vh',
      display: 'flex',
      flexDirection: 'column',
      background: 'var(--paper)',
      fontFamily: 'var(--sans)',
    }}>
      {/* Minimal topbar */}
      <div style={{
        display: 'flex',
        alignItems: 'center',
        gap: '10px',
        padding: '10px 16px',
        borderBottom: '1px solid var(--line)',
        flexShrink: 0,
        background: 'var(--paper-2)',
      }}>
        <a
          href="/"
          style={{ color: 'var(--accent)', textDecoration: 'none', fontSize: '13px', flexShrink: 0 }}
        >← Archive</a>
        <span style={{
          flex: 1,
          fontSize: '14px',
          fontWeight: 600,
          color: 'var(--ink)',
          overflow: 'hidden',
          textOverflow: 'ellipsis',
          whiteSpace: 'nowrap',
        }}>{title}</span>
        {originalUrl && (
          <a
            href={originalUrl}
            target="_blank"
            rel="noopener noreferrer"
            style={{ color: 'var(--muted)', textDecoration: 'none', fontSize: '13px', flexShrink: 0 }}
          >Original ↗</a>
        )}
      </div>

      {/* Content */}
      <div style={{ flex: 1, minHeight: 0, overflow: 'auto', display: 'flex', flexDirection: 'column' }}>
        {loading && (
          <div style={{ padding: '48px', textAlign: 'center', color: 'var(--muted)', fontSize: '14px' }}>
            Loading…
          </div>
        )}
        {error && (
          <div style={{ padding: '48px', textAlign: 'center', color: 'var(--alert)', fontSize: '14px' }}>
            {error}
          </div>
        )}
        {detail && (
          <PreviewPanel
            archiveId={archiveId}
            entry={detail.summary}
            detail={detail}
          />
        )}
      </div>
    </div>
  )
}
