import { useRef, useEffect, useState, useCallback } from 'react'
import { submitCapture, pollCaptureJob, probeCapture, probePlaylist, getInstanceSettings, uploadFile, deleteUpload } from '../api'

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
      if (after.startsWith('video/') || after.startsWith('short/') || after.startsWith('shorts/'))
        return true
      // bare yt:ID — exactly 11 chars [A-Za-z0-9_-], same predicate as is_youtube_video_id in core
      if (/^[a-z0-9_-]{11}$/i.test(after)) return true
      return false
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

function isPlaylistSource(locator) {
  const l = locator.trim()
  const ll = l.toLowerCase()

  // yt: / youtube: shorthands — playlist, channel, @ handles
  for (const scheme of ['yt:', 'youtube:']) {
    if (ll.startsWith(scheme)) {
      const after = ll.slice(scheme.length)
      return after.startsWith('playlist/') || after.startsWith('@') ||
             after.startsWith('channel/') || after.startsWith('c/') || after.startsWith('user/')
    }
  }

  // ytm: shorthand — playlist
  if (ll.startsWith('ytm:')) {
    return ll.slice(4).startsWith('playlist/')
  }

  // spotify: shorthands — album and playlist (not track)
  if (ll.startsWith('spotify:')) {
    const after = ll.slice(8)
    return after.startsWith('album:') || after.startsWith('playlist:')
  }

  // HTTP/HTTPS URLs
  if (ll.startsWith('http://') || ll.startsWith('https://')) {
    try {
      const url = new URL(l)
      const host = url.hostname
      if (host === 'youtube.com' || host === 'www.youtube.com') {
        if (url.pathname === '/playlist' && url.searchParams.has('list')) return true
        if (url.pathname.startsWith('/@') || url.pathname.startsWith('/channel/') ||
            url.pathname.startsWith('/c/') || url.pathname.startsWith('/user/')) return true
      }
      if (host === 'music.youtube.com') {
        if (url.pathname === '/playlist' && url.searchParams.has('list')) return true
      }
      if (host === 'open.spotify.com') {
        if (url.pathname.startsWith('/album/') || url.pathname.startsWith('/playlist/')) return true
      }
    } catch {}
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
    // playlist probe state
    playlistProbeState: 'idle',  // 'idle' | 'probing' | 'done' | 'error'
    playlistInfo: null,          // raw API response or null
    playlistItems: null,         // [{id, url, title, qualities, has_audio, quality}] or null
    playlistQuality: null,       // selected playlist-level quality string or null
    playlistExpanded: false,     // whether per-video list is expanded
    syncEnabled: false,          // sync mode toggle
  }
}

function makeFileItem(filename) {
  return {
    id: nextItemId++,
    kind: 'file',
    filename,
    uploadProgress: 0,
    uploadStatus: 'uploading',
    uploadLocator: null,
    uploadError: null,
    // Fields present for submission-logic compatibility
    locator: '',
    quality: 'best',
    probeState: 'idle',
    probeQualities: null,
    probeHasAudio: false,
    playlistProbeState: 'idle',
    playlistInfo: null,
    playlistItems: null,
    playlistQuality: null,
    playlistExpanded: false,
    syncEnabled: false,
    error: null,
    status: 'idle',
  }
}

function applyPlaylistQuality(newQ, currentItems) {
  if (newQ === 'best') {
    return currentItems.map(item => ({ ...item, quality: 'best' }))
  }
  if (newQ === 'audio') {
    return currentItems.map(item => {
      if (item.has_audio) return { ...item, quality: 'audio' }
      // No audio track — same conflict rule as unsupported height:
      // keep a prior selection if one exists, otherwise leave null (blocks archive).
      if (item.quality !== null) return item
      return { ...item, quality: null }
    })
  }
  return currentItems.map(item => {
    // Exact match only: the video must list this quality in its available formats.
    // maxHeight >= newHeight would be a cap (yt-dlp silent fallback), not what
    // the user selected; a video with [2160p, 1080p] does NOT support 1440p.
    if (item.qualities.includes(newQ)) {
      return { ...item, quality: newQ }
    }
    // Quality not available for this item — keep prior selection if one exists,
    // otherwise null (conflict, blocks archive until user picks manually).
    if (item.quality !== null) return item
    return { ...item, quality: null }
  })
}

function hasConflict(item) {
  return Array.isArray(item.playlistItems) && item.playlistItems.some(pi => pi.quality === null)
}

