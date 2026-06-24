export default function Topbar({ archives, archiveId, onArchiveChange, view, onViewChange, onCaptureClick }) {
  return (
    <header className="topbar">
      <div className="brand">Archivr</div>
      <select className="archive-switcher" aria-label="Select archive"
        value={archiveId ?? ''} onChange={e => onArchiveChange(e.target.value)}>
        {archives.map(a => <option key={a.id} value={a.id}>{a.label}</option>)}
      </select>
      <nav className="nav" aria-label="Primary">
        {['archive', 'runs', 'admin', 'tags'].map(name => (
          <button key={name} className={`nav-link${view === name ? ' is-active' : ''}`}
            onClick={() => onViewChange(name)}>
            {name.charAt(0).toUpperCase() + name.slice(1)}
          </button>
        ))}
      </nav>
      <button className="capture-button" onClick={onCaptureClick}>+ Capture</button>
    </header>
  )
}
