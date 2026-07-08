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

function Toast({ toast, onDismiss, onIgnoreUblock }) {
  const [expanded, setExpanded] = useState(false)
  const isWarning = toast.type === 'warning'

  // Auto-dismiss after TTL; paused while detail is expanded
  useEffect(() => {
    if (expanded) return
    const timer = setTimeout(() => onDismiss(toast.id), TOAST_TTL)
    return () => clearTimeout(timer)
  }, [expanded, toast.id, onDismiss])

  const short = toast.locator
    ? (toast.locator.length > 48 ? toast.locator.slice(0, 45) + '\u2026' : toast.locator)
    : null

  if (isWarning) {
    return (
      <div className="toast toast--warning" role="alert" aria-atomic="true">
        <div className="toast-top">
          <span className="toast-icon" aria-hidden="true">⚠</span>
          <div className="toast-body">
            <span className="toast-headline">Ad-blocking unavailable</span>
            {short && <span className="toast-locator">{short}</span>}
          </div>
          <div className="toast-btns">
            <button
              type="button"
              className="toast-view-btn"
              onClick={() => setExpanded(v => !v)}
              aria-expanded={expanded}
            >
              {expanded ? 'Hide' : 'Details'}
            </button>
            <button
              type="button"
              className="toast-view-btn toast-ignore-btn"
              onClick={() => { onIgnoreUblock?.(); onDismiss(toast.id) }}
            >
              Ignore
            </button>
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
            {toast.text || 'ARCHIVR_UBLOCK=true but ARCHIVR_UBLOCK_EXT is not set or the path is invalid. The page was captured without ad-blocking. Set ARCHIVR_UBLOCK_EXT to the unpacked uBlock Origin Lite extension directory to enable ad-blocking, or set ARCHIVR_UBLOCK=false to silence this warning.'}
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
          <span className="toast-headline">Capture failed</span>
          {short && <span className="toast-locator">{short}</span>}
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
