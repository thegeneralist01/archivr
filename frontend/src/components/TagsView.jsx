import { useState, useRef, useEffect } from 'react';
import { renameTag, deleteTag, createTag, moveTag } from '../api';

// ── TagPickerNode ─────────────────────────────────────────────────────────
// A node inside the destination picker modal.
function TagPickerNode({ node, onPick }) {
  return (
    <li>
      <button
        className="tag-picker-node-btn"
        title={node.tag.full_path}
        onClick={() => onPick(node.tag)}
      >
        {node.tag.slug}
      </button>
      {node.children?.length > 0 && (
        <div className="tag-children">
          <ul className="tag-tree-list">
            {node.children.map(child => (
              <TagPickerNode key={child.tag.tag_uid} node={child} onPick={onPick} />
            ))}
          </ul>
        </div>
      )}
    </li>
  );
}

// ── TagPickerModal ────────────────────────────────────────────────────────
// Shared modal for "create under" and "move under" destination selection.
// `excludeUid` hides that tag and all its descendants (used for move).
// `onPick(tag | null)` — null means "make root / no parent".
function TagPickerModal({ title, tagNodes, excludeUid, onPick, onCancel }) {
  useEffect(() => {
    function onKey(e) { if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); onCancel(); } }
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [onCancel]);
  function filterTree(nodes) {
    return nodes
      .filter(n => n.tag.tag_uid !== excludeUid)
      .map(n => ({ ...n, children: filterTree(n.children) }));
  }
  const visibleNodes = excludeUid ? filterTree(tagNodes) : tagNodes;

  return (
    <div
      className="tag-picker-backdrop"
      onClick={e => { if (e.target === e.currentTarget) onCancel(); }}
    >
      <div className="tag-picker-modal" role="dialog" aria-modal="true">
        <div className="tag-picker-header">
          <span className="tag-picker-title">{title}</span>
          <button
            className="tag-picker-close"
            onClick={onCancel}
            title="Cancel"
            aria-label="Cancel"
          >×</button>
        </div>
        <div className="tag-picker-body">
          <button
            className="tag-picker-root-btn"
            onClick={() => onPick(null)}
            title="Place at root level (no parent)"
          >
            ↑ Root tag (no parent)
          </button>
          {visibleNodes.length > 0 ? (
            <ul className="tag-tree-list tag-picker-tree">
              {visibleNodes.map(node => (
                <TagPickerNode key={node.tag.tag_uid} node={node} onPick={onPick} />
              ))}
            </ul>
          ) : (
            <p className="tag-picker-empty">No other tags available.</p>
          )}
        </div>
      </div>
    </div>
  );
}

// ── CreateInput ───────────────────────────────────────────────────────────
// Inline text input that appears in the tag tree when creating a new tag.
// Matches the rename-input UX: Enter saves, Escape cancels, blur saves.
function CreateInput({ parentPath, archiveId, onDone, onCancel }) {
  const [draft, setDraft] = useState('');
  const cancelRef = useRef(false);

  async function submit() {
    const name = draft.trim();
    if (!name) { onCancel(); return; }
    const path = parentPath ? `${parentPath}/${name}` : `/${name}`;
    try {
      await createTag(archiveId, path);
      onDone();
    } catch (err) {
      alert(err.message || 'Create failed');
      onCancel();
    }
  }

  return (
    <input
      className="tag-rename-input"
      autoFocus
      placeholder="tag name"
      value={draft}
      onChange={e => setDraft(e.target.value)}
      onKeyDown={e => {
        if (e.key === 'Enter') e.currentTarget.blur();
        if (e.key === 'Escape') { cancelRef.current = true; e.currentTarget.blur(); }
      }}
      onBlur={() => {
        if (cancelRef.current) { cancelRef.current = false; onCancel(); }
        else { submit(); }
      }}
    />
  );
}

