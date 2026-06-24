import { formatTimestamp, formatBytes, valueText, sourceIconSvg } from '../utils';

export default function EntryRow({ entry, isSelected, onSelect }) {
  return (
    <div
      className={isSelected ? 'is-selected' : undefined}
      tabIndex={0}
      data-entry-uid={entry.entry_uid}
      onClick={onSelect}
      onKeyDown={e => { if (e.key === 'Enter') onSelect(); }}
    >
      <div className="col-added">{formatTimestamp(entry.archived_at)}</div>
      <div className="col-title">
        <span className="source-icon" dangerouslySetInnerHTML={{ __html: sourceIconSvg(entry.source_kind) }} />
        <span className="entry-title">{valueText(entry.title) || valueText(entry.entry_uid)}</span>
      </div>
      <div className="col-type">
        <span className="type-pill">{valueText(entry.entity_kind)}</span>
      </div>
      <div className="col-size">{formatBytes(entry.total_artifact_bytes)}</div>
      <div className="url-cell col-url">{valueText(entry.original_url)}</div>
    </div>
  );
}
