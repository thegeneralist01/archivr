import { useState, useRef, useEffect } from 'react';
import { formatTimestamp, formatBytes, valueText, sourceIconSvg } from '../utils';
import { fetchEntryChildren, reorderEntryChildren } from '../api';

function ChildRow({
  entry, index, onRowClick, selectedUids,
  isFirst, isLast, reorderDisabled, onMove, canReorder,
  isDragging, dropEdge, onHandleDragStart, onHandleDragEnd, onRowDragOver, onRowDrop,
}) {
  const isSelected = (selectedUids?.size === 1) && selectedUids.has(entry.entry_uid);
  const isMultiSelected = (selectedUids?.size >= 2) && selectedUids.has(entry.entry_uid);
  const label = valueText(entry.title) || valueText(entry.entry_uid);

  const cls = ['child-entry-row',
    index % 2 === 0 ? 'child-entry-row--light' : 'child-entry-row--dark',
    isSelected && 'is-selected',
    isMultiSelected && 'is-multi-selected',
    isDragging && 'is-dragging',
    dropEdge === 'before' && 'is-drop-before',
    dropEdge === 'after' && 'is-drop-after',
  ].filter(Boolean).join(' ');

  return (
    <div
      className={cls}
      tabIndex={0}
      data-entry-uid={entry.entry_uid}
      onMouseDown={e => { if (e.shiftKey) e.preventDefault(); }}
      onClick={e => onRowClick(entry, e)}
      onDragOver={canReorder ? onRowDragOver : undefined}
      onDrop={canReorder ? onRowDrop : undefined}
      aria-keyshortcuts={canReorder ? 'Alt+ArrowUp Alt+ArrowDown' : undefined}
      onKeyDown={e => {
        if (canReorder && e.altKey && (e.key === 'ArrowUp' || e.key === 'ArrowDown')) {
          e.preventDefault();
          if (!reorderDisabled) onMove(e.key === 'ArrowUp' ? -1 : 1);
          return;
        }
        if (e.key === 'Enter') onRowClick(entry, e);
      }}
    >
      <div className="col-check" aria-hidden="true" />
      <div className="col-added">{formatTimestamp(entry.archived_at)}</div>
      <div className="col-title">
        {canReorder && (
        <span className="child-reorder-controls">
          <span
            className="child-drag-handle"
            draggable={!reorderDisabled}
            title="Drag to reorder (or Alt+↑/↓)"
            aria-hidden="true"
            onClick={e => e.stopPropagation()}
            onDragStart={onHandleDragStart}
            onDragEnd={onHandleDragEnd}
          >⋮⋮</span>
          <button
            type="button"
            className="child-move-btn"
            aria-label={`Move ${label} up`}
            disabled={reorderDisabled || isFirst}
            onClick={e => { e.stopPropagation(); onMove(-1); }}
            onKeyDown={e => e.stopPropagation()}
          >↑</button>
          <button
            type="button"
            className="child-move-btn"
            aria-label={`Move ${label} down`}
            disabled={reorderDisabled || isLast}
            onClick={e => { e.stopPropagation(); onMove(1); }}
            onKeyDown={e => e.stopPropagation()}
          >↓</button>
        </span>
        )}
        <span className="source-icon">
          <span dangerouslySetInnerHTML={{ __html: sourceIconSvg(entry.source_kind) }} />
        </span>
        <span className="entry-title">{label}</span>
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

export default function EntryRow({ entry, archiveId, rowIndex, isSelected, isMultiSelected, onRowClick, selectedUids, deletedUids, renamedTitles, isPublicSession, canReorder = false, onReorderError }) {
  const [favFailed, setFavFailed] = useState(false);
  const [expanded, setExpanded] = useState(false);
  const [children, setChildren] = useState(null);
  const [childrenLoading, setChildrenLoading] = useState(false);
  const [reorderSaving, setReorderSaving] = useState(false);
  const [dragUid, setDragUid] = useState(null);
  const [dropTarget, setDropTarget] = useState(null); // { uid, after: bool }
  const childListRef = useRef(null);
  const focusUidRef = useRef(null);

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
  const hasChildren = entry.child_count > 0 && !isPublicSession;

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

  // Deleted children are hidden locally and already gone server-side, so the
  // filtered list is exactly the set the server expects. Renames are applied on
  // top of the fetched list, since that list is only fetched once per expansion.
  const visibleChildren = (children ?? [])
    .filter(c => !deletedUids?.has(c.entry_uid))
    .map(c => (renamedTitles?.has(c.entry_uid) ? { ...c, title: renamedTitles.get(c.entry_uid) } : c));

  async function commitChildOrder(next) {
    const prev = children;
    setChildren(next); // optimistic
    setReorderSaving(true);
    try {
      await reorderEntryChildren(archiveId, entry.entry_uid, next.map(c => c.entry_uid));
    } catch (err) {
      setChildren(prev); // revert
      onReorderError?.(err.message);
      if (err.status === 400) { // stale set: resync with the server
        try { setChildren(await fetchEntryChildren(archiveId, entry.entry_uid)); } catch (_) { /* keep reverted list */ }
      }
    } finally {
      setReorderSaving(false);
    }
  }

  function moveChild(fromIdx, toIdx) {
    if (!canReorder || reorderSaving || fromIdx === toIdx || toIdx < 0 || toIdx >= visibleChildren.length) return;
    const next = visibleChildren.slice();
    const [moved] = next.splice(fromIdx, 1);
    next.splice(toIdx, 0, moved);
    commitChildOrder(next);
  }

  function handleChildMove(idx, delta) {
    focusUidRef.current = visibleChildren[idx]?.entry_uid ?? null;
    moveChild(idx, idx + delta);
  }

  // Keep keyboard focus on the moved row (React re-inserts keyed nodes,
  // which can drop focus in some browsers).
  useEffect(() => {
    const uid = focusUidRef.current;
    if (!uid || !childListRef.current) return;
    focusUidRef.current = null;
    const row = childListRef.current.querySelector(`[data-entry-uid="${CSS.escape(uid)}"]`);
    if (row && !row.contains(document.activeElement)) row.focus();
  }, [children]);

  function handleDragStart(uid, e) {
    e.stopPropagation();
    e.dataTransfer.effectAllowed = 'move';
    e.dataTransfer.setData('text/plain', uid); // Firefox requires data to start a drag
    const rowEl = e.currentTarget.closest('.child-entry-row');
    if (rowEl) e.dataTransfer.setDragImage(rowEl, 16, rowEl.offsetHeight / 2);
    setDragUid(uid);
  }
  function handleDragEnd() { setDragUid(null); setDropTarget(null); }
  function handleRowDragOver(uid, e) {
    if (!dragUid) return; // foreign drags (files, other parents) are not droppable
    e.preventDefault();
    e.dataTransfer.dropEffect = 'move';
    const rect = e.currentTarget.getBoundingClientRect();
    const after = e.clientY > rect.top + rect.height / 2;
    if (dropTarget?.uid !== uid || dropTarget?.after !== after) setDropTarget({ uid, after });
  }
  function handleRowDrop(uid, e) {
    if (!dragUid) return;
    e.preventDefault();
    const fromIdx = visibleChildren.findIndex(c => c.entry_uid === dragUid);
    const targetIdx = visibleChildren.findIndex(c => c.entry_uid === uid);
    const after = dropTarget?.uid === uid ? dropTarget.after : false;
    handleDragEnd();
    if (fromIdx === -1 || targetIdx === -1) return;
    let toIdx = targetIdx + (after ? 1 : 0);
    if (fromIdx < toIdx) toIdx -= 1;
    moveChild(fromIdx, toIdx);
  }

  const outerClass = [
    'entry-row-outer',
    rowIndex % 2 === 0 ? 'entry-row-outer--light' : 'entry-row-outer--dark',
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
          {entry.cached_bytes > 0 && entry.cacheable_bytes > 0 && (
            <span className="size-cached-pct" title={`${formatBytes(entry.cached_bytes)} already on disk from an earlier entry`}>
              {Math.round(entry.cached_bytes / entry.cacheable_bytes * 100)}% cached
            </span>
          )}
        </div>
        <div className="url-cell col-url">{valueText(entry.original_url)}</div>
      </div>
      {expanded && (
        <>
          {childrenLoading && <div className="child-entries-loading">Loading…</div>}
          <div
            className="child-entries"
            ref={childListRef}
            aria-busy={reorderSaving}
            aria-label={`${entry.child_count} child entries`}
          >
            {visibleChildren.map((child, idx) => (
              <ChildRow
                key={child.entry_uid}
                entry={child}
                index={idx}
                onRowClick={onRowClick}
                selectedUids={selectedUids}
                isFirst={idx === 0}
                isLast={idx === visibleChildren.length - 1}
                reorderDisabled={reorderSaving}
                canReorder={canReorder}
                onMove={delta => handleChildMove(idx, delta)}
                isDragging={dragUid === child.entry_uid}
                dropEdge={dropTarget?.uid === child.entry_uid && dragUid !== child.entry_uid ? (dropTarget.after ? 'after' : 'before') : null}
                onHandleDragStart={e => handleDragStart(child.entry_uid, e)}
                onHandleDragEnd={handleDragEnd}
                onRowDragOver={e => handleRowDragOver(child.entry_uid, e)}
                onRowDrop={e => handleRowDrop(child.entry_uid, e)}
              />
            ))}
          </div>
        </>
      )}
    </div>
  );
}
