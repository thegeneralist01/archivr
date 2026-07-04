import { useContext, useState } from 'react';
import { AuthContext } from '../App.jsx';
import { logout as apiLogout } from '../api.js';

export default function Topbar({ archives, archiveId, onArchiveChange, view, onViewChange, onCaptureClick }) {
  const { currentUser, setCurrentUser } = useContext(AuthContext) ?? {};
  const [loggingOut, setLoggingOut] = useState(false);

  async function handleLogout() {
    setLoggingOut(true);
    await apiLogout();
    setCurrentUser(null);
    window.location.reload();
  }

  return (
    <header className="topbar">
      <div className="brand">Archivr</div>
      <div className="switcher">
        <select aria-label="Select archive"
          value={archiveId ?? ''} onChange={e => onArchiveChange(e.target.value)}>
          {archives.map(a => <option key={a.id} value={a.id}>{a.label}</option>)}
        </select>
      </div>
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
    </header>
  );
}
