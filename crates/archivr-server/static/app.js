const state = {
  archives: [],
  archiveId: null,
  entries: [],
  selectedEntryUid: null,
  selectedEntry: null,
  tagFilter: null,
};
let selectSeq = 0;

const archiveSwitcher = document.querySelector("#archive-switcher");
const entriesBody = document.querySelector("#entries-body");
const runsBody = document.querySelector("#runs-body");
const contextBody = document.querySelector("#context-body");
const navButtons = document.querySelectorAll(".nav-link");
const searchInput = document.querySelector("#search");
const resultCount = document.querySelector("#result-count");
const adminArchives = document.querySelector("#admin-archives");
const tagTree = document.querySelector("#tag-tree");
const entryTagsEl = document.querySelector("#entry-tags");
const assignTagForm = document.querySelector("#assign-tag-form");
const assignTagInput = document.querySelector("#assign-tag-input");
const assignTagBtn = document.querySelector("#assign-tag-btn");
const captureButton = document.querySelector('.capture-button');
const captureDialog = document.querySelector('#capture-dialog');
const captureLocatorInput = document.querySelector('#capture-locator');
const captureSubmitBtn = document.querySelector('#capture-submit-btn');
const captureCancelBtn = document.querySelector('#capture-cancel-btn');
const captureError = document.querySelector('#capture-error');

function formatBytes(bytes) {
  if (!bytes) return "0 B";
  const units = ["B", "KB", "MB", "GB"];
  let size = bytes;
  let unit = 0;
  while (size >= 1024 && unit < units.length - 1) {
    size /= 1024;
    unit += 1;
  }
  return `${size.toFixed(unit === 0 ? 0 : 1)} ${units[unit]}`;
}

function valueText(value) {
  return value ?? "";
}

