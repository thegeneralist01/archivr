import { useState, useEffect, useLayoutEffect, useRef } from 'react'
import { fetchEntryTags, assignTag, removeTag, listEntryCollections, listCollections, addEntryToCollection, updateEntryTitle, deleteEntry, rearchiveEntry, pollCaptureJob, fetchEntrySummary, requestEntrySummary } from '../api'
import { formatTimestamp, formatBytes, valueText, sourceIconSvg, displayPath } from '../utils'

const VIS_LABEL = { 0: 'Private', 1: 'Public', 2: 'Users only', 3: 'Public' }

// Provider labels are display-only; the values are the provider_kind strings
// the server persists in entry_summaries.provider_kind.
const SUMMARY_PROVIDERS = [
  { value: 'anthropic_http', label: 'Anthropic API' },
  { value: 'openai_compatible', label: 'OpenAI-compatible API' },
  { value: 'claude_cli', label: 'Claude CLI' },
  { value: 'codex_cli', label: 'Codex CLI' },
]
const PROVIDER_LABEL = Object.fromEntries(SUMMARY_PROVIDERS.map(p => [p.value, p.label]))
const SUMMARY_PROVIDER_KEY = 'archivr:summary:provider'
const SUMMARY_POLL_MS = 1500
const UNSUPPORTED_SUMMARY_CONTENT_HEADING = 'This entry can’t be summarized yet.'
const UNSUPPORTED_SUMMARY_CONTENT_DETAIL = 'It doesn’t contain archived text that a summary provider can read. Summaries currently support text notes, web pages, X posts and threads, and X Articles. Video, audio, and image-only entries need a transcript or text source.'
const UNSUPPORTED_SUMMARY_CONTENT_MESSAGE = `${UNSUPPORTED_SUMMARY_CONTENT_HEADING}\n\n${UNSUPPORTED_SUMMARY_CONTENT_DETAIL}`

// Summaries are stored as the raw JSON string the model produced (normalized
// server-side to {tldr, summary, tags}). Parsing can still fail for rows written
// by an older prompt version, so fall back to showing the text as-is rather than
// hiding a summary the user can perfectly well read.
function parseSummaryText(text) {
  if (!text) return null
  try {
    const parsed = JSON.parse(text)
    if (parsed && typeof parsed === 'object') {
      return {
        tldr: typeof parsed.tldr === 'string' ? parsed.tldr : '',
        summary: typeof parsed.summary === 'string' ? parsed.summary : '',
        tags: Array.isArray(parsed.tags) ? parsed.tags.filter(t => typeof t === 'string') : [],
      }
    }
  } catch { /* not JSON — fall through */ }
  return { tldr: '', summary: text, tags: [] }
}


const ExternalIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <path d="M7 17 17 7M9 7h8v8"/>
  </svg>
)

