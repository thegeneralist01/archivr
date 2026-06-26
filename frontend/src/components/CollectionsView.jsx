import { useState, useEffect, useCallback } from 'react'
import { listCollections, createCollection, getCollection } from '../api.js'

const VIS_LABELS = { 0: 'Private', 1: 'Public', 2: 'Users only', 3: 'Public' }

export default function CollectionsView({ archiveId }) {
  const [collections, setCollections] = useState([])
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState(null)
  const [selectedColl, setSelectedColl] = useState(null)
  const [collDetail, setCollDetail] = useState(null)
  const [detailLoading, setDetailLoading] = useState(false)

  // Create form
  const [newName, setNewName] = useState('')
  const [newSlug, setNewSlug] = useState('')
  const [newVis, setNewVis] = useState(2)
  const [creating, setCreating] = useState(false)
  const [createError, setCreateError] = useState(null)

  const refresh = useCallback(async () => {
    if (!archiveId) return
    setLoading(true)
    setError(null)
    try {
      const cols = await listCollections(archiveId)
      setCollections(cols)
    } catch (e) {
      setError(e.message)
    } finally {
      setLoading(false)
    }
  }, [archiveId])

  useEffect(() => { refresh() }, [refresh])

  useEffect(() => {
    if (!selectedColl) { setCollDetail(null); return }
    setDetailLoading(true)
    getCollection(archiveId, selectedColl.collection_uid)
      .then(d => setCollDetail(d))
      .catch(e => setError(e.message))
      .finally(() => setDetailLoading(false))
  }, [selectedColl, archiveId])

  async function handleCreate(e) {
    e.preventDefault()
    const name = newName.trim()
    const slug = newSlug.trim()
    if (!name || !slug) return
    setCreating(true)
    setCreateError(null)
    try {
      await createCollection(archiveId, name, slug, newVis)
      setNewName('')
      setNewSlug('')
      setNewVis(2)
      await refresh()
    } catch (err) {
      setCreateError(err.message)
    } finally {
      setCreating(false)
    }
  }

  if (!archiveId) return <div className="view-placeholder">Select an archive.</div>

  return (
    <div className="collections-view">
      <h2 className="view-heading">Collections</h2>

      {loading && <div className="muted">Loading…</div>}
      {error && <div className="error-text">{error}</div>}

      <div className="collections-layout">
        <div className="collections-list">
          {collections.map(c => (
            <div
              key={c.collection_uid}
              className={`collection-row${selectedColl?.collection_uid === c.collection_uid ? ' is-selected' : ''}`}
              onClick={() => setSelectedColl(c)}
            >
              <span className="collection-name">{c.name}</span>
              <span className="muted" style={{ fontSize: '0.8em' }}>
                {VIS_LABELS[c.default_visibility_bits] ?? c.default_visibility_bits}
              </span>
            </div>
          ))}
          {collections.length === 0 && !loading && (
            <div className="muted">No collections yet.</div>
          )}
        </div>

        {selectedColl && (
          <div className="collection-detail">
            <h3 className="collection-detail-name">{selectedColl.name}</h3>
            <div className="muted" style={{ marginBottom: '0.75rem' }}>
              Default visibility: {VIS_LABELS[selectedColl.default_visibility_bits] ?? selectedColl.default_visibility_bits}
            </div>
            {detailLoading ? (
              <div className="muted">Loading entries…</div>
            ) : collDetail ? (
              collDetail.entries.length === 0 ? (
                <div className="muted">No entries visible to you in this collection.</div>
              ) : (
                <ul className="collection-entries-list">
                  {collDetail.entries.map(entry => (
                    <li key={entry.entry_uid} className="collection-entry-item">
                      <span className="entry-title">
                        {entry.title || entry.entry_uid}
                      </span>
                      <span className="muted" style={{ fontSize: '0.8em' }}>
                        {entry.source_kind}
                      </span>
                    </li>
                  ))}
                </ul>
              )
            ) : null}
          </div>
        )}
      </div>

      <details className="create-collection-form" style={{ marginTop: '1.5rem' }}>
        <summary style={{ cursor: 'pointer', fontWeight: 600 }}>Create collection</summary>
        <form onSubmit={handleCreate} style={{ marginTop: '0.75rem', display: 'flex', flexDirection: 'column', gap: '0.5rem', maxWidth: 400 }}>
          <label>
            Name
            <input
              className="form-input"
              type="text"
              value={newName}
              onChange={e => { setNewName(e.target.value); if (!newSlug) setNewSlug(e.target.value.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '')) }}
              placeholder="My Collection"
              required
            />
          </label>
          <label>
            Slug
            <input
              className="form-input"
              type="text"
              value={newSlug}
              onChange={e => setNewSlug(e.target.value)}
              placeholder="my-collection"
              required
            />
          </label>
          <label>
            Default visibility
            <select className="form-input" value={newVis} onChange={e => setNewVis(Number(e.target.value))}>
              <option value={0}>Private (admin/owner only)</option>
              <option value={2}>Users only (logged in)</option>
              <option value={3}>Public</option>
            </select>
          </label>
          {createError && <div className="muted" style={{ color: 'var(--accent)' }}>{createError}</div>}
          <button className="btn" type="submit" disabled={creating}>
            {creating ? 'Creating…' : 'Create'}
          </button>
        </form>
      </details>
    </div>
  )
}