const SOURCE_ICONS = {
  youtube: `<svg viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg"><path fill="#FF0000" d="M23.5 6.2a3 3 0 0 0-2.1-2.1C19.5 3.6 12 3.6 12 3.6s-7.5 0-9.4.5A3 3 0 0 0 .5 6.2C0 8.1 0 12 0 12s0 3.9.5 5.8a3 3 0 0 0 2.1 2.1c1.9.5 9.4.5 9.4.5s7.5 0 9.4-.5a3 3 0 0 0 2.1-2.1C24 15.9 24 12 24 12s0-3.9-.5-5.8z"/><polygon fill="#fff" points="9.6,15.6 15.8,12 9.6,8.4"/></svg>`,
  x: `<svg viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg"><path d="M18.2 2h3.3l-7.2 8.2L23 22h-6.6l-5.2-6.8L5 22H1.7l7.7-8.8L1 2h6.8l4.7 6.2zm-1.1 18h1.8L6.9 3.9H5z"/></svg>`,
  instagram: `<svg viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg"><defs><linearGradient id="ig" x1="0%" y1="100%" x2="100%" y2="0%"><stop offset="0%" stop-color="#f09433"/><stop offset="25%" stop-color="#e6683c"/><stop offset="50%" stop-color="#dc2743"/><stop offset="75%" stop-color="#cc2366"/><stop offset="100%" stop-color="#bc1888"/></linearGradient></defs><rect width="24" height="24" rx="5" fill="url(#ig)"/><rect x="2.5" y="2.5" width="19" height="19" rx="4" fill="none" stroke="#fff" stroke-width="1.5"/><circle cx="12" cy="12" r="4" fill="none" stroke="#fff" stroke-width="1.5"/><circle cx="17.5" cy="6.5" r="1" fill="#fff"/></svg>`,
  facebook: `<svg viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg"><rect width="24" height="24" rx="4" fill="#1877F2"/><path fill="#fff" d="M16 8h-2c-.6 0-1 .4-1 1v2h3l-.4 3H13v8h-3v-8H8v-3h2V9a4 4 0 0 1 4-4h2v3z"/></svg>`,
  tiktok: `<svg viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg"><path d="M19.6 5.4a4.8 4.8 0 0 1-4.8-4.8h-3v14.7a2 2 0 1 1-2-2l.1-3a5 5 0 1 0 4.9 5V8.4a8 8 0 0 0 4.8 1.6V6.7a4.8 4.8 0 0 1-0-.2l0 0 .1.9z" fill="#000"/><path d="M18.6 4.4a4.8 4.8 0 0 1-4.8-4.8h-3v14.7a2 2 0 1 1-2-2l.1-3a5 5 0 1 0 4.9 5V7.4a8 8 0 0 0 4.8 1.6V5.7" fill="none" stroke="#69C9D0" stroke-width="1.5"/></svg>`,
  reddit: `<svg viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg"><circle cx="12" cy="12" r="12" fill="#FF4500"/><path fill="#fff" d="M20 12a2 2 0 0 0-2-2 2 2 0 0 0-1.3.5c-1.3-.9-3-.9-4.6-.9l.8-3.7 2.6.5a1.4 1.4 0 1 0 1.4-1.3 1.4 1.4 0 0 0-1.3.8l-2.9-.5a.3.3 0 0 0-.4.3l-.9 4.1c-1.6 0-3.1.1-4.3.9A2 2 0 0 0 4 12a2 2 0 0 0 1 1.7 3.3 3.3 0 0 0 0 .5c0 2.6 3 4.8 6.8 4.8s6.8-2.1 6.8-4.8a3.3 3.3 0 0 0 0-.5A2 2 0 0 0 20 12zm-13.6 1a1.1 1.1 0 1 1 1.1 1.1A1.1 1.1 0 0 1 6.4 13zm6.2 3.1a3.5 3.5 0 0 1-2.3.7 3.5 3.5 0 0 1-2.3-.7.3.3 0 0 1 .4-.4 3 3 0 0 0 1.9.5 3 3 0 0 0 1.9-.5.3.3 0 0 1 .4.4zm-.3-2a1.1 1.1 0 1 1 1.1-1.1A1.1 1.1 0 0 1 12.3 14.1z"/></svg>`,
  snapchat: `<svg viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg"><rect width="24" height="24" rx="4" fill="#FFFC00"/><path d="M12 3.5c-2.4 0-4.4 2-4.4 4.4v1.3l-.8.1c-.3 0-.5.2-.5.5s.3.5.6.6c.3.1.5.3.5.8 0 .3-.1.5-.3.7-.6.8-1.5 1.3-2.2 1.4-.1.7.7 1 1.7 1.2.1.6.3.8.8.8.6 0 1.1.4 2.7.4 1.5 0 2.1-.4 2.7-.4.5 0 .7-.2.8-.8 1-.2 1.8-.5 1.7-1.2-.7-.1-1.6-.6-2.2-1.4-.2-.2-.3-.4-.3-.7 0-.5.2-.7.5-.8.3-.1.6-.3.6-.6s-.2-.5-.5-.5l-.8-.1V7.9C16.4 5.5 14.4 3.5 12 3.5z"/></svg>`,
  local: `<svg viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg"><path fill="none" stroke="#666" stroke-width="1.5" stroke-linejoin="round" d="M4 4h7l2 2h7v14H4z"/></svg>`,
  web: `<svg viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg"><circle cx="12" cy="12" r="10" fill="none" stroke="#555" stroke-width="1.5"/><ellipse cx="12" cy="12" rx="4" ry="10" fill="none" stroke="#555" stroke-width="1.5"/><line x1="2" y1="9" x2="22" y2="9" stroke="#555" stroke-width="1.5"/><line x1="2" y1="15" x2="22" y2="15" stroke="#555" stroke-width="1.5"/></svg>`,
  other: `<svg viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg"><circle cx="12" cy="12" r="10" fill="none" stroke="#999" stroke-width="1.5"/><text x="12" y="16" text-anchor="middle" font-size="12" fill="#999">?</text></svg>`,
};

function sourceIcon(kind) {
  const svg = SOURCE_ICONS[kind] || SOURCE_ICONS.other;
  const el = document.createElement("span");
  el.className = "source-icon";
  el.innerHTML = svg;
  return el;
}

function formatTimestamp(value) {
  if (!value) return "";
  // value is an ISO-8601 string from SQLite; display as "YYYY-MM-DD HH:MM"
  const d = new Date(value.endsWith("Z") ? value : value + "Z");
  if (isNaN(d)) return value;
  const pad = (n) => String(n).padStart(2, "0");
  return `${d.getUTCFullYear()}-${pad(d.getUTCMonth() + 1)}-${pad(d.getUTCDate())} ${pad(d.getUTCHours())}:${pad(d.getUTCMinutes())}`;
}

async function getJson(url) {
  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(`${response.status} ${response.statusText}`);
  }
  return response.json();
}

function appendCell(row, text, className) {
  const cell = document.createElement("td");
  if (className) cell.className = className;
  cell.textContent = text;
  row.append(cell);
  return cell;
}

