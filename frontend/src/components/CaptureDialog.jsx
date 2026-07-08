import { useRef, useEffect, useState, useCallback } from 'react'
import { submitCapture, pollCaptureJob, probeCapture, getInstanceSettings } from '../api'

let nextItemId = 1

// Returns true only for locators that determine_source() routes to yt-dlp download.
// Mirrors the exact conditions in capture.rs — playlist/channel shorthands are excluded
// (they return an "not yet implemented" error), as are tweet/thread shorthands.
function isVideoSource(locator) {
  const l = locator.trim()
  const ll = l.toLowerCase()

  // yt: / youtube: shorthands — video/short/shorts only; playlist and channel are unsupported
  for (const scheme of ['yt:', 'youtube:']) {
    if (ll.startsWith(scheme)) {
      const after = ll.slice(scheme.length)
      return after.startsWith('video/') || after.startsWith('short/') || after.startsWith('shorts/')
    }
  }

  // ytm: shorthand — track only; playlist is not yet implemented
  if (ll.startsWith('ytm:')) {
    return !ll.slice(4).startsWith('playlist/')
  }

  // x: / twitter: / tweet: shorthands — only x:media:ID routes to yt-dlp (Source::X)
  for (const scheme of ['x:', 'twitter:', 'tweet:']) {
    if (ll.startsWith(scheme)) {
      return ll.slice(scheme.length).startsWith('media:')
    }
  }

  // spotify: shorthands — all will fail with a clear error; no probe needed
  if (ll.startsWith('spotify:')) return false

  // Other platform shorthands — all go to yt-dlp
  if (ll.startsWith('instagram:') || ll.startsWith('facebook:') ||
      ll.startsWith('tiktok:') || ll.startsWith('reddit:') ||
      ll.startsWith('snapchat:')) return true

  // HTTP/HTTPS URLs — match the same regexes and prefix checks as determine_source
  if (ll.startsWith('http://') || ll.startsWith('https://')) {
    // YouTube Music track (watch) — before generic YouTube check
    if (/^https?:\/\/music\.youtube\.com\/watch/.test(ll)) return true
    // YouTube video (watch, youtu.be, shorts) — not playlist or channel
    if (/^https?:\/\/(?:www\.)?(?:youtu\.be\/[0-9A-Za-z_-]+|youtube\.com\/watch\?v=[0-9A-Za-z_-]+|youtube\.com\/shorts\/[0-9A-Za-z_-]+)/.test(l)) return true
    // x.com → Source::X → yt-dlp (note: twitter.com URLs fall through to Source::Url, not yt-dlp)
    if (ll.startsWith('https://x.com/') || ll.startsWith('http://x.com/')) return true
    // Instagram
    if (/^https?:\/\/(?:www\.)?instagram\.com\//.test(ll)) return true
    // Facebook + fb.watch
    if (/^https?:\/\/(?:www\.)?facebook\.com\//.test(ll) || ll.startsWith('https://fb.watch/') || ll.startsWith('http://fb.watch/')) return true
    // TikTok
    if (/^https?:\/\/(?:www\.)?tiktok\.com\//.test(ll)) return true
    // Reddit + redd.it
    if (/^https?:\/\/(?:www\.)?reddit\.com\//.test(ll) || ll.startsWith('https://redd.it/') || ll.startsWith('http://redd.it/')) return true
    // Snapchat
    if (/^https?:\/\/(?:www\.)?snapchat\.com\//.test(ll)) return true
    // Spotify — all will fail with a clear error; no probe needed
    if (ll.startsWith('https://open.spotify.com/') || ll.startsWith('http://open.spotify.com/')) return false
  }

  return false
}

function makeItem(locator = '') {
  return {
    id: nextItemId++, locator, quality: 'best',
    // probe: tracks yt-dlp metadata fetch for the locator
    probeState: 'idle',    // 'idle' | 'probing' | 'done'
    probeQualities: null,  // null | string[] when done, e.g. ["1080p","720p","480p"]
    probeHasAudio: false,  // true when probe confirms at least one audio track
    status: 'idle', error: null, jobUid: null, archiveId: null,
  }
}

function hasActiveJobs(items) {
  return items.some(it => it.status === 'submitting' || it.status === 'running')
}

export default function CaptureDialog({ open, archiveId, onClose, onCaptured, onToast }) {
  const dialogRef = useRef(null)
  const isFirstRenderRef = useRef(true)
  // jobUid → intervalId; survives dialog close since component stays mounted
  const pollIntervals = useRef(new Map())
  // itemId → debounce timeoutId for probe calls
  const probeTimers = useRef(new Map())
  // stable ref so probe callbacks always see the current archiveId
  const archiveIdRef = useRef(archiveId)
  useEffect(() => { archiveIdRef.current = archiveId }, [archiveId])

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

  // Advanced options panel state
  const [advancedOpen, setAdvancedOpen] = useState(false)
  // null = use server default; true/false = per-session override
  const [ublockOverride, setUblockOverride] = useState(null)
  // Server-side global settings (loaded on mount, null until loaded)
  const [globalSettings, setGlobalSettings] = useState(null)
  // Cookie consent: session-level only, initialized from server default
  const [cookieExtEnabled, setCookieExtEnabled] = useState(true)

  // Load global settings from server once on mount
  useEffect(() => {
    getInstanceSettings()
      .then(s => {
        setGlobalSettings(s)
        setCookieExtEnabled(s.cookie_ext_enabled ?? true)
      })
      .catch(() => setGlobalSettings({}))
  }, [])

  // Effective uBlock for this session
  const ublockEnabled = ublockOverride !== null ? ublockOverride : (globalSettings?.ublock_enabled ?? true)

  // Reader mode: off by default, per-session only
  const [readerMode, setReaderMode] = useState(false)

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
    const handler = () => {
      probeTimers.current.forEach(id => clearTimeout(id))
      probeTimers.current.clear()
      onClose()
    }
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
      probeTimers.current.forEach(id => clearTimeout(id))
      probeTimers.current.clear()
      if (dialog.open) dialog.close()
    }
  }, [open]) // eslint-disable-line react-hooks/exhaustive-deps

  // Clear all intervals and probe timers on unmount (component teardown)
  useEffect(() => {
    return () => {
      pollIntervals.current.forEach(id => clearInterval(id))
      probeTimers.current.forEach(id => clearTimeout(id))
    }
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
          // Warn if uBlock was requested but the extension wasn't available
          try {
            const notes = updated.notes_json ? JSON.parse(updated.notes_json) : null
            if (notes?.ublock_skipped || notes?.cookie_ext_skipped) {
              onToastRef.current(null, locator, 'warning')
            }
          } catch {}
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
    const qual = item.quality || 'best'
    setItems(prev => prev.map(it => it.id === item.id ? { ...it, status: 'submitting', error: null } : it))
    try {
      const extensions = { ublock_enabled: ublockEnabled, reader_mode: readerMode, cookie_ext_enabled: cookieExtEnabled }
      const job = await submitCapture(aid, loc, qual, extensions)
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
    clearTimeout(probeTimers.current.get(id))
    probeTimers.current.delete(id)
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
    // Cancel any in-flight debounce and immediately clear stale probe results.
    // This prevents old qualities from being visible (and submittable) while
    // the 600ms debounce is pending for the new URL.
    clearTimeout(probeTimers.current.get(id))
    setItems(prev => prev.map(it =>
      it.id === id
        ? { ...it, locator: val, probeState: 'idle', probeQualities: null, probeHasAudio: false, quality: 'best' }
        : it
    ))

    if (!isVideoSource(val)) return

    // Schedule a fresh probe after the user stops typing
    const timer = setTimeout(async () => {
      probeTimers.current.delete(id)
      setItems(prev => prev.map(it => it.id === id ? { ...it, probeState: 'probing' } : it))
      try {
        const result = await probeCapture(archiveIdRef.current, val.trim())
        setItems(prev => prev.map(it => {
          if (it.id !== id || it.locator !== val) return it // stale — locator changed again
          const qualities = result.qualities ?? []
          const hasAudio = result.has_audio ?? false
          // Audio-only source: no video heights but audio confirmed — force audio mode
          const quality = (qualities.length === 0 && hasAudio) ? 'audio' : 'best'
          return { ...it, probeState: 'done', probeQualities: qualities, probeHasAudio: hasAudio, quality }
        }))
      } catch {
        // Probe failed (network error, etc.) — clear silently; user can still submit
        setItems(prev => prev.map(it =>
          it.id === id ? { ...it, probeState: 'idle', probeQualities: null } : it
        ))
      }
    }, 600)
    probeTimers.current.set(id, timer)
  }

  function updateQuality(id, val) {
    setItems(prev => prev.map(it => it.id === id ? { ...it, quality: val } : it))
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
              onQualityChange={val => updateQuality(item.id, val)}
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

        {/* ── Advanced options ────────────────────────────── */}
        <div className="capture-advanced">
          <button
            type="button"
            className="capture-advanced-toggle"
            onClick={() => setAdvancedOpen(v => !v)}
            aria-expanded={advancedOpen}
          >
            <svg
              className={`capture-chevron${advancedOpen ? ' capture-chevron--open' : ''}`}
              viewBox="0 0 16 16" fill="none" stroke="currentColor"
              strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"
            >
              <polyline points="4 6 8 10 12 6"/>
            </svg>
            Advanced options
          </button>
          {advancedOpen && (
            <div className="capture-advanced-panel">
              <label className="capture-ext-row">
                <span className="capture-ext-label">
                  <span className="capture-ext-name">uBlock Origin Lite</span>
                  <span className="capture-ext-desc">Block ads during this capture</span>
                </span>
                <button
                  type="button"
                  role="switch"
                  aria-checked={ublockEnabled}
                  className={`ext-toggle ext-toggle--sm${ublockEnabled ? ' ext-toggle--on' : ''}`}
                  onClick={() => setUblockOverride(v => v === null ? !ublockEnabled : !v)}
                  aria-label="Toggle uBlock for this capture"
                >
                  <span className="ext-toggle-knob" />
                </button>
              </label>
              <label className="capture-ext-row" style={{ marginTop: 8 }}>
                <span className="capture-ext-label">
                  <span className="capture-ext-name">Block cookie banners</span>
                  <span className="capture-ext-desc">Dismiss cookie consent banners during this capture</span>
                  {!globalSettings?.cookie_ext_available && (
                    <span className="capture-ext-hint">Not configured &mdash; set <code>ARCHIVR_COOKIE_EXT</code></span>
                  )}
                </span>
                <button
                  type="button"
                  role="switch"
                  aria-checked={cookieExtEnabled}
                  className={`ext-toggle ext-toggle--sm${cookieExtEnabled ? ' ext-toggle--on' : ''}`}
                  onClick={() => setCookieExtEnabled(v => !v)}
                  aria-label="Toggle cookie banner blocking for this capture"
                >
                  <span className="ext-toggle-knob" />
                </button>
              </label>
              <label className="capture-ext-row" style={{ marginTop: 8 }}>
                <span className="capture-ext-label">
                  <span className="capture-ext-name">Reader mode</span>
                  <span className="capture-ext-desc">Distil to article text via Readability (off by default)</span>
                </span>
                <button
                  type="button"
                  role="switch"
                  aria-checked={readerMode}
                  className={`ext-toggle ext-toggle--sm${readerMode ? ' ext-toggle--on' : ''}`}
                  onClick={() => setReaderMode(v => !v)}
                  aria-label="Toggle reader mode for this capture"
                >
                  <span className="ext-toggle-knob" />
                </button>
              </label>
            </div>
          )}
        </div>

        {/* ── Primary action ──────────────────────────────── */}
        <div className="capture-actions">
          <button
            type="button"
            className="capture-submit"
            onClick={handleArchive}
            disabled={pendingCount === 0}
          >
            {pendingCount > 1 ? `Archive ${pendingCount}` : 'Archive'}
          </button>
          <button type="button" className="capture-cancel" onClick={() => dialogRef.current?.close()}>
            {anyActive ? 'Close' : 'Cancel'}
          </button>
        </div>
      </div>
    </dialog>
  )
}

function CaptureRow({ item, autoFocus, onLocatorChange, onQualityChange, onRemove, onReset, onSubmit }) {
  const inputRef = useRef(null)
  const isActive = item.status === 'submitting' || item.status === 'running'

  useEffect(() => {
    if (autoFocus && item.status === 'idle') {
      inputRef.current?.focus()
    }
  }, [autoFocus]) // eslint-disable-line react-hooks/exhaustive-deps

  // Quality control shown right of the input (hidden when active or completed)
  const qualityEl = (() => {
    if (item.status === 'completed' || isActive) return null
    if (!isVideoSource(item.locator)) return null
    if (item.probeState === 'probing') {
      return <span className="capture-quality-probing" aria-label="Checking available qualities">…</span>
    }
    if (item.probeState === 'done') {
      const qualities = item.probeQualities ?? []
      const hasAudio = item.probeHasAudio ?? false
      if (qualities.length === 0 && !hasAudio) {
        return <span className="capture-quality-hint">No media detected</span>
      }
      if (qualities.length === 0 && hasAudio) {
        // Audio-only source: no video tracks, only audio available.
        // Don't offer "Best quality" — it would request a video format and fail.
        return (
          <select
            className="capture-quality"
            value="audio"
            onChange={e => onQualityChange(e.target.value)}
            aria-label="Video quality"
          >
            <option value="audio">Audio only</option>
          </select>
        )
      }
      return (
        <select
          className="capture-quality"
          value={item.quality || 'best'}
          onChange={e => onQualityChange(e.target.value)}
          aria-label="Video quality"
        >
          <option value="best">Best quality</option>
          {qualities.map(q => <option key={q} value={q}>{q}</option>)}
          {hasAudio && <option value="audio">Audio only</option>}
        </select>
      )
    }
    return null // probeState === 'idle', debounce not yet fired
  })()

  return (
    <div className={`capture-row capture-row--${item.status}`}>
      <div className="capture-row-main">
        <CapStatusDot status={item.status} />
        <input
          ref={inputRef}
          className="capture-input"
          type="text"
          placeholder="https://… · yt:ID · ytm:ID · tweet:ID · x:ID"
          value={item.locator}
          onChange={e => onLocatorChange(e.target.value)}
          onKeyDown={e => { if (e.key === 'Enter') onSubmit() }}
          disabled={isActive || item.status === 'completed'}
          autoComplete="off"
          spellCheck={false}
        />
        {qualityEl}
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
