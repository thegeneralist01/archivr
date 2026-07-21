import { useState } from 'react';
import { formatTimestamp, formatBytes, valueText, sourceIconSvg } from '../utils';
import { fetchEntryChildren } from '../api';

function ChildRow({ entry, index, onRowClick, selectedUids }) {
  const isSelected = (selectedUids?.size === 1) && selectedUids.has(entry.entry_uid);
  const isMultiSelected = (selectedUids?.size >= 2) && selectedUids.has(entry.entry_uid);

  const cls = ['child-entry-row',
    index % 2 === 0 ? 'child-entry-row--light' : 'child-entry-row--dark',
    isSelected && 'is-selected',
    isMultiSelected && 'is-multi-selected',
  ].filter(Boolean).join(' ');

  return (
    <div
      className={cls}
      tabIndex={0}
      data-entry-uid={entry.entry_uid}
      onMouseDown={e => { if (e.shiftKey) e.preventDefault(); }}
      onClick={e => onRowClick(entry, e)}
      onKeyDown={e => { if (e.key === 'Enter') onRowClick(entry, e); }}
    >
      <div className="col-check" aria-hidden="true" />
      <div className="col-added">{formatTimestamp(entry.archived_at)}</div>
      <div className="col-title">
        <span className="source-icon">
          <span dangerouslySetInnerHTML={{ __html: sourceIconSvg(entry.source_kind) }} />
        </span>
        <span className="entry-title">{valueText(entry.title) || valueText(entry.entry_uid)}</span>
      </div>
      <div className="col-type">
        <span className="type-pill">{valueText(entry.entity_kind)}</span>
      </div>
      <div className="col-size">
        <span className="size-total">{formatBytes(entry.total_artifact_bytes)}</span>
      </div>
      <div className="url-cell col-url">{valueText(entry.original_url)}</div>
    </div>
  );
}

export default function EntryRow({ entry, archiveId, isSelected, isMultiSelected, onRowClick, selectedUids, deletedUids }) {
  const [favFailed, setFavFailed] = useState(false);
  const [expanded, setExpanded] = useState(false);
  const [children, setChildren] = useState(null);
  const [childrenLoading, setChildrenLoading] = useState(false);

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
  const hasChildren = entry.child_count > 0;

  function handleCheckboxClick(e) {
    e.stopPropagation();
    onRowClick(entry, { ctrlKey: true, metaKey: false, shiftKey: false, preventDefault() {} });
  }

  async function handleExpandClick(e) {
    e.stopPropagation();
    if (expanded) {
      setExpanded(false);
      return;
    }
    setExpanded(true);
    if (children === null && !childrenLoading) {
      setChildrenLoading(true);
      try {
        const result = await fetchEntryChildren(archiveId, entry.entry_uid);
        setChildren(result);
      } catch (_) {
        setChildren([]);
      } finally {
        setChildrenLoading(false);
      }
    }
  }

  const outerClass = [
    'entry-row-outer',
    isSelected && 'is-selected',
    isMultiSelected && 'is-multi-selected',
  ].filter(Boolean).join(' ');

  return (
    <div className={outerClass} data-entry-uid={entry.entry_uid}>
      <div
        className="entry-row-main"
        tabIndex={0}
        onMouseDown={e => { if (e.shiftKey) e.preventDefault(); }}
        onClick={e => onRowClick(entry, e)}
        onKeyDown={e => { if (e.key === 'Enter') onRowClick(entry, e); }}
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
          {hasChildren && (
            <button
              type="button"
              className={`entry-expand-btn${expanded ? ' is-expanded' : ''}`}
              aria-label={expanded ? 'Collapse children' : `Expand ${entry.child_count} items`}
              aria-expanded={expanded}
              onClick={handleExpandClick}
              onKeyDown={e => e.stopPropagation()}
            />
          )}
          <span className="source-icon">{icon}</span>
          <span className="entry-title">{valueText(entry.title) || valueText(entry.entry_uid)}</span>
          {hasChildren && (
            <span className="child-count-badge" aria-hidden="true">{entry.child_count}</span>
          )}
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
      {expanded && (
        <>
          {childrenLoading && <div className="child-entries-loading">Loading…</div>}
          <div className="child-entries" aria-label={`${entry.child_count} child entries`}>
            {children && children
              .filter(c => !deletedUids?.has(c.entry_uid))
              .map((child, idx) => (
                <ChildRow
                  key={child.entry_uid}
                  entry={child}
                  index={idx}
                  onRowClick={onRowClick}
                  selectedUids={selectedUids}
                />
              ))}
          </div>
        </>
      )}
    </div>
  );
}