function renderArchives() {
  archiveSwitcher.innerHTML = "";
  for (const archive of state.archives) {
    const option = document.createElement("option");
    option.value = archive.id;
    option.textContent = archive.label;
    archiveSwitcher.append(option);
  }
  archiveSwitcher.value = state.archiveId ?? "";

  adminArchives.innerHTML = "";
  for (const archive of state.archives) {
    const item = document.createElement("div");
    item.className = "admin-archive";
    const label = document.createElement("strong");
    label.textContent = archive.label;
    const path = document.createElement("div");
    path.className = "muted";
    path.textContent = archive.archive_path;
    item.append(label, path);
    adminArchives.append(item);
  }
}


function renderEntries() {
  entriesBody.innerHTML = "";
  if (state.entries.length === 0 && searchInput.value.trim()) {
    resultCount.textContent = "No results.";
  } else {
    resultCount.textContent = `${state.entries.length} entries`;
  }
  if (state.tagFilter) {
    const badge = document.createElement("button");
    badge.className = "tag-filter-badge";
    badge.textContent = `× ${state.tagFilter}`;
    badge.addEventListener("click", () => {
      state.tagFilter = null;
      if (state.archiveId) loadEntries(searchInput.value);
    });
    resultCount.appendChild(badge);
  }

  for (const entry of state.entries) {
    const row = document.createElement("tr");
    row.tabIndex = 0;
    row.dataset.entryUid = entry.entry_uid;
    if (entry.entry_uid === state.selectedEntryUid) {
      row.classList.add("is-selected");
    }

    appendCell(row, formatTimestamp(entry.archived_at));

    const titleCell = appendCell(row, "");
    titleCell.append(sourceIcon(entry.source_kind));
    const title = document.createElement("span");
    title.className = "entry-title";
    title.textContent = valueText(entry.title) || valueText(entry.entry_uid);
    titleCell.append(title);

    const typeCell = appendCell(row, "");
    const type = document.createElement("span");
    type.className = "type-pill";
    type.textContent = valueText(entry.entity_kind);
    typeCell.append(type);

    appendCell(row, formatBytes(entry.total_artifact_bytes));
    appendCell(row, valueText(entry.original_url), "url-cell");

    row.addEventListener("click", () => selectEntry(entry));
    row.addEventListener("keydown", (event) => {
      if (event.key === "Enter") selectEntry(entry);
    });
    entriesBody.append(row);
  }
}

function renderContextDetail(detail) {
  contextBody.innerHTML = "";

  // Title
  const titleEl = document.createElement("strong");
  titleEl.className = "rail-entry-title";
  titleEl.textContent =
    valueText(detail.summary.title) || valueText(detail.summary.entry_uid);
  contextBody.append(titleEl);

  // Metadata section
  const metaSection = document.createElement("div");
  metaSection.className = "rail-section";

  if (detail.summary.original_url) {
    const urlRow = document.createElement("div");
    urlRow.className = "rail-item";
    const urlLabel = document.createElement("span");
    urlLabel.className = "rail-label";
    urlLabel.textContent = "Original URL";
    const urlLink = document.createElement("a");
    urlLink.href = detail.summary.original_url;
    urlLink.target = "_blank";
    urlLink.rel = "noopener noreferrer";
    urlLink.className = "rail-url-link";
    urlLink.textContent = detail.summary.original_url;
    urlRow.append(urlLabel, document.createTextNode(": "), urlLink);
    metaSection.append(urlRow);
  }

  const metaFields = [
    ["Added", formatTimestamp(detail.summary.archived_at)],
    ["Source", detail.summary.source_kind],
    ["Type", detail.summary.entity_kind],
    ["Visibility", detail.summary.visibility],
    ["Structured root", detail.structured_root_relpath],
  ];
  for (const [label, value] of metaFields) {
    const item = document.createElement("div");
    item.className = "rail-item";
    const labelEl = document.createElement("span");
    labelEl.className = "rail-label";
    labelEl.textContent = label;
    item.append(labelEl, document.createTextNode(`: ${valueText(value)}`));
    metaSection.append(item);
  }
  contextBody.append(metaSection);

  // Artifacts section
  if (detail.artifacts.length > 0) {
    const artifactsSection = document.createElement("div");
    artifactsSection.className = "rail-section";
    const artifactsHeading = document.createElement("div");
    artifactsHeading.className = "rail-section-heading";
    artifactsHeading.textContent = `Artifacts (${detail.artifacts.length})`;
    artifactsSection.append(artifactsHeading);
    const list = document.createElement("ul");
    list.className = "artifact-list";
    detail.artifacts.forEach((artifact, index) => {
      const li = document.createElement("li");
      const a = document.createElement("a");
      a.href = `/api/archives/${state.archiveId}/entries/${detail.summary.entry_uid}/artifacts/${index}`;
      a.target = "_blank";
      a.rel = "noopener noreferrer";
      a.className = "artifact-link";
      const roleName = artifact.artifact_role.replace(/_/g, " ");
      const size =
        artifact.byte_size != null ? ` (${formatBytes(artifact.byte_size)})` : "";
      a.textContent = `${roleName}${size}`;
      li.append(a);
      list.append(li);
    });
    artifactsSection.append(list);
    contextBody.append(artifactsSection);
  } else {
    const noArtifacts = document.createElement("div");
    noArtifacts.className = "rail-item muted";
    noArtifacts.textContent = "No artifacts.";
    contextBody.append(noArtifacts);
  }
}

