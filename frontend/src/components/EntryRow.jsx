import { useState } from 'react';
import { formatTimestamp, formatBytes, valueText, sourceIconSvg } from '../utils';

export default function EntryRow({ entry, archiveId, isSelected, isMultiSelected, onRowClick }) {
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

  const checked = isSelected || isMultiSelected;

  function handleCheckboxClick(e) {
    e.stopPropagation();
    // treat checkbox tap as ctrl+click: toggle this entry without clearing others
    onRowClick(entry, { ctrlKey: true, metaKey: false, shiftKey: false, preventDefault() {} });
  }

  return (
    <div
      className={[isSelected && 'is-selected', isMultiSelected && 'is-multi-selected'].filter(Boolean).join(' ') || undefined}
      tabIndex={0}
      data-entry-uid={entry.entry_uid}
      onMouseDown={e => { if (e.shiftKey) e.preventDefault() }}
      onClick={e => onRowClick(entry, e)}
      onKeyDown={e => { if (e.key === 'Enter') onRowClick(entry, e) }}
    >
      <div className="col-check">
        <button
          type="button"
          className={`row-checkbox${checked ? ' is-checked' : ''}`}
          aria-pressed={checked}
          aria-label={checked ? 'Deselect entry' : 'Select entry'}
          onClick={handleCheckboxClick}
          onKeyDown={e => e.stopPropagation()}
        />
      </div>
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
