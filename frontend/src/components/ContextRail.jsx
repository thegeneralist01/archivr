import { useState, useEffect, useRef } from 'react'
import { fetchEntryTags, assignTag, removeTag, listEntryCollections, listCollections, addEntryToCollection, updateEntryTitle, deleteEntry, rearchiveEntry, pollCaptureJob } from '../api'
import { formatTimestamp, formatBytes, valueText, sourceIconSvg, displayPath } from '../utils'

const VIS_LABEL = { 0: 'Private', 1: 'Public', 2: 'Users only', 3: 'Public' }


const ExternalIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <path d="M7 17 17 7M9 7h8v8"/>
  </svg>
)

export default function ContextRail({ archiveId, selectedEntry, selectedUids, selectedEntries, detail, onTagFilterSet, tagNodes, onTagsRefresh, onEntryTitleChange, onEntryDeleted, onBulkDeleted, humanizeTags, onDetailRefresh, onOpenPreview, onPlay }) {
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
  }, [selectedEntry, archiveId])

  useEffect(() => {
    return () => {
      clearInterval(rearchivePollRef.current)
    }
  }, [])

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
  const PREVIEW_EXTS = new Set(['mp4','webm','mov','mkv','avi','m4v','ogv','pdf','html','htm','jpg','jpeg','png','gif','webp','avif','svg','bmp'])
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
      ) : !selectedEntry ? (
        <p className="tags-empty">Select an entry.</p>
      ) : !detail ? (
        <p className="tags-empty">Loading\u2026</p>
      ) : (
        <>
          {editingTitle ? (
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

      {selectedEntry && !isBulk && (
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