export default function ContextRail({ archiveId, selectedEntry, selectedUids, selectedEntries, detail, onTagFilterSet, tagNodes, onTagsRefresh, onEntryTitleChange, onEntryDeleted, onBulkDeleted, humanizeTags, onDetailRefresh, onOpenPreview, onPlay, isPublicSession }) {
  const [tags, setTags] = useState([])
  const [assignInput, setAssignInput] = useState('')
  const [entryCollections, setEntryCollections] = useState([])
  const [assignError, setAssignError] = useState('')
  const selectSeqRef = useRef(0)
  const titleCancelRef = useRef(false)
  const [editingTitle, setEditingTitle] = useState(false)
  const [titleDraft, setTitleDraft] = useState('')
  const [rearchiveState, setRearchiveState] = useState('idle') // 'idle' | 'running' | 'done' | 'error'
  const [rearchiveError, setRearchiveError] = useState('')
  const rearchivePollRef = useRef(null)
  const [fontsOpen, setFontsOpen] = useState(false)
  useEffect(() => { setFontsOpen(false) }, [detail?.summary?.entry_uid])

  // ── Summary state ───────────────────────────────────────────────────────
  // `summary` mirrors the server row. It is seeded from detail.latest_summary so
  // the section renders immediately on selection, then kept fresh by polling
  // only while a job is non-terminal.
  const [summary, setSummary] = useState(null)
  const [summaryError, setSummaryError] = useState('')
  const [summaryBusy, setSummaryBusy] = useState(false)
  const [summaryProvider, setSummaryProvider] = useState(() => {
    try {
      return sessionStorage.getItem(SUMMARY_PROVIDER_KEY) || SUMMARY_PROVIDERS[0].value
    } catch { return SUMMARY_PROVIDERS[0].value }
  })
  const [includeSummaryImages, setIncludeSummaryImages] = useState(false)
  const summaryPollRef = useRef(null)
  const summaryPollAbortRef = useRef(null)
  const summaryGenerateAbortRef = useRef(null)
  const summarySelectionRef = useRef(null)

  // ── Bulk-panel state ────────────────────────────────────────────────────
  const isBulk = selectedUids?.size >= 2
  const [bulkTagInput, setBulkTagInput] = useState('')
  const [bulkTagState, setBulkTagState] = useState('idle') // 'idle'|'running'|'done'|'error'
  const [bulkTagError, setBulkTagError] = useState('')
  const [collections, setCollections] = useState([])
  const [bulkCollUid, setBulkCollUid] = useState('')
  const [bulkCollState, setBulkCollState] = useState('idle') // 'idle'|'running'|'done'|'error'
  const [bulkCollError, setBulkCollError] = useState('')
  const [bulkDeleteState, setBulkDeleteState] = useState('idle') // 'idle'|'running'
  const [singleCollUid, setSingleCollUid] = useState('')
  const [singleCollState, setSingleCollState] = useState('idle')
  const [singleCollError, setSingleCollError] = useState('')

  useEffect(() => {
    const seq = ++selectSeqRef.current
    if (rearchivePollRef.current) { clearInterval(rearchivePollRef.current); rearchivePollRef.current = null }
    setRearchiveState('idle')
    setRearchiveError('')
    if (!selectedEntry || !archiveId) {
      setTags([])
      setEntryCollections([])
      return
    }
    // Skip auth-required tag/collection fetches for public guests.
    if (isPublicSession) {
      setTags([])
      setEntryCollections([])
      return
    }
    setEditingTitle(false)
    setTitleDraft('')
    titleCancelRef.current = false
    setTags([])
    Promise.all([
      fetchEntryTags(archiveId, selectedEntry.entry_uid),
      listEntryCollections(archiveId, selectedEntry.entry_uid),
    ]).then(([tgs, ecs]) => {
      if (seq !== selectSeqRef.current) return
      setTags(tgs)
      setEntryCollections(ecs)
    }).catch(() => {})
  }, [selectedEntry, archiveId, isPublicSession])

  useEffect(() => {
    return () => {
      clearInterval(rearchivePollRef.current)
    }
  }, [])

  // Seed the summary from the entry detail payload and stop any poll left over
  // from the previously selected entry.
  useLayoutEffect(() => {
    const selectionKey = archiveId && detail?.summary?.entry_uid
      ? `${archiveId}:${detail.summary.entry_uid}`
      : null
    summarySelectionRef.current = selectionKey
    clearInterval(summaryPollRef.current)
    summaryPollRef.current = null
    summaryPollAbortRef.current?.abort()
    summaryPollAbortRef.current = null
    summaryGenerateAbortRef.current?.abort()
    summaryGenerateAbortRef.current = null
    setSummary(detail?.latest_summary ?? null)
    setSummaryError('')
    setSummaryBusy(false)
    setIncludeSummaryImages(false)
  }, [archiveId, detail?.summary?.entry_uid])

  // Poll only while the latest summary is non-terminal. Anchoring the effect on
  // the status (rather than starting a timer inside the click handler) means a
  // job still running when the user navigates away and back is picked up again.
  const summaryStatus = summary?.status
  useEffect(() => {
    clearInterval(summaryPollRef.current)
    summaryPollRef.current = null
    if (summaryStatus !== 'pending' && summaryStatus !== 'running') return
    if (!archiveId || !detail?.summary?.entry_uid) return
    const entryUid = detail.summary.entry_uid
    const selectionKey = `${archiveId}:${entryUid}`
    const controller = new AbortController()
    summaryPollAbortRef.current = controller
    const poll = async () => {
      try {
        const res = await fetchEntrySummary(archiveId, entryUid, { signal: controller.signal })
        if (controller.signal.aborted || summarySelectionRef.current !== selectionKey) return
        setSummary(res.summary ?? null)
        const st = res.summary?.status
        if (st !== 'pending' && st !== 'running') {
          clearInterval(intervalId)
          if (summaryPollRef.current === intervalId) summaryPollRef.current = null
          setSummaryBusy(false)
          if (st === 'completed') onDetailRefresh?.()
        }
      } catch (e) {
        if (controller.signal.aborted || summarySelectionRef.current !== selectionKey) return
        // A transient poll failure is not worth tearing the section down; the
        // next tick retries, and a real failure lands as status === 'failed'.
      }
    }
    const intervalId = setInterval(poll, SUMMARY_POLL_MS)
    summaryPollRef.current = intervalId
    return () => {
      clearInterval(intervalId)
      if (summaryPollRef.current === intervalId) summaryPollRef.current = null
      controller.abort()
      if (summaryPollAbortRef.current === controller) summaryPollAbortRef.current = null
    }
  }, [summaryStatus, archiveId, detail?.summary?.entry_uid])

  useEffect(() => () => {
    clearInterval(summaryPollRef.current)
    summaryPollAbortRef.current?.abort()
    summaryGenerateAbortRef.current?.abort()
  }, [])

  async function handleGenerateSummary(force = false) {
    if (!archiveId || !detail?.summary?.entry_uid || summaryBusy) return
    const entryUid = detail.summary.entry_uid
    const selectionKey = `${archiveId}:${entryUid}`
    const controller = new AbortController()
    summaryGenerateAbortRef.current?.abort()
    summaryGenerateAbortRef.current = controller
    setSummaryBusy(true)
    setSummaryError('')
    try {
      const res = await requestEntrySummary(archiveId, entryUid, {
        provider: summaryProvider,
        force,
        includeImages: includeSummaryImages,
        signal: controller.signal,
      })
      if (controller.signal.aborted || summarySelectionRef.current !== selectionKey) return
      if (res.status === 'completed') {
        // 200 cache hit: the response *is* the row, no polling needed.
        setSummary(res)
        setSummaryBusy(false)
        onDetailRefresh?.()
      } else {
        // 202: seed a local pending row so the poll effect starts immediately
        // rather than waiting a tick for the first GET.
        setSummary({ ...(res ?? {}), status: 'pending' })
      }
    } catch (e) {
      if (controller.signal.aborted || summarySelectionRef.current !== selectionKey) return
      setSummaryError(e.message || 'Summary request failed')
      setSummaryBusy(false)
    } finally {
      if (summaryGenerateAbortRef.current === controller) summaryGenerateAbortRef.current = null
    }
  }

  function handleProviderChange(value) {
    setSummaryProvider(value)
    if (value === 'claude_cli') setIncludeSummaryImages(false)
    try { sessionStorage.setItem(SUMMARY_PROVIDER_KEY, value) } catch { /* private mode */ }
  }

  // Fetch available collections whenever archiveId is available
  useEffect(() => {
    if (!archiveId) { setCollections([]); return }
    listCollections(archiveId).then(setCollections).catch(() => setCollections([]))
  }, [archiveId])

  // Reset transient bulk state when selection changes
  useEffect(() => {
    setBulkTagInput('')
    setBulkTagState('idle')
    setBulkTagError('')
    setBulkCollUid('')
    setBulkCollState('idle')
    setBulkCollError('')
    setBulkDeleteState('idle')
    setSingleCollUid('')
    setSingleCollState('idle')
    setSingleCollError('')
  }, [selectedUids])

  async function handleBulkDelete() {
    const n = selectedUids.size
    if (!window.confirm(`Delete ${n} entr${n === 1 ? 'y' : 'ies'}? This cannot be undone.`)) return
    setBulkDeleteState('running')
    const deletedUids = new Set()
    for (const uid of selectedUids) {
      try {
        await deleteEntry(archiveId, uid)
        deletedUids.add(uid)
      } catch {
        // partial failure — skip and continue
      }
    }
    setBulkDeleteState('idle')
    onBulkDeleted?.(deletedUids)
  }

  async function handleBulkTag() {
    const path = bulkTagInput.trim()
    if (!path) return
    setBulkTagState('running')
    setBulkTagError('')
    try {
      for (const uid of selectedUids) {
        await assignTag(archiveId, uid, path)
      }
      setBulkTagInput('')
      setBulkTagState('done')
      onTagsRefresh?.()
      setTimeout(() => setBulkTagState('idle'), 1800)
    } catch (err) {
      setBulkTagError(err.message)
      setBulkTagState('error')
    }
  }

  async function handleBulkAddToCollection() {
    if (!bulkCollUid) return
    setBulkCollState('running')
    setBulkCollError('')
    const failed = []
    const coll = collections.find(c => c.collection_uid === bulkCollUid)
    for (const uid of selectedUids) {
      try {
        await addEntryToCollection(archiveId, bulkCollUid, uid, coll?.default_visibility_bits ?? 2)
      } catch (err) {
        failed.push(uid)
      }
    }
    if (failed.length > 0) {
      setBulkCollError(`Failed for ${failed.length} entr${failed.length === 1 ? 'y' : 'ies'}.`)
      setBulkCollState('error')
    } else {
      setBulkCollState('done')
      setTimeout(() => setBulkCollState('idle'), 1800)
    }
  }

  async function handleSingleAddToCollection() {
    if (!singleCollUid || !selectedEntry) return
    setSingleCollState('running')
    setSingleCollError('')
    const coll = collections.find(c => c.collection_uid === singleCollUid)
    try {
      await addEntryToCollection(archiveId, singleCollUid, selectedEntry.entry_uid, coll?.default_visibility_bits ?? 2)
      setSingleCollState('done')
      setSingleCollUid('')
      // Refresh collection membership list
      const updated = await listEntryCollections(archiveId, selectedEntry.entry_uid)
      setEntryCollections(updated)
      setTimeout(() => setSingleCollState('idle'), 1800)
    } catch (err) {
      setSingleCollError(err.message)
      setSingleCollState('error')
    }
  }

  async function handleTitleSave() {
    const newTitle = titleDraft.trim() || null
    try {
      await updateEntryTitle(archiveId, selectedEntry.entry_uid, newTitle)
      onEntryTitleChange?.(selectedEntry.entry_uid, newTitle)
    } catch {
      // silently revert
    } finally {
      setEditingTitle(false)
    }
  }

  async function handleAssignTag() {
    const path = assignInput.trim()
    if (!path || !selectedEntry) return
    try {
      await assignTag(archiveId, selectedEntry.entry_uid, path)
      setAssignInput('')
      setAssignError('')
      const updated = await fetchEntryTags(archiveId, selectedEntry.entry_uid)
      setTags(updated)
      onTagsRefresh()
    } catch (e) {
      setAssignError(e.message)
    }
  }

  async function handleRemoveTag(tagUid) {
    try {
      await removeTag(archiveId, selectedEntry.entry_uid, tagUid)
      const updated = await fetchEntryTags(archiveId, selectedEntry.entry_uid)
      setTags(updated)
      onTagsRefresh()
    } catch {
      // silently ignore
    }
  }

  async function handleDeleteEntry() {
    if (!selectedEntry || !archiveId) return
    if (!window.confirm('Delete this entry? This cannot be undone.')) return
    try {
      await deleteEntry(archiveId, selectedEntry.entry_uid)
      onEntryDeleted?.(selectedEntry.entry_uid)
    } catch {
      // silently ignore — entry stays selected if delete failed
    }
  }

  async function handleRearchive() {
    if (!selectedEntry || !archiveId || rearchiveState === 'running') return
    // Capture identity at start so closure comparisons are stable
    const startSeq = selectSeqRef.current
    const entryUid = selectedEntry.entry_uid
    setRearchiveState('running')
    setRearchiveError('')
    try {
      const { job_uid } = await rearchiveEntry(archiveId, entryUid)
      // If selection changed while waiting for the kick-off response, bail.
      if (selectSeqRef.current !== startSeq) return
      rearchivePollRef.current = setInterval(async () => {
        try {
          const job = await pollCaptureJob(archiveId, job_uid)
          if (job.status === 'completed') {
            clearInterval(rearchivePollRef.current)
            rearchivePollRef.current = null
            if (selectSeqRef.current !== startSeq) return
            setRearchiveState('done')
            const updated = await fetchEntryTags(archiveId, entryUid)
            if (selectSeqRef.current !== startSeq) return
            setTags(updated)
            onDetailRefresh?.()
          } else if (job.status === 'failed') {
            clearInterval(rearchivePollRef.current)
            rearchivePollRef.current = null
            if (selectSeqRef.current !== startSeq) return
            setRearchiveState('error')
            setRearchiveError(job.error_text || 'Re-archive failed.')
          }
        } catch {
          clearInterval(rearchivePollRef.current)
          rearchivePollRef.current = null
          if (selectSeqRef.current !== startSeq) return
          setRearchiveState('error')
          setRearchiveError('Network error while polling.')
        }
      }, 500)
    } catch (e) {
      if (selectSeqRef.current !== startSeq) return
      setRearchiveState('error')
      setRearchiveError(e.message || 'Failed to start re-archive.')
    }
  }

  const metaRows = detail ? [
    ['Added',      formatTimestamp(detail.summary.archived_at)],
    ['Source',     detail.summary.source_kind],
    ['Type',       detail.summary.entity_kind],
    ['Visibility', VIS_LABEL[detail.summary.visibility] ?? detail.summary.visibility],
    ['Root',       detail.structured_root_relpath],
  ] : []

  const AUDIO_EXTS = new Set(['mp3','ogg','m4a','opus','wav','flac','aac'])
  const PREVIEW_EXTS = new Set(['mp4','webm','mov','mkv','avi','m4v','ogv','pdf','html','htm','md','markdown','txt','jpg','jpeg','png','gif','webp','avif','svg','bmp'])
  const primaryMediaIdx = detail ? detail.artifacts.findIndex(a => a.artifact_role === 'primary_media') : -1
  const primaryMedia = primaryMediaIdx >= 0 ? detail.artifacts[primaryMediaIdx] : null
  const pmExt = primaryMedia ? primaryMedia.relpath.split('.').pop().toLowerCase() : ''
  const isAudio = primaryMedia && AUDIO_EXTS.has(pmExt)
  const primaryMediaUrl = (primaryMediaIdx >= 0 && selectedEntry)
    ? `/api/archives/${archiveId}/entries/${selectedEntry.entry_uid}/artifacts/${primaryMediaIdx}`
    : null
  const isPreviewable = detail && !isAudio && (
    (detail.summary.entity_kind === 'tweet' || detail.summary.entity_kind === 'tweet_thread') ||
    (primaryMedia && PREVIEW_EXTS.has(pmExt))
  )

  return (
    <aside className="context-rail">
      <div className="rail-eyebrow">Context</div>

      {isBulk ? (
        isPublicSession ? (
          <p className="bulk-count">
            <span className="bulk-count-num">{selectedUids.size}</span>
            {' entries selected'}
          </p>
        ) : (
        <div className="bulk-panel">
          <p className="bulk-count">
            <span className="bulk-count-num">{selectedUids.size}</span>
            {' entries selected'}
          </p>

          <div className="rail-section">
            <div className="rail-section-heading">Assign tag</div>
            {bulkTagError && (
              <p className="form-msg form-msg--err" style={{ margin: '0 0 8px' }}>{bulkTagError}</p>
            )}
            <div className="tag-input-wrap">
              <span className="hash">/</span>
              <input
                className="tag-input"
                type="text"
                placeholder="science/cs"
                autoComplete="off"
                value={bulkTagInput}
                onChange={e => setBulkTagInput(e.target.value)}
                onKeyDown={e => { if (e.key === 'Enter') handleBulkTag() }}
              />
              <button
                className="tag-add-btn"
                onClick={handleBulkTag}
                disabled={bulkTagState === 'running' || !bulkTagInput.trim()}
              >
                {bulkTagState === 'running' ? '…' : bulkTagState === 'done' ? '✓' : 'Add'}
              </button>
            </div>
          </div>

          {collections.length > 0 && (
            <div className="rail-section">
              <div className="rail-section-heading">Add to collection</div>
              <div className="bulk-coll-row">
                <select
                  className="bulk-coll-select"
                  value={bulkCollUid}
                  onChange={e => setBulkCollUid(e.target.value)}
                >
                  <option value="">Pick a collection…</option>
                  {collections.filter(c => c.slug !== '_default_').map(c => (
                    <option key={c.collection_uid} value={c.collection_uid}>{c.name}</option>
                  ))}
                </select>
                <button
                  className="tag-add-btn"
                  onClick={handleBulkAddToCollection}
                  disabled={!bulkCollUid || bulkCollState === 'running'}
                >
                  {bulkCollState === 'running' ? '…' : bulkCollState === 'done' ? '✓' : bulkCollState === 'error' ? '!' : 'Add'}
                </button>
              </div>
              {bulkCollError && (
                <p className="form-msg form-msg--err" style={{ margin: '6px 0 0' }}>{bulkCollError}</p>
              )}
            </div>
          )}

          <div className="rail-delete-zone">
            <button
              className="rail-delete-btn"
              onClick={handleBulkDelete}
              disabled={bulkDeleteState === 'running'}
            >
              {bulkDeleteState === 'running'
                ? 'Deleting\u2026'
                : `Delete ${selectedUids.size} entr${selectedUids.size === 1 ? 'y' : 'ies'}`}
            </button>
          </div>
        </div>
        )
      ) : !selectedEntry ? (
        <p className="tags-empty">Select an entry.</p>
      ) : !detail ? (
        <p className="tags-empty">Loading\u2026</p>
      ) : (
        <>
          {isPublicSession ? (
            <h2 className="rail-title">
              {valueText(detail.summary.title) || valueText(detail.summary.entry_uid)}
            </h2>
          ) : editingTitle ? (
            <input
              className="rail-title-input"
              autoFocus
              value={titleDraft}
              onChange={e => setTitleDraft(e.target.value)}
              onKeyDown={e => {
                if (e.key === 'Enter') e.currentTarget.blur()
                if (e.key === 'Escape') { titleCancelRef.current = true; e.currentTarget.blur() }
              }}
              onBlur={() => { if (titleCancelRef.current) { setEditingTitle(false) } else { handleTitleSave() } titleCancelRef.current = false }}
            />
          ) : (
            <h2
              className="rail-title rail-title--editable"
              title="Click to rename"
              onClick={() => {
                setTitleDraft(detail.summary.title ?? '')
                setEditingTitle(true)
              }}
            >
              {valueText(detail.summary.title) || valueText(detail.summary.entry_uid)}
              <svg className="edit-icon" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <path d="M11.5 2.5a1.5 1.5 0 0 1 2 2L5 13l-3 1 1-3 8.5-8.5z"/>
              </svg>
            </h2>
          )}

          {detail.summary.original_url && (
            <a
              className="url-tile"
              href={detail.summary.original_url}
              target="_blank"
              rel="noopener noreferrer"
            >
              <span className="ico" dangerouslySetInnerHTML={{ __html: sourceIconSvg(detail.summary.source_kind) }} />
              <span className="u-text">{detail.summary.original_url}</span>
              <span className="ext"><ExternalIcon /></span>
            </a>
          )}

          {isAudio && onPlay && (
            <button className="rail-preview-btn" onClick={() => onPlay(primaryMediaUrl, selectedEntry)}>
              ▶ Play
            </button>
          )}
          {isPreviewable && onOpenPreview && (
            <button className="rail-preview-btn" onClick={onOpenPreview}>
              Preview
            </button>
          )}

          {(() => {
            // Public sessions get read-only treatment: the completed text if the
            // server's visibility gate let the detail through at all, and never
            // the provider selector or Generate button.
            const parsed = summary?.status === 'completed'
              ? parseSummaryText(summary.summary_text)
              : null
            const running = summary?.status === 'pending' || summary?.status === 'running'
            const unsupportedContent =
              (summary?.status === 'failed' && summary.error_text === UNSUPPORTED_SUMMARY_CONTENT_MESSAGE) ||
              summaryError === UNSUPPORTED_SUMMARY_CONTENT_MESSAGE
            if (isPublicSession && !parsed) return null
            return (
              <div className="rail-section rail-summary">
                <div className="rail-section-heading">Summary</div>

                {parsed && (
                  <div className="rail-summary-body">
                    {parsed.tldr && <p className="rail-summary-tldr">{parsed.tldr}</p>}
                    {parsed.summary && <p className="rail-summary-text">{parsed.summary}</p>}
                    {parsed.tags.length > 0 && (
                      <div className="rail-summary-tags">
                        {parsed.tags.map(t => (
                          <span key={t} className="rail-summary-tag">{t}</span>
                        ))}
                      </div>
                    )}
                    <p className="rail-summary-provider">
                      {PROVIDER_LABEL[summary.provider_kind] || summary.provider_kind}
                      {summary.provider_model ? ` \u00b7 ${summary.provider_model}` : ''}
                    </p>
                  </div>
                )}

                {running && (
                  <p className="rail-summary-status">
                    <span className="rail-summary-spinner" aria-hidden="true" />
                    {'Generating\u2026'}
                  </p>
                )}

                {unsupportedContent && !isPublicSession && (
                  <div className="rail-summary-info" role="status">
                    <p className="rail-summary-info__heading">{UNSUPPORTED_SUMMARY_CONTENT_HEADING}</p>
                    <p className="rail-summary-info__detail">{UNSUPPORTED_SUMMARY_CONTENT_DETAIL}</p>
                  </div>
                )}
                {summary?.status === 'failed' && summary.error_text && !unsupportedContent && !isPublicSession && (
                  <p className="form-msg form-msg--err rail-summary-error">
                    {summary.error_text}
                  </p>
                )}
                {summaryError && !unsupportedContent && (
                  <p className="form-msg form-msg--err rail-summary-error">
                    {summaryError}
                  </p>
                )}

                {!isPublicSession && !running && (
                  <div className="rail-summary-controls">
                    <select
                      className="rail-summary-select"
                      value={summaryProvider}
                      onChange={e => handleProviderChange(e.target.value)}
                      aria-label="Summary provider"
                    >
                      {SUMMARY_PROVIDERS.map(p => (
                        <option key={p.value} value={p.value}>{p.label}</option>
                      ))}
                    </select>
                    <div className={`rail-summary-image-option${summaryProvider === 'claude_cli' ? ' rail-summary-image-option--disabled' : ''}`}>
                      <label className="rail-summary-image-option__label">
                        <input
                          type="checkbox"
                          checked={includeSummaryImages}
                          disabled={summaryProvider === 'claude_cli'}
                          onChange={e => setIncludeSummaryImages(e.target.checked)}
                        />
                        Include attached images
                      </label>
                      <p className="rail-summary-image-option__note">
                        {summaryProvider === 'claude_cli'
                          ? 'Claude CLI cannot attach local images. Choose an HTTP provider or Codex CLI.'
                          : 'Selected archived images are sent to the chosen provider. Up to 4 supported images (5 MiB each, 12 MiB total) can be attached; unsupported or oversized artifacts are skipped.'}
                      </p>
                    </div>
                    <button
                      className="rail-rearchive-btn"
                      onClick={() => handleGenerateSummary(!!parsed)}
                      disabled={summaryBusy}
                    >
                      {summaryBusy ? '\u2026' : parsed ? 'Regenerate' : 'Generate'}
                    </button>
                  </div>
                )}
              </div>
            )
          })()}

          <div className="meta-list">
            {metaRows.filter(([, v]) => v != null && v !== '').map(([label, value]) => (
              <div key={label} className="meta-item">
                <span className="meta-k">{label}</span>
                <span className={`meta-v${label === 'Root' ? ' mono' : ''}`}>{valueText(value)}</span>
              </div>
            ))}
          </div>

          {detail.artifacts.length > 0 && (() => {
            const indexed = detail.artifacts.map((a, i) => ({ ...a, _idx: i }))
            const fonts = indexed.filter(a => a.artifact_role === 'font')
            const others = indexed.filter(a => a.artifact_role !== 'font')
            const fontTotalBytes = fonts.reduce((s, a) => s + (a.byte_size || 0), 0)
            const entryUid = detail.summary.entry_uid
            const renderRow = (artifact) => (
              <li key={artifact._idx}>
                <a
                  href={`/api/archives/${archiveId}/entries/${entryUid}/artifacts/${artifact._idx}`}
                  target="_blank"
                  rel="noopener noreferrer"
                  className="artifact-link"
                >
                  <span className="artifact-name">
                    {artifact.artifact_role === 'font'
                      ? artifact.relpath.split('/').pop()
                      : artifact.artifact_role.replace(/_/g, ' ')}
                  </span>
                  <span className="artifact-size">
                    {artifact.byte_size != null ? formatBytes(artifact.byte_size) : '—'}
                  </span>
                </a>
              </li>
            )
            return (
              <div className="rail-section">
                <div className="rail-section-heading">
                  Artifacts <span className="num">{detail.artifacts.length}</span>
                </div>
                <ul className="artifact-list">
                  {others.map(renderRow)}
                  {fonts.length > 0 && (
                    <li className="artifact-group">
                      <button
                        type="button"
                        className="artifact-group-header artifact-link"
                        aria-expanded={fontsOpen}
                        onClick={() => setFontsOpen(o => !o)}
                      >
                        <span className="artifact-name">
                          <span aria-hidden="true" className={`artifact-group-chevron${fontsOpen ? ' open' : ''}`}>›</span>
                          {` fonts (${fonts.length})`}
                        </span>
                        <span className="artifact-size">{formatBytes(fontTotalBytes)}</span>
                      </button>
                      {fontsOpen && (
                        <ul className="artifact-list artifact-group-body">
                          {fonts.map(renderRow)}
                        </ul>
                      )}
                    </li>
                  )}
                </ul>
              </div>
            )
          })()}
        </>
      )}
      {selectedEntry && !isBulk && !isPublicSession && (
        <>
          <div className="rail-section">
            <div className="rail-section-heading">Tags</div>
            {tags.length === 0 ? (
              <p className="tags-empty">No tags yet.</p>
            ) : (
              <div className="tags-wrap">
                {tags.map(tag => (
                  <span key={tag.tag_uid} className="tag-pill" title={tag.full_path}>
                    {humanizeTags ? displayPath(tag.full_path) : tag.full_path}
                    <button
                      className="remove"
                      title={`Remove tag ${tag.full_path}`}
                      onClick={() => handleRemoveTag(tag.tag_uid)}
                    >×</button>
                  </span>
                ))}
              </div>
            )}
            {assignError && (
              <p className="form-msg form-msg--err" style={{ margin: '0 0 8px' }}>{assignError}</p>
            )}
            <div className="tag-input-wrap">
              <span className="hash">/</span>
              <input
                className="tag-input"
                type="text"
                placeholder="science/cs"
                autoComplete="off"
                value={assignInput}
                onChange={e => setAssignInput(e.target.value)}
                onKeyDown={e => { if (e.key === 'Enter') handleAssignTag() }}
              />
              <button className="tag-add-btn" onClick={handleAssignTag}>Add</button>
            </div>
          </div>

          {(entryCollections.length > 0 || collections.filter(c => c.slug !== '_default_').length > 0) && (
            <div className="rail-section">
              <div className="rail-section-heading">Collections</div>
              {entryCollections.map(c => (
                <div key={c.collection_uid} className="coll-row">
                  <span className="coll-name">{c.name}</span>
                  <span className="vis-badge">
                    {VIS_LABEL[c.visibility_bits] ?? `bits:${c.visibility_bits}`}
                  </span>
                </div>
              ))}
              {collections.filter(c => c.slug !== '_default_').length > 0 && (
                <div className="bulk-coll-row" style={{ marginTop: 8 }}>
                  <select
                    className="bulk-coll-select"
                    value={singleCollUid}
                    onChange={e => setSingleCollUid(e.target.value)}
                  >
                    <option value="">Add to collection…</option>
                    {collections.filter(c => c.slug !== '_default_').map(c => (
                      <option key={c.collection_uid} value={c.collection_uid}>{c.name}</option>
                    ))}
                  </select>
                  <button
                    className="tag-add-btn"
                    onClick={handleSingleAddToCollection}
                    disabled={!singleCollUid || singleCollState === 'running'}
                  >
                    {singleCollState === 'running' ? '…' : singleCollState === 'done' ? '✓' : singleCollState === 'error' ? '!' : 'Add'}
                  </button>
                </div>
              )}
              {singleCollError && (
                <p className="form-msg form-msg--err" style={{ margin: '4px 0 0' }}>{singleCollError}</p>
              )}
            </div>
          )}

          {detail && (detail.summary.entity_kind === 'tweet' || detail.summary.entity_kind === 'tweet_thread') && (
            <div className="rail-section">
              <div className="rail-section-heading">Actions</div>
              <button
                className="rail-rearchive-btn"
                onClick={handleRearchive}
                disabled={rearchiveState === 'running'}
              >
                {rearchiveState === 'running' ? 'Re-archiving\u2026' : 'Re-archive'}
              </button>
              {rearchiveState === 'done' && (
                <p className="form-msg form-msg--ok" style={{ marginTop: '6px' }}>Re-archived successfully.</p>
              )}
              {rearchiveState === 'error' && (
                <p className="form-msg form-msg--err" style={{ marginTop: '6px' }}>{rearchiveError}</p>
              )}
            </div>
          )}

          <div className="rail-delete-zone">
            <button className="rail-delete-btn" onClick={handleDeleteEntry}>
              Delete entry
            </button>
          </div>
        </>
      )}
    </aside>
  )
}