function renderEntryTags(tags, entryUid) {
  entryTagsEl.innerHTML = "";
  if (!tags.length) {
    entryTagsEl.textContent = "No tags.";
    return;
  }
  for (const tag of tags) {
    const pill = document.createElement("span");
    pill.className = "tag-pill";
    pill.textContent = tag.name;
    pill.title = tag.full_path;
    const removeBtn = document.createElement("button");
    removeBtn.className = "remove-tag";
    removeBtn.textContent = "×";
    removeBtn.title = `Remove tag ${tag.full_path}`;
    removeBtn.addEventListener("click", async () => {
      const resp = await fetch(
        `/api/archives/${state.archiveId}/entries/${entryUid}/tags/${tag.tag_uid}`,
        { method: "DELETE" }
      );
      if (!resp.ok) {
        removeBtn.title = `Remove failed (${resp.status})`;
        return;
      }
      const updated = await getJson(
        `/api/archives/${state.archiveId}/entries/${entryUid}/tags`
      );
      renderEntryTags(updated, entryUid);
      loadTagTree();
    });
    pill.appendChild(removeBtn);
    entryTagsEl.appendChild(pill);
  }
}

async function selectEntry(entry) {
  const seq = ++selectSeq;
  state.selectedEntryUid = entry.entry_uid;
  state.selectedEntry = entry;
  renderEntries();
  const detail = await getJson(
    `/api/archives/${state.archiveId}/entries/${entry.entry_uid}`
  );
  if (seq !== selectSeq) return;
  renderContextDetail(detail);
  entryTagsEl.hidden = false;
  assignTagForm.hidden = false;
  entryTagsEl.innerHTML = "";
  const tags = await getJson(
    `/api/archives/${state.archiveId}/entries/${entry.entry_uid}/tags`
  );
  if (seq !== selectSeq) return;
  renderEntryTags(tags, entry.entry_uid);
}

async function loadRuns() {
  const runs = await getJson(`/api/archives/${state.archiveId}/runs`);
  runsBody.innerHTML = "";
  for (const run of runs) {
    const row = document.createElement("tr");
    appendCell(row, valueText(run.started_at));
    appendCell(row, valueText(run.status));
    appendCell(row, String(run.requested_count));
    appendCell(row, String(run.completed_count));
    appendCell(row, String(run.failed_count));
    runsBody.append(row);
  }
}

async function loadEntries(q = "") {
  const trimmed = q.trim();
  const params = new URLSearchParams();
  if (trimmed) params.set("q", trimmed);
  if (state.tagFilter) params.set("tag", state.tagFilter);
  const url =
    trimmed || state.tagFilter
      ? `/api/archives/${state.archiveId}/entries/search?${params}`
      : `/api/archives/${state.archiveId}/entries`;
  searchInput.setAttribute("aria-busy", "true");
  try {
    state.entries = await getJson(url);
  } catch (err) {
    resultCount.textContent = "Search failed. Try again.";
    state.entries = [];
  } finally {
    searchInput.removeAttribute("aria-busy");
  }
  renderEntries();
}

async function loadTagTree() {
  if (!state.archiveId) return;
  const nodes = await getJson(`/api/archives/${state.archiveId}/tags`);
  tagTree.innerHTML = "";
  renderTagTree(nodes, tagTree);
}

