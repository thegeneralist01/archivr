export default function RunsView({ runs }) {
  return (
    <section id="runs-view" className="view is-active">
      <table className="entry-table">
        <thead>
          <tr>
            <th>Started</th><th>Status</th><th>Requested</th><th>Completed</th><th>Failed</th>
          </tr>
        </thead>
        <tbody>
          {runs.map((run, i) => (
            <tr key={i}>
              <td>{run.started_at ?? ''}</td>
              <td>{run.status ?? ''}</td>
              <td>{run.requested_count ?? ''}</td>
              <td>{run.completed_count ?? ''}</td>
              <td>{run.failed_count ?? ''}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </section>
  )
}
