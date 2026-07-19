import { useState, useEffect } from 'react';
import TagsView from './TagsView';

// Installs a per-story fetch stub that intercepts /api/ tag mutations and
// returns realistic Tag shapes so renameTag/moveTag/createTag don't throw.
// Installed in useEffect so it never leaks into other stories and won't
// double-wrap on re-renders.
function ApiStub({ children }) {
  useEffect(() => {
    const realFetch = window.fetch.bind(window);
    window.fetch = (url, opts) => {
      if (typeof url === 'string' && url.startsWith('/api/')) {
        const method = (opts?.method ?? 'GET').toUpperCase();
        if (method === 'DELETE') {
          return Promise.resolve(new Response(null, { status: 204 }));
        }
        // Parse request body to construct a realistic Tag shape.
        // renameTag sends { name }, moveTag/createTag send path info.
        // full_path must be present or callers throw on updated.full_path.
        let body = {};
        try { body = JSON.parse(opts?.body ?? '{}'); } catch { /* ignore */ }
        const slug = (body.name ?? body.path ?? 'stub')
          .trim().replace(/\s+/g, '-').replace(/[^a-zA-Z0-9-]/g, '').replace(/^-+|-+$/g, '') || 'stub';
        const stubTag = {
          tag_uid: 'stub-uid',
          name: slug.replace(/-/g, ' ').replace(/\b\w/g, c => c.toUpperCase()),
          slug,
          full_path: `/${slug}`,
        };
        return Promise.resolve(
          new Response(JSON.stringify(stubTag), {
            status: method === 'POST' ? 201 : 200,
            headers: { 'Content-Type': 'application/json' },
          })
        );
      }
      return realFetch(url, opts);
    };
    return () => { window.fetch = realFetch; };
  }, []);
  return children;
}

function withApiStub(Story) {
  return <ApiStub><Story /></ApiStub>;
}

export default {
  component: TagsView,
  tags: ['autodocs'],
  parameters: { layout: 'padded' },
  decorators: [withApiStub],
};

// ── Shared fixtures ───────────────────────────────────────────────────────

const noop = () => {};

function tag(tag_uid, name, slug, full_path, entry_count = 0, children = [], subtree_count = null) {
  return { tag: { tag_uid, name, slug, full_path }, entry_count, subtree_count: subtree_count ?? entry_count, children };
}

const sampleTree = [
  tag('t1', 'Science', 'science', '/science', 12, [
    tag('t2', 'Computer Science', 'computer-science', '/science/computer-science', 7, [
      tag('t3', 'Algorithms', 'algorithms', '/science/computer-science/algorithms', 3),
      tag('t4', 'Compilers', 'compilers', '/science/computer-science/compilers', 1),
    ], 11),
    tag('t5', 'Physics', 'physics', '/science/physics', 4),
  ], 23),
  tag('t6', 'History', 'history', '/history', 5, [
    tag('t7', 'Ancient', 'ancient', '/history/ancient', 2),
  ], 7),
  tag('t8', 'Reading List', 'reading-list', '/reading-list', 0),
];

// Wrapper that wires local state so onTagsRefresh/onTagRenamed callbacks
// keep the tree consistent within a story session.
function TagsViewSandbox({ initialNodes = sampleTree, tagFilter = null, humanizeTags = false }) {
  const [nodes, setNodes] = useState(initialNodes);
  const [filter, setFilter] = useState(tagFilter);

  function handleTagRenamed(oldPath, newPath) {
    if (filter === oldPath) setFilter(newPath);
    else if (filter?.startsWith(oldPath + '/')) setFilter(newPath + filter.slice(oldPath.length));
  }

  function handleTagDeleted(deletedPath) {
    if (filter === deletedPath || filter?.startsWith(deletedPath + '/')) setFilter(null);
  }

  // onTagsRefresh is a no-op here: in a real app it re-fetches; the stub
  // tag returned from fetch won't match our fixture tree, so the tree stays
  // as-is after mutations. That is acceptable for visual QA purposes.

  return (
    <div style={{ maxWidth: 340, fontFamily: 'Helvetica Neue, sans-serif' }}>
      <TagsView
        archiveId="demo"
        tagNodes={nodes}
        tagFilter={filter}
        onTagFilterSet={setFilter}
        onViewChange={noop}
        onTagRenamed={handleTagRenamed}
        onTagDeleted={handleTagDeleted}
        onTagsRefresh={noop}
        humanizeTags={humanizeTags}
      />
    </div>
  );
}

// ── Stories ───────────────────────────────────────────────────────────────

/** Default view with a nested tag tree. All interactions are exercisable:
 *  click + New / Move to open the picker modal; click a tag to filter;
 *  double-click or pencil to rename; × to delete.
 *  API mutations are stubbed — the tree won't re-fetch after saves,
 *  but no network errors will occur. */
export const Default = {
  render: () => <TagsViewSandbox />,
};

/** Empty archive — "No tags yet." shown; + New still works. */
export const Empty = {
  render: () => <TagsViewSandbox initialNodes={[]} />,
};

/** Humanize mode: slugs displayed as Title Case names. */
export const HumanizedNames = {
  render: () => <TagsViewSandbox humanizeTags />,
};

/** Active tag filter — header shows the current filter path. */
export const WithActiveFilter = {
  render: () => (
    <TagsViewSandbox tagFilter="/science/computer-science" />
  ),
};

/** Flat list with no nesting. */
export const FlatList = {
  render: () => (
    <TagsViewSandbox
      initialNodes={[
        tag('a1', 'Books', 'books', '/books', 8),
        tag('a2', 'Films', 'films', '/films', 3),
        tag('a3', 'Music', 'music', '/music', 0),
        tag('a4', 'Podcasts', 'podcasts', '/podcasts', 14),
      ]}
    />
  ),
};

/** Tags with large entry counts — verifies count badge layout. */
export const HighCounts = {
  render: () => (
    <TagsViewSandbox
      initialNodes={[
        tag('h1', 'All', 'all', '/all', 9999, [
          tag('h2', 'Starred', 'starred', '/all/starred', 432),
          tag('h3', 'Archive', 'archive', '/all/archive', 1204),
        ]),
      ]}
    />
  ),
};
