import { useState, useEffect } from 'react'

const TOAST_TTL = 7000 // ms before auto-dismiss; paused when detail is expanded

export default function ToastStack({ toasts, onDismiss, onIgnoreUblock }) {
  if (!toasts.length) return null
  return (
    <div className="toast-stack" role="log" aria-live="polite" aria-label="Notifications">
      {toasts.map(t => (
        <Toast key={t.id} toast={t} onDismiss={onDismiss} onIgnoreUblock={onIgnoreUblock} />
      ))}
    </div>
  )
}

// For URLs: show hostname/…/tail (drops protocol + middle path, keeps domain + identifier).
// For shorthands (yt:, x:, etc.): keep the tail since it's the whole identifier.
// CSS text-overflow:ellipsis on .toast-locator handles any remaining overflow.
function shortLocator(locator) {
  if (!locator) return null
  if (locator.length <= 52) return locator
  try {
    const { hostname, pathname, search } = new URL(locator)
    const segments = pathname.split('/').filter(Boolean)
    const tail = (segments[segments.length - 1] ?? '') + search
    const candidate = tail ? `${hostname}/\u2026/${tail}` : hostname
    return candidate.length <= 56 ? candidate : candidate.slice(0, 53) + '\u2026'
  } catch {
    return '\u2026' + locator.slice(-51)
  }
}

function Toast({ toast, onDismiss, onIgnoreUblock }) {
  const [expanded, setExpanded] = useState(false)
  const isWarning = toast.type === 'warning'
  const isSuccess = toast.type === 'success'

  // Auto-dismiss after TTL; paused while detail is expanded
  useEffect(() => {
    if (expanded) return
    const timer = setTimeout(() => onDismiss(toast.id), TOAST_TTL)
    return () => clearTimeout(timer)
  }, [expanded, toast.id, onDismiss])

  const short = shortLocator(toast.locator)

  if (isSuccess) {
    return (
      <div className="toast toast--success" role="alert" aria-atomic="true">
        <div className="toast-top">
          <span className="toast-icon" aria-hidden="true">✓</span>
          <div className="toast-body">
            <span className="toast-headline">{toast.headline || 'Archived'}</span>
            {short && <span className="toast-locator" title={toast.locator}>{short}</span>}
          </div>
          <div className="toast-btns">
            <button type="button" className="toast-dismiss" onClick={() => onDismiss(toast.id)} aria-label="Dismiss">×</button>
          </div>
        </div>
      </div>
    )
  }

  if (isWarning) {
    return (
      <div className="toast toast--warning" role="alert" aria-atomic="true">
        <div className="toast-top">
          <span className="toast-icon" aria-hidden="true">⚠</span>
          <div className="toast-body">
            <span className="toast-headline">{toast.headline || 'Archived with warnings'}</span>
            {short && <span className="toast-locator" title={toast.locator}>{short}</span>}
          </div>
          <div className="toast-btns">
            {toast.text && (
              <button
                type="button"
                className="toast-view-btn"
                onClick={() => setExpanded(v => !v)}
                aria-expanded={expanded}
              >
                {expanded ? 'Hide' : 'Details'}
              </button>
            )}
            {toast.locator && (
              <button
                type="button"
                className="toast-view-btn toast-ignore-btn"
                onClick={() => { onIgnoreUblock?.(); onDismiss(toast.id) }}
              >
                Ignore
              </button>
            )}
            <button
              type="button"
              className="toast-dismiss"
              onClick={() => onDismiss(toast.id)}
              aria-label="Dismiss"
            >
              ×
            </button>
          </div>
        </div>
        {expanded && (
          <p className="toast-warning-detail">
            {toast.text || 'The page was captured but one or more browser extensions were unavailable (ad-blocking or cookie-consent). Check ARCHIVR_UBLOCK_EXT and ARCHIVR_COOKIE_EXT in your server config.'}
          </p>
        )}
      </div>
    )
  }

  return (
    <div className="toast toast--error" role="alert" aria-atomic="true">
      <div className="toast-top">
        <span className="toast-icon" aria-hidden="true">✕</span>
        <div className="toast-body">
          <span className="toast-headline">{toast.headline || 'Capture failed'}</span>
          {short && <span className="toast-locator" title={toast.locator}>{short}</span>}
        </div>
        <div className="toast-btns">
          {toast.text && (
            <button
              type="button"
              className="toast-view-btn"
              onClick={() => setExpanded(v => !v)}
            >
              {expanded ? 'Hide' : 'View error'}
            </button>
          )}
          <button
            type="button"
            className="toast-dismiss"
            onClick={() => onDismiss(toast.id)}
            aria-label="Dismiss"
          >
            ×
          </button>
        </div>
      </div>
      {expanded && toast.text && (
        <pre className="toast-error-detail">{toast.text}</pre>
      )}
    </div>
  )
}
