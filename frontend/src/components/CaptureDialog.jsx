import { useRef, useEffect, useState } from 'react'
import { submitCapture, pollCaptureJob } from '../api'

export default function CaptureDialog({ open, archiveId, onClose, onCaptured }) {
  const dialogRef = useRef(null)
  const [locator, setLocator] = useState('')
  const [error, setError] = useState(null)
  const [busy, setBusy] = useState(false)
  const [jobStatus, setJobStatus] = useState(null) // null | 'running' | 'completed' | 'failed'
  const pollRef = useRef(null)

  useEffect(() => {
    const dialog = dialogRef.current
    if (!dialog) return
    const handleClose = () => {
      clearInterval(pollRef.current)
      onClose()
    }
    dialog.addEventListener('close', handleClose)
    return () => dialog.removeEventListener('close', handleClose)
  }, [onClose])

  useEffect(() => {
    const dialog = dialogRef.current
    if (!dialog) return
    if (open) {
      setLocator('')
      setError(null)
      setJobStatus(null)
      setBusy(false)
      clearInterval(pollRef.current)
      if (!dialog.open) dialog.showModal()
    } else {
      if (dialog.open) dialog.close()
    }
  }, [open])

  async function handleSubmit() {
    if (!locator.trim()) { setError('Enter a locator.'); return }
    setBusy(true)
    setError(null)
    setJobStatus(null)
    try {
      const job = await submitCapture(archiveId, locator.trim())
      setJobStatus('running')
      pollRef.current = setInterval(async () => {
        try {
          const updated = await pollCaptureJob(archiveId, job.job_uid)
          if (updated.status === 'completed') {
            clearInterval(pollRef.current)
            pollRef.current = null
            setBusy(false)
            setJobStatus('completed')
            dialogRef.current?.close()
            onCaptured()
          } else if (updated.status === 'failed') {
            clearInterval(pollRef.current)
            pollRef.current = null
            setBusy(false)
            setJobStatus('failed')
            setError(updated.error_text || 'Capture failed.')
          }
          // pending / running: keep polling
        } catch (pollErr) {
          clearInterval(pollRef.current)
          pollRef.current = null
          setBusy(false)
          setError(pollErr.message)
        }
      }, 500)
    } catch (e) {
      setError(e.message)
      setBusy(false)
    }
  }

  function buttonLabel() {
    if (!busy) return 'Capture'
    if (jobStatus === 'running') return 'Running\u2026'
    return 'Capturing\u2026'
  }

  return (
    <dialog ref={dialogRef} className="capture-dialog">
      <div className="capture-dialog-inner">
        <h2 className="capture-dialog-title">Capture</h2>
        <label htmlFor="capture-locator" className="capture-label">Locator</label>
        <input id="capture-locator" className="capture-input" type="text"
          placeholder="tweet:1234567890 or https://..."
          value={locator} onChange={e => setLocator(e.target.value)}
          onKeyDown={e => { if (e.key === 'Enter') handleSubmit() }}
          autoComplete="off" />
        {error && <div className="capture-error">{error}</div>}
        <div className="capture-actions">
          <button type="button" className="capture-cancel" onClick={() => dialogRef.current?.close()}>Cancel</button>
          <button type="button" className="capture-submit" onClick={handleSubmit} disabled={busy}>
            {buttonLabel()}
          </button>
        </div>
      </div>
    </dialog>
  )
}
