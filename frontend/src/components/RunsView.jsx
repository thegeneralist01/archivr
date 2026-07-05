import { useState } from 'react'

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
    : status === 'in_progress' ? 'run-status--in-progress'
    : '';
  const label = status ? status.replace(/_/g, ' ') : '—';
  return <span className={`run-status ${cls}`}>{label}</span>;
}

export default function RunsView({ runs }) {
  const [expanded, setExpanded] = useState(null) // run_uid of the expanded row

  function toggle(uid) {
    setExpanded(prev => prev === uid ? null : uid)
  }

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
            <tr>
              <td colSpan={5} style={{ color: 'var(--muted)', padding: '24px 16px', textAlign: 'center' }}>
                No runs yet.
              </td>
            </tr>
          ) : runs.map(run => {
            const hasError = run.status === 'failed' && run.error_summary
            const isExpanded = expanded === run.run_uid
            return [
              <tr
                key={run.run_uid}
                className={hasError ? 'run-row run-row--failed' : 'run-row'}
                onClick={hasError ? () => toggle(run.run_uid) : undefined}
                title={hasError ? (isExpanded ? 'Click to hide error' : 'Click to view error') : undefined}
              >
                <td>{fmtDate(run.started_at)}</td>
                <td>
                  <StatusBadge status={run.status} />
                  {hasError && (
                    <span className="run-expand-hint" aria-hidden="true">
                      {isExpanded ? '▴' : '▾'}
                    </span>
                  )}
                </td>
                <td>{run.requested_count ?? '—'}</td>
                <td>{run.completed_count ?? '—'}</td>
                <td>{run.failed_count ?? '—'}</td>
              </tr>,
              hasError && isExpanded && (
                <tr key={`${run.run_uid}-detail`} className="run-error-row">
                  <td colSpan={5}>
                    <pre className="run-error-detail">{run.error_summary}</pre>
                  </td>
                </tr>
              ),
            ]
          })}
        </tbody>
      </table>
    </section>
  );
}
