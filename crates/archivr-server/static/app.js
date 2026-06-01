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
  const title = document.createElement("strong");
  title.textContent = valueText(detail.summary.title) || valueText(detail.summary.entry_uid);
  contextBody.append(title);

  const items = [
    ["Type", detail.summary.entity_kind],
    ["Visibility", detail.summary.visibility],
    ["Artifacts", detail.artifacts.length],
    ["Structured root", detail.structured_root_relpath],
  ];

  for (const [label, value] of items) {
    const item = document.createElement("div");
    item.className = "rail-item";
    item.textContent = `${label}: ${valueText(value)}`;
    contextBody.append(item);
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
