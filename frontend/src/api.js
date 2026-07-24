async function getJson(url) {
  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(`${response.status} ${response.statusText}`);
  }
  return response.json();
}

export async function fetchArchives() {
  return getJson("/api/archives");
}

export async function fetchEntries(archiveId) {
  return getJson(`/api/archives/${archiveId}/entries`);
}

export async function searchEntries(archiveId, q, tag) {
  const params = new URLSearchParams();
  if (q) params.set("q", q);
  if (tag) params.set("tag", tag);
  return getJson(`/api/archives/${archiveId}/entries/search?${params}`);
}

export async function fetchEntryDetail(archiveId, entryUid) {
  return getJson(`/api/archives/${archiveId}/entries/${entryUid}`);
}

export async function fetchEntryChildren(archiveId, entryUid) {
  return getJson(`/api/archives/${archiveId}/entries/${entryUid}/children`);
}

// Fetch multiple artifact JSON payloads for an entry in parallel.
// Returns a Promise<Array> preserving index order.
export function fetchEntryArtifacts(archiveId, entryUid, indices) {
  return Promise.all(
    indices.map(idx =>
      getJson(`/api/archives/${archiveId}/entries/${entryUid}/artifacts/${idx}`)
    )
  );
}

// Resolve t.co short URLs server-side (HEAD→GET, no-follow).
// Returns a map { [tcoUrl]: expandedUrl }.
// Silently returns {} on failure — callers fall back to plain t.co links.
export async function resolveTcoUrls(urls) {
  if (!urls || urls.length === 0) return {};
  const res = await fetch('/api/util/resolve-tco', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(urls),
  });
  if (!res.ok) return {};
  return res.json();
}

export async function updateEntryTitle(archiveId, entryUid, title) {
  const res = await fetch(`/api/archives/${archiveId}/entries/${entryUid}`, {
    method: 'PATCH',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ title: title ?? null }),
  });
  if (!res.ok) throw new Error(await res.text());
}

export async function fetchEntryTags(archiveId, entryUid) {
  return getJson(`/api/archives/${archiveId}/entries/${entryUid}/tags`);
}

export async function assignTag(archiveId, entryUid, tagPath) {
  const resp = await fetch(
    `/api/archives/${archiveId}/entries/${entryUid}/tags`,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ tag_path: tagPath }),
    }
  );
  if (!resp.ok) throw new Error(`Failed to add tag (${resp.status})`);
}

export async function removeTag(archiveId, entryUid, tagUid) {
  const resp = await fetch(
    `/api/archives/${archiveId}/entries/${entryUid}/tags/${tagUid}`,
    { method: "DELETE" }
  );
  if (!resp.ok) throw new Error(`Remove failed (${resp.status})`);
}

export async function deleteEntry(archiveId, entryUid) {
  const resp = await fetch(
    `/api/archives/${archiveId}/entries/${entryUid}`,
    { method: 'DELETE' }
  );
  if (!resp.ok) throw new Error(`Delete failed (${resp.status})`);
}

export async function renameTag(archiveId, tagUid, name) {
  const res = await fetch(
    `/api/archives/${archiveId}/tags/${tagUid}`,
    {
      method: 'PATCH',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ name }),
    }
  );
  if (!res.ok) throw new Error(await res.text());
  return res.json(); // returns the updated Tag: { tag_uid, name, slug, full_path }
}

export async function deleteTag(archiveId, tagUid) {
  const res = await fetch(
    `/api/archives/${archiveId}/tags/${tagUid}`,
    { method: 'DELETE' }
  );
  if (!res.ok) throw new Error(await res.text());
}

export async function createTag(archiveId, path) {
  const res = await fetch(`/api/archives/${archiveId}/tags`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ path }),
  });
  if (!res.ok) throw new Error(await res.text());
  return res.json();
}

export async function moveTag(archiveId, tagUid, parentUid) {
  const res = await fetch(`/api/archives/${archiveId}/tags/${tagUid}/move`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ parent_uid: parentUid ?? null }),
  });
  if (!res.ok) throw new Error(await res.text());
  return res.json();
}

export async function fetchRuns(archiveId) {
  return getJson(`/api/archives/${archiveId}/runs`);
}

export async function fetchTags(archiveId) {
  return getJson(`/api/archives/${archiveId}/tags`);
}

