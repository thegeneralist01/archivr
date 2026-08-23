import { useEffect, useState } from 'react'

/**
 * Renders the primary_media artifact of a text/document entry as plain text.
 *
 * v1 intentionally does NOT render Markdown — we don't want a Markdown parser
 * dep just to unblock the "no preview available" fallback, and monospace text
 * with visible fences reads fine for the note-length content this feature
 * captures. Bump to a real renderer if/when Markdown-authored entries grow.
 */
export default function TextPreview({ src, mime, title }) {
  const [text, setText] = useState(null)
  const [error, setError] = useState(null)

  useEffect(() => {
    let cancelled = false
    setText(null)
    setError(null)
    fetch(src, { credentials: 'same-origin' })
      .then(r => {
        if (!r.ok) throw new Error(`HTTP ${r.status}`)
        return r.text()
      })
      .then(t => { if (!cancelled) setText(t) })
      .catch(e => { if (!cancelled) setError(e.message || String(e)) })
    return () => { cancelled = true }
  }, [src])

  if (error) {
    return (
      <div className="text-preview text-preview--error">
        Failed to load text: {error}
      </div>
    )
  }
  if (text === null) {
    return <div className="text-preview text-preview--loading">Loading…</div>
  }
  return (
    <div className="text-preview">
      {title && <h1 className="text-preview__title">{title}</h1>}
      <pre className="text-preview__body">{text}</pre>
      {mime && <div className="text-preview__mime">{mime}</div>}
    </div>
  )
}