// ── TagNode ───────────────────────────────────────────────────────────────
function TagNode({
  node,
  archiveId,
  tagFilter,
  onTagFilterSet,
  onViewChange,
  onTagRenamed,
  onTagDeleted,
  onTagsRefresh,
  humanizeTags,
  // Move source-selection mode: clicking a tag selects it as the thing to move.
  moveSelectMode,
  onMoveSourceSelect,
  // Pending create: the tag_uid of the parent that should render an inline input,
  // or '__root__' for the root list (handled in TagsView, not here).
  pendingCreateParentUid,
  onCreateDone,
  onCreateCancel,
}) {
  const isActive = tagFilter === node.tag.full_path;
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState('');
  const cancelRef = useRef(false);

  function handleClick() {
    if (editing) return;
    if (moveSelectMode) {
      onMoveSourceSelect(node);
      return;
    }
    const next = isActive ? null : node.tag.full_path;
    onTagFilterSet(next);
    onViewChange('archive');
  }

  function startEdit(e) {
    e.stopPropagation();
    if (moveSelectMode) return;
    setDraft(node.tag.slug);
    setEditing(true);
  }

  async function handleRenameSave() {
    const value = draft.trim();
    if (!value || value === node.tag.slug) { setEditing(false); return; }
    try {
      const updated = await renameTag(archiveId, node.tag.tag_uid, value);
      onTagRenamed(node.tag.full_path, updated.full_path);
      onTagsRefresh();
    } catch {
      // silently revert
    } finally {
      setEditing(false);
    }
  }

  async function handleDelete(e) {
    e.stopPropagation();
    const msg = node.children?.length > 0
      ? `Delete tag "${node.tag.full_path}" and all its child tags? This cannot be undone.`
      : `Delete tag "${node.tag.full_path}"? This cannot be undone.`;
    if (!window.confirm(msg)) return;
    try {
      await deleteTag(archiveId, node.tag.tag_uid);
      onTagDeleted(node.tag.full_path);
      onTagsRefresh();
    } catch {
      // silently ignore
    }
  }

  const childProps = {
    archiveId, tagFilter, onTagFilterSet, onViewChange, onTagRenamed, onTagDeleted,
    onTagsRefresh, humanizeTags, moveSelectMode, onMoveSourceSelect,
    pendingCreateParentUid, onCreateDone, onCreateCancel,
  };

  const showCreateInput = pendingCreateParentUid === node.tag.tag_uid;
  const hasChildren = node.children?.length > 0;

  return (
    <li>
      <div className={`tag-node-row${moveSelectMode ? ' tag-node-row--move-select' : ''}`}>
        {editing ? (
          <input
            className="tag-rename-input"
            autoFocus
            value={draft}
            onChange={e => setDraft(e.target.value)}
            onKeyDown={e => {
              if (e.key === 'Enter') e.currentTarget.blur();
              if (e.key === 'Escape') { cancelRef.current = true; e.currentTarget.blur(); }
            }}
            onBlur={() => {
              if (cancelRef.current) { cancelRef.current = false; setEditing(false); }
              else { handleRenameSave(); }
            }}
          />
        ) : (
          <button
            className={`tag-node-btn${isActive ? ' is-active' : ''}${moveSelectMode ? ' tag-node-btn--move-select' : ''}`}
            title={moveSelectMode ? `Select "${node.tag.full_path}" to move` : node.tag.full_path}
            onClick={handleClick}
            onDoubleClick={moveSelectMode ? undefined : startEdit}
          >
            <span className="tag-node-label">{humanizeTags ? node.tag.name : node.tag.slug}</span>
            <span className="tag-node-count">
              {node.children.length === 0
                ? `(${node.entry_count})`
                : `(${node.entry_count}) (${node.subtree_count} Total)`}
            </span>
            {!moveSelectMode && (
              <svg
                className="edit-icon"
                viewBox="0 0 16 16"
                fill="none"
                stroke="currentColor"
                strokeWidth="1.5"
                strokeLinecap="round"
                strokeLinejoin="round"
                aria-hidden="true"
                title="Rename tag"
                onClick={e => { e.stopPropagation(); startEdit(e); }}
              >
                <path d="M11.5 2.5a1.5 1.5 0 0 1 2 2L5 13l-3 1 1-3 8.5-8.5z"/>
              </svg>
            )}
          </button>
        )}
        {!editing && !moveSelectMode && (
          <button
            className="remove tag-node-delete"
            title={`Delete "${node.tag.full_path}"`}
            onClick={handleDelete}
            aria-label={`Delete "${node.tag.full_path}"`}
          >×</button>
        )}
      </div>
      {(hasChildren || showCreateInput) && (
        <div className="tag-children">
          <ul className="tag-tree-list">
            {node.children.map(child => (
              <TagNode key={child.tag.tag_uid} node={child} {...childProps} />
            ))}
            {showCreateInput && (
              <li>
                <CreateInput
                  parentPath={node.tag.full_path}
                  archiveId={archiveId}
                  onDone={onCreateDone}
                  onCancel={onCreateCancel}
                />
              </li>
            )}
          </ul>
        </div>
      )}
    </li>
  );
}

