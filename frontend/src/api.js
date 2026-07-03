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

export async function fetchRuns(archiveId) {
  return getJson(`/api/archives/${archiveId}/runs`);
}

export async function fetchTags(archiveId) {
  return getJson(`/api/archives/${archiveId}/tags`);
}

export async function submitCapture(archiveId, locator) {
  const res = await fetch(`/api/archives/${archiveId}/captures`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ locator }),
  });
  if (!res.ok) {
    const body = await res.json().catch(() => ({}));
    throw new Error(body.error || `HTTP ${res.status}`);
  }
  return res.json(); // { job_uid, status: "pending" }
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

export async function createCollection(archiveId, name, slug, defaultVisibilityBits = 2) {
  const res = await fetch(`/api/archives/${archiveId}/collections`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ name, slug, default_visibility_bits: defaultVisibilityBits }),
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
