import { useState, useEffect, useCallback, useRef } from 'react'
import {
  listCollections, createCollection, getCollection,
  addEntryToCollection, removeEntryFromCollection,
  updateEntryVisibility, updateCollection, deleteCollection,
} from '../api.js'

const VIS_OPTIONS = [
  { value: 0, label: 'Private' },
  { value: 2, label: 'Users only' },
  { value: 3, label: 'Public' },
]
const VIS_LABEL = v => VIS_OPTIONS.find(o => o.value === v)?.label ?? String(v)

export default function CollectionsView({ archiveId }) {
  const [collections, setCollections] = useState([])
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState(null)
  const [selectedUid, setSelectedUid] = useState(null)
  const [collDetail, setCollDetail] = useState(null)
  const [detailLoading, setDetailLoading] = useState(false)
  const [detailError, setDetailError] = useState(null)

  // Create form
  const [newName, setNewName] = useState('')
  const [newSlug, setNewSlug] = useState('')
  const [newVis, setNewVis] = useState(2)
  const [creating, setCreating] = useState(false)
  const [createError, setCreateError] = useState(null)

  // Add-entry form
  const [addUid, setAddUid] = useState('')
  const [addVis, setAddVis] = useState(2)
  const [adding, setAdding] = useState(false)
  const [addError, setAddError] = useState(null)

  // Inline rename state
  const [renaming, setRenaming] = useState(false)
  const [renameVal, setRenameVal] = useState('')
  const renameRef = useRef(null)

  const selected = collections.find(c => c.collection_uid === selectedUid) ?? null
  const isDefault = selected?.slug === '_default_'

  const refreshList = useCallback(async () => {
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

  const refreshDetail = useCallback(async (uid) => {
    if (!uid) { setCollDetail(null); return }
    setDetailLoading(true)
    setDetailError(null)
    try {
      const d = await getCollection(archiveId, uid)
      setCollDetail(d)
    } catch (e) {
      setDetailError(e.message)
    } finally {
      setDetailLoading(false)
    }
  }, [archiveId])

  useEffect(() => { refreshList() }, [refreshList])
  useEffect(() => { refreshDetail(selectedUid) }, [selectedUid, refreshDetail])

  // Auto-focus rename input
  useEffect(() => { if (renaming && renameRef.current) renameRef.current.focus() }, [renaming])

  async function handleCreate(e) {
    e.preventDefault()
    const name = newName.trim()
    const slug = newSlug.trim()
    if (!name || !slug) return
    setCreating(true)
    setCreateError(null)
    try {
      const coll = await createCollection(archiveId, name, slug, newVis)
      setNewName('')
      setNewSlug('')
      setNewVis(2)
      await refreshList()
      setSelectedUid(coll.collection_uid)
    } catch (err) {
      setCreateError(err.message)
    } finally {
      setCreating(false)
    }
  }

  async function handleRenameCommit() {
    const name = renameVal.trim()
    if (!name || !selected) { setRenaming(false); return }
    try {
      await updateCollection(archiveId, selected.collection_uid, { name })
      await refreshList()
      setCollDetail(d => d ? { ...d, name } : d)
    } catch (e) {
      setError(e.message)
    } finally {
      setRenaming(false)
    }
  }

  async function handleVisChange(newVisVal) {
    if (!selected) return
    try {
      await updateCollection(archiveId, selected.collection_uid, { default_visibility_bits: newVisVal })
      await refreshList()
      setCollDetail(d => d ? { ...d, default_visibility_bits: newVisVal } : d)
    } catch (e) {
      setError(e.message)
    }
  }

  async function handleDelete() {
    if (!selected) return
    if (!window.confirm(`Delete collection "${selected.name}"? Entries will not be deleted.`)) return
    try {
      await deleteCollection(archiveId, selected.collection_uid)
      setSelectedUid(null)
      setCollDetail(null)
      await refreshList()
    } catch (e) {
      setError(e.message)
    }
  }

  async function handleAddEntry(e) {
    e.preventDefault()
    const uid = addUid.trim()
    if (!uid || !selected) return
    setAdding(true)
    setAddError(null)
    try {
      await addEntryToCollection(archiveId, selected.collection_uid, uid, addVis)
      setAddUid('')
      await refreshDetail(selected.collection_uid)
    } catch (err) {
      setAddError(err.message)
    } finally {
      setAdding(false)
    }
  }

  async function handleRemoveEntry(entryUid) {
    if (!selected) return
    try {
      await removeEntryFromCollection(archiveId, selected.collection_uid, entryUid)
      await refreshDetail(selected.collection_uid)
    } catch (e) {
      setDetailError(e.message)
    }
  }

  async function handleEntryVisChange(entryUid, vis) {
    if (!selected) return
    try {
      await updateEntryVisibility(archiveId, selected.collection_uid, entryUid, vis)
      setCollDetail(d => d ? {
        ...d,
        entries: d.entries.map(en =>
          en.entry_uid === entryUid ? { ...en, collection_visibility_bits: vis } : en
        ),
      } : d)
    } catch (e) {
      setDetailError(e.message)
    }
  }

  if (!archiveId) return <div className="view-placeholder">Select an archive.</div>

  return (
    <div className="collections-view">
      <h2 className="collections-heading">Collections</h2>

      {loading && <div className="muted">Loading…</div>}
      {error && <div className="collections-error">{error} <button onClick={() => setError(null)} className="coll-dismiss">×</button></div>}

      <div className="collections-layout">
        {/* Sidebar */}
        <div className="collections-sidebar">
          {collections.map(c => (
            <button
              key={c.collection_uid}
              className={`coll-row${selectedUid === c.collection_uid ? ' is-active' : ''}`}
              onClick={() => setSelectedUid(c.collection_uid)}
            >
              <span className="coll-row-name">{c.name}</span>
              <span className="coll-row-meta">{VIS_LABEL(c.default_visibility_bits)}</span>
            </button>
          ))}
          {collections.length === 0 && !loading && (
            <div className="muted" style={{ padding: '8px 12px' }}>No collections yet.</div>
          )}
        </div>

        {/* Detail pane */}
        {selected ? (
          <div className="coll-detail">
            {/* Header */}
            <div className="coll-detail-header">
              {renaming ? (
                <input
                  ref={renameRef}
                  className="coll-rename-input"
                  value={renameVal}
                  onChange={e => setRenameVal(e.target.value)}
                  onBlur={handleRenameCommit}
                  onKeyDown={e => {
                    if (e.key === 'Enter') handleRenameCommit()
                    if (e.key === 'Escape') setRenaming(false)
                  }}
                />
              ) : (
                <h3
                  className={`coll-detail-name${isDefault ? '' : ' coll-detail-name--editable'}`}
                  title={isDefault ? undefined : 'Click to rename'}
                  onClick={() => { if (!isDefault) { setRenameVal(selected.name); setRenaming(true) } }}
                >
                  {collDetail?.name ?? selected.name}
                  {!isDefault && <span className="coll-edit-hint"> ✎</span>}
                </h3>
              )}
              {!isDefault && (
                <button className="coll-delete-btn" onClick={handleDelete} title="Delete collection">Delete</button>
              )}
            </div>

            {/* Visibility */}
            <div className="coll-detail-vis">
              <span className="coll-vis-label">Default visibility</span>
              <select
                className="coll-vis-select"
                value={collDetail?.default_visibility_bits ?? selected.default_visibility_bits}
                onChange={e => handleVisChange(Number(e.target.value))}
                disabled={isDefault}
              >
                {VIS_OPTIONS.map(o => <option key={o.value} value={o.value}>{o.label}</option>)}
              </select>
            </div>

            {/* Entries */}
            <div className="coll-entries-section">
              <div className="coll-section-heading">Entries</div>
              {detailLoading && <div className="muted">Loading…</div>}
              {detailError && <div className="collections-error">{detailError}</div>}
              {!detailLoading && collDetail && (
                collDetail.entries.length === 0 ? (
                  <div className="muted">No entries in this collection.</div>
                ) : (
                  <ul className="coll-entries-list">
                    {collDetail.entries.map(entry => (
                      <li key={entry.entry_uid} className="coll-entry-row">
                        <div className="coll-entry-info">
                          <span className="coll-entry-title">{entry.title || entry.entry_uid}</span>
                          <span className="coll-entry-kind muted">{entry.source_kind}</span>
                        </div>
                        <div className="coll-entry-actions">
                          <select
                            className="coll-entry-vis-select"
                            value={entry.collection_visibility_bits}
                            onChange={e => handleEntryVisChange(entry.entry_uid, Number(e.target.value))}
                          >
                            {VIS_OPTIONS.map(o => <option key={o.value} value={o.value}>{o.label}</option>)}
                          </select>
                          {!isDefault && (
                            <button
                              className="coll-entry-remove"
                              onClick={() => handleRemoveEntry(entry.entry_uid)}
                              title="Remove from collection"
                            >×</button>
                          )}
                        </div>
                      </li>
                    ))}
                  </ul>
                )
              )}
            </div>

            {/* Add entry (non-default only) */}
            {!isDefault && (
              <form className="coll-add-entry-form" onSubmit={handleAddEntry}>
                <div className="coll-section-heading">Add entry</div>
                <div className="coll-add-entry-row">
                  <input
                    className="coll-add-entry-input"
                    type="text"
                    value={addUid}
                    onChange={e => setAddUid(e.target.value)}
                    placeholder="entry_uid"
                    required
                  />
                  <select
                    className="coll-vis-select"
                    value={addVis}
                    onChange={e => setAddVis(Number(e.target.value))}
                  >
                    {VIS_OPTIONS.map(o => <option key={o.value} value={o.value}>{o.label}</option>)}
                  </select>
                  <button className="coll-add-btn" type="submit" disabled={adding}>
                    {adding ? '…' : 'Add'}
                  </button>
                </div>
                {addError && <div className="collections-error" style={{ marginTop: 4 }}>{addError}</div>}
              </form>
            )}
          </div>
        ) : (
          <div className="coll-detail coll-detail--empty">
            <div className="muted">Select a collection to view details.</div>
          </div>
        )}
      </div>

      {/* Create form */}
      <details className="coll-create-details" style={{ marginTop: '1.5rem' }}>
        <summary style={{ cursor: 'pointer', fontWeight: 600 }}>Create collection</summary>
        <form onSubmit={handleCreate} style={{ marginTop: '0.75rem', display: 'flex', flexDirection: 'column', gap: '0.5rem', maxWidth: 400 }}>
          <label>
            Name
            <input
              className="capture-input"
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
              className="capture-input"
              type="text"
              value={newSlug}
              onChange={e => setNewSlug(e.target.value)}
              placeholder="my-collection"
              required
            />
          </label>
          <label>
            Default visibility
            <select className="capture-input" style={{ height: 42 }} value={newVis} onChange={e => setNewVis(Number(e.target.value))}>
              {VIS_OPTIONS.map(o => <option key={o.value} value={o.value}>{o.label}</option>)}
            </select>
          </label>
          {createError && <div className="collections-error">{createError}</div>}
          <button className="capture-submit" type="submit" disabled={creating} style={{ alignSelf: 'flex-start', padding: '8px 20px' }}>
            {creating ? 'Creating…' : 'Create'}
          </button>
        </form>
      </details>
    </div>
  )
}
