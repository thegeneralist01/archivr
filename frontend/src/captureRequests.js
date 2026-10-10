// A supplied capture request must never replace the ordinary web dialog draft.
export function capturePersistenceKey(key = 'captureItems', initialItems, requestKey) {
  return key === 'captureItems' && (initialItems != null || requestKey != null)
    ? `${key}:request:${requestKey ?? 'seeded'}` : key
}

export function seedCaptureItems(initialItems) {
  return (Array.isArray(initialItems) ? initialItems : []).map(item => item.kind === 'text'
    ? { kind: 'text', title: typeof item.title === 'string' ? item.title : '', body: typeof item.body === 'string' ? item.body : '', mime: item.mime === 'text/plain' ? 'text/plain' : 'text/markdown' }
    : { locator: typeof item.locator === 'string' ? item.locator : '' })
}

export function captureDraftItems(saved, initialItems, requestKey) {
  const separateRequest = initialItems != null || requestKey != null
  const matching = separateRequest
    ? saved?.requestKey === (requestKey ?? null) && Array.isArray(saved?.items) && saved.items
    : Array.isArray(saved) && saved
  return matching || seedCaptureItems(initialItems)
}

export function applyGeneratedTitle(item, snapshot, title) {
  if (item.titleRequestId !== snapshot.titleRequestId) return item
  const unchanged = item.draftRevision === snapshot.draftRevision && item.title === snapshot.title && item.body === snapshot.body
  return { ...item, titleBusy: false, ...(unchanged ? { title, draftRevision: (item.draftRevision ?? 0) + 1 } : {}) }
}

export function applyTitleGenerationError(item, requestId, message) {
  return item.titleRequestId === requestId ? { ...item, titleBusy: false, titleError: message } : item
}

export function parseCaptureLink(search) {
  const params = new URLSearchParams(search)
  return params.has('capture') ? { archiveId: params.get('archive'), locator: params.get('capture') } : null
}

export function resolveCaptureLink(link, archives) {
  if (!link?.archiveId) return { error: 'Capture link must specify an archive.' }
  if (!archives.some(archive => archive.id === link.archiveId)) return { error: `Capture archive "${link.archiveId}" is unavailable. Choose an archive and try again.` }
  if (!link.locator?.trim()) return { error: 'Capture link has an empty locator.' }
  return { archiveId: link.archiveId, locator: link.locator }
}

export function makeCaptureLinkRequest(resolved) {
  return {
    ...resolved,
    initialItems: [{ locator: resolved.locator }],
    // Counters reset on full-page navigation, but sessionStorage does not.
    requestKey: globalThis.crypto?.randomUUID?.() ?? `capture-${Date.now()}-${Math.random()}`,
    open: true,
  }
}
