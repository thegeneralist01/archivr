import { useRef, useEffect, useState } from 'react'
import { submitCapture } from '../api'

export default function CaptureDialog({ open, archiveId, onClose, onCaptured }) {
  const dialogRef = useRef(null)
  const [locator, setLocator] = useState('')
  const [error, setError] = useState(null)
  const [busy, setBusy] = useState(false)

  useEffect(() => {
    const dialog = dialogRef.current
    if (!dialog) return
    const handleClose = () => onClose()
    dialog.addEventListener('close', handleClose)
    return () => dialog.removeEventListener('close', handleClose)
  }, [onClose])

  useEffect(() => {
    const dialog = dialogRef.current
    if (!dialog) return
    if (open) {
      setLocator('')
      setError(null)
      if (!dialog.open) dialog.showModal()
    } else {
      if (dialog.open) dialog.close()
    }
  }, [open])

  async function handleSubmit() {
    if (!locator.trim()) { setError('Enter a locator.'); return }
    setBusy(true)
    setError(null)
    try {
      await submitCapture(archiveId, locator.trim())
      dialogRef.current?.close()
      onCaptured()
    } catch (e) {
      setError(e.message)
    } finally {
      setBusy(false)
    }
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
            {busy ? 'Capturing\u2026' : 'Capture'}
          </button>
        </div>
      </div>
    </dialog>
  )
}
