# Playlist / Channel Captures + Parent–Child Entry Display

**Date:** 2026-07-20
**Branch:** `feat-captures-with-children`

---

## Goal

1. **YouTube (Music) playlists and channels** — when the user submits a playlist or channel URL/shorthand, archive every video in it as a tree of entries: one root "container" entry for the playlist/channel, one child entry per video.
2. **Parent–child display** — the entry list and detail view can show children of a container entry in a collapsible inline section, replacing the current flat/single-entry-only model.

---

## Schema Contract (no migrations needed — columns already exist)

| Column | Meaning |
|---|---|
| `archived_entries.parent_entry_id` | `NULL` for root entries; points to container id for children |
| `archived_entries.root_entry_id` | Same as `parent_entry_id` for depth-1 children (direct children of a container) |
| `archive_run_items.parent_item_id` | `NULL` for the container item; points to container item id for video items |
| `archive_runs.requested_count` | Always `1` (user submitted one locator) |
| `archive_runs.discovered_count` | `N + 1` — container item + N video items (from `refresh_run_counters`) |
| `archive_runs.completed_count` | Items completed (container + each successful video) |
| `archive_runs.failed_count` | Items that failed to download |

**Counter note:** `refresh_run_counters` counts every `archive_run_items` row, so `discovered_count = 1 + N` when a playlist has N videos. This is intentional and accurate — the container item itself counts as a discovered/completed item.

---

## yt-dlp Probe Strategy

For container sources (playlist, channel, YouTube Music playlist), use:

```bash
yt-dlp -J --flat-playlist <url>
```

`-J` (`--dump-single-json`) returns a **single JSON object** for the whole container — reliable `title` and `uploader` fields at the top level, plus an `entries` array (shallow per-item objects with at minimum `id`, `url`, `title`). This avoids the per-item `playlist_title` reliability issue that `--dump-json` (multi-line) has.

Individual child video downloads stay single-item (`--no-playlist`), same as current video capture.

---

## File Map

| File | Action | What changes |
|---|---|---|
| `crates/archivr-core/src/downloader/ytdlp.rs` | Modify | Add `PlaylistItem`, `PlaylistInfo` structs; `fetch_playlist_info()` |
| `crates/archivr-core/src/capture.rs` | Modify | `record_media_entry` gains `parent_entry_id`/`root_entry_id` params; add `record_container_entry()`; implement playlist/channel/YTM-playlist capture paths |
| `crates/archivr-core/src/archive.rs` | Modify | Add `child_count: i64` to `EntrySummary`; add `get_entry_summary()`; fix `get_entry_detail()` to handle child entries; add `list_child_entries()` |
| `crates/archivr-server/src/routes.rs` | Modify | Add `GET /api/archives/:id/entries/:uid/children` route + handler |
| `frontend/src/api.js` | Modify | Add `fetchEntryChildren()` |
| `frontend/src/components/EntryRow.jsx` | Modify | Expand toggle when `child_count > 0`; inline child sub-rows on expand |
| `frontend/src/styles.css` | Modify | Child row indentation + expand toggle styling |

---

## Detailed Design

### `ytdlp.rs` additions

```rust
pub struct PlaylistItem {
    pub id: String,
    pub url: String,
    pub title: Option<String>,
    pub uploader: Option<String>,
}

pub struct PlaylistInfo {
    pub playlist_id: String,
    pub title: Option<String>,
    pub uploader: Option<String>,
    pub items: Vec<PlaylistItem>,
}

/// Runs `yt-dlp -J --flat-playlist <url>` and parses the result.
/// Returns Err if yt-dlp fails or the output isn't a playlist object.
pub fn fetch_playlist_info(url: &str, cookies: &HashMap<String, String>) -> Result<PlaylistInfo>
```

### `capture.rs` changes

**`record_media_entry` signature extension:**
```rust
fn record_media_entry(
    ...,
    title: Option<String>,
    parent_entry_id: Option<i64>,  // NEW
    root_entry_id: Option<i64>,    // NEW
) -> Result<database::ArchivedEntry>
```
All existing call sites pass `None, None`. Playlist child calls pass the container entry's ids.

**New `record_container_entry()`:**
- Creates an entry with no blob and no `primary_media` artifact.
- `source_kind/entity_kind` from `source_metadata(source)`.
- `parent_entry_id: None, root_entry_id: None`.
- Stores playlist metadata in `source_metadata_json`.
- Returns the `ArchivedEntry` (needed for child `parent_entry_id`).
- Calls `database::complete_archive_run_item()` on the container run item.