export async function submitCapture(archiveId, locator, quality = null, extensions = null) {
  const payload = { locator }
  if (quality && quality !== 'best') payload.quality = quality
  // extensions: { ublock_enabled?: bool, reader_mode?: bool, cookie_ext_enabled?: bool, modal_closer_enabled?: bool, via_freedium?: bool }
  if (extensions) {
    if (typeof extensions.ublock_enabled === 'boolean') payload.ublock_enabled = extensions.ublock_enabled
    if (typeof extensions.reader_mode === 'boolean') payload.reader_mode = extensions.reader_mode
    if (typeof extensions.cookie_ext_enabled === 'boolean') payload.cookie_ext_enabled = extensions.cookie_ext_enabled
    if (typeof extensions.modal_closer_enabled === 'boolean') payload.modal_closer_enabled = extensions.modal_closer_enabled
    if (typeof extensions.via_freedium === 'boolean') payload.via_freedium = extensions.via_freedium
    if (extensions.per_item_quality && typeof extensions.per_item_quality === 'object' && Object.keys(extensions.per_item_quality).length > 0) payload.per_item_quality = extensions.per_item_quality
    if (extensions.sync === true) payload.sync = true
  }
  const res = await fetch(`/api/archives/${archiveId}/captures`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(payload),
  });
  if (!res.ok) {
    const body = await res.json().catch(() => ({}));
    const err = new Error(body.error || `HTTP ${res.status}`);
    err.status = res.status;
    throw err;
  }
  return res.json(); // { job_uid, status: "pending" }
}

// Returns { has_video: bool, qualities: string[] } e.g. { has_video: true, qualities: ["1080p","720p","480p"] }
// Throws on network error; returns { has_video: false, qualities: [] } on non-video locators.
export async function probeCapture(archiveId, locator) {
  return getJson(`/api/archives/${archiveId}/captures/probe?locator=${encodeURIComponent(locator)}`);
}

export async function probePlaylist(archiveId, locator) {
  const res = await fetch(`/api/archives/${archiveId}/captures/probe-playlist`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ locator }),
  })
  if (!res.ok) {
    const body = await res.json().catch(() => ({}))
    throw new Error(body.error || `HTTP ${res.status}`)
  }
  return res.json()
}

export async function pollCaptureJob(archiveId, jobUid) {
  return getJson(`/api/archives/${archiveId}/capture_jobs/${jobUid}`);
}

// ── Auth helpers ─────────────────────────────────────────────────────────────

export async function checkSetup() {
  const r = await fetch('/api/auth/setup');
  const data = await r.json();
  return data.setup_required === true;
}

export async function doSetup(username, password) {
  const r = await fetch('/api/auth/setup', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ username, password }),
  });
  if (!r.ok) throw new Error((await r.json()).error || 'Setup failed');
  return r.json();
}

export async function login(username, password) {
  const r = await fetch('/api/auth/login', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ username, password }),
  });
  if (!r.ok) throw new Error((await r.json()).error || 'Login failed');
  return r.json();
}

export async function logout() {
  await fetch('/api/auth/logout', { method: 'POST' });
}

export async function fetchMe() {
  const r = await fetch('/api/auth/me');
  if (r.status === 401) return null;
  return r.json();
}

// ── Profile & settings helpers ───────────────────────────────────────────────

export async function updateProfile(displayName) {
  const res = await fetch('/api/auth/me', {
    method: 'PATCH',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ display_name: displayName }),
  });
  if (!res.ok) throw new Error(await res.text());
}

export async function patchMe(patch) {
  const res = await fetch('/api/auth/me', {
    method: 'PATCH',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(patch),
  });
  if (!res.ok) throw new Error(await res.text());
}

export async function changePassword(currentPassword, newPassword) {
  const res = await fetch('/api/auth/me', {
    method: 'PATCH',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ current_password: currentPassword, new_password: newPassword }),
  });
  if (!res.ok) {
    let msg = await res.text();
    try { msg = JSON.parse(msg).error ?? msg; } catch {}
    throw new Error(msg);
  }
}

export async function listTokens() {
  return getJson('/api/auth/tokens');
}

export async function createToken(name) {
  const res = await fetch('/api/auth/tokens', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ name }),
  });
  if (!res.ok) throw new Error(await res.text());
  return res.json();
}

export async function deleteToken(tokenUid) {
  const res = await fetch(`/api/auth/tokens/${tokenUid}`, { method: 'DELETE' });
  if (!res.ok) throw new Error(await res.text());
}

export async function getInstanceSettings() {
  return getJson('/api/admin/instance-settings');
}

export async function updateInstanceSettings(patch) {
  const res = await fetch('/api/admin/instance-settings', {
    method: 'PATCH',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(patch),
  });
  if (!res.ok) throw new Error(await res.text());
}

// ── Admin helpers ─────────────────────────────────────────────────────────────

export async function listAdminUsers() {
  return getJson('/api/admin/users');
}

