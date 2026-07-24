import { useContext, useState } from 'react';
import { AuthContext } from '../App.jsx';
import { logout as apiLogout } from '../api.js';

export default function Topbar({ archives, archiveId, onArchiveChange, view, onViewChange, onCaptureClick, collections, selectedCollectionUid, onCollectionChange, isPublicSession, onSignInClick }) {
  const { currentUser, setCurrentUser } = useContext(AuthContext) ?? {};
  const [loggingOut, setLoggingOut] = useState(false);

  async function handleLogout() {
    setLoggingOut(true);
    await apiLogout();
    setCurrentUser(null);
    window.location.reload();
  }

  // For guests: hide auth-required collections; "All entries" only if _default_ is public.
  const defaultColl = collections.find(c => c.slug === '_default_')
  const showAllEntriesOption = !isPublicSession || (!!defaultColl && !defaultColl.requires_auth)
  const namedCollections = collections
    .filter(c => c.slug !== '_default_')
    .filter(c => !isPublicSession || !c.requires_auth)

  return (
    <header className="topbar">
      <div className="brand">Archivr</div>
      <div className="switchers">
        {!isPublicSession && (
          <div className="switcher">
            <select aria-label="Select archive"
              value={archiveId ?? ''} onChange={e => onArchiveChange(e.target.value)}>
              {archives.map(a => <option key={a.id} value={a.id}>{a.label}</option>)}
            </select>
          </div>
        )}
        {namedCollections.length > 0 && (
          <div className="switcher">
            <select
              aria-label="Select collection"
              value={selectedCollectionUid ?? ''}
              onChange={e => onCollectionChange(e.target.value || null)}
            >
              {showAllEntriesOption && <option value="">All entries</option>}
              {namedCollections.map(c => (
                <option key={c.collection_uid} value={c.collection_uid}>{c.name}</option>
              ))}
            </select>
          </div>
        )}
      </div>
      {isPublicSession ? (
        <>
          <span style={{ flex: 1 }} />
          <button className="logout-btn" onClick={onSignInClick}>Sign in</button>
        </>
      ) : (
        <>
          <nav className="nav" aria-label="Primary">
            {['archive', 'tags', 'collections', 'runs', 'admin', 'settings'].map(name => (
              <button key={name} className={`nav-link${view === name ? ' is-active' : ''}`}
                onClick={() => onViewChange(name)}>
                {name.charAt(0).toUpperCase() + name.slice(1)}
              </button>
            ))}
          </nav>
          <button className="capture-button" onClick={onCaptureClick}>Capture</button>
          {currentUser && (
            <div className="user-menu">
              <span className="user-name">{currentUser.display_name || currentUser.username}</span>
              <button className="logout-btn" onClick={handleLogout} disabled={loggingOut}>
                {loggingOut ? 'Logging out\u2026' : 'Log out'}
              </button>
            </div>
          )}
        </>
      )}
    </header>
  )
}
