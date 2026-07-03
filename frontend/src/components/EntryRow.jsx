import { useState } from 'react';
import { formatTimestamp, formatBytes, valueText, sourceIconSvg } from '../utils';

export default function EntryRow({ entry, archiveId, isSelected, onSelect }) {
  const [favFailed, setFavFailed] = useState(false);
  const showFavicon =
    entry.source_kind === 'web' &&
    entry.entity_kind === 'page' &&
    entry.has_favicon &&
    archiveId &&
    !favFailed;

  const icon = showFavicon ? (
    <img
      src={`/api/archives/${archiveId}/entries/${entry.entry_uid}/favicon`}
      width="16"
      height="16"
      alt=""
      onError={() => setFavFailed(true)}
      style={{ objectFit: 'contain' }}
    />
  ) : (
    <span dangerouslySetInnerHTML={{ __html: sourceIconSvg(entry.source_kind) }} />
  );

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
        <span className="source-icon">{icon}</span>
        <span className="entry-title">{valueText(entry.title) || valueText(entry.entry_uid)}</span>
      </div>
      <div className="col-type">
        <span className="type-pill">{valueText(entry.entity_kind)}</span>
      </div>
      <div className="col-size">
        <span className="size-total">{formatBytes(entry.total_artifact_bytes)}</span>
        {entry.cached_bytes > 0 && entry.total_artifact_bytes > 0 && (
          <span className="size-cached-pct" title={`${formatBytes(entry.cached_bytes)} already on disk from an earlier entry`}>
            {Math.round(entry.cached_bytes / entry.total_artifact_bytes * 100)}% cached
          </span>
        )}
      </div>
      <div className="url-cell col-url">{valueText(entry.original_url)}</div>
    </div>
  );
}