function renderTagTree(nodes, container) {
  if (!nodes.length) {
    container.textContent = "No tags yet.";
    return;
  }
  const ul = document.createElement("ul");
  ul.className = "tag-tree-list";
  for (const node of nodes) {
    const li = document.createElement("li");
    const btn = document.createElement("button");
    btn.className = "tag-node-btn";
    if (state.tagFilter === node.tag.full_path) btn.classList.add("is-active");
    btn.textContent = node.tag.name;
    btn.title = node.tag.full_path;
    btn.addEventListener("click", () => {
      if (state.tagFilter === node.tag.full_path) {
        state.tagFilter = null;
      } else {
        state.tagFilter = node.tag.full_path;
      }
      // Switch to archive view and reload
      switchView("archive");
      if (state.archiveId) loadEntries(searchInput.value);
    });
    li.appendChild(btn);
    if (node.children?.length) {
      const childContainer = document.createElement("div");
      childContainer.className = "tag-children";
      renderTagTree(node.children, childContainer);
      li.appendChild(childContainer);
    }
    ul.appendChild(li);
  }
  container.appendChild(ul);
}

async function loadArchives() {
  state.archives = await getJson("/api/archives");
  state.archiveId = state.archives[0]?.id ?? null;
  renderArchives();
  if (state.archiveId) {
    await loadEntries();
    await loadRuns();
    loadTagTree();
  } else {
    contextBody.textContent = "No archives mounted.";
    resultCount.textContent = "0 entries";
  }
}

function debounce(fn, ms) {
  let timer;
  return (...args) => {
    clearTimeout(timer);
    timer = setTimeout(() => fn(...args), ms);
  };
}

archiveSwitcher.addEventListener("change", async () => {
  state.tagFilter = null;
  state.selectedEntry = null;
  state.selectedEntryUid = null;
  entryTagsEl.hidden = true;
  assignTagForm.hidden = true;
  state.archiveId = archiveSwitcher.value;
  await loadEntries();
  await loadRuns();
  loadTagTree();
});

const debouncedSearch = debounce((q) => {
  if (state.archiveId) loadEntries(q);
}, 300);

searchInput.addEventListener("input", () => {
  debouncedSearch(searchInput.value);
});

function switchView(name) {
  navButtons.forEach(b => b.classList.toggle("is-active", b.dataset.view === name));
  document.querySelectorAll(".view").forEach(v => v.classList.remove("is-active"));
  document.querySelector(`#${name}-view`)?.classList.add("is-active");
}

navButtons.forEach((button) => {
  button.addEventListener("click", () => {
    switchView(button.dataset.view);
    if (button.dataset.view === "tags") loadTagTree();
  });
});

assignTagBtn.addEventListener("click", async () => {
  const path = assignTagInput.value.trim();
  if (!path || !state.selectedEntry) return;
  const resp = await fetch(
    `/api/archives/${state.archiveId}/entries/${state.selectedEntry.entry_uid}/tags`,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ tag_path: path }),
    }
  );
  if (resp.ok) {
    assignTagInput.setCustomValidity("");
    assignTagInput.value = "";
    const tags = await getJson(
      `/api/archives/${state.archiveId}/entries/${state.selectedEntry.entry_uid}/tags`
    );
    renderEntryTags(tags, state.selectedEntry.entry_uid);
    loadTagTree();
  } else {
    assignTagInput.setCustomValidity(`Failed to add tag (${resp.status})`);
    assignTagInput.reportValidity();
  }
});

captureButton.addEventListener('click', () => {
  captureLocatorInput.value = '';
  captureError.hidden = true;
  captureDialog.showModal();
});

captureCancelBtn.addEventListener('click', () => captureDialog.close());

captureSubmitBtn.addEventListener('click', async () => {
  const locator = captureLocatorInput.value.trim();
  if (!locator) {
    captureError.textContent = 'Enter a locator.';
    captureError.hidden = false;
    return;
  }
  captureSubmitBtn.disabled = true;
  captureSubmitBtn.textContent = 'Capturing\u2026';
  captureError.hidden = true;
  try {
    const res = await fetch(`/api/archives/${state.archiveId}/captures`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ locator }),
    });
    if (!res.ok) {
      const msg = await res.text();
      throw new Error(msg || `HTTP ${res.status}`);
    }
    captureDialog.close();
    await Promise.all([loadEntries(searchInput.value), loadRuns()]);
  } catch (e) {
    captureError.textContent = e.message;
    captureError.hidden = false;
  } finally {
    captureSubmitBtn.disabled = false;
    captureSubmitBtn.textContent = 'Capture';
  }
});

loadArchives().catch((error) => {
  contextBody.textContent = `Failed to load archives: ${error.message}`;
});
