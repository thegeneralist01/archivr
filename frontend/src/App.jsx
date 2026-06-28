import { useState, useEffect, useCallback, useRef, createContext } from 'react'
import { fetchArchives, fetchEntries, searchEntries, fetchRuns, fetchTags, checkSetup, fetchMe } from './api'
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

export const AuthContext = createContext(null);

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

  const [archives, setArchives] = useState([])
  const [archiveId, setArchiveId] = useState(null)
  const [entries, setEntries] = useState([])
  const [selectedEntryUid, setSelectedEntryUid] = useState(null)
  const [selectedEntry, setSelectedEntry] = useState(null)
  const [tagFilter, setTagFilter] = useState(null)
  const [view, setView] = useState('archive')
  const [searchQuery, setSearchQuery] = useState('')
  const [resultCount, setResultCount] = useState('')
  const [searchBusy, setSearchBusy] = useState(false)
  const [runs, setRuns] = useState([])
  const [tagNodes, setTagNodes] = useState([])
  const [captureDialogOpen, setCaptureDialogOpen] = useState(false)

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
      setResultCount(results.length === 0 ? 'No results' : `${results.length} result${results.length === 1 ? '' : 's'}`)
    } catch {
      setEntries([])
      setResultCount('Search failed. Try again.')
    } finally {
      setSearchBusy(false)
    }
  }, [])

  // Mount: load archives, pick first, then parallel load
  useEffect(() => {
    fetchArchives().then(list => {
      setArchives(list)
      if (list.length > 0) {
        const first = list[0].id
        setArchiveId(first)
      }
    })
  }, [])

  // Archive change: parallel load entries + runs + tags
  useEffect(() => {
    if (!archiveId) return
    setTagFilter(null)
    setSelectedEntry(null)
    setSelectedEntryUid(null)
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

  // Tag filter change: switch to archive view, reload
  useEffect(() => {
    if (archiveId === null) return
    setView('archive')
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

  const selectEntry = useCallback((entry) => {
    setSelectedEntryUid(entry ? entry.entry_uid : null)
    setSelectedEntry(entry)
  }, [])

  const handleTagFilterSet = useCallback((fullPath) => {
    setTagFilter(fullPath)
  }, [])

  const handleClearTagFilter = useCallback(() => {
    setTagFilter(null)
  }, [])

  const handleTagsRefresh = useCallback(() => {
    if (archiveId) fetchTags(archiveId).then(setTagNodes)
  }, [archiveId])

  const handleCaptureClick = useCallback(() => {
    setCaptureDialogOpen(true)
  }, [])

  const handleCaptureClose = useCallback(() => {
    setCaptureDialogOpen(false)
  }, [])

  const handleCaptured = useCallback(() => {
    if (!archiveId) return
    Promise.all([
      loadEntries(archiveId, searchQuery, tagFilter),
      fetchRuns(archiveId).then(setRuns),
    ])
  }, [archiveId, searchQuery, tagFilter, loadEntries])

  if (authState === 'loading') return <div className="auth-loading">Loading\u2026</div>;
  if (authState === 'setup')   return <SetupPage onComplete={() => setAuthState('login')} />;
  if (authState === 'login')   return <LoginPage onLogin={user => { setCurrentUser(user); setAuthState('authenticated'); }} />;

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
                      × {tagFilter}
                    </button>
                  )}
                </span>
              </div>
            )}
            {view === 'archive' && (
              <EntriesView
                entries={entries}
                selectedEntryUid={selectedEntryUid}
                onSelectEntry={selectEntry}
                archiveId={archiveId}
                tagFilter={tagFilter}
                onClearTagFilter={handleClearTagFilter}
                searchQuery={searchQuery}
                onSearchChange={setSearchQuery}
                resultCount={resultCount}
                searchBusy={searchBusy}
              />
            )}
            {view === 'runs' && <RunsView runs={runs} />}
            {view === 'admin' && <AdminView archives={archives} />}
            {view === 'tags' && (
              <TagsView
                tagNodes={tagNodes}
                tagFilter={tagFilter}
                onTagFilterSet={handleTagFilterSet}
                onViewChange={handleViewChange}
              />
            )}
            {view === 'collections' && (
              <CollectionsView archiveId={archiveId} />
            )}
            {view === 'settings' && (
              <SettingsView />
            )}
          </div>
          <ContextRail
            archiveId={archiveId}
            selectedEntry={selectedEntry}
            onTagFilterSet={handleTagFilterSet}
            tagNodes={tagNodes}
            onTagsRefresh={handleTagsRefresh}
          />
        </main>
        <CaptureDialog
          open={captureDialogOpen}
          archiveId={archiveId}
          onClose={handleCaptureClose}
          onCaptured={handleCaptured}
        />
      </>
    </AuthContext.Provider>
  )
}
