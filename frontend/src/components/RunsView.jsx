function fmtDate(iso) {
  if (!iso) return '—';
  try {
    return new Date(iso).toLocaleString(undefined, {
      year: 'numeric', month: 'short', day: 'numeric',
      hour: '2-digit', minute: '2-digit',
    });
  } catch {
    return iso;
  }
}

function StatusBadge({ status }) {
  const cls = status === 'completed' ? 'run-status--completed'
    : status === 'failed' ? 'run-status--failed'
    : status === 'running' ? 'run-status--running'
    : '';
  return <span className={`run-status ${cls}`}>{status || '—'}</span>;
}

export default function RunsView({ runs }) {
  return (
    <section id="runs-view" className="view is-active">
      <table className="entry-table">
        <thead>
          <tr>
            <th>Started</th>
            <th>Status</th>
            <th>Requested</th>
            <th>Completed</th>
            <th>Failed</th>
          </tr>
        </thead>
        <tbody>
          {runs.length === 0 ? (
            <tr><td colSpan={5} style={{ color: 'var(--muted)', padding: '24px 16px', textAlign: 'center' }}>No runs yet.</td></tr>
          ) : runs.map((run, i) => (
            <tr key={i}>
              <td>{fmtDate(run.started_at)}</td>
              <td><StatusBadge status={run.status} /></td>
              <td>{run.requested_count ?? '—'}</td>
              <td>{run.completed_count ?? '—'}</td>
              <td>{run.failed_count ?? '—'}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </section>
  );
}
