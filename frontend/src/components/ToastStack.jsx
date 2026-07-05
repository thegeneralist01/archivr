import { useState, useEffect } from 'react'

const TOAST_TTL = 7000 // ms before auto-dismiss; paused when error is expanded

export default function ToastStack({ toasts, onDismiss }) {
  if (!toasts.length) return null
  return (
    <div className="toast-stack" role="log" aria-live="polite" aria-label="Notifications">
      {toasts.map(t => (
        <Toast key={t.id} toast={t} onDismiss={onDismiss} />
      ))}
    </div>
  )
}

function Toast({ toast, onDismiss }) {
  const [expanded, setExpanded] = useState(false)

  // Auto-dismiss after TTL; paused while error detail is expanded
  useEffect(() => {
    if (expanded) return
    const timer = setTimeout(() => onDismiss(toast.id), TOAST_TTL)
    return () => clearTimeout(timer)
  }, [expanded, toast.id, onDismiss])

  const short = toast.locator
    ? (toast.locator.length > 48 ? toast.locator.slice(0, 45) + '\u2026' : toast.locator)
    : null

  return (
    <div className="toast toast--error" role="alert" aria-atomic="true">
      <div className="toast-top">
        <span className="toast-icon" aria-hidden="true">✕</span>
        <div className="toast-body">
          <span className="toast-headline">Capture failed</span>
          {short && <span className="toast-locator">{short}</span>}
        </div>
        <div className="toast-btns">
          {toast.errorText && (
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
      {expanded && toast.errorText && (
        <pre className="toast-error-detail">{toast.errorText}</pre>
      )}
    </div>
  )
}