export default function CaptureDialog({ open, archiveId, onClose, onCaptured, onToast, onJobStarted, onJobSettled, activeJobs = [] }) {
  const dialogRef = useRef(null)
  const isFirstRenderRef = useRef(true)
  // jobUid → intervalId; survives dialog close since component stays mounted
  const pollIntervals = useRef(new Map())
  // itemId → debounce timeoutId for probe calls
  const probeTimers = useRef(new Map())
  // batchId → { total, archived, warnings, failed }; only populated for multi-URL submits
  const batchRef = useRef(new Map())
  // stable ref so probe callbacks always see the current archiveId
  const archiveIdRef = useRef(archiveId)
  useEffect(() => { archiveIdRef.current = archiveId }, [archiveId])
  const fileInputRef = useRef(null)
  const [dragOver, setDragOver] = useState(false)
  // Mirror of items state kept in a ref so the native 'close' event handler
  // can read the latest items without going stale. Updated every render.
  const itemsRef = useRef([])
  // Set to true in handleArchive before dialog.close() so the 'close' handler
  // skips staged-file cleanup (the capture job will clean those up on success).
  const isSubmittingRef = useRef(false)
  // item.id → abort() function for in-flight XHR uploads
  const uploadXhrs = useRef(new Map())

  // Stable refs so polling callbacks always use the latest prop values
  const onCapturedRef = useRef(onCaptured)
  const onToastRef = useRef(onToast)
  useEffect(() => { onCapturedRef.current = onCaptured }, [onCaptured])
  useEffect(() => { onToastRef.current = onToast }, [onToast])

  const onJobStartedRef = useRef(onJobStarted)
  const onJobSettledRef = useRef(onJobSettled)
  useEffect(() => { onJobStartedRef.current = onJobStarted }, [onJobStarted])
  useEffect(() => { onJobSettledRef.current = onJobSettled }, [onJobSettled])

  const [items, setItems] = useState(() => {
    try {
      const saved = JSON.parse(sessionStorage.getItem('captureItems') || 'null')
      if (Array.isArray(saved) && saved.length > 0) {
        const idle = saved.filter(it => !it.status || it.status === 'idle')
        if (idle.length > 0) {
          idle.forEach(it => { if (it.id >= nextItemId) nextItemId = it.id + 1 })
          // Merge with makeItem() defaults so items saved before the playlist
          // fields were added don't have undefined where null/false is expected.
          return idle.map(it => ({ ...makeItem(it.locator), ...it }))
        }
      }
    } catch {}
    return [makeItem()]
  })

  // Persist items to sessionStorage on every change
  useEffect(() => {
    sessionStorage.setItem('captureItems', JSON.stringify(items.filter(it => it.kind !== 'file')))
  }, [items])
  // Keep itemsRef in sync so the 'close' handler always sees current items.
  useEffect(() => { itemsRef.current = items }, [items])

  // Advanced options panel state
  const [advancedOpen, setAdvancedOpen] = useState(false)
  // null = use server default; true/false = per-session override
  const [ublockOverride, setUblockOverride] = useState(null)
  // Server-side global settings (loaded on mount, null until loaded)
  const [globalSettings, setGlobalSettings] = useState(null)
  // Cookie consent: session-level only, initialized from server default
  const [cookieExtEnabled, setCookieExtEnabled] = useState(true)
  const [modalCloserEnabled, setModalCloserEnabled] = useState(true)
  const [freediumEnabled, setFreediumEnabled] = useState(true)

  // Load global settings from server once on mount
  useEffect(() => {
    getInstanceSettings()
      .then(s => {
        setGlobalSettings(s)
        setCookieExtEnabled(s.cookie_ext_enabled ?? true)
        setModalCloserEnabled(s.modal_closer_enabled ?? true)
      })
      .catch(() => setGlobalSettings({}))
  }, [])

  // Effective uBlock for this session
  const ublockEnabled = ublockOverride !== null ? ublockOverride : (globalSettings?.ublock_enabled ?? true)

  // Reader mode: off by default, per-session only
  const [readerMode, setReaderMode] = useState(false)

  // On mount: clean up old single-locator sessionStorage keys; reconnect running bg jobs
  useEffect(() => {
    ;['captureDialogLocator','captureDialogError','captureDialogBusy',
      'captureDialogJobStatus','captureDialogJobUid'].forEach(k => sessionStorage.removeItem(k))

    // Reconnect polling for any bg jobs still running from a previous session.
    // activeJobs is read from the initial prop value (App's sessionStorage-seeded state).
    activeJobs.forEach(job => {
      if (job.jobUid && !pollIntervals.current.has(job.jobUid)) {
        startPolling(job.id, job.jobUid, job.locator, job.archiveId, null)
      }
    })
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  // Handle native dialog 'close' event (Escape key or programmatic .close())
  useEffect(() => {
    const dialog = dialogRef.current
    if (!dialog) return
    const handler = () => {
      probeTimers.current.forEach(id => clearTimeout(id))
      probeTimers.current.clear()
      // On cancel (Escape / Cancel button) clean up any staged upload files.
      // Guard against the Archive path: handleArchive sets isSubmittingRef true
      // before dialog.close() so this block is skipped when archiving — the
      // capture job handles staged-file cleanup on success instead.
      if (!isSubmittingRef.current) {
        const aid = archiveIdRef.current
        itemsRef.current.forEach(it => {
          if (it.kind !== 'file') return
          if (it.uploadStatus === 'uploading') {
            // Abort the XHR — 'Upload cancelled' rejection is swallowed in
            // handleFiles so no spurious error row appears after close.
            uploadXhrs.current.get(it.id)?.()
            uploadXhrs.current.delete(it.id)
          } else if (it.uploadStatus === 'done' && it.uploadLocator) {
            deleteUpload(aid, it.uploadLocator).catch(() => {})
          }
        })
      }
      isSubmittingRef.current = false
      onClose()
    }
    dialog.addEventListener('close', handler)
    return () => dialog.removeEventListener('close', handler)
  }, [onClose])

  // Open/close driven by parent; always reset form on reopen
  useEffect(() => {
    const dialog = dialogRef.current
    if (!dialog) return
    if (open) {
      if (!isFirstRenderRef.current) {
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

  function startPolling(bgJobId, jobUid, locator, aid, batchId = null) {
    if (pollIntervals.current.has(jobUid)) return
    const intervalId = setInterval(async () => {
      try {
        const updated = await pollCaptureJob(aid, jobUid)
        if (updated.status === 'completed') {
          clearInterval(pollIntervals.current.get(jobUid))
          pollIntervals.current.delete(jobUid)
          await Promise.resolve(onCapturedRef.current?.())
          onJobSettledRef.current?.(bgJobId)
          try {
            const notes = updated.notes_json ? JSON.parse(updated.notes_json) : null
            if (notes?.ublock_skipped || notes?.cookie_ext_skipped) {
              const both = notes.ublock_skipped && notes.cookie_ext_skipped
              const msg = both
                ? 'Captured without ad-blocking or cookie-consent extension. Check ARCHIVR_UBLOCK_EXT and ARCHIVR_COOKIE_EXT in your server config.'
                : notes.ublock_skipped
                  ? 'Captured without ad-blocking. ARCHIVR_UBLOCK=true but ARCHIVR_UBLOCK_EXT is not set or the path is invalid.'
                  : 'Captured without cookie-consent extension. ARCHIVR_COOKIE_EXT is not set or the path is invalid.'
              onToastRef.current(msg, locator, 'warning')
              settleBatch(batchId, 'warning', locator)
            } else {
              if (!batchId) onToastRef.current(null, locator, 'success')
              settleBatch(batchId, 'archived')
            }
          } catch {
            if (!batchId) onToastRef.current(null, locator, 'success')
            settleBatch(batchId, 'archived')
          }
        } else if (updated.status === 'failed') {
          clearInterval(pollIntervals.current.get(jobUid))
          pollIntervals.current.delete(jobUid)
          onJobSettledRef.current?.(bgJobId)
          const errText = updated.error_text || 'Capture failed.'
          onToastRef.current(errText, locator)
          settleBatch(batchId, 'failed', locator)
        }
        // 'pending' / 'running': keep polling
      } catch (e) {
        clearInterval(pollIntervals.current.get(jobUid))
        pollIntervals.current.delete(jobUid)
        onJobSettledRef.current?.(bgJobId)
        const msg = e.message || 'Network error'
        onToastRef.current(msg, locator)
        settleBatch(batchId, 'failed', locator)
      }
    }, 500)
    pollIntervals.current.set(jobUid, intervalId)
  }

  // Increments the batch counter for the given outcome and emits a summary
  // toast once all jobs in the batch have settled.
  // 'warning' counts as archived (succeeded with caveats); locator recorded for detail text.
  function settleBatch(batchId, outcome, locator = null) {
    if (!batchId) return
    const batch = batchRef.current.get(batchId)
    if (!batch) return
    if (outcome === 'archived' || outcome === 'warning') {
      batch.archived++
      if (outcome === 'warning') { batch.warnings++; if (locator) batch.warningLocators.push(locator) }
    } else {
      batch.failed++
      if (locator) batch.failedLocators.push(locator)
    }
    if (batch.archived + batch.failed < batch.total) return
    // All settled — build headline + detail text, then emit summary and clean up.
    batchRef.current.delete(batchId)
    const { archived, warnings, failed, failedLocators, warningLocators } = batch
    let headline
    if (archived === 0) {
      headline = `${failed} failed`
    } else {
      const archivedStr = warnings > 0
        ? `${archived} archived (${warnings} with warnings)`
        : `${archived} archived`
      headline = failed > 0 ? `${archivedStr}, ${failed} failed` : archivedStr
    }
    const type = archived === 0 ? 'error' : (failed > 0 || warnings > 0) ? 'warning' : 'success'
    const parts = []
    if (failedLocators.length > 0) parts.push(`Failed:\n${failedLocators.map(l => `  ${l}`).join('\n')}`)
    if (warningLocators.length > 0) parts.push(`With warnings:\n${warningLocators.map(l => `  ${l}`).join('\n')}`)
    const text = parts.length > 0 ? parts.join('\n') : null
    onToastRef.current(text, null, type, headline)
  }

  async function submitBgJob(locator, quality, batchId, extraExtensions = {}) {
    const aid = archiveIdRef.current
    const id = crypto.randomUUID?.() ?? `job-${Date.now()}-${Math.random()}`
    // Capture session options at call time (synchronous — before first await)
    const extensions = {
      ublock_enabled: ublockEnabled,
      reader_mode: readerMode,
      cookie_ext_enabled: cookieExtEnabled,
      modal_closer_enabled: modalCloserEnabled,
      via_freedium: freediumEnabled,
      ...extraExtensions,
    }
    try {
      const job = await submitCapture(aid, locator, quality, extensions)
      // Notify App to add skeleton + persist
      onJobStartedRef.current?.({ id, jobUid: job.job_uid, locator, archiveId: aid })
      startPolling(id, job.job_uid, locator, aid, batchId)
    } catch (e) {
      const msg = e.message || 'Submission failed.'
      onToastRef.current(msg, locator)
      settleBatch(batchId, 'failed', locator)
    }
  }

  function handleArchive() {
    // Guard against the Enter-key shortcut in CaptureRow bypassing the
    // disabled button — uploads must be complete before archiving starts.
    if (items.some(it => it.kind === 'file' && it.uploadStatus === 'uploading')) return
    const toSubmit = items.filter(it =>
      it.kind === 'file' ? (it.uploadStatus === 'done' && it.uploadLocator) : it.locator.trim()
    )
    if (toSubmit.length === 0) return
    if (toSubmit.some(it => it.kind !== 'file' && hasConflict(it))) return
    if (toSubmit.some(it => it.kind !== 'file' && (
        it.probeState === 'probing' ||
        (isPlaylistSource(it.locator) && it.playlistProbeState !== 'done'))))
      return
    if (toSubmit.some(it => it.kind !== 'file' && Array.isArray(it.playlistItems) && it.playlistItems.length === 0)) return
    const batchId = toSubmit.length > 1
      ? (crypto.randomUUID?.() ?? `batch-${Date.now()}`)
      : null
    if (batchId) {
      batchRef.current.set(batchId, { total: toSubmit.length, archived: 0, warnings: 0, failed: 0, failedLocators: [], warningLocators: [] })
    }
    // Capture all submission data before any state changes
    const submissions = toSubmit.map(it => {
      if (it.kind === 'file') {
        return { locator: it.uploadLocator, quality: 'best', extraExtensions: {} }
      }
      return {
        locator: it.locator.trim(),
        quality: it.playlistItems !== null ? null : (it.quality || 'best'),
        extraExtensions: it.playlistItems !== null
          ? { per_item_quality: Object.fromEntries(it.playlistItems.map(pi => [pi.id, pi.quality])), sync: it.syncEnabled }
          : {},
      }
    })
    // Reset form and close dialog immediately.
    // Set isSubmittingRef before close() — the native 'close' event fires
    // synchronously and the handler checks this flag to skip staged-file cleanup.
    isSubmittingRef.current = true
    setItems([makeItem()])
    dialogRef.current?.close()
    // Submit each in background
    submissions.forEach(({ locator, quality, extraExtensions }) =>
      submitBgJob(locator, quality, batchId, extraExtensions)
    )
  }

  function addRow() {
    setItems(prev => [...prev, makeItem()])
  }

  function removeRow(id) {
    clearTimeout(probeTimers.current.get(id))
    probeTimers.current.delete(id)
    const item = itemsRef.current.find(it => it.id === id)
    if (item?.kind === 'file') {
      if (item.uploadStatus === 'uploading') {
        // Abort the in-flight XHR; the server's partial-upload cleanup handles
        // any bytes already written to temp/uploads/.
        uploadXhrs.current.get(id)?.()
        uploadXhrs.current.delete(id)
      } else if (item.uploadStatus === 'done' && item.uploadLocator) {
        // Discard the fully staged file that was never submitted for capture.
        deleteUpload(archiveIdRef.current, item.uploadLocator).catch(() => {})
      }
    }
    setItems(prev => {
      const next = prev.filter(it => it.id !== id)
      return next.length === 0 ? [makeItem()] : next
    })
  }

  function updateLocator(id, val) {
    // Cancel any in-flight debounce and immediately clear stale probe results.
    // This prevents old qualities from being visible (and submittable) while
    // the debounce is pending for the new URL.
    clearTimeout(probeTimers.current.get(id))
    setItems(prev => prev.map(it =>
      it.id === id
        ? { ...it, locator: val, probeState: 'idle', probeQualities: null, probeHasAudio: false, quality: 'best',
            playlistProbeState: 'idle', playlistInfo: null, playlistItems: null, playlistQuality: null, playlistExpanded: false }
        : it
    ))

    if (isVideoSource(val)) {
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
    } else if (isPlaylistSource(val)) {
      // Schedule a playlist probe (playlists are slower — 800ms debounce)
      const timer = setTimeout(async () => {
        probeTimers.current.delete(id)
        setItems(prev => prev.map(it => it.id === id ? { ...it, playlistProbeState: 'probing' } : it))
        try {
          const result = await probePlaylist(archiveIdRef.current, val.trim())
          setItems(prev => prev.map(it => {
            if (it.id !== id || it.locator !== val) return it // stale — locator changed again
            return {
              ...it,
              playlistProbeState: 'done',
              playlistInfo: result,
              playlistItems: result.items.map(pi => ({ ...pi, quality: null })),
              playlistQuality: null,
            }
          }))
        } catch {
          setItems(prev => prev.map(it =>
            it.id === id ? { ...it, playlistProbeState: 'error' } : it
          ))
        }
      }, 800)
      probeTimers.current.set(id, timer)
    }
  }

  function updateQuality(id, val) {
    setItems(prev => prev.map(it => it.id === id ? { ...it, quality: val } : it))
  }

  function updatePlaylistQuality(id, q) {
    setItems(prev => prev.map(it => {
      if (it.id !== id) return it
      const newItems = applyPlaylistQuality(q, it.playlistItems)
      return { ...it, playlistQuality: q, playlistItems: newItems }
    }))
  }
  function updatePlaylistItemQuality(id, videoId, q) {
    setItems(prev => prev.map(it => {
      if (it.id !== id) return it
      return { ...it, playlistItems: it.playlistItems.map(pi => pi.id === videoId ? { ...pi, quality: q } : pi) }
    }))
  }
  function togglePlaylistExpanded(id) {
    setItems(prev => prev.map(it => it.id === id ? { ...it, playlistExpanded: !it.playlistExpanded } : it))
  }
  function updateSync(id, val) {
    setItems(prev => prev.map(it => it.id === id ? { ...it, syncEnabled: val } : it))
  }
  function deletePlaylistItem(itemId, videoId) {
    setItems(prev => prev.map(it =>
      it.id !== itemId ? it :
      { ...it, playlistItems: it.playlistItems.filter(pi => pi.id !== videoId) }
    ))
  }

  function handleFiles(fileList) {
    const files = Array.from(fileList)
    if (files.length === 0) return
    files.forEach(file => {
      const newItem = makeFileItem(file.name)
      setItems(prev => {
        // Replace a sole empty URL row with the file item; otherwise append
        if (prev.length === 1 && prev[0].kind !== 'file' && !prev[0].locator.trim()) {
          return [newItem]
        }
        return [...prev, newItem]
      })
      const aid = archiveIdRef.current
      const { promise, abort } = uploadFile(aid, file, progress => {
        setItems(prev => prev.map(it =>
          it.id === newItem.id ? { ...it, uploadProgress: progress } : it
        ))
      })
      uploadXhrs.current.set(newItem.id, abort)
      promise
        .then(result => {
          uploadXhrs.current.delete(newItem.id)
          setItems(prev => prev.map(it =>
            it.id === newItem.id
              ? { ...it, uploadStatus: 'done', uploadLocator: result.locator, uploadProgress: 100 }
              : it
          ))
        })
        .catch(err => {
          uploadXhrs.current.delete(newItem.id)
          // 'Upload cancelled' means we aborted deliberately (row removed / dialog
          // closed); skip the error-state update since the row is already gone.
          if (err.message === 'Upload cancelled') return
          setItems(prev => prev.map(it =>
            it.id === newItem.id
              ? { ...it, uploadStatus: 'error', uploadError: err.message }
              : it
          ))
        })
    })
  }
  function handleDragOver(e) {
    e.preventDefault()
    e.stopPropagation()
    if (e.dataTransfer.types.includes('Files')) setDragOver(true)
  }
  function handleDragLeave(e) {
    // Only clear when leaving the dialog itself, not a child element
    if (!e.currentTarget.contains(e.relatedTarget)) setDragOver(false)
  }
  function handleDrop(e) {
    e.preventDefault()
    e.stopPropagation()
    setDragOver(false)
    handleFiles(e.dataTransfer.files)
  }
  function handleFileInput(e) {
    handleFiles(e.target.files)
    e.target.value = ''
  }


  const anyUploading = items.some(it => it.kind === 'file' && it.uploadStatus === 'uploading')
  const pendingCount = items.filter(it =>
    it.kind === 'file' ? (it.uploadStatus === 'done' && it.uploadLocator) : it.locator.trim()
  ).length
  const anyConflict = items.some(it => it.kind !== 'file' && hasConflict(it))
  // True if any playlist row has had all its videos deleted — archive would be a no-op.
  const anyEmptyPlaylist = items.some(it =>
    it.kind !== 'file' && Array.isArray(it.playlistItems) && it.playlistItems.length === 0
  )
  const anyProbing = items.some(it =>
    it.kind !== 'file' && (
      it.probeState === 'probing' ||
      // For playlist sources block unless probe completed successfully:
      // idle = debounce not yet fired; probing = in flight; error = no quality data.
      (isPlaylistSource(it.locator) && it.playlistProbeState !== 'done')
    )
  )

  return (
    <dialog
      ref={dialogRef}
      className={`capture-dialog${dragOver ? ' capture-dialog--dragover' : ''}`}
      onDragOver={handleDragOver}
      onDragLeave={handleDragLeave}
      onDrop={handleDrop}
    >
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
            item.kind === 'file' ? (
              <CaptureFileRow
                key={item.id}
                item={item}
                onRemove={() => removeRow(item.id)}
              />
            ) : (
              <CaptureRow
                key={item.id}
                item={item}
                autoFocus={idx === items.length - 1}
                onLocatorChange={val => updateLocator(item.id, val)}
                onQualityChange={val => updateQuality(item.id, val)}
                onRemove={() => removeRow(item.id)}
                onSubmit={handleArchive}
                onPlaylistQualityChange={q => updatePlaylistQuality(item.id, q)}
                onPlaylistItemQualityChange={(vid, q) => updatePlaylistItemQuality(item.id, vid, q)}
                onPlaylistToggle={() => togglePlaylistExpanded(item.id)}
                onSyncChange={val => updateSync(item.id, val)}
                onPlaylistItemDelete={(vid) => deletePlaylistItem(item.id, vid)}
              />
            )
          ))}
        </div>

        {/* Hidden file input */}
        <input
          ref={fileInputRef}
          type="file"
          multiple
          hidden
          onChange={handleFileInput}
          aria-hidden="true"
          tabIndex={-1}
        />

        <div className="capture-add-row-group">
          <button type="button" className="capture-add-row" onClick={addRow}>
            <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
              <line x1="8" y1="2" x2="8" y2="14"/><line x1="2" y1="8" x2="14" y2="8"/>
            </svg>
            Add URL
          </button>
          <button type="button" className="capture-add-row capture-add-file" onClick={() => fileInputRef.current?.click()}>
            <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.75" strokeLinecap="round" strokeLinejoin="round">
              <rect x="3" y="1" width="10" height="14" rx="1.5"/>
              <line x1="5.5" y1="5.5" x2="10.5" y2="5.5"/>
              <line x1="5.5" y1="8" x2="10.5" y2="8"/>
              <line x1="5.5" y1="10.5" x2="8.5" y2="10.5"/>
            </svg>
            Upload file
          </button>
        </div>

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
              <label className="capture-ext-row" style={{ marginTop: 8 }}>
                <span className="capture-ext-label">
                  <span className="capture-ext-name">Close modals and dialogs</span>
                  <span className="capture-ext-desc">Auto-dismiss cookie banners and overlays during this capture</span>
                </span>
                <button
                  type="button"
                  role="switch"
                  aria-checked={modalCloserEnabled}
                  className={`ext-toggle ext-toggle--sm${modalCloserEnabled ? ' ext-toggle--on' : ''}`}
                  onClick={() => setModalCloserEnabled(v => !v)}
                  aria-label="Toggle modal closer for this capture"
                >
                  <span className="ext-toggle-knob" />
                </button>
              </label>
              <label className="capture-ext-row" style={{ marginTop: 8 }}>
                <span className="capture-ext-label">
                  <span className="capture-ext-name">Freedium mirror</span>
                  <span className="capture-ext-desc">Route paywalled articles through a Freedium mirror (Medium, NYT, WaPo, etc.)</span>
                </span>
                <button
                  type="button"
                  role="switch"
                  aria-checked={freediumEnabled}
                  className={`ext-toggle ext-toggle--sm${freediumEnabled ? ' ext-toggle--on' : ''}`}
                  onClick={() => setFreediumEnabled(v => !v)}
                  aria-label="Toggle Freedium mirror for this capture"
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
            disabled={pendingCount === 0 || anyConflict || anyProbing || anyEmptyPlaylist || anyUploading}
          >
            {pendingCount > 1 ? `Archive ${pendingCount}` : 'Archive'}
          </button>
          <button type="button" className="capture-cancel" onClick={() => dialogRef.current?.close()}>
            Cancel
          </button>
        </div>
      </div>
    </dialog>
  )
}

function CaptureRow({ item, autoFocus, onLocatorChange, onQualityChange, onRemove, onSubmit,
  onPlaylistQualityChange, onPlaylistItemQualityChange, onPlaylistToggle, onSyncChange, onPlaylistItemDelete }) {
  const inputRef = useRef(null)

  useEffect(() => {
    if (autoFocus) {
      inputRef.current?.focus()
    }
  }, [autoFocus]) // eslint-disable-line react-hooks/exhaustive-deps

  // Quality control shown right of the input
  const qualityEl = (() => {
    // Playlist source handling
    if (isPlaylistSource(item.locator)) {
      if (item.playlistProbeState === 'probing') {
        return <span className="capture-quality-probing" aria-label="Probing playlist…">…</span>
      }
      if (item.playlistProbeState === 'done') {
        const allHeights = [...new Set(
          item.playlistItems.flatMap(pi => pi.qualities.map(q => parseInt(q)))
        )].sort((a, b) => b - a)
        const allHaveAudio = item.playlistItems.every(pi => pi.has_audio)
        const conflictCount = item.playlistItems.filter(pi => pi.quality === null).length
        return (
          <>
            <select
              className="capture-quality"
              value={item.playlistQuality ?? ''}
              onChange={e => onPlaylistQualityChange(e.target.value)}
              aria-label="Playlist quality"
            >
              {!item.playlistQuality && <option value="" disabled>Select quality…</option>}
              <option value="best">Best quality</option>
              {allHeights.map(h => <option key={h} value={`${h}p`}>{h}p</option>)}
              {allHaveAudio && <option value="audio">Audio only</option>}
            </select>
            {conflictCount > 0 && (
              <span className="capture-conflict-badge">{conflictCount} need selection</span>
            )}
          </>
        )
      }
      if (item.playlistProbeState === 'error') {
        return (
          <span className="capture-quality-hint capture-quality-hint--error">
            Probe failed — edit URL to retry
          </span>
        )
      }
      return null
    }

    // Video source handling (unchanged)
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

  const syncToggle = isPlaylistSource(item.locator) && item.playlistProbeState === 'done' ? (
    <label className="capture-sync-row">
      <input type="checkbox" checked={item.syncEnabled} onChange={e => onSyncChange(e.target.checked)} />
      <span>Sync — skip already-archived videos</span>
    </label>
  ) : null

  return (
    <div className="capture-row">
      <div className="capture-row-main">
        {isPlaylistSource(item.locator) ? (
          item.playlistProbeState === 'done' ? (
            <button
              type="button"
              className="capture-playlist-toggle capture-playlist-toggle--left"
              onClick={onPlaylistToggle}
              aria-label={item.playlistExpanded ? 'Collapse video list' : 'Expand video list'}
              aria-expanded={item.playlistExpanded}
            >
              {item.playlistExpanded ? '▲' : '▼'}
            </button>
          ) : null
        ) : null}
        <input
          ref={inputRef}
          className="capture-input"
          type="text"
          placeholder="https://… · yt:ID · ytm:ID · tweet:ID · x:ID"
          value={item.locator}
          onChange={e => onLocatorChange(e.target.value)}
          onKeyDown={e => { if (e.key === 'Enter') onSubmit() }}
          autoComplete="off"
          spellCheck={false}
        />
        {qualityEl}
        <button type="button" className="capture-row-action capture-row-remove" onClick={onRemove} aria-label="Remove">
          <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
            <line x1="3" y1="3" x2="13" y2="13"/><line x1="13" y1="3" x2="3" y2="13"/>
          </svg>
        </button>
      </div>
      {item.error && (
        <p className="capture-row-error">{item.error}</p>
      )}
      {isPlaylistSource(item.locator) && item.playlistProbeState === 'done' && item.playlistExpanded ? (
        <div className="capture-playlist-items">
          {item.playlistItems.map(pi => (
            <div
              key={pi.id}
              className={`capture-playlist-item${pi.quality === null ? ' capture-playlist-item--conflict' : ''}`}
            >
              <span className="capture-playlist-item-title">{pi.title || pi.url}</span>
              <select
                className="capture-item-quality"
                value={pi.quality ?? ''}
                onChange={e => onPlaylistItemQualityChange(pi.id, e.target.value)}
                aria-label={`Quality for ${pi.title || pi.url}`}
              >
                {pi.quality === null && <option value="" disabled>Choose…</option>}
                <option value="best">Best quality</option>
                {pi.qualities.map(q => <option key={q} value={q}>{q}</option>)}
                {pi.has_audio && <option value="audio">Audio only</option>}
              </select>
              {pi.quality === null && (
                <span className="capture-playlist-conflict-badge">Choose quality</span>
              )}
              <button
                type="button"
                className="capture-playlist-item-remove"
                aria-label={`Remove ${pi.title || pi.url}`}
                onClick={() => onPlaylistItemDelete(pi.id)}
              >
                &times;
              </button>
            </div>
          ))}
          {syncToggle}
        </div>
      ) : syncToggle}
    </div>
  )
}
function CaptureFileRow({ item, onRemove }) {
  const uploading = item.uploadStatus === 'uploading'
  const done = item.uploadStatus === 'done'
  const errored = item.uploadStatus === 'error'

  return (
    <div className="capture-row">
      <div className="capture-row-main">
        <span className="capture-file-icon" aria-hidden="true">
          <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.75" strokeLinecap="round" strokeLinejoin="round" style={{ width: 14, height: 14 }}>
            <rect x="3" y="1" width="10" height="14" rx="1.5"/>
            <line x1="5.5" y1="5.5" x2="10.5" y2="5.5"/>
            <line x1="5.5" y1="8" x2="10.5" y2="8"/>
            <line x1="5.5" y1="10.5" x2="8.5" y2="10.5"/>
          </svg>
        </span>
        <span className={`capture-file-name${errored ? ' capture-file-name--error' : ''}`}>
          {item.filename}
        </span>
        {uploading && (
          <span className="capture-file-progress-wrap" aria-label={`Uploading ${item.uploadProgress}%`}>
            <span className="capture-file-progress-bar">
              <span
                className="capture-file-progress-fill"
                style={{ width: `${item.uploadProgress}%` }}
              />
            </span>
            <span className="capture-file-progress-pct">{item.uploadProgress}%</span>
          </span>
        )}
        {done && (
          <span className="capture-file-badge capture-file-badge--done" aria-label="Upload complete">
            <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round" style={{ width: 11, height: 11 }}>
              <polyline points="3 8.5 6.5 12 13 5"/>
            </svg>
          </span>
        )}
        {errored && (
          <span className="capture-file-badge capture-file-badge--error">!</span>
        )}
        <button
          type="button"
          className="capture-row-action capture-row-remove"
          onClick={onRemove}
          aria-label="Remove"
          disabled={uploading}
        >
          <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
            <line x1="3" y1="3" x2="13" y2="13"/><line x1="13" y1="3" x2="3" y2="13"/>
          </svg>
        </button>
      </div>
      {errored && (
        <p className="capture-row-error">{item.uploadError || 'Upload failed'}</p>
      )}
    </div>
  )
}