// ── TagsView ──────────────────────────────────────────────────────────────
export default function TagsView({
  archiveId, tagNodes, tagFilter, onTagFilterSet, onViewChange,
  onTagRenamed, onTagDeleted, onTagsRefresh, humanizeTags,
}) {
  // ── Move state ────────────────────────────────────────────────────────
  // moveStep: null | 'select-source' | 'select-dest'
  const [moveStep, setMoveStep] = useState(null);
  const [moveSourceNode, setMoveSourceNode] = useState(null);

  function startMoveMode() { setMoveStep('select-source'); setMoveSourceNode(null); }
  function cancelMove() { setMoveStep(null); setMoveSourceNode(null); }
  // Esc while in source-selection mode cancels the move.
  useEffect(() => {
    if (moveStep !== 'select-source') return;
    function onKey(e) { if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); cancelMove(); } }
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [moveStep]);

  function handleMoveSourceSelect(node) {
    setMoveSourceNode(node);
    setMoveStep('select-dest');
  }

  async function handleMoveDest(destTag) {
    // destTag: Tag object | null (null = make root)
    const sourceOldPath = moveSourceNode.tag.full_path;
    const sourceUid = moveSourceNode.tag.tag_uid;
    const destUid = destTag?.tag_uid ?? null;
    cancelMove();
    try {
      const updated = await moveTag(archiveId, sourceUid, destUid);
      // Keep the active tag filter consistent when the moved tag/subtree was active.
      onTagRenamed(sourceOldPath, updated.full_path);
      onTagsRefresh();
    } catch (err) {
      alert(err.message || 'Move failed');
    }
  }

  // ── Create state ──────────────────────────────────────────────────────
  // createStep: null | 'select-parent' | 'input'
  const [createStep, setCreateStep] = useState(null);
  const [createParentTag, setCreateParentTag] = useState(undefined);

  function startCreateMode() { setCreateStep('select-parent'); setCreateParentTag(undefined); }
  function cancelCreate() { setCreateStep(null); setCreateParentTag(undefined); }

  function handleCreateParentPick(tag) {
    // tag: Tag | null (null = root)
    setCreateParentTag(tag ?? null);
    setCreateStep('input');
  }

  function handleCreateDone() {
    setCreateStep(null);
    setCreateParentTag(undefined);
    onTagsRefresh();
  }

  // ── Derived ───────────────────────────────────────────────────────────
  const moveSelectMode = moveStep === 'select-source';

  // The tag_uid of the parent that should render a create input inside its
  // children list. '__root__' means the root-level list handles it.
  const pendingCreateParentUid = createStep === 'input'
    ? (createParentTag ? createParentTag.tag_uid : '__root__')
    : undefined;

  const nodeProps = {
    archiveId, tagFilter, onTagFilterSet, onViewChange, onTagRenamed, onTagDeleted,
    onTagsRefresh, humanizeTags, moveSelectMode, onMoveSourceSelect: handleMoveSourceSelect,
    pendingCreateParentUid, onCreateDone: handleCreateDone, onCreateCancel: cancelCreate,
  };

  const showRootCreateInput = pendingCreateParentUid === '__root__';

  return (
    <section id="tags-view" className="view is-active">
      <div className="tag-tree">
        <div className="tag-tree-header">
          {moveSelectMode ? (
            <>
              <span className="tag-tree-title tag-tree-title--move">Select a tag to move</span>
              <button
                className="tag-tree-action-btn tag-tree-action-btn--cancel"
                onClick={cancelMove}
              >Cancel</button>
            </>
          ) : (
            <>
              <span className="tag-tree-title">Tags</span>
              {tagFilter && (
                <span className="tag-tree-active" title={tagFilter}>Filtering: {tagFilter}</span>
              )}
              <div className="tag-tree-actions">
                <button
                  className="tag-tree-action-btn"
                  onClick={startCreateMode}
                  title="Create a new tag"
                  disabled={!!createStep || !!moveStep}
                >+ New</button>
                <button
                  className="tag-tree-action-btn"
                  onClick={startMoveMode}
                  title="Move a tag to a different parent"
                  disabled={!!createStep || !!moveStep || tagNodes.length === 0}
                >Move</button>
              </div>
            </>
          )}
        </div>

        {tagNodes.length === 0 && !showRootCreateInput ? (
          <p className="muted" style={{ padding: '8px 0' }}>No tags yet.</p>
        ) : (
          <ul className="tag-tree-list">
            {tagNodes.map(node => (
              <TagNode key={node.tag.tag_uid} node={node} {...nodeProps} />
            ))}
            {showRootCreateInput && (
              <li>
                <CreateInput
                  parentPath={null}
                  archiveId={archiveId}
                  onDone={handleCreateDone}
                  onCancel={cancelCreate}
                />
              </li>
            )}
          </ul>
        )}
      </div>

      {/* Create: pick parent modal */}
      {createStep === 'select-parent' && (
        <TagPickerModal
          title="Create tag under…"
          tagNodes={tagNodes}
          excludeUid={null}
          onPick={handleCreateParentPick}
          onCancel={cancelCreate}
        />
      )}

      {/* Move: pick destination modal (shown after source is selected) */}
      {moveStep === 'select-dest' && moveSourceNode && (
        <TagPickerModal
          title={`Move "${moveSourceNode.tag.slug}" under…`}
          tagNodes={tagNodes}
          excludeUid={moveSourceNode.tag.tag_uid}
          onPick={handleMoveDest}
          onCancel={cancelMove}
        />
      )}
    </section>
  );
}
