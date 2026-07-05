import { useRef, useEffect, useState } from 'react'
import { submitCapture, pollCaptureJob } from '../api'

let nextItemId = 1

function makeItem(locator = '') {
  return { id: nextItemId++, locator, status: 'idle', error: null, jobUid: null, archiveId: null }
}

function hasActiveJobs(items) {
  return items.some(it => it.status === 'submitting' || it.status === 'running')
}

export default function CaptureDialog({ open, archiveId, onClose, onCaptured, onToast }) {
  const dialogRef = useRef(null)
  const isFirstRenderRef = useRef(true)
  // jobUid → intervalId; survives dialog close since component stays mounted
  const pollIntervals = useRef(new Map())

  // Stable refs so polling callbacks always use the latest prop values
  const onCapturedRef = useRef(onCaptured)
  const onToastRef = useRef(onToast)
  useEffect(() => { onCapturedRef.current = onCaptured }, [onCaptured])
  useEffect(() => { onToastRef.current = onToast }, [onToast])

  const [items, setItems] = useState(() => {
    try {
      const saved = JSON.parse(sessionStorage.getItem('captureItems') || 'null')
      if (Array.isArray(saved) && saved.length > 0) {
        // Ensure nextItemId stays ahead of restored ids
        saved.forEach(it => { if (it.id >= nextItemId) nextItemId = it.id + 1 })
        return saved
      }
    } catch {}
    return [makeItem()]
  })

  // Persist items to sessionStorage on every change
  useEffect(() => {
    sessionStorage.setItem('captureItems', JSON.stringify(items))
  }, [items])

  // On mount: clean up old single-locator sessionStorage keys; reconnect running jobs
  useEffect(() => {
    ;['captureDialogLocator','captureDialogError','captureDialogBusy',
      'captureDialogJobStatus','captureDialogJobUid'].forEach(k => sessionStorage.removeItem(k))

    setItems(prev => prev.map(it =>
      // 'submitting' means page was refreshed mid-fetch — reset to idle so user can retry
      it.status === 'submitting' ? { ...it, status: 'idle', error: null } : it
    ))

    // Reconnect polling for any jobs still running from a previous session
    items.forEach(it => {
      if (it.status === 'running' && it.jobUid && it.archiveId && !pollIntervals.current.has(it.jobUid)) {
        startPolling(it.id, it.jobUid, it.locator, it.archiveId)
      }
    })
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []) // intentionally runs once on mount with initial values

  // Handle native dialog 'close' event (Escape key or programmatic .close())
  useEffect(() => {
    const dialog = dialogRef.current
    if (!dialog) return
    const handler = () => onClose()
    dialog.addEventListener('close', handler)
    return () => dialog.removeEventListener('close', handler)
  }, [onClose])

  // Open/close driven by parent; don't reset if active jobs are in flight
  useEffect(() => {
    const dialog = dialogRef.current
    if (!dialog) return
    if (open) {
      if (!isFirstRenderRef.current && !hasActiveJobs(items)) {
        setItems([makeItem()])
      }
      isFirstRenderRef.current = false
      if (!dialog.open) dialog.showModal()
    } else {
      if (dialog.open) dialog.close()
    }
  }, [open]) // eslint-disable-line react-hooks/exhaustive-deps

  // Clear all intervals on unmount (component teardown)
  useEffect(() => {
    return () => pollIntervals.current.forEach(id => clearInterval(id))
  }, [])

  function startPolling(itemId, jobUid, locator, aid) {
    if (pollIntervals.current.has(jobUid)) return // already polling
    const intervalId = setInterval(async () => {
      try {
        const updated = await pollCaptureJob(aid, jobUid)
        if (updated.status === 'completed') {
          clearInterval(pollIntervals.current.get(jobUid))
          pollIntervals.current.delete(jobUid)
          // Show ✓ briefly then remove the row; if last row add a fresh one
          setItems(prev => prev.map(it => it.id === itemId ? { ...it, status: 'completed' } : it))
          setTimeout(() => {
            setItems(prev => {
              const next = prev.filter(it => it.id !== itemId)
              return next.length === 0 ? [makeItem()] : next
            })
          }, 1400)
          onCapturedRef.current()
        } else if (updated.status === 'failed') {
          clearInterval(pollIntervals.current.get(jobUid))
          pollIntervals.current.delete(jobUid)
          const errText = updated.error_text || 'Capture failed.'
          setItems(prev => prev.map(it =>
            it.id === itemId ? { ...it, status: 'failed', error: errText } : it
          ))
          onToastRef.current(errText, locator)
        }
        // 'pending' / 'running': keep polling
      } catch (e) {
        clearInterval(pollIntervals.current.get(jobUid))
        pollIntervals.current.delete(jobUid)
        const msg = e.message || 'Network error'
        setItems(prev => prev.map(it =>
          it.id === itemId ? { ...it, status: 'failed', error: msg } : it
        ))
        onToastRef.current(msg, locator)
      }
    }, 500)
    pollIntervals.current.set(jobUid, intervalId)
  }

  async function submitItem(item) {
    if (!item.locator.trim()) return
    const aid = archiveId // capture at submit time
    const loc = item.locator.trim()
    setItems(prev => prev.map(it => it.id === item.id ? { ...it, status: 'submitting', error: null } : it))
    try {
      const job = await submitCapture(aid, loc)
      setItems(prev => prev.map(it =>
        it.id === item.id ? { ...it, status: 'running', jobUid: job.job_uid, archiveId: aid } : it
      ))
      startPolling(item.id, job.job_uid, loc, aid)
    } catch (e) {
      const msg = e.message || 'Submission failed.'
      setItems(prev => prev.map(it => it.id === item.id ? { ...it, status: 'failed', error: msg } : it))
      onToastRef.current(msg, loc)
    }
  }

  function handleArchive() {
    const toSubmit = items.filter(it => it.status === 'idle' && it.locator.trim())
    toSubmit.forEach(it => submitItem(it))
  }

  function addRow() {
    setItems(prev => [...prev, makeItem()])
  }

  function removeRow(id) {
    setItems(prev => {
      const next = prev.filter(it => it.id !== id)
      return next.length === 0 ? [makeItem()] : next
    })
  }

  function resetRow(id) {
    setItems(prev => prev.map(it =>
      it.id === id ? { ...it, status: 'idle', error: null } : it
    ))
  }

  function updateLocator(id, val) {
    setItems(prev => prev.map(it => it.id === id ? { ...it, locator: val } : it))
  }

  const pendingCount = items.filter(it => it.status === 'idle' && it.locator.trim()).length
  const anyActive = hasActiveJobs(items)

  return (
    <dialog ref={dialogRef} className="capture-dialog">
      <div className="capture-dialog-inner">
        <div className="capture-dialog-header">
          <h2 className="capture-dialog-title">Capture</h2>
          <button
            type="button"
            className="capture-dialog-close"
            onClick={() => dialogRef.current?.close()}
            aria-label="Close"
          >
            <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
              <line x1="3" y1="3" x2="13" y2="13"/><line x1="13" y1="3" x2="3" y2="13"/>
            </svg>
          </button>
        </div>

        <div className="capture-rows">
          {items.map((item, idx) => (
            <CaptureRow
              key={item.id}
              item={item}
              autoFocus={idx === items.length - 1 && item.status === 'idle'}
              onLocatorChange={val => updateLocator(item.id, val)}
              onRemove={() => removeRow(item.id)}
              onReset={() => resetRow(item.id)}
              onSubmit={handleArchive}
            />
          ))}
        </div>

        <button type="button" className="capture-add-row" onClick={addRow}>
          <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
            <line x1="8" y1="2" x2="8" y2="14"/><line x1="2" y1="8" x2="14" y2="8"/>
          </svg>
          Add another
        </button>

        <div className="capture-actions">
          <button type="button" className="capture-cancel" onClick={() => dialogRef.current?.close()}>
            {anyActive ? 'Close' : 'Cancel'}
          </button>
          <button
            type="button"
            className="capture-submit"
            onClick={handleArchive}
            disabled={pendingCount === 0}
          >
            {pendingCount > 1 ? `Archive ${pendingCount}` : 'Archive'}
          </button>
        </div>
      </div>
    </dialog>
  )
}

function CaptureRow({ item, autoFocus, onLocatorChange, onRemove, onReset, onSubmit }) {
  const inputRef = useRef(null)
  const isActive = item.status === 'submitting' || item.status === 'running'

  useEffect(() => {
    if (autoFocus && item.status === 'idle') {
      inputRef.current?.focus()
    }
  }, [autoFocus]) // eslint-disable-line react-hooks/exhaustive-deps

  return (
    <div className={`capture-row capture-row--${item.status}`}>
      <div className="capture-row-main">
        <CapStatusDot status={item.status} />
        <input
          ref={inputRef}
          className="capture-input"
          type="text"
          placeholder="https://… · yt:ID · tweet:ID · x:ID"
          value={item.locator}
          onChange={e => onLocatorChange(e.target.value)}
          onKeyDown={e => { if (e.key === 'Enter') onSubmit() }}
          disabled={isActive || item.status === 'completed'}
          autoComplete="off"
          spellCheck={false}
        />
        {item.status === 'failed' && (
          <button type="button" className="capture-row-action" onClick={onReset} title="Retry">
            <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <path d="M13 2.5A7 7 0 1 1 6.5 1"/>
              <polyline points="6.5 1 4 3.5 6.5 6"/>
            </svg>
          </button>
        )}
        {!isActive && item.status !== 'completed' && item.status !== 'failed' && (
          <button type="button" className="capture-row-action capture-row-remove" onClick={onRemove} aria-label="Remove">
            <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
              <line x1="3" y1="3" x2="13" y2="13"/><line x1="13" y1="3" x2="3" y2="13"/>
            </svg>
          </button>
        )}
      </div>
      {item.error && (
        <p className="capture-row-error">{item.error}</p>
      )}
    </div>
  )
}

function CapStatusDot({ status }) {
  if (status === 'submitting' || status === 'running') {
    return (
      <span className="cap-dot cap-dot--running" aria-label="Running">
        <span className="cap-spinner" />
      </span>
    )
  }
  if (status === 'completed') {
    return <span className="cap-dot cap-dot--ok" aria-label="Done">✓</span>
  }
  if (status === 'failed') {
    return <span className="cap-dot cap-dot--err" aria-label="Failed">✕</span>
  }
  return <span className="cap-dot cap-dot--idle" aria-hidden="true" />
}