export async function createAdminUser(username, password, email) {
  const res = await fetch('/api/admin/users', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ username, password, email: email || undefined }),
  });
  if (!res.ok) { const b = await res.json().catch(() => ({})); throw new Error(b.error || `HTTP ${res.status}`); }
  return res.json();
}

export async function setUserStatus(userUid, status) {
  const res = await fetch(`/api/admin/users/${userUid}/status`, {
    method: 'PATCH',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ status }),
  });
  if (!res.ok) { const b = await res.json().catch(() => ({})); throw new Error(b.error || `HTTP ${res.status}`); }
  return res.json();
}

export async function assignRole(userUid, roleSlug) {
  const res = await fetch(`/api/admin/users/${userUid}/roles`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ role_slug: roleSlug }),
  });
  if (!res.ok) { const b = await res.json().catch(() => ({})); throw new Error(b.error || `HTTP ${res.status}`); }
}

export async function removeRole(userUid, roleSlug) {
  const res = await fetch(`/api/admin/users/${userUid}/roles/${encodeURIComponent(roleSlug)}`, {
    method: 'DELETE',
  });
  if (!res.ok && res.status !== 204) { const b = await res.json().catch(() => ({})); throw new Error(b.error || `HTTP ${res.status}`); }
}

export async function listRoles() {
  return getJson('/api/admin/roles');
}

export async function createRole(slug, name) {
  const res = await fetch('/api/admin/roles', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ slug, name }),
  });
  if (!res.ok) { const b = await res.json().catch(() => ({})); throw new Error(b.error || `HTTP ${res.status}`); }
  return res.json();
}

// ── Collection helpers ─────────────────────────────────────────────────────

export async function listCollections(archiveId) {
  return getJson(`/api/archives/${archiveId}/collections`);
}

export async function createCollection(archiveId, name, slug, defaultVisibilityBits = 2, requiresAuth = true) {
  const res = await fetch(`/api/archives/${archiveId}/collections`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ name, slug, default_visibility_bits: defaultVisibilityBits, requires_auth: requiresAuth }),
  });
  if (!res.ok) {
    const err = await res.json().catch(() => ({ error: res.statusText }));
    throw new Error(err.error || res.statusText);
  }
  return res.json();
}

export async function getCollection(archiveId, collUid) {
  return getJson(`/api/archives/${archiveId}/collections/${collUid}`);
}

export async function addEntryToCollection(archiveId, collUid, entryUid, visibilityBits = 2) {
  const res = await fetch(`/api/archives/${archiveId}/collections/${collUid}/entries`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ entry_uid: entryUid, visibility_bits: visibilityBits }),
  });
  if (!res.ok) {
    const err = await res.json().catch(() => ({ error: res.statusText }));
    throw new Error(err.error || res.statusText);
  }
}

export async function removeEntryFromCollection(archiveId, collUid, entryUid) {
  const res = await fetch(`/api/archives/${archiveId}/collections/${collUid}/entries/${entryUid}`, {
    method: 'DELETE',
  });
  if (!res.ok) {
    const err = await res.json().catch(() => ({ error: res.statusText }));
    throw new Error(err.error || res.statusText);
  }
}

export async function updateEntryVisibility(archiveId, collUid, entryUid, visibilityBits) {
  const res = await fetch(`/api/archives/${archiveId}/collections/${collUid}/entries/${entryUid}`, {
    method: 'PATCH',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ visibility_bits: visibilityBits }),
  });
  if (!res.ok) {
    const err = await res.json().catch(() => ({ error: res.statusText }));
    throw new Error(err.error || res.statusText);
  }
}

export async function listEntryCollections(archiveId, entryUid) {
  return getJson(`/api/archives/${archiveId}/entries/${entryUid}/collections`);
}

export async function updateCollection(archiveId, collUid, patch) {
  const res = await fetch(`/api/archives/${archiveId}/collections/${collUid}`, {
    method: 'PATCH',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(patch),
  })
  if (!res.ok) { const e = await res.json().catch(() => ({})); throw new Error(e.error ?? res.statusText) }
}

export async function deleteCollection(archiveId, collUid) {
  const res = await fetch(`/api/archives/${archiveId}/collections/${collUid}`, { method: 'DELETE' })
  if (!res.ok) { const e = await res.json().catch(() => ({})); throw new Error(e.error ?? res.statusText) }
}

// ── Blob / orphan cleanup ─────────────────────────────────────────────────────
export async function scanOrphanBlobs(archiveId) {
  return getJson(`/api/archives/${archiveId}/blob-cleanup`)
}

