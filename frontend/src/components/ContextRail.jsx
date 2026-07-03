import { useState, useEffect, useRef } from 'react'
import { fetchEntryDetail, fetchEntryTags, assignTag, removeTag, listEntryCollections, updateEntryTitle } from '../api'
import { formatTimestamp, formatBytes, valueText, sourceIconSvg } from '../utils'

const VIS_LABEL = { 0: 'Private', 1: 'Public', 2: 'Users only', 3: 'Public' }

const ExternalIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <path d="M7 17 17 7M9 7h8v8"/>
  </svg>
)

export default function ContextRail({ archiveId, selectedEntry, onTagFilterSet, tagNodes, onTagsRefresh, onEntryTitleChange }) {
  const [detail, setDetail] = useState(null)
  const [tags, setTags] = useState([])
  const [assignInput, setAssignInput] = useState('')
  const [entryCollections, setEntryCollections] = useState([])
  const [assignError, setAssignError] = useState('')
  const selectSeqRef = useRef(0)
  const titleCancelRef = useRef(false)
  const [editingTitle, setEditingTitle] = useState(false)
  const [titleDraft, setTitleDraft] = useState('')

  useEffect(() => {
    if (!selectedEntry || !archiveId) {
      setDetail(null)
      setTags([])
      setEntryCollections([])
      return
    }
    setEditingTitle(false)
    setTitleDraft('')
    titleCancelRef.current = false
    const seq = ++selectSeqRef.current
    setDetail(null)
    setTags([])
    Promise.all([
      fetchEntryDetail(archiveId, selectedEntry.entry_uid),
      fetchEntryTags(archiveId, selectedEntry.entry_uid),
      listEntryCollections(archiveId, selectedEntry.entry_uid),
    ]).then(([det, tgs, ecs]) => {
      if (seq !== selectSeqRef.current) return
      setDetail(det)
      setTags(tgs)
      setEntryCollections(ecs)
    }).catch(() => {})
  }, [selectedEntry, archiveId])

  async function handleTitleSave() {
    const newTitle = titleDraft.trim() || null
    try {
      await updateEntryTitle(archiveId, selectedEntry.entry_uid, newTitle)
      setDetail(prev => prev ? { ...prev, summary: { ...prev.summary, title: newTitle } } : prev)
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

  const metaRows = detail ? [
    ['Added',      formatTimestamp(detail.summary.archived_at)],
    ['Source',     detail.summary.source_kind],
    ['Type',       detail.summary.entity_kind],
    ['Visibility', VIS_LABEL[detail.summary.visibility] ?? detail.summary.visibility],
    ['Root',       detail.structured_root_relpath],
  ] : []

  return (
    <aside className="context-rail">
      <div className="rail-eyebrow">Context</div>

      {!selectedEntry ? (
        <p className="tags-empty">Select an entry.</p>
      ) : !detail ? (
        <p className="tags-empty">Loading…</p>
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

          <div className="meta-list">
            {metaRows.filter(([, v]) => v != null && v !== '').map(([label, value]) => (
              <div key={label} className="meta-item">
                <span className="meta-k">{label}</span>
                <span className={`meta-v${label === 'Root' ? ' mono' : ''}`}>{valueText(value)}</span>
              </div>
            ))}
          </div>

          {detail.artifacts.length > 0 && (
            <div className="rail-section">
              <div className="rail-section-heading">
                Artifacts <span className="num">{detail.artifacts.length}</span>
              </div>
              <ul className="artifact-list">
                {detail.artifacts.map((artifact, index) => (
                  <li key={index}>
                    <a
                      href={`/api/archives/${archiveId}/entries/${detail.summary.entry_uid}/artifacts/${index}`}
                      target="_blank"
                      rel="noopener noreferrer"
                      className="artifact-link"
                    >
                      <span className="artifact-name">{artifact.artifact_role.replace(/_/g, ' ')}</span>
                      <span className="artifact-size">
                        {artifact.byte_size != null ? formatBytes(artifact.byte_size) : '—'}
                      </span>
                    </a>
                  </li>
                ))}
              </ul>
            </div>
          )}
        </>
      )}

      {selectedEntry && (
        <>
          <div className="rail-section">
            <div className="rail-section-heading">Tags</div>
            {tags.length === 0 ? (
              <p className="tags-empty">No tags yet.</p>
            ) : (
              <div className="tags-wrap">
                {tags.map(tag => (
                  <span key={tag.tag_uid} className="tag-pill" title={tag.full_path}>
                    {tag.name}
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

          {entryCollections.length > 0 && (
            <div className="rail-section">
              <div className="rail-section-heading">Collections</div>
              {entryCollections.map(c => (
                <div key={c.collection_uid} className="coll-row">
                  <span className="coll-name">{c.collection_uid}</span>
                  <span className="vis-badge">
                    {VIS_LABEL[c.visibility_bits] ?? `bits:${c.visibility_bits}`}
                  </span>
                </div>
              ))}
            </div>
          )}
        </>
      )}
    </aside>
  )
}
