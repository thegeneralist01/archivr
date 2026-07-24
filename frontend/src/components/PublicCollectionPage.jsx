import { useState, useEffect } from 'react'
import { getCollection } from '../api.js'
import { formatTimestamp } from '../utils.js'

// Standalone public collection page — rendered at /c/:archiveId/:collUid.
// Bypasses the login gate; the server only returns guest-visible entries.
export default function PublicCollectionPage({ archiveId, collUid }) {
  const [collection, setCollection] = useState(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState(null)

  useEffect(() => {
    getCollection(archiveId, collUid)
      .then(data => { setCollection(data); setLoading(false) })
      .catch(err => { setError(err?.message || 'Failed to load collection'); setLoading(false) })
  }, [archiveId, collUid])

  if (loading) return <div className="auth-loading">Loading…</div>

  if (error) return (
    <div className="pub-coll-page">
      <div className="pub-coll-topbar">
        <span className="pub-coll-brand">Archivr</span>
      </div>
      <div className="pub-coll-body">
        <p className="pub-coll-empty">{error}</p>
      </div>
    </div>
  )

  const entries = collection.entries ?? []

  return (
    <div className="pub-coll-page">
      <div className="pub-coll-topbar">
        <span className="pub-coll-brand">Archivr</span>
        <span className="pub-coll-divider">·</span>
        <span className="pub-coll-name">{collection.name}</span>
      </div>

      <div className="pub-coll-body">
        <p className="pub-coll-meta">
          {entries.length} entr{entries.length === 1 ? 'y' : 'ies'}
        </p>

        {entries.length === 0 ? (
          <p className="pub-coll-empty">No public entries in this collection.</p>
        ) : (
          <ul className="coll-entries-list">
            {entries.map(entry => (
              <li key={entry.entry_uid} className="coll-entry-row">
                <div className="coll-entry-info">
                  {entry.original_url ? (
                    <a
                      className="pub-coll-entry-title"
                      href={entry.original_url}
                      target="_blank"
                      rel="noopener noreferrer"
                    >
                      {entry.title || entry.entry_uid}
                    </a>
                  ) : (
                    <span className="pub-coll-entry-title--plain">
                      {entry.title || entry.entry_uid}
                    </span>
                  )}
                  <span className="coll-entry-kind muted">
                    {entry.source_kind} · {formatTimestamp(entry.archived_at)}
                  </span>
                </div>
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  )
}
