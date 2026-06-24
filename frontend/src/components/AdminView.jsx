export default function AdminView({ archives }) {
  return (
    <section id="admin-view" className="view admin-view is-active">
      <h1>Mounted Archives</h1>
      <div className="admin-list">
        {archives.map(archive => (
          <div key={archive.id} className="admin-archive">
            <strong>{archive.label}</strong>
            <div className="muted">{archive.archive_path}</div>
          </div>
        ))}
      </div>
    </section>
  )
}
