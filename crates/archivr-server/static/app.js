const state = {
  archives: [],
  archiveId: null,
  entries: [],
  filteredEntries: [],
  selectedEntryUid: null,
};

const archiveSwitcher = document.querySelector("#archive-switcher");
const entriesBody = document.querySelector("#entries-body");
const runsBody = document.querySelector("#runs-body");
const contextBody = document.querySelector("#context-body");
const navButtons = document.querySelectorAll(".nav-link");
const searchInput = document.querySelector("#search");
const resultCount = document.querySelector("#result-count");
const adminArchives = document.querySelector("#admin-archives");

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

function applyEntryFilter() {
  const query = searchInput.value.trim().toLowerCase();
  if (!query) {
    state.filteredEntries = state.entries;
    return;
  }
  state.filteredEntries = state.entries.filter((entry) => {
    const haystack = [
      entry.title,
      entry.original_url,
      entry.entry_uid,
      entry.source_kind,
      entry.entity_kind,
      entry.visibility,
    ]
      .filter(Boolean)
      .join(" ")
      .toLowerCase();
    return haystack.includes(query);
  });
}

function renderEntries() {
  entriesBody.innerHTML = "";
  resultCount.textContent = `${state.filteredEntries.length} entries`;

  for (const entry of state.filteredEntries) {
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

async function selectEntry(entry) {
  state.selectedEntryUid = entry.entry_uid;
  renderEntries();
  const detail = await getJson(`/api/archives/${state.archiveId}/entries/${entry.entry_uid}`);
  renderContextDetail(detail);
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

async function loadEntries() {
  state.entries = await getJson(`/api/archives/${state.archiveId}/entries`);
  state.selectedEntryUid = null;
  applyEntryFilter();
  renderEntries();
  contextBody.textContent = "Select an entry.";
}

async function loadArchives() {
  state.archives = await getJson("/api/archives");
  state.archiveId = state.archives[0]?.id ?? null;
  renderArchives();
  if (state.archiveId) {
    await loadEntries();
    await loadRuns();
  } else {
    contextBody.textContent = "No archives mounted.";
    resultCount.textContent = "0 entries";
  }
}

archiveSwitcher.addEventListener("change", async () => {
  state.archiveId = archiveSwitcher.value;
  await loadEntries();
  await loadRuns();
});

searchInput.addEventListener("input", () => {
  applyEntryFilter();
  renderEntries();
});

navButtons.forEach((button) => {
  button.addEventListener("click", () => {
    navButtons.forEach((candidate) => candidate.classList.remove("is-active"));
    document.querySelectorAll(".view").forEach((view) => view.classList.remove("is-active"));
    button.classList.add("is-active");
    document.querySelector(`#${button.dataset.view}-view`).classList.add("is-active");
  });
});

loadArchives().catch((error) => {
  contextBody.textContent = `Failed to load archives: ${error.message}`;
});
