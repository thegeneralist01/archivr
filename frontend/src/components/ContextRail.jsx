import { useState, useEffect, useRef } from 'react'
import { fetchEntryDetail, fetchEntryTags, assignTag, removeTag } from '../api'
import { formatTimestamp, formatBytes, valueText } from '../utils'

export default function ContextRail({ archiveId, selectedEntry, onTagFilterSet, tagNodes, onTagsRefresh }) {
  const [detail, setDetail] = useState(null)
  const [tags, setTags] = useState([])
  const [assignInput, setAssignInput] = useState('')
  const [assignError, setAssignError] = useState('')
  const selectSeqRef = useRef(0)

  useEffect(() => {
    if (!selectedEntry || !archiveId) {
      setDetail(null)
      setTags([])
      return
    }
    const seq = ++selectSeqRef.current
    setDetail(null)
    setTags([])
    Promise.all([
      fetchEntryDetail(archiveId, selectedEntry.entry_uid),
      fetchEntryTags(archiveId, selectedEntry.entry_uid),
    ]).then(([det, tgs]) => {
      if (seq !== selectSeqRef.current) return
      setDetail(det)
      setTags(tgs)
    }).catch(() => {})
  }, [selectedEntry, archiveId])

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
    } catch (e) {
      // silently ignore for now; could add error state
    }
  }

  return (
    <aside className="context-rail">
      <div className="rail-title">Context</div>
      {!selectedEntry ? (
        <div className="rail-body">Select an entry.</div>
      ) : !detail ? (
        <div className="rail-body">Loading…</div>
      ) : (
        <div className="rail-body">
          <strong className="rail-entry-title">
            {valueText(detail.summary.title) || valueText(detail.summary.entry_uid)}
          </strong>

          <div className="rail-section">
            {detail.summary.original_url && (
              <div className="rail-item">
                <span className="rail-label">Original URL</span>:{' '}
                <a
                  href={detail.summary.original_url}
                  target="_blank"
                  rel="noopener noreferrer"
                  className="rail-url-link"
                >
                  {detail.summary.original_url}
                </a>
              </div>
            )}
            {[
              ['Added', formatTimestamp(detail.summary.archived_at)],
              ['Source', detail.summary.source_kind],
              ['Type', detail.summary.entity_kind],
              ['Visibility', detail.summary.visibility],
              ['Structured root', detail.structured_root_relpath],
            ].map(([label, value]) => (
              <div key={label} className="rail-item">
                <span className="rail-label">{label}</span>: {valueText(value)}
              </div>
            ))}
          </div>

          {detail.artifacts.length > 0 ? (
            <div className="rail-section">
              <div className="rail-section-heading">Artifacts ({detail.artifacts.length})</div>
              <ul className="artifact-list">
                {detail.artifacts.map((artifact, index) => (
                  <li key={index}>
                    <a
                      href={`/api/archives/${archiveId}/entries/${detail.summary.entry_uid}/artifacts/${index}`}
                      target="_blank"
                      rel="noopener noreferrer"
                      className="artifact-link"
                    >
                      {artifact.artifact_role.replace(/_/g, ' ')}
                      {artifact.byte_size != null ? ` (${formatBytes(artifact.byte_size)})` : ''}
                    </a>
                  </li>
                ))}
              </ul>
            </div>
          ) : (
            <div className="rail-item muted">No artifacts.</div>
          )}
        </div>
      )}

      {selectedEntry && (
        <>
          <div className="entry-tags">
            {tags.length === 0 ? (
              <span className="muted">No tags.</span>
            ) : (
              tags.map(tag => (
                <span key={tag.tag_uid} className="tag-pill" title={tag.full_path}>
                  {tag.name}
                  <button
                    className="remove-tag"
                    title={`Remove tag ${tag.full_path}`}
                    onClick={() => handleRemoveTag(tag.tag_uid)}
                  >
                    ×
                  </button>
                </span>
              ))
            )}
          </div>
          <div className="assign-tag-form">
            <input
              className="assign-tag-input"
              type="text"
              placeholder="/science/cs"
              autoComplete="off"
              value={assignInput}
              onChange={e => setAssignInput(e.target.value)}
              onKeyDown={e => { if (e.key === 'Enter') handleAssignTag() }}
            />
            <button className="assign-tag-btn" onClick={handleAssignTag}>Add tag</button>
            {assignError && (
              <div className="muted" style={{ fontSize: '0.85em', color: 'var(--accent)' }}>
                {assignError}
              </div>
            )}
          </div>
        </>
      )}
    </aside>
  )
}
