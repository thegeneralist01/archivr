import { useState, useEffect } from 'react'
import { getCollection } from '../api.js'
import { formatTimestamp } from '../utils.js'

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
    <div style={{ padding: '40px 24px', fontFamily: 'system-ui, sans-serif', color: '#888' }}>
      <p>{error}</p>
    </div>
  )

  const entries = collection.entries ?? []

  return (
    <div style={{ maxWidth: 760, margin: '0 auto', padding: '40px 24px', fontFamily: 'system-ui, sans-serif' }}>
      <h1 style={{ fontSize: 28, fontWeight: 700, marginBottom: 4 }}>{collection.name}</h1>
      <p style={{ color: '#888', fontSize: 13, marginBottom: 32 }}>
        {entries.length} entr{entries.length === 1 ? 'y' : 'ies'}
      </p>
      {entries.length === 0 ? (
        <p style={{ color: '#888' }}>No entries in this collection.</p>
      ) : (
        <ul style={{ listStyle: 'none', padding: 0, margin: 0 }}>
          {entries.map(entry => (
            <li key={entry.entry_uid} style={{
              borderBottom: '1px solid #e5e5e5',
              padding: '14px 0',
            }}>
              {entry.original_url ? (
                <a
                  href={entry.original_url}
                  target="_blank"
                  rel="noopener noreferrer"
                  style={{ fontWeight: 600, color: '#111', textDecoration: 'none', fontSize: 15 }}
                >
                  {entry.title || entry.entry_uid}
                </a>
              ) : (
                <span style={{ fontWeight: 600, color: '#111', fontSize: 15 }}>
                  {entry.title || entry.entry_uid}
                </span>
              )}
              <div style={{ fontSize: 12, color: '#888', marginTop: 4 }}>
                {entry.source_kind} · {formatTimestamp(entry.archived_at)}
              </div>
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}
