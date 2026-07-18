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
  // Set to true by ArticleRenderer when the entry turns out to be an X article,
  // triggering the X dark palette on the page shell.
  const [xArticle, setXArticle] = useState(false)

  // When displaying an X article, make the document scroll (not an inner div)
  // and apply color-scheme:dark so native scrollbars match the dark palette.
  useEffect(() => {
    if (!xArticle) return
    const html = document.documentElement
    const body = document.body
    const prevScheme = html.style.colorScheme
    const prevBg     = body.style.background
    html.style.colorScheme = 'dark'
    body.style.background  = '#000'
    return () => {
      html.style.colorScheme = prevScheme
      body.style.background  = prevBg
    }
  }, [xArticle])

  useEffect(() => {
    setXArticle(false)
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
      background: xArticle ? '#000' : 'var(--paper)',
      fontFamily: 'var(--sans)',
    }}>
      {/* Topbar — sticky + blur when showing an X article, matching a-topbar in the renderer */}
      <div style={{
        display: 'flex',
        alignItems: 'center',
        gap: '10px',
        padding: xArticle ? '8px 12px' : '10px 16px',
        borderBottom: xArticle ? 'none' : '1px solid var(--line)',
        flexShrink: 0,
        position: xArticle ? 'sticky' : 'relative',
        top: 0,
        zIndex: 20,
        background: xArticle ? 'rgba(0,0,0,0.82)' : 'var(--paper-2)',
        backdropFilter: xArticle ? 'blur(12px)' : 'none',
        WebkitBackdropFilter: xArticle ? 'blur(12px)' : 'none',
      }}>
        <a
          href="/"
          style={{ color: xArticle ? '#1d9bf0' : 'var(--accent)', textDecoration: 'none', fontSize: '13px', flexShrink: 0 }}
        >← Archive</a>
        <span style={{
          flex: 1,
          fontSize: '14px',
          fontWeight: 600,
          color: xArticle ? '#e7e9ea' : 'var(--ink)',
          overflow: 'hidden',
          textOverflow: 'ellipsis',
          whiteSpace: 'nowrap',
        }}>{title}</span>
        {originalUrl && (
          <a
            href={originalUrl}
            target="_blank"
            rel="noopener noreferrer"
            style={{ color: xArticle ? '#71767b' : 'var(--muted)', textDecoration: 'none', fontSize: '13px', flexShrink: 0 }}
          >Original ↗</a>
        )}
      </div>

      {/* Content */}
      <div style={xArticle
        ? { flex: 1 }
        : { flex: 1, minHeight: 0, overflow: 'auto', display: 'flex', flexDirection: 'column' }}>
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
            fullPage
            onXArticle={setXArticle}
          />
        )}
      </div>
    </div>
  )
}
