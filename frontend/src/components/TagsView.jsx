import { useState, useRef } from 'react';
import { renameTag, deleteTag } from '../api';

function TagNode({ node, archiveId, tagFilter, onTagFilterSet, onViewChange, onTagRenamed, onTagDeleted, onTagsRefresh }) {
  const isActive = tagFilter === node.tag.full_path;
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState('');
  const cancelRef = useRef(false);

  function handleFilterClick() {
    if (editing) return;
    const next = isActive ? null : node.tag.full_path;
    onTagFilterSet(next);
    onViewChange('archive');
  }

  function startEdit(e) {
    e.stopPropagation();
    setDraft(node.tag.slug);
    setEditing(true);
  }

  async function handleRenameSave() {
    const value = draft.trim();
    if (!value || value === node.tag.slug) {
      setEditing(false);
      return;
    }
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

  const childProps = { archiveId, tagFilter, onTagFilterSet, onViewChange, onTagRenamed, onTagDeleted, onTagsRefresh };

  return (
    <li>
      <div className="tag-node-row">
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
              if (cancelRef.current) {
                cancelRef.current = false;
                setEditing(false);
              } else {
                handleRenameSave();
              }
            }}
          />
        ) : (
          <button
            className={`tag-node-btn${isActive ? ' is-active' : ''}`}
            title={node.tag.full_path}
            onClick={handleFilterClick}
            onDoubleClick={startEdit}
          >
            {node.tag.name}
            <svg
              className="edit-icon"
              viewBox="0 0 16 16"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.5"
              strokeLinecap="round"
              strokeLinejoin="round"
              aria-hidden="true"
              onClick={e => { e.stopPropagation(); startEdit(e); }}
            >
              <path d="M11.5 2.5a1.5 1.5 0 0 1 2 2L5 13l-3 1 1-3 8.5-8.5z"/>
            </svg>
          </button>
        )}
        <button
          className="remove tag-node-delete"
          title={`Delete tag ${node.tag.full_path}`}
          onClick={handleDelete}
          aria-label={`Delete tag ${node.tag.full_path}`}
        >×</button>
      </div>
      {node.children?.length > 0 && (
        <div className="tag-children">
          <ul className="tag-tree-list">
            {node.children.map(child => (
              <TagNode key={child.tag.tag_uid} node={child} {...childProps} />
            ))}
          </ul>
        </div>
      )}
    </li>
  );
}

export default function TagsView({ archiveId, tagNodes, tagFilter, onTagFilterSet, onViewChange, onTagRenamed, onTagDeleted, onTagsRefresh }) {
  return (
    <section id="tags-view" className="view is-active">
      <div className="tag-tree">
        <div className="tag-tree-header">
          <span className="tag-tree-title">Tags</span>
          {tagFilter && (
            <span className="tag-tree-active">Filtering: {tagFilter}</span>
          )}
        </div>
        {tagNodes.length === 0 ? (
          <p className="muted" style={{ padding: '8px 0' }}>No tags yet.</p>
        ) : (
          <ul className="tag-tree-list">
            {tagNodes.map(node => (
              <TagNode key={node.tag.tag_uid} node={node}
                archiveId={archiveId}
                tagFilter={tagFilter}
                onTagFilterSet={onTagFilterSet}
                onViewChange={onViewChange}
                onTagRenamed={onTagRenamed}
                onTagDeleted={onTagDeleted}
                onTagsRefresh={onTagsRefresh}
              />
            ))}
          </ul>
        )}
      </div>
    </section>
  );
}
