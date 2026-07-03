import { useRef, useEffect, useState } from 'react'
import { submitCapture, pollCaptureJob } from '../api'

export default function CaptureDialog({ open, archiveId, onClose, onCaptured }) {
  const dialogRef = useRef(null)
  const isFirstRenderRef = useRef(true)
  const hasResumedPollingRef = useRef(false)
  
  const [locator, setLocator] = useState(() => {
    const saved = sessionStorage.getItem('captureDialogLocator')
    return saved || ''
  })
  const [error, setError] = useState(() => {
    const saved = sessionStorage.getItem('captureDialogError')
    return saved || null
  })
  const [busy, setBusy] = useState(() => {
    const saved = sessionStorage.getItem('captureDialogBusy')
    return saved === 'true'
  })
  const [jobStatus, setJobStatus] = useState(() => {
    const saved = sessionStorage.getItem('captureDialogJobStatus')
    return saved || null
  })
  const [jobUid, setJobUid] = useState(() => {
    const saved = sessionStorage.getItem('captureDialogJobUid')
    return saved || null
  })
  const pollRef = useRef(null)

  // Persist state to sessionStorage
  useEffect(() => {
    sessionStorage.setItem('captureDialogLocator', locator)
  }, [locator])

  useEffect(() => {
    sessionStorage.setItem('captureDialogError', error || '')
  }, [error])

  useEffect(() => {
    sessionStorage.setItem('captureDialogBusy', busy)
  }, [busy])

  useEffect(() => {
    sessionStorage.setItem('captureDialogJobStatus', jobStatus || '')
  }, [jobStatus])

  useEffect(() => {
    sessionStorage.setItem('captureDialogJobUid', jobUid || '')
  }, [jobUid])

  // On mount, resume polling if a job was in progress before page refresh
  useEffect(() => {
    if (hasResumedPollingRef.current) return
    if (!jobUid || jobStatus !== 'running' || !archiveId) return
    
    hasResumedPollingRef.current = true
    
    // Resume polling for the saved job
    pollRef.current = setInterval(async () => {
      try {
        const updated = await pollCaptureJob(archiveId, jobUid)
        if (updated.status === 'completed') {
          clearInterval(pollRef.current)
          pollRef.current = null
          setBusy(false)
          setJobStatus('completed')
          clearCaptureState()
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
  }, [jobUid, jobStatus, archiveId, onCaptured])

  // Handle dialog close event
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

  // Handle open/close from parent
  useEffect(() => {
    const dialog = dialogRef.current
    if (!dialog) return
    
    if (open) {
      // Only clear state if this is a fresh open from user click (not a restore on first render)
      if (!isFirstRenderRef.current) {
        setLocator('')
        setError(null)
        setJobStatus(null)
        setBusy(false)
        setJobUid(null)
        clearInterval(pollRef.current)
      }
      isFirstRenderRef.current = false
      if (!dialog.open) dialog.showModal()
    } else {
      if (dialog.open) dialog.close()
    }
  }, [open])

  function clearCaptureState() {
    sessionStorage.removeItem('captureDialogLocator')
    sessionStorage.removeItem('captureDialogError')
    sessionStorage.removeItem('captureDialogBusy')
    sessionStorage.removeItem('captureDialogJobStatus')
    sessionStorage.removeItem('captureDialogJobUid')
    setLocator('')
    setError(null)
    setBusy(false)
    setJobStatus(null)
    setJobUid(null)
  }

  async function handleSubmit() {
    if (!locator.trim()) { setError('Enter a locator.'); return }
    setBusy(true)
    setError(null)
    setJobStatus(null)
    try {
      const job = await submitCapture(archiveId, locator.trim())
      setJobUid(job.job_uid)
      setJobStatus('running')
      pollRef.current = setInterval(async () => {
        try {
          const updated = await pollCaptureJob(archiveId, job.job_uid)
          if (updated.status === 'completed') {
            clearInterval(pollRef.current)
            pollRef.current = null
            setBusy(false)
            setJobStatus('completed')
            clearCaptureState()
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