export async function deleteOrphanBlobs(archiveId) {
  const res = await fetch(`/api/archives/${archiveId}/blob-cleanup`, { method: 'DELETE' })
  if (!res.ok) { const e = await res.json().catch(() => ({})); throw new Error(e.error ?? res.statusText) }
  return res.json()
}

// ── Re-archive ────────────────────────────────────────────────────────────────
export async function rearchiveEntry(archiveId, entryUid) {
  const res = await fetch(`/api/archives/${archiveId}/entries/${entryUid}/rearchive`, {
    method: 'POST',
  })
  if (!res.ok) {
    const body = await res.json().catch(() => ({}))
    throw new Error(body.message || `rearchive failed: ${res.status}`)
  }
  return res.json() // { job_uid, status: 'pending' }
}

// ── Media token ────────────────────────────────────────────────────────────────
// Issues a short-lived signed URL for one artifact — used so Cast / AirPlay
// devices (no session cookie) can fetch the media file directly.
// Returns { url: string, expires_in_secs: number }.
export async function issueMediaToken(archiveId, entryUid, artifactIndex) {
  const res = await fetch(
    `/api/archives/${archiveId}/entries/${entryUid}/artifacts/${artifactIndex}/media-token`,
    { method: 'POST' }
  )
  if (!res.ok) throw new Error(`media-token ${res.status}`)
  return res.json()
}

// ── Cookie rules ──────────────────────────────────────────────────────────────

export async function listCookieRules() {
  return getJson('/api/admin/cookie-rules')
}

export async function createCookieRule(urlPattern, patternKind, cookiesJson) {
  const res = await fetch('/api/admin/cookie-rules', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({
      url_pattern: urlPattern || null,
      pattern_kind: patternKind,
      cookies_json: cookiesJson,
    }),
  })
  if (!res.ok) {
    const body = await res.json().catch(() => ({}))
    throw new Error(body.message || `HTTP ${res.status}`)
  }
  return res.json()
}

export async function updateCookieRule(ruleUid, patch) {
  const res = await fetch(`/api/admin/cookie-rules/${ruleUid}`, {
    method: 'PATCH',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(patch),
  })
  if (!res.ok) {
    const body = await res.json().catch(() => ({}))
    throw new Error(body.message || `HTTP ${res.status}`)
  }
}

export async function deleteCookieRule(ruleUid) {
  const res = await fetch(`/api/admin/cookie-rules/${ruleUid}`, { method: 'DELETE' })
  if (!res.ok) {
    const body = await res.json().catch(() => ({}))
    throw new Error(body.message || `HTTP ${res.status}`)
  }
}

// Returns { promise, abort } so callers can cancel an in-flight upload.
// Aborting rejects the promise with "Upload cancelled" and lets the server's
// partial-upload cleanup handle any bytes already written to temp/uploads/.
export function uploadFile(archiveId, file, onProgress) {
  let xhr
  const promise = new Promise((resolve, reject) => {
    const formData = new FormData()
    formData.append('file', file)
    xhr = new XMLHttpRequest()
    xhr.open('POST', `/api/archives/${archiveId}/uploads`)
    xhr.upload.addEventListener('progress', e => {
      if (e.lengthComputable && onProgress) onProgress(Math.round((e.loaded / e.total) * 100))
    })
    xhr.addEventListener('load', () => {
      if (xhr.status >= 200 && xhr.status < 300) {
        try { resolve(JSON.parse(xhr.responseText)) }
        catch { reject(new Error('Invalid server response')) }
      } else {
        let msg = `Upload failed (${xhr.status})`
        try { msg = JSON.parse(xhr.responseText).message || msg } catch {}
        reject(new Error(msg))
      }
    })
    xhr.addEventListener('error', () => reject(new Error('Network error during upload')))
    xhr.addEventListener('abort', () => reject(new Error('Upload cancelled')))
    xhr.send(formData)
  })
  return { promise, abort: () => xhr?.abort() }
}

// Best-effort discard of a staged upload file (DELETE /archives/:id/uploads).
// Errors are swallowed — the server also prunes stale dirs on startup.
export async function deleteUpload(archiveId, locator) {
  try {
    await fetch(`/api/archives/${archiveId}/uploads`, {
      method: 'DELETE',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ locator }),
    })
  } catch {}
}

// ── 401 interceptor ───────────────────────────────────────────────────────────
const _origFetch = window.fetch;
window.fetch = async (...args) => {
  const r = await _origFetch(...args);
  if (r.status === 401) {
    const url = typeof args[0] === 'string' ? args[0] : args[0]?.url ?? '';
    if (!url.includes('/api/auth/')) {
      window.dispatchEvent(new CustomEvent('auth:expired'));
    }
  }
  return r;
};
