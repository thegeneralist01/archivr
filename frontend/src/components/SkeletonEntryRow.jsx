export default function SkeletonEntryRow() {
  return (
    <div className="skeleton-row">
      <div className="col-added">
        <span className="skeleton-cell" style={{ width: 108, height: 13 }} />
      </div>
      <div className="col-title" style={{ gap: '0.42em', display: 'flex', alignItems: 'center' }}>
        <span className="skeleton-cell" style={{ width: 14, height: 14, borderRadius: '50%', flexShrink: 0 }} />
        <span className="skeleton-cell" style={{ width: '58%', height: 13 }} />
      </div>
      <div className="col-type">
        <span className="skeleton-cell" style={{ width: 58, height: 20, borderRadius: 99 }} />
      </div>
      <div className="col-size">
        <span className="skeleton-cell" style={{ width: 44, height: 12 }} />
      </div>
      <div className="col-url">
        <span className="skeleton-cell" style={{ width: '65%', height: 12 }} />
      </div>
    </div>
  )
}
