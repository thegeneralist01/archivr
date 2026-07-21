import { useState, useEffect, useCallback, useRef, useMemo, createContext } from 'react'
import { fetchArchives, fetchEntries, searchEntries, fetchRuns, fetchTags, checkSetup, fetchMe, fetchEntryDetail } from './api'
import LoginPage from './components/LoginPage.jsx'
import SetupPage from './components/SetupPage.jsx'

import Topbar from './components/Topbar'
import CaptureDialog from './components/CaptureDialog'
import EntriesView from './components/EntriesView'
import RunsView from './components/RunsView'
import AdminView from './components/AdminView'
import TagsView from './components/TagsView'
import CollectionsView from './components/CollectionsView'
import SettingsView from './components/SettingsView'
import ContextRail from './components/ContextRail'
import PreviewModal from './components/PreviewModal'
import AudioBar from './components/AudioBar'
import PreviewPage from './components/PreviewPage'
import { displayPath } from './utils'
import ToastStack from './components/ToastStack'

export const AuthContext = createContext(null);

// Detect /preview/:archiveId/:entryUid at load time (static — no navigation)
const PREVIEW_ROUTE = (() => {
  const m = window.location.pathname.match(/^\/preview\/([^/]+)\/([^/]+)/)
  return m ? { archiveId: m[1], entryUid: m[2] } : null
})()

const VIEWS = ['archive','tags','collections','runs','admin','settings']
const SETTINGS_TABS = ['profile','tokens','instance','cookies','extensions','storage']

function parseLocation() {
  const parts = window.location.pathname.split('/').filter(Boolean)
  const view = VIEWS.includes(parts[0]) ? parts[0] : 'archive'
  const settingsTab = (view === 'settings' && SETTINGS_TABS.includes(parts[1])) ? parts[1] : 'profile'
  const params = new URLSearchParams(window.location.search)
  const q = params.get('q') ?? ''
  const tag = view === 'archive' ? (params.get('tag') ?? null) : null
  const entry = view === 'archive' ? (params.get('entry') ?? null) : null
  return { view, settingsTab, q, tag, entry }
}

function locationPath(view, settingsTab) {
  if (view === 'archive') return '/'
  if (view === 'settings') return `/settings/${settingsTab}`
  return `/${view}`
}

