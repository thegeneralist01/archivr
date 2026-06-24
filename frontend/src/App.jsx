import { useState, useEffect, useCallback, useRef } from 'react'
import { fetchArchives, fetchEntries, searchEntries, fetchRuns, fetchTags } from './api'
import Topbar from './components/Topbar'
import CaptureDialog from './components/CaptureDialog'
import EntriesView from './components/EntriesView'
import RunsView from './components/RunsView'
import AdminView from './components/AdminView'
import TagsView from './components/TagsView'
import ContextRail from './components/ContextRail'

export default function App() {
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

  return (
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
            <div className="search-row">
              <input
                className="search-input"
                type="search"
                aria-label="Search archive"
                aria-busy={searchBusy}
                value={searchQuery}
                onChange={e => setSearchQuery(e.target.value)}
              />
              <div className="result-count">
                {resultCount}
                {tagFilter && (
                  <button className="tag-filter-badge" onClick={handleClearTagFilter}>
                    × {tagFilter}
                  </button>
                )}
              </div>
            </div>
          )}
          {view === 'archive' && (
            <EntriesView
              entries={entries}
              selectedEntryUid={selectedEntryUid}
              onSelectEntry={selectEntry}
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
  )
}
