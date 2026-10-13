import { useRef, useEffect, useState } from 'react'
import { downloadEntryExport } from '../api'

export default function ExportDialog({ open, archiveId, entryUids, onClose }) {
  const dialogRef = useRef(null)

  const [format, setFormat] = useState('html')
  const [together, setTogether] = useState(true)
  const [credit, setCredit] = useState(true)
  const [status, setStatus] = useState('idle') // 'idle' | 'running' | 'error'
  const [error, setError] = useState(null)

  // Sync open state with the native dialog.
  useEffect(() => {
    const dialog = dialogRef.current
    if (!dialog) return
    if (open) {
      if (!dialog.open) dialog.showModal()
    } else {
      if (dialog.open) dialog.close()
    }
  }, [open])

  // Wire the native close event (Escape key, etc.) back to onClose.
  useEffect(() => {
    const dialog = dialogRef.current
    if (!dialog) return
    const handler = () => onClose()
    dialog.addEventListener('close', handler)
    return () => dialog.removeEventListener('close', handler)
  }, [onClose])

  // Reset state each time the dialog opens.
  useEffect(() => {
    if (open) {
      setFormat('html')
      setTogether(true)
      setCredit(true)
      setStatus('idle')
      setError(null)
    }
  }, [open])

  const n = entryUids?.length ?? 0
  const title = n === 1 ? 'Export entry' : `Export ${n} entries`

  async function handleSubmit(e) {
    e.preventDefault()
    setStatus('running')
    setError(null)
    try {
      const { blob, filename } = await downloadEntryExport(archiveId, {
        entryUids,
        format,
        layout: together ? 'together' : 'separate',
        credit,
      })
      // Trigger browser download via a temporary anchor.
      const url = URL.createObjectURL(blob)
      const a = document.createElement('a')
      a.href = url
      a.download = filename
      document.body.appendChild(a)
      a.click()
      document.body.removeChild(a)
      setTimeout(() => URL.revokeObjectURL(url), 1000)
      dialogRef.current?.close()
    } catch (err) {
      setStatus('error')
      setError(err.message || 'Export failed')
    }
  }

  return (
    <dialog ref={dialogRef} className="capture-dialog export-dialog">
      <div className="capture-dialog-inner">
        <div className="capture-dialog-header">
          <h2 className="capture-dialog-title">{title}</h2>
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

        <form onSubmit={handleSubmit}>
          <div className="capture-rows">

            {/* Format selector */}
            <label className="capture-ext-row" style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
              <span className="capture-ext-name">Format</span>
              <select
                className="capture-quality export-format"
                value={format}
                onChange={e => setFormat(e.target.value)}
                disabled={status === 'running'}
              >
                <option value="html">Web page — HTML (.zip)</option>
                <option value="markdown">Markdown (.zip)</option>
              </select>
            </label>

            {/* Combine toggle — only when ≥2 entries */}
            {n >= 2 && (
              <label className="capture-ext-row">
                <span className="capture-ext-label">
                  <span className="capture-ext-name">Combine into one document</span>
                  <span className="capture-ext-desc">Off: each entry is exported on its own</span>
                </span>
                <button
                  type="button"
                  role="switch"
                  aria-checked={together}
                  className={`ext-toggle ext-toggle--sm${together ? ' ext-toggle--on' : ''}`}
                  onClick={() => setTogether(v => !v)}
                  aria-label="Toggle combine"
                  disabled={status === 'running'}
                >
                  <span className="ext-toggle-knob" />
                </button>
              </label>
            )}

            {/* Credit toggle */}
            <label className="capture-ext-row">
              <span className="capture-ext-label">
                <span className="capture-ext-name">Include Archivr credit</span>
                <span className="capture-ext-desc">Adds a small "Preserved with archivr" line at the end</span>
              </span>
              <button
                type="button"
                role="switch"
                aria-checked={credit}
                className={`ext-toggle ext-toggle--sm${credit ? ' ext-toggle--on' : ''}`}
                onClick={() => setCredit(v => !v)}
                aria-label="Toggle Archivr credit"
                disabled={status === 'running'}
              >
                <span className="ext-toggle-knob" />
              </button>
            </label>

          </div>

          {status === 'error' && error && (
            <p className="form-msg form-msg--err">{error}</p>
          )}

          <div className="capture-actions">
            <button
              type="submit"
              className="capture-submit"
              disabled={status === 'running'}
            >
              {status === 'running' ? 'Exporting\u2026' : 'Export'}
            </button>
            <button
              type="button"
              className="capture-cancel"
              onClick={() => dialogRef.current?.close()}
              disabled={status === 'running'}
            >
              Cancel
            </button>
          </div>
        </form>
      </div>
    </dialog>
  )
}