export default function App() {
  const [authState, setAuthState] = useState('loading');
  const [currentUser, setCurrentUser] = useState(null);

  useEffect(() => {
    (async () => {
      const needsSetup = await checkSetup();
      if (needsSetup) { setAuthState('setup'); return; }
      const user = await fetchMe();
      if (!user) { setAuthState('login'); return; }
      setCurrentUser(user);
      setAuthState('authenticated');
    })();
  }, []);

  useEffect(() => {
    const handler = () => { setCurrentUser(null); setAuthState('login'); };
    window.addEventListener('auth:expired', handler);
    return () => window.removeEventListener('auth:expired', handler);
  }, []);

  // Sync URL → state on back/forward
  useEffect(() => {
    const handler = () => {
      const { view, settingsTab, q, tag, entry } = parseLocation()
      setView(view)
      setSettingsTab(settingsTab)
      setSearchQuery(q)
      setTagFilter(tag)
      setSelectedEntryUid(entry)
      setSelectedEntry(null)
      setSelectedUids(entry ? new Set([entry]) : new Set())
    }
    window.addEventListener('popstate', handler)
    return () => window.removeEventListener('popstate', handler)
  }, [])

  const [archives, setArchives] = useState([])
  const [archiveId, setArchiveId] = useState(null)
  const [entries, setEntries] = useState([])
  const [deletedUids, setDeletedUids] = useState(() => new Set())
  const [selectedEntryUid, setSelectedEntryUid] = useState(() => parseLocation().entry)
  const [selectedEntry, setSelectedEntry] = useState(null)
  const [selectedUids, setSelectedUids] = useState(() => {
    const e = parseLocation().entry
    return e ? new Set([e]) : new Set()
  })
  const [tagFilter, setTagFilter] = useState(() => parseLocation().tag)
  const [view, setView] = useState(() => parseLocation().view)
  const [settingsTab, setSettingsTab] = useState(() => parseLocation().settingsTab)
  const [searchQuery, setSearchQuery] = useState(() => parseLocation().q)
  const [resultCount, setResultCount] = useState('')
  const [searchBusy, setSearchBusy] = useState(false)
  const [runs, setRuns] = useState([])
  const [tagNodes, setTagNodes] = useState([])
  const [captureDialogOpen, setCaptureDialogOpen] = useState(() => {
    const saved = sessionStorage.getItem('captureDialogOpen')
    return saved === 'true'
  })

  const [toasts, setToasts] = useState([])
  const toastIdRef = useRef(0)
  const [pendingCaptures, setPendingCaptures] = useState(() => {
    try {
      const saved = JSON.parse(sessionStorage.getItem('pendingCaptures') || '[]')
      if (Array.isArray(saved)) return saved
    } catch {}
    return []
  })

  useEffect(() => {
    sessionStorage.setItem('pendingCaptures', JSON.stringify(pendingCaptures))
  }, [pendingCaptures])

  const [ublockWarningIgnored, setUblockWarningIgnored] = useState(
    () => sessionStorage.getItem('ublockWarningIgnored') === 'true'
  )


  // ── Entry detail (shared between PreviewPanel and ContextRail) ──────────
  const [entryDetail, setEntryDetail] = useState(null)
  const detailSeqRef = useRef(0)
  const searchInputRef = useRef(null)
  const pendingSearchFocus = useRef(false)
  const firstArchiveLoad = useRef(true)
  const lastAnchorIndexRef = useRef(null)
  // uid → entry object cache; populated on every row click so ctrl/shift
  // selections can resolve child entries that aren't in the root entries array.
  const entryCacheRef = useRef(new Map())

  const humanizeTags = currentUser?.humanize_slugs ?? false;

  // Fetch entry detail whenever selected entry changes
  useEffect(() => {
    const seq = ++detailSeqRef.current
    setEntryDetail(null)
    if (!selectedEntry || !archiveId) return
    fetchEntryDetail(archiveId, selectedEntry.entry_uid).then(det => {
      if (seq !== detailSeqRef.current) return
      setEntryDetail(det)
    }).catch(() => {})
  }, [selectedEntry, archiveId])

  // Persist captureDialogOpen to sessionStorage
  useEffect(() => {
    sessionStorage.setItem('captureDialogOpen', captureDialogOpen)
  }, [captureDialogOpen])

  const loadEntries = useCallback(async (aid, q, tag) => {
    if (!aid) return
    setSearchBusy(true)
    try {
      let results
      if (q || tag) {
        results = await searchEntries(aid, q, tag)
      } else {
        results = await fetchEntries(aid)
      }
      setEntries(results)
      // Prune multi-selection to only entries still visible after load.
      // Single-select (size 1) survives naturally; bulk-select is clipped to visible rows.
      setSelectedUids(prev => {
        if (prev.size < 2) return prev  // single-select persists across search/filter
        const visible = new Set(results.map(e => e.entry_uid))
        const pruned = new Set([...prev].filter(uid => visible.has(uid)))
        return pruned.size === prev.size ? prev : pruned
      })
      setResultCount(results.length === 0 ? 'No results' : `${results.length} result${results.length === 1 ? '' : 's'}`)
    } catch {
      setEntries([])
      setResultCount('Search failed. Try again.')
    } finally {
      setSearchBusy(false)
    }
  }, [])

  // Load archives once authenticated (re-runs when authState changes so
  // it triggers correctly after first login or a session refresh).
  useEffect(() => {
    if (authState !== 'authenticated') return
    fetchArchives().then(list => {
      setArchives(list)
      if (list.length > 0) {
        const first = list[0].id
        setArchiveId(first)
      }
    })
  }, [authState])

  // Archive change: parallel load entries + runs + tags
  useEffect(() => {
    if (!archiveId) return
    if (firstArchiveLoad.current) {
      // First load: URL-initialized filters are already in state; the debounced
      // search and tagFilter effects will call loadEntries with the right values.
      firstArchiveLoad.current = false
      Promise.all([
        fetchRuns(archiveId).then(setRuns),
        fetchTags(archiveId).then(setTagNodes),
      ])
      return
    }
    setTagFilter(null)
    setSelectedEntry(null)
    setSelectedEntryUid(null)
    setSelectedUids(new Set())
    Promise.all([
      loadEntries(archiveId, '', null),
      fetchRuns(archiveId).then(setRuns),
      fetchTags(archiveId).then(setTagNodes),
    ])
  }, [archiveId]) // intentionally not including loadEntries to avoid re-running on its recreation

  // Debounced search
  useEffect(() => {
    if (archiveId === null) return
    const timer = setTimeout(() => {
      loadEntries(archiveId, searchQuery, tagFilter)
    }, 300)
    return () => clearTimeout(timer)
  }, [searchQuery, archiveId]) // tagFilter handled separately below

  // Tag filter applied: switch to archive view and reload.
  // Only reset view when tagFilter is non-null; archive change alone (tagFilter=null)
  // must not stomp the URL-initialised view on first load.
  useEffect(() => {
    if (archiveId === null) return
    if (tagFilter !== null) setView('archive')
    loadEntries(archiveId, searchQuery, tagFilter)
  }, [tagFilter, archiveId]) // intentional: searchQuery excluded to avoid double-fire

  const handleArchiveChange = useCallback((id) => {
    setArchiveId(id)
  }, [])

  const handleViewChange = useCallback((name) => {
    setView(name)
    if (name === 'tags' && archiveId) {
      fetchTags(archiveId).then(setTagNodes)
    }
  }, [archiveId])

  // Sync view + settingsTab → URL (skip when serving a standalone preview page)
  // Preserve existing search params so ?q/tag/entry survive view navigation.
  useEffect(() => {
    if (PREVIEW_ROUTE) return
    const path = locationPath(view, settingsTab)
    if (window.location.pathname !== path) {
      history.pushState(null, '', path + window.location.search)
    }
  }, [view, settingsTab])

  const selectEntry = useCallback((entry) => {
    setSelectedEntryUid(entry ? entry.entry_uid : null)
    setSelectedEntry(entry)
  }, [])

  const handleRowClick = useCallback((entry, e) => {
    // Cache every clicked entry so shift/ctrl can resolve child entries
    // that are not present in the root `entries` array.
    entryCacheRef.current.set(entry.entry_uid, entry)

    if (e.shiftKey && lastAnchorIndexRef.current !== null) {
      e.preventDefault()
      // Use DOM order so child rows (not in the `entries` array) participate
      // in range selection. Every rendered row has data-entry-uid.
      const allNodes = [...document.querySelectorAll('#entries-body [data-entry-uid]')]
      const anchorIdx = allNodes.findIndex(n => n.dataset.entryUid === lastAnchorIndexRef.current)
      const clickIdx  = allNodes.findIndex(n => n.dataset.entryUid === entry.entry_uid)
      if (anchorIdx === -1 || clickIdx === -1) {
        lastAnchorIndexRef.current = entry.entry_uid
        setSelectedUids(new Set([entry.entry_uid]))
        selectEntry(entry)
        return
      }
      const lo = Math.min(anchorIdx, clickIdx)
      const hi = Math.max(anchorIdx, clickIdx)
      const uids = new Set(allNodes.slice(lo, hi + 1).map(n => n.dataset.entryUid))
      setSelectedUids(uids)
      if (uids.size === 1) selectEntry(entry)
    } else if (e.ctrlKey || e.metaKey) {
      lastAnchorIndexRef.current = entry.entry_uid
      const next = new Set(selectedUids)
      if (next.has(entry.entry_uid)) next.delete(entry.entry_uid)
      else next.add(entry.entry_uid)
      setSelectedUids(next)
      if (next.size === 0) {
        selectEntry(null)
      } else if (next.size === 1) {
        // Resolve the remaining UID — may be a child not in the root entries array
        // and not yet cached (e.g. picked up via shift-range without a direct click).
        const [remainingUid] = next
        const cached = entryCacheRef.current.get(remainingUid)
            ?? entries.find(x => x.entry_uid === remainingUid)
            ?? null
        if (cached) {
          selectEntry(cached)
        } else {
          // Cache miss — fetch from server rather than clearing the detail panel.
          fetchEntryDetail(archiveId, remainingUid)
            .then(det => { if (det?.summary) selectEntry(det.summary) })
            .catch(() => {})
        }
      } else {
        selectEntry(null)
      }
    } else {
      lastAnchorIndexRef.current = entry.entry_uid
      setSelectedUids(new Set([entry.entry_uid]))
      selectEntry(entry)
    }
  }, [entries, selectedUids, selectEntry, archiveId])

  const handleTagFilterSet = useCallback((fullPath) => {
    setTagFilter(fullPath)
  }, [])

  const handleClearTagFilter = useCallback(() => {
    setTagFilter(null)
  }, [])

  const handleTagsRefresh = useCallback(() => {
    if (archiveId) fetchTags(archiveId).then(setTagNodes)
  }, [archiveId])

  const handleTagRenamed = useCallback((oldFullPath, newFullPath) => {
    if (tagFilter === oldFullPath) {
      setTagFilter(newFullPath);
    } else if (tagFilter?.startsWith(oldFullPath + '/')) {
      setTagFilter(newFullPath + tagFilter.slice(oldFullPath.length));
    }
  }, [tagFilter]);

  const handleTagDeleted = useCallback((deletedFullPath) => {
    if (tagFilter === deletedFullPath || tagFilter?.startsWith(deletedFullPath + '/')) {
      setTagFilter(null);
    }
  }, [tagFilter]);

  const handleEntryTitleChange = useCallback((entryUid, newTitle) => {
    setEntries(prev => prev.map(e =>
      e.entry_uid === entryUid ? { ...e, title: newTitle } : e
    ))
    setSelectedEntry(prev =>
      prev && prev.entry_uid === entryUid ? { ...prev, title: newTitle } : prev
    )
    setEntryDetail(prev =>
      prev && prev.summary.entry_uid === entryUid
        ? { ...prev, summary: { ...prev.summary, title: newTitle } }
        : prev
    )
  }, [])

  const handleDetailRefresh = useCallback(() => {
    if (!archiveId || !selectedEntry) return
    const seq = ++detailSeqRef.current
    fetchEntryDetail(archiveId, selectedEntry.entry_uid).then(det => {
      if (seq !== detailSeqRef.current) return
      setEntryDetail(det)
    }).catch(() => {})
  }, [archiveId, selectedEntry])

  const handleEntryDeleted = useCallback((entryUid) => {
    const isRoot = entries.some(e => e.entry_uid === entryUid)
    setDeletedUids(prev => { const n = new Set(prev); n.add(entryUid); return n })
    setEntries(prev => prev.filter(e => e.entry_uid !== entryUid))
    setSelectedEntry(prev => prev?.entry_uid === entryUid ? null : prev)
    setSelectedEntryUid(prev => prev === entryUid ? null : prev)
    setSelectedUids(prev => { const n = new Set(prev); n.delete(entryUid); return n })
    // Child delete: parent row's child_count/size are stale — reload after state updates.
    if (!isRoot) loadEntries(archiveId, searchQuery, tagFilter)
  }, [entries, archiveId, searchQuery, tagFilter, loadEntries])

  const handleBulkDeleted = useCallback((uids) => {
    const rootUids = new Set(entries.map(e => e.entry_uid))
    const hasChildDelete = [...uids].some(u => !rootUids.has(u))
    setDeletedUids(prev => { const n = new Set(prev); uids.forEach(u => n.add(u)); return n })
    setEntries(prev => prev.filter(e => !uids.has(e.entry_uid)))
    setSelectedUids(new Set())
    setSelectedEntry(null)
    setSelectedEntryUid(null)
    if (hasChildDelete) loadEntries(archiveId, searchQuery, tagFilter)
  }, [entries, archiveId, searchQuery, tagFilter, loadEntries])

  // Auto-snap: drive selectedEntryUid from selectedUids so URL sync and detail
  // panel stay correct. size >= 2 clears single-entry state (bulk panel takes over).
  useEffect(() => {
    if (selectedUids.size >= 2) {
      setSelectedEntryUid(null)
      setSelectedEntry(null)
    } else if (selectedUids.size === 1) {
      const [uid] = selectedUids
      setSelectedEntryUid(uid)
      // selectedEntry object restored by the existing restore effect below
    } else {
      setSelectedEntryUid(null)
      setSelectedEntry(null)
    }
  }, [selectedUids])

  const selectedEntries = useMemo(
    () => entries.filter(e => selectedUids.has(e.entry_uid)),
    [entries, selectedUids]
  )

  // Restore selectedEntry object from selectedEntryUid when entries load.
  // Handles page refresh and back/forward navigation where only the UID is known.
  useEffect(() => {
    if (!selectedEntryUid || selectedEntry) return
    const found = entries.find(e => e.entry_uid === selectedEntryUid)
    if (found) setSelectedEntry(found)
  }, [entries, selectedEntryUid, selectedEntry])

  // Sync search params → URL via replaceState (no new history entry).
  useEffect(() => {
    if (PREVIEW_ROUTE) return
    const params = new URLSearchParams()
    if (searchQuery) params.set('q', searchQuery)
    if (view === 'archive' && tagFilter) params.set('tag', tagFilter)
    if (view === 'archive' && selectedEntryUid) params.set('entry', selectedEntryUid)
    const qs = params.toString()
    const url = window.location.pathname + (qs ? '?' + qs : '')
    const current = window.location.pathname + window.location.search
    if (current !== url) history.replaceState(null, '', url)
  }, [searchQuery, tagFilter, selectedEntryUid, view])

  // ⌘K / Ctrl+K: focus the search input, switching to archive view first if needed.
  useEffect(() => {
    const handler = (e) => {
      if ((e.metaKey || e.ctrlKey) && e.key === 'k') {
        e.preventDefault()
        if (view === 'archive') {
          searchInputRef.current?.focus()
          searchInputRef.current?.select()
        } else {
          pendingSearchFocus.current = true
          setView('archive')
        }
      }
    }
    document.addEventListener('keydown', handler)
    return () => document.removeEventListener('keydown', handler)
  }, [view])

  // j/k: navigate entries down/up when not focused on an input element.
  useEffect(() => {
    const handler = (e) => {
      // Ignore when a modifier is held (don't steal browser/app shortcuts)
      if (e.metaKey || e.ctrlKey || e.altKey) return
      if (e.key !== 'j' && e.key !== 'k') return
      // Ignore when focus is on any editable target
      const tag = document.activeElement?.tagName
      if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT') return
      if (document.activeElement?.isContentEditable) return

      const allNodes = [...document.querySelectorAll('#entries-body [data-entry-uid]')]
      if (allNodes.length === 0) return

      const [currentUid] = selectedUids.size === 1 ? selectedUids : [null]
      const currentIdx = currentUid
        ? allNodes.findIndex(n => n.dataset.entryUid === currentUid)
        : -1

      const nextIdx = e.key === 'j'
        ? Math.min(currentIdx + 1, allNodes.length - 1)
        : Math.max(currentIdx - 1, 0)

      if (nextIdx === currentIdx && currentIdx !== -1) return

      const nextNode = allNodes[nextIdx < 0 ? 0 : nextIdx]
      const nextUid = nextNode.dataset.entryUid

      lastAnchorIndexRef.current = nextUid
      setSelectedUids(new Set([nextUid]))

      // Resolve entry object: cache → root entries array → server fetch
      const cached = entryCacheRef.current.get(nextUid)
          ?? entries.find(x => x.entry_uid === nextUid)
          ?? null
      if (cached) {
        selectEntry(cached)
      } else {
        fetchEntryDetail(archiveId, nextUid)
          .then(det => { if (det?.summary) selectEntry(det.summary) })
          .catch(() => {})
      }

      nextNode.scrollIntoView({ block: 'nearest' })
      e.preventDefault()
    }
    document.addEventListener('keydown', handler)
    return () => document.removeEventListener('keydown', handler)
  }, [selectedUids, entries, selectEntry, archiveId])

  // After switching to archive view via ⌘K, focus the search input once rendered.
  useEffect(() => {
    if (view === 'archive' && pendingSearchFocus.current) {
      pendingSearchFocus.current = false
      requestAnimationFrame(() => {
        searchInputRef.current?.focus()
        searchInputRef.current?.select()
      })
    }
  }, [view])

  const handleCaptureClick = useCallback(() => {
    setCaptureDialogOpen(true)
  }, [])

  const handleCaptureClose = useCallback(() => {
    setCaptureDialogOpen(false)
  }, [])

  const handleCaptured = useCallback(() => {
    if (!archiveId) return
    return Promise.allSettled([
      loadEntries(archiveId, searchQuery, tagFilter),
      fetchRuns(archiveId).then(setRuns),
    ])
  }, [archiveId, searchQuery, tagFilter, loadEntries])

  const handleToast = useCallback((text, locator, type = 'error', headline = null) => {
    // Only suppress per-item ublock/cookie warnings (those carry a locator).
    // Batch summary warnings (locator = null) must always show.
    if (type === 'warning' && ublockWarningIgnored && locator) return
    const id = ++toastIdRef.current
    setToasts(prev => [...prev, { id, text, locator, type, headline }])
  }, [ublockWarningIgnored])

  const handleDismissToast = useCallback((id) => {
    setToasts(prev => prev.filter(t => t.id !== id))
  }, [])

  const handleIgnoreUblock = useCallback(() => {
    sessionStorage.setItem('ublockWarningIgnored', 'true')
    setUblockWarningIgnored(true)
    setToasts(prev => prev.filter(t => !(t.type === 'warning' && t.locator)))
  }, [])

  const handleJobStarted = useCallback((record) => {
    setPendingCaptures(prev => [...prev, record])
  }, [])

  const handleJobSettled = useCallback((id) => {
    setPendingCaptures(prev => prev.filter(c => c.id !== id))
  }, [])

  const [previewEntryUid, setPreviewEntryUid] = useState(null)
  const [currentAudio, setCurrentAudio] = useState(null)

  const handleOpenPreview = useCallback(() => {
    if (selectedEntry) setPreviewEntryUid(selectedEntry.entry_uid)
  }, [selectedEntry])
  const handleClosePreview = useCallback(() => setPreviewEntryUid(null), [])
  const handlePlay = useCallback((src, entry) => {
    setCurrentAudio({ src, entry })
  }, [])
  const handleCloseAudio = useCallback(() => setCurrentAudio(null), [])

  // Close stale modal when selection changes (audio persists intentionally)
  useEffect(() => {
    setPreviewEntryUid(null)
  }, [selectedEntry])

  // Esc: deselect current entry/entries, unless a modal or input has focus.
  useEffect(() => {
    const handler = (e) => {
      if (e.key !== 'Escape') return
      if (captureDialogOpen || previewEntryUid) return
      const tag = document.activeElement?.tagName
      if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT') return
      if (selectedUids.size > 0) {
        e.preventDefault()
        setSelectedUids(new Set())
        document.activeElement?.blur?.()
      }
    }
    window.addEventListener('keydown', handler)
    return () => window.removeEventListener('keydown', handler)
  }, [captureDialogOpen, previewEntryUid, selectedUids])

  // Toggle body class so fixed AudioBar doesn't obscure scrollable content
  useEffect(() => {
    document.body.classList.toggle('has-audio-bar', !!currentAudio)
    return () => document.body.classList.remove('has-audio-bar')
  }, [currentAudio])

  if (authState === 'loading') return <div className="auth-loading">Loading\u2026</div>;
  if (authState === 'setup')   return <SetupPage onComplete={() => setAuthState('login')} />;
  if (authState === 'login')   return <LoginPage onLogin={user => { setCurrentUser(user); setAuthState('authenticated'); }} />;
  if (PREVIEW_ROUTE)           return <PreviewPage archiveId={PREVIEW_ROUTE.archiveId} entryUid={PREVIEW_ROUTE.entryUid} />;

  return (
    <AuthContext.Provider value={{ currentUser, setCurrentUser }}>
      <>
        <Topbar
          archives={archives}
          archiveId={archiveId}
          onArchiveChange={handleArchiveChange}
          view={view}
          onViewChange={handleViewChange}
          onCaptureClick={handleCaptureClick}
        />
        <main className="app-shell">
          <div className="workspace">
            {view === 'archive' && (
              <div className="toolbar">
                <div className="search-field">
                  <span className="ico" aria-hidden="true">
                    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
                      <circle cx="11" cy="11" r="7"/><line x1="16.5" y1="16.5" x2="21" y2="21"/>
                    </svg>
                  </span>
                  <input
                    ref={searchInputRef}
                    className="search-input"
                    type="search"
                    aria-label="Search archive"
                    aria-busy={searchBusy}
                    placeholder="Search titles, URLs, types, tags…"
                    value={searchQuery}
                    onChange={e => setSearchQuery(e.target.value)}
                  />
                  <span className="kbd">⌘K</span>
                </div>
                <span className="result-count">
                  {resultCount && <><b>{resultCount.split(' ')[0]}</b>{' '}{resultCount.split(' ').slice(1).join(' ')}</>}
                  {tagFilter && (
                    <button className="tag-filter-badge" onClick={handleClearTagFilter}>
                      × {humanizeTags ? displayPath(tagFilter) : tagFilter}
                    </button>
                  )}
                </span>
              </div>
            )}
            {view === 'archive' && (
            <EntriesView
                entries={entries}
                selectedUids={selectedUids}
                onRowClick={handleRowClick}
                archiveId={archiveId}
                pendingCaptures={pendingCaptures}
                deletedUids={deletedUids}
              />
            )}
            {view === 'runs' && <RunsView runs={runs} />}
            {view === 'admin' && <AdminView archives={archives} />}
            {view === 'tags' && (
              <TagsView
                archiveId={archiveId}
                tagNodes={tagNodes}
                tagFilter={tagFilter}
                onTagFilterSet={handleTagFilterSet}
                onViewChange={handleViewChange}
                onTagRenamed={handleTagRenamed}
                onTagDeleted={handleTagDeleted}
                onTagsRefresh={handleTagsRefresh}
                humanizeTags={humanizeTags}
              />
            )}
            {view === 'collections' && (
              <CollectionsView archiveId={archiveId} />
            )}
            {view === 'settings' && (
              <SettingsView tab={settingsTab} onTabChange={setSettingsTab} archiveId={archiveId} />
            )}
          </div>
          <ContextRail
            archiveId={archiveId}
            selectedEntry={selectedEntry}
            selectedUids={selectedUids}
            selectedEntries={selectedEntries}
            detail={entryDetail}
            onTagFilterSet={handleTagFilterSet}
            tagNodes={tagNodes}
            onTagsRefresh={handleTagsRefresh}
            onEntryTitleChange={handleEntryTitleChange}
            onEntryDeleted={handleEntryDeleted}
            onBulkDeleted={handleBulkDeleted}
            humanizeTags={humanizeTags}
            onDetailRefresh={handleDetailRefresh}
            onOpenPreview={handleOpenPreview}
            onPlay={handlePlay}
          />
        </main>
        {previewEntryUid && selectedEntry && selectedEntry.entry_uid === previewEntryUid && (
          <PreviewModal
            archiveId={archiveId}
            entry={selectedEntry}
            detail={entryDetail}
            onClose={handleClosePreview}
          />
        )}
        {currentAudio && (
          <AudioBar
            entry={currentAudio.entry}
            src={currentAudio.src}
            archiveId={archiveId}
            onClose={handleCloseAudio}
          />
        )}
        <CaptureDialog
          open={captureDialogOpen}
          archiveId={archiveId}
          onClose={handleCaptureClose}
          onCaptured={handleCaptured}
          onToast={handleToast}
          activeJobs={pendingCaptures}
          onJobStarted={handleJobStarted}
          onJobSettled={handleJobSettled}
        />
        <ToastStack toasts={toasts} onDismiss={handleDismissToast} onIgnoreUblock={handleIgnoreUblock} />
      </>
    </AuthContext.Provider>
  )
}
