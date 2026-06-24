function TagNode({ node, tagFilter, onTagFilterSet, onViewChange }) {
  const isActive = tagFilter === node.tag.full_path
  function handleClick() {
    const next = isActive ? null : node.tag.full_path
    onTagFilterSet(next)
    onViewChange('archive')
  }
  return (
    <li>
      <button
        className={`tag-node-btn${isActive ? ' is-active' : ''}`}
        title={node.tag.full_path}
        onClick={handleClick}>
        {node.tag.name}
      </button>
      {node.children?.length > 0 && (
        <div className="tag-children">
          <ul className="tag-tree-list">
            {node.children.map(child => (
              <TagNode key={child.tag.tag_uid} node={child}
                tagFilter={tagFilter} onTagFilterSet={onTagFilterSet} onViewChange={onViewChange} />
            ))}
          </ul>
        </div>
      )}
    </li>
  )
}

export default function TagsView({ tagNodes, tagFilter, onTagFilterSet, onViewChange }) {
  return (
    <section id="tags-view" className="view is-active">
      <div className="tag-tree">
        {tagNodes.length === 0 ? (
          <div>No tags yet.</div>
        ) : (
          <ul className="tag-tree-list">
            {tagNodes.map(node => (
              <TagNode key={node.tag.tag_uid} node={node}
                tagFilter={tagFilter} onTagFilterSet={onTagFilterSet} onViewChange={onViewChange} />
            ))}
          </ul>
        )}
      </div>
    </section>
  )
}
