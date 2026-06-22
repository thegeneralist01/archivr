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

    appendCell(row, valueText(entry.archived_at));

    const titleCell = appendCell(row, "");
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
    ["Added", detail.summary.archived_at],
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

loadArchives().catch((error) => {
  contextBody.textContent = `Failed to load archives: ${error.message}`;
});
