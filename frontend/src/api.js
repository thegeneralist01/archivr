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
    const msg = await res.text();
    throw new Error(msg || `HTTP ${res.status}`);
  }
}