**Playlist/channel capture path (replaces `return Err(...)` stubs):**
```
1. fetch_playlist_info(url, cookies)            → PlaylistInfo
2. create_archive_run(conn, user_id, 1)         → run (requested_count=1)
3. create_archive_run_item(run, None, 0, ..., "playlist"/"channel", "container")
4. record_container_entry(...)                  → container_entry
5. complete_archive_run_item(container_item, container_entry.id)
6. for (ordinal, item) in playlist_info.items:
   a. create_archive_run_item(run, Some(container_item.id), ordinal, item.url, ...)
   b. fetch_metadata(item.url, cookies)          → metadata_json (for title)
   c. ytdlp::download(item.url, ...)            → (hash, ext)
   d. record_media_entry(..., Some(container.id), Some(container.id))
      OR: fail_archive_run_item(child_item, error) and continue
7. finish_archive_run(conn, run.id)
```

Error handling: if a child video fails, call `fail_archive_run_item` and continue — partial success is correct for playlists.

### `archive.rs` changes

**`EntrySummary` new field:**
```rust
pub child_count: i64,  // number of direct children; 0 for non-container entries
```

**`ENTRY_SELECT_COLS` extension** (adds col 12):
```sql
(SELECT COUNT(*) FROM archived_entries child WHERE child.parent_entry_id = e.id) AS child_count
```

**`list_root_entries` inline SQL** also extended the same way (col 12).

All `query_map` closures that build `EntrySummary` get `child_count: row.get(12)?`.

**`get_entry_summary(conn, entry_uid)`** — new private helper:
- Fetches one entry by uid without the `parent_entry_id IS NULL` constraint.
- Used by the fixed `get_entry_detail`.

**`get_entry_detail` fix:**
```rust
// BEFORE (broken for child entries):
let summary = list_root_entries(conn, u32::MAX)?
    .into_iter()
    .find(|entry| entry.entry_uid == entry_uid)
    .context("entry disappeared")?;

// AFTER:
let summary = get_entry_summary(conn, entry_uid)?
    .context("entry disappeared")?;
```

**New `list_child_entries(conn, parent_uid) -> Result<Vec<EntrySummary>>`:**
```sql
ENTRY_SELECT_COLS ENTRY_FROM_JOINS
WHERE e.parent_entry_id = (SELECT id FROM archived_entries WHERE entry_uid = ?1)
GROUP BY e.id
ORDER BY e.archived_at ASC, e.id ASC
```
(ascending order — preserves playlist ordinal feel)

### `routes.rs` addition

```
.route(
    "/api/archives/:archive_id/entries/:entry_uid/children",
    get(list_entry_children),
)
```

```rust
async fn list_entry_children(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((archive_id, entry_uid)): Path<(String, String)>,
) -> Result<Json<Vec<archive::EntrySummary>>, ApiError> {
    auth.require_auth()?;
    let mounted = mounted_archive(&state, &archive_id)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    Ok(Json(archive::list_child_entries(&conn, &entry_uid)?))
}
```

### Frontend changes

**`api.js`:**
```js
export async function fetchEntryChildren(archiveId, entryUid) {
  return getJson(`/api/archives/${archiveId}/entries/${entryUid}/children`);
}
```

**`EntryRow.jsx`:**
- Accept optional `archiveId` for child fetching (already passed).
- When `entry.child_count > 0`: render a chevron expand button in `col-title` area.
- Local state: `expanded` (bool), `children` (array | null), `loading` (bool).
- On chevron click: toggle; if expanding and `children === null`, call `fetchEntryChildren` and store result.
- Render children as `<div className="child-entries">` containing simplified `<ChildEntryRow>` elements (or reuse `EntryRow` without nesting).

**`styles.css`:**
- `.child-entries` — slight left indent, separator line.
- `.entry-expand-btn` — minimal chevron button.

---

## Acceptance Criteria

- `cargo test -p archivr-core` green.
- `cargo check -p archivr-server` clean.
- Submitting `yt:playlist/PLxxx` archives the playlist as a container entry with N child video entries in the DB.
- Submitting `yt:@handle` or `yt:channel/UC...` does the same for channel uploads.
- Submitting `ytm:playlist/PLxxx` does the same for YouTube Music playlists.
- Individual video/track captures (`yt:video/ID`, `ytm:ID`) unchanged.
- If one video in a playlist fails, the run status is `failed` but other children are still archived.
- `GET /api/archives/:id/entries` returns root entries only, each with correct `child_count`.
- `GET /api/archives/:id/entries/:uid` works for both root and child entries.
- `GET /api/archives/:id/entries/:uid/children` returns the child entries for a container.
- Frontend expand button appears on container entries; clicking it fetches and shows children inline.
