# Archivr Web UI Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Refactor Archivr into a workspace with reusable core archive logic, rename taxonomy concepts to hierarchical tags, and add the first multi-archive web server/UI foundation.

**Architecture:** Split the current single binary into `archivr-core`, `archivr-cli`, and `archivr-server`. Keep every mounted archive self-contained with its own `.archivr/archivr.sqlite`, and give the server a separate registry config for mounting many archives. The first UI renders the approved dense archive table, a contextual right rail, a runs view, and a secondary capture button.

**Tech Stack:** Rust 2024 workspace, `rusqlite` for archive DBs, `axum`/`tokio` for HTTP, `tower-http` for static assets, vanilla HTML/CSS/JS for the first web UI, `serde`/`toml` for server registry config.

---

## Scope And Sequencing

This plan intentionally separates the foundational refactor from the first web UI slice:

1. Create a workspace while keeping CLI behavior intact.
2. Extract reusable archive logic into `archivr-core`.
3. Rename taxonomy schema/API/tests to hierarchical tags.
4. Add archive query APIs needed by the server.
5. Add `archivr-server` with a multi-archive registry.
6. Add server API routes.
7. Add the first static web UI.
8. Verify with unit tests, API smoke tests, and browser inspection.

Do not implement full OP search, production auth/session handling, browser capture flow, public publishing UI, full tag management UI, or final row-click/open behavior in this plan.

## File Structure

Create this workspace structure:

```text
Cargo.toml
Cargo.lock
crates/
  archivr-core/
    Cargo.toml
    src/
      lib.rs
      archive.rs
      database.rs
      hash.rs
      twitter.rs
      downloader/
        mod.rs
        local.rs
        store.rs
        tweets.rs
        ytdlp.rs
  archivr-cli/
    Cargo.toml
    src/
      main.rs
  archivr-server/
    Cargo.toml
    src/
      main.rs
      registry.rs
      routes.rs
    static/
      index.html
      styles.css
      app.js
docs/
  superpowers/
    specs/
      2026-06-01-web-ui-design.md
    plans/
      2026-06-01-web-ui-foundation.md
```

Responsibilities:

- `archivr-core/src/archive.rs`: archive directory discovery/opening, archive metadata, store path reading, entry/run query APIs.
- `archivr-core/src/database.rs`: SQLite schema and domain operations, including hierarchical tags.
- `archivr-core/src/downloader/*`: existing archive download/store behavior.
- `archivr-cli/src/main.rs`: command-line parsing and terminal behavior only.
- `archivr-server/src/registry.rs`: server-owned mounted archive registry config.
- `archivr-server/src/routes.rs`: API and static route construction.
- `archivr-server/static/*`: first browser UI.

## Task 1: Convert To A Workspace With CLI Crate

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/archivr-cli/Cargo.toml`
- Move: `src/main.rs` -> `crates/archivr-cli/src/main.rs`
- Move: `src/database.rs` -> `crates/archivr-cli/src/database.rs`
- Move: `src/hash.rs` -> `crates/archivr-cli/src/hash.rs`
- Move: `src/twitter.rs` -> `crates/archivr-cli/src/twitter.rs`
- Move: `src/downloader/` -> `crates/archivr-cli/src/downloader/`

- [ ] **Step 1: Record the current baseline**

Run:

```bash
cargo test
```

Expected: all current tests pass.

- [ ] **Step 2: Replace root manifest with a workspace manifest**

Replace `Cargo.toml` with:

```toml
[workspace]
members = [
    "crates/archivr-cli",
]
resolver = "2"

[workspace.package]
version = "0.1.0"
edition = "2024"

[workspace.dependencies]
anyhow = "1.0.100"
chrono = "0.4.42"
clap = { version = "4.5.48", features = ["derive"] }
hex = "0.4.3"
regex = "1.12.2"
rusqlite = { version = "0.32.1", features = ["bundled"] }
serde_json = "1.0.132"
sha3 = "0.10.8"
uuid = { version = "1.18.1", features = ["v4"] }
```

- [ ] **Step 3: Create the CLI crate manifest**

Create `crates/archivr-cli/Cargo.toml`:

```toml
[package]
name = "archivr-cli"
version.workspace = true
edition.workspace = true

[[bin]]
name = "archivr"
path = "src/main.rs"

[dependencies]
anyhow.workspace = true
chrono.workspace = true
clap.workspace = true
hex.workspace = true
regex.workspace = true
rusqlite.workspace = true
serde_json.workspace = true
sha3.workspace = true
uuid.workspace = true
```

- [ ] **Step 4: Move current source files into the CLI crate**

Run:

```bash
mkdir -p crates/archivr-cli/src
git mv src/main.rs crates/archivr-cli/src/main.rs
git mv src/database.rs crates/archivr-cli/src/database.rs
git mv src/hash.rs crates/archivr-cli/src/hash.rs
git mv src/twitter.rs crates/archivr-cli/src/twitter.rs
git mv src/downloader crates/archivr-cli/src/downloader
```

- [ ] **Step 5: Verify the CLI crate still builds and tests**

Run:

```bash
cargo test -p archivr-cli
```

Expected: all moved tests pass.

- [ ] **Step 6: Commit the workspace conversion**

```bash
git add Cargo.toml Cargo.lock crates/archivr-cli
git commit -m "chore: move cli into workspace crate"
```

## Task 2: Extract Core Crate

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/archivr-core/Cargo.toml`
- Create: `crates/archivr-core/src/lib.rs`
- Move: `crates/archivr-cli/src/database.rs` -> `crates/archivr-core/src/database.rs`
- Move: `crates/archivr-cli/src/hash.rs` -> `crates/archivr-core/src/hash.rs`
- Move: `crates/archivr-cli/src/twitter.rs` -> `crates/archivr-core/src/twitter.rs`
- Move: `crates/archivr-cli/src/downloader/` -> `crates/archivr-core/src/downloader/`
- Modify: `crates/archivr-cli/src/main.rs`

- [ ] **Step 1: Add `archivr-core` to the workspace**

Update root `Cargo.toml`:

```toml
[workspace]
members = [
    "crates/archivr-core",
    "crates/archivr-cli",
]
resolver = "2"
```

Keep the existing `[workspace.package]` and `[workspace.dependencies]` sections unchanged.

- [ ] **Step 2: Create the core crate manifest**

Create `crates/archivr-core/Cargo.toml`:

```toml
[package]
name = "archivr-core"
version.workspace = true
edition.workspace = true

[dependencies]
anyhow.workspace = true
chrono.workspace = true
hex.workspace = true
regex.workspace = true
rusqlite.workspace = true
serde_json.workspace = true
sha3.workspace = true
uuid.workspace = true
```

- [ ] **Step 3: Add the core crate as a CLI dependency**

Modify `crates/archivr-cli/Cargo.toml`:

```toml
[dependencies]
archivr-core = { path = "../archivr-core" }
anyhow.workspace = true
chrono.workspace = true
clap.workspace = true
regex.workspace = true
rusqlite.workspace = true
serde_json.workspace = true
```

Remove `hex`, `sha3`, and `uuid` from `archivr-cli` dependencies after moving the modules that use them into core.

- [ ] **Step 4: Move reusable modules into core**

Run:

```bash
mkdir -p crates/archivr-core/src
git mv crates/archivr-cli/src/database.rs crates/archivr-core/src/database.rs
git mv crates/archivr-cli/src/hash.rs crates/archivr-core/src/hash.rs
git mv crates/archivr-cli/src/twitter.rs crates/archivr-core/src/twitter.rs
git mv crates/archivr-cli/src/downloader crates/archivr-core/src/downloader
```

- [ ] **Step 5: Create `archivr-core` module exports**

Create `crates/archivr-core/src/lib.rs`:

```rust
pub mod database;
pub mod downloader;
pub mod hash;
pub mod twitter;
```

- [ ] **Step 6: Update core module imports**

In `crates/archivr-core/src/downloader/tweets.rs`, replace:

```rust
use crate::twitter::parse_tweet_id;

use super::store;
```

with:

```rust
use crate::{downloader::store, twitter::parse_tweet_id};
```

Confirm `crates/archivr-core/src/downloader/local.rs`, `store.rs`, and `ytdlp.rs` still refer to `crate::hash::hash_file`.

- [ ] **Step 7: Update CLI imports**

In `crates/archivr-cli/src/main.rs`, remove:

```rust
mod database;
mod downloader;
mod hash;
mod twitter;
```

Add:

```rust
use archivr_core::{
    database, downloader,
    twitter::parse_tweet_id,
};
```

Remove the old line:

```rust
use crate::twitter::parse_tweet_id;
```

- [ ] **Step 8: Verify core and CLI tests**

Run:

```bash
cargo test -p archivr-core
cargo test -p archivr-cli
```

Expected: core database/downloader/hash/twitter tests pass under `archivr-core`; CLI source-detection and initialization tests pass under `archivr-cli`.

- [ ] **Step 9: Commit the core extraction**

```bash
git add Cargo.toml Cargo.lock crates/archivr-core crates/archivr-cli
git commit -m "refactor: extract archive core crate"
```

## Task 3: Add Archive Opening APIs To Core

**Files:**
- Create: `crates/archivr-core/src/archive.rs`
- Modify: `crates/archivr-core/src/lib.rs`
- Modify: `crates/archivr-cli/src/main.rs`

- [ ] **Step 1: Write core archive API tests**

Create `crates/archivr-core/src/archive.rs` with tests first:

```rust
use anyhow::{Context, Result, bail};
use std::{
    env, fs,
    path::{Path, PathBuf},
};

use crate::database;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchivePaths {
    pub archive_path: PathBuf,
    pub store_path: PathBuf,
    pub name: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_path(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        env::temp_dir().join(format!("{prefix}-{nanos}-{}", std::process::id()))
    }

    #[test]
    fn find_archive_path_walks_up_to_dot_archivr() {
        let root = unique_path("archivr-core-find");
        let nested = root.join("a").join("b");
        fs::create_dir_all(root.join(".archivr")).unwrap();
        fs::create_dir_all(&nested).unwrap();

        let found = find_archive_path_from(&nested).unwrap().unwrap();

        assert_eq!(found, root.join(".archivr"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn read_archive_paths_returns_name_and_store_path() {
        let root = unique_path("archivr-core-open");
        let archive_path = root.join(".archivr");
        let store_path = root.join("store");
        fs::create_dir_all(&archive_path).unwrap();
        fs::create_dir_all(&store_path).unwrap();
        fs::write(archive_path.join("name"), "Personal").unwrap();
        fs::write(archive_path.join("store_path"), store_path.display().to_string()).unwrap();

        let paths = read_archive_paths(&archive_path).unwrap();

        assert_eq!(paths.archive_path, archive_path);
        assert_eq!(paths.store_path, store_path);
        assert_eq!(paths.name, "Personal");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn initialize_archive_creates_database_store_and_metadata() {
        let root = unique_path("archivr-core-init");
        let archive_parent = root.join("archive");
        let store_path = root.join("store");

        let paths = initialize_archive(&archive_parent, &store_path, "Personal", false).unwrap();

        assert_eq!(paths.archive_path, archive_parent.join(".archivr"));
        assert!(paths.archive_path.join(database::DATABASE_FILE_NAME).is_file());
        assert!(paths.store_path.join("raw").is_dir());
        assert!(paths.store_path.join("raw_tweets").is_dir());
        assert!(paths.store_path.join("structured").is_dir());
        assert!(paths.store_path.join("temp").is_dir());
        let _ = fs::remove_dir_all(root);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run:

```bash
cargo test -p archivr-core archive::
```

Expected: FAIL because `find_archive_path_from`, `read_archive_paths`, and `initialize_archive` are not defined.

- [ ] **Step 3: Implement archive APIs**

Add this implementation above the test module in `crates/archivr-core/src/archive.rs`:

```rust
pub fn find_archive_path_from(start: &Path) -> Result<Option<PathBuf>> {
    let mut dir = start.to_path_buf();
    loop {
        let candidate = dir.join(".archivr");
        if candidate.is_dir() {
            return Ok(Some(candidate));
        }
        if !dir.pop() {
            return Ok(None);
        }
    }
}

pub fn find_archive_path() -> Result<Option<PathBuf>> {
    let cwd = env::current_dir().context("failed to read current working directory")?;
    find_archive_path_from(&cwd)
}

pub fn read_archive_paths(archive_path: &Path) -> Result<ArchivePaths> {
    if !archive_path.is_dir() {
        bail!("archive path does not exist: {}", archive_path.display());
    }

    let name = fs::read_to_string(archive_path.join("name"))
        .with_context(|| format!("failed to read archive name in {}", archive_path.display()))?
        .trim()
        .to_string();
    let store_path = fs::read_to_string(archive_path.join("store_path"))
        .with_context(|| format!("failed to read store path in {}", archive_path.display()))?;

    Ok(ArchivePaths {
        archive_path: archive_path.to_path_buf(),
        store_path: PathBuf::from(store_path.trim()),
        name,
    })
}

pub fn initialize_archive(
    archive_parent: &Path,
    store_path: &Path,
    archive_name: &str,
    force_with_info_removal: bool,
) -> Result<ArchivePaths> {
    let archive_path = archive_parent.join(".archivr");

    if archive_path.exists() {
        if !archive_path.is_dir() {
            bail!("Archive path exists and is not a directory: {}", archive_path.display());
        }

        if force_with_info_removal {
            fs::remove_dir_all(&archive_path)?;
        } else if fs::read_dir(&archive_path)?.next().is_some() {
            bail!(
                "Archive already exists at {} and is not empty. Use --force-with-info-removal to reinitialize.",
                archive_path.display()
            );
        }
    }

    if store_path.exists() && !force_with_info_removal {
        bail!("Store path already exists at {}", store_path.display());
    }

    fs::create_dir_all(&archive_path)?;
    fs::create_dir_all(store_path)?;
    fs::write(archive_path.join("name"), archive_name)?;
    let canonical_store_path = store_path
        .canonicalize()
        .with_context(|| format!("failed to canonicalize {}", store_path.display()))?;
    fs::write(
        archive_path.join("store_path"),
        canonical_store_path
            .to_str()
            .context("store path is not valid UTF-8")?,
    )?;

    initialize_store_directories(&canonical_store_path)?;
    let conn = database::open_or_initialize(&archive_path)?;
    let _ = database::ensure_default_user(&conn)?;

    Ok(ArchivePaths {
        archive_path,
        store_path: canonical_store_path,
        name: archive_name.to_string(),
    })
}

pub fn initialize_store_directories(store_path: &Path) -> Result<()> {
    fs::create_dir_all(store_path.join("raw"))?;
    fs::create_dir_all(store_path.join("raw_tweets"))?;
    fs::create_dir_all(store_path.join("structured"))?;
    fs::create_dir_all(store_path.join("temp"))?;
    Ok(())
}
```

- [ ] **Step 4: Export the archive module**

Modify `crates/archivr-core/src/lib.rs`:

```rust
pub mod archive;
pub mod database;
pub mod downloader;
pub mod hash;
pub mod twitter;
```

- [ ] **Step 5: Update CLI to call core archive APIs**

In `crates/archivr-cli/src/main.rs`, add `archive` to the import:

```rust
use archivr_core::{
    archive, database, downloader,
    twitter::parse_tweet_id,
};
```

Replace the local `get_archive_path` function with calls to `archive::find_archive_path()`.

Replace the local `initialize_store_directories` function with `archive::initialize_store_directories` or remove it if only `archive::initialize_archive` is used.

In the `Init` command branch, replace the manual `.archivr` creation block with:

```rust
let archive_parent = Path::new(&archive_path_string);
let store_path = if Path::new(&store_path_string).is_relative() {
    env::current_dir()
        .context("failed to read current working directory")?
        .join(store_path_string)
} else {
    Path::new(store_path_string).to_path_buf()
};

let paths = archive::initialize_archive(
    archive_parent,
    &store_path,
    archive_name,
    force_with_info_removal,
)?;

println!("Initialized empty archive in {}", paths.archive_path.display());
```

- [ ] **Step 6: Verify tests**

Run:

```bash
cargo test -p archivr-core archive::
cargo test -p archivr-cli
```

Expected: all tests pass.

- [ ] **Step 7: Commit archive API extraction**

```bash
git add crates/archivr-core crates/archivr-cli
git commit -m "refactor: add core archive opening APIs"
```

## Task 4: Rename Taxonomy To Hierarchical Tags

**Files:**
- Modify: `crates/archivr-core/src/database.rs`

- [ ] **Step 1: Rename test names first**

In `crates/archivr-core/src/database.rs`, rename:

```rust
fn taxonomy_assignments_are_discoverable_through_ancestors()
```

to:

```rust
fn hierarchical_tag_assignments_are_discoverable_through_ancestors()
```

Update the test body to call the new API names:

```rust
let tag_id = create_tag_path(&conn, "/sciences/computer-science/compilers").unwrap();
assign_entry_to_tag(&conn, entry.id, tag_id).unwrap();

assert_eq!(
    entry_count_for_tag_path(&conn, "/sciences/computer-science/compilers").unwrap(),
    1
);
assert_eq!(
    entry_count_for_tag_path(&conn, "/sciences/computer-science").unwrap(),
    1
);
assert_eq!(entry_count_for_tag_path(&conn, "/sciences").unwrap(), 1);
```

- [ ] **Step 2: Run test to verify it fails**

Run:

```bash
cargo test -p archivr-core hierarchical_tag_assignments_are_discoverable_through_ancestors
```

Expected: FAIL because tag function names do not exist yet.

- [ ] **Step 3: Rename schema tables and indexes**

In `initialize_schema`, replace the taxonomy table block with:

```sql
        CREATE TABLE IF NOT EXISTS tags (
            id INTEGER PRIMARY KEY,
            tag_uid TEXT NOT NULL UNIQUE,
            parent_tag_id INTEGER REFERENCES tags(id),
            name TEXT NOT NULL,
            slug TEXT NOT NULL,
            full_path TEXT NOT NULL UNIQUE
        );

        CREATE TABLE IF NOT EXISTS entry_tag_assignments (
            entry_id INTEGER NOT NULL REFERENCES archived_entries(id) ON DELETE CASCADE,
            tag_id INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
            PRIMARY KEY (entry_id, tag_id)
        );
```

Replace:

```sql
        CREATE INDEX IF NOT EXISTS idx_taxonomy_nodes_parent_id ON taxonomy_nodes(parent_id);
```

with:

```sql
        CREATE INDEX IF NOT EXISTS idx_tags_parent_tag_id ON tags(parent_tag_id);
```

- [ ] **Step 4: Rename helper functions**

Replace `create_taxonomy_path` with:

```rust
#[cfg(test)]
pub fn create_tag_path(conn: &Connection, full_path: &str) -> Result<i64> {
    let segments = normalized_tag_segments(full_path)?;
    let mut parent_tag_id = None;
    let mut current_path = String::new();
    let mut current_id = 0;

    for segment in segments {
        current_path.push('/');
        current_path.push_str(segment);

        if let Some(id) = conn
            .query_row(
                "SELECT id FROM tags WHERE full_path = ?1",
                [current_path.as_str()],
                |row| row.get(0),
            )
            .optional()?
        {
            current_id = id;
            parent_tag_id = Some(id);
            continue;
        }

        conn.execute(
            "INSERT INTO tags (tag_uid, parent_tag_id, name, slug, full_path)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                public_id("tag"),
                parent_tag_id,
                humanize_slug(segment),
                segment,
                current_path
            ],
        )?;
        current_id = conn.last_insert_rowid();
        parent_tag_id = Some(current_id);
    }

    Ok(current_id)
}
```

Replace `assign_entry_to_taxonomy` with:

```rust
#[cfg(test)]
pub fn assign_entry_to_tag(conn: &Connection, entry_id: i64, tag_id: i64) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO entry_tag_assignments (entry_id, tag_id)
         VALUES (?1, ?2)",
        params![entry_id, tag_id],
    )?;
    Ok(())
}
```

Replace `entry_count_for_taxonomy_path` with:

```rust
#[cfg(test)]
pub fn entry_count_for_tag_path(conn: &Connection, full_path: &str) -> Result<i64> {
    let count = conn.query_row(
        "WITH RECURSIVE descendants(id) AS (
            SELECT id FROM tags WHERE full_path = ?1
            UNION ALL
            SELECT child.id
            FROM tags child
            JOIN descendants parent ON child.parent_tag_id = parent.id
         )
         SELECT COUNT(DISTINCT eta.entry_id)
         FROM entry_tag_assignments eta
         JOIN descendants d ON eta.tag_id = d.id",
        [full_path],
        |row| row.get(0),
    )?;
    Ok(count)
}
```

Rename `normalized_taxonomy_segments` to `normalized_tag_segments`.

- [ ] **Step 5: Verify there are no taxonomy identifiers left**

Run:

```bash
rg "taxonomy|Taxonomy|category|Category" crates/archivr-core/src/database.rs
```

Expected: no output.

- [ ] **Step 6: Run core tests**

Run:

```bash
cargo test -p archivr-core
```

Expected: all core tests pass.

- [ ] **Step 7: Commit the tag rename**

```bash
git add crates/archivr-core/src/database.rs
git commit -m "refactor: rename taxonomy model to tags"
```

## Task 5: Add Core Entry And Run Query APIs

**Files:**
- Modify: `crates/archivr-core/src/archive.rs`
- Modify: `crates/archivr-core/src/database.rs`

- [ ] **Step 1: Add query result types**

In `crates/archivr-core/src/archive.rs`, add:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntrySummary {
    pub entry_uid: String,
    pub archived_at: String,
    pub source_kind: String,
    pub entity_kind: String,
    pub title: Option<String>,
    pub visibility: String,
    pub original_url: Option<String>,
    pub artifact_count: i64,
    pub total_artifact_bytes: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryDetail {
    pub summary: EntrySummary,
    pub structured_root_relpath: String,
    pub source_metadata_json: String,
    pub display_metadata_json: Option<String>,
    pub artifacts: Vec<EntryArtifactSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryArtifactSummary {
    pub artifact_role: String,
    pub storage_area: String,
    pub relpath: String,
    pub byte_size: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSummary {
    pub run_uid: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub status: String,
    pub requested_count: i64,
    pub discovered_count: i64,
    pub completed_count: i64,
    pub failed_count: i64,
    pub error_summary: Option<String>,
}
```

- [ ] **Step 2: Add failing query tests**

In `crates/archivr-core/src/archive.rs`, add tests that initialize an in-memory DB, create a user/run/source/entry/artifact with existing database helpers, then call:

```rust
let entries = list_root_entries(&conn).unwrap();
assert_eq!(entries.len(), 1);
assert_eq!(entries[0].title.as_deref(), Some("Saved Article"));
assert_eq!(entries[0].artifact_count, 1);

let detail = get_entry_detail(&conn, &entries[0].entry_uid).unwrap().unwrap();
assert_eq!(detail.artifacts.len(), 1);
assert_eq!(detail.artifacts[0].artifact_role, "primary_media");

let runs = list_runs(&conn).unwrap();
assert_eq!(runs.len(), 1);
assert_eq!(runs[0].status, "completed");
```

Use existing helpers `database::ensure_default_user`, `create_archive_run`, `upsert_source_identity`, `create_archived_entry`, `add_entry_artifact`, `complete_archive_run_item`, and `finish_archive_run`.

- [ ] **Step 3: Run tests to verify they fail**

Run:

```bash
cargo test -p archivr-core archive::tests::list_root_entries
```

Expected: FAIL because query functions are not defined.

- [ ] **Step 4: Implement `list_root_entries`**

Add:

```rust
pub fn list_root_entries(conn: &rusqlite::Connection) -> Result<Vec<EntrySummary>> {
    let mut stmt = conn.prepare(
        "SELECT
            e.entry_uid,
            e.archived_at,
            e.source_kind,
            e.entity_kind,
            e.title,
            e.visibility,
            si.canonical_url,
            COUNT(ea.id) AS artifact_count,
            COALESCE(SUM(b.byte_size), 0) AS total_artifact_bytes
         FROM archived_entries e
         JOIN source_identities si ON si.id = e.source_identity_id
         LEFT JOIN entry_artifacts ea ON ea.entry_id = e.id
         LEFT JOIN blobs b ON b.id = ea.blob_id
         WHERE e.parent_entry_id IS NULL
         GROUP BY e.id
         ORDER BY e.archived_at DESC, e.id DESC",
    )?;

    let entries = stmt
        .query_map([], |row| {
            Ok(EntrySummary {
                entry_uid: row.get(0)?,
                archived_at: row.get(1)?,
                source_kind: row.get(2)?,
                entity_kind: row.get(3)?,
                title: row.get(4)?,
                visibility: row.get(5)?,
                original_url: row.get(6)?,
                artifact_count: row.get(7)?,
                total_artifact_bytes: row.get(8)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    Ok(entries)
}
```

- [ ] **Step 5: Implement `get_entry_detail`**

Add:

```rust
pub fn get_entry_detail(
    conn: &rusqlite::Connection,
    entry_uid: &str,
) -> Result<Option<EntryDetail>> {
    let Some((entry_id, structured_root_relpath, source_metadata_json, display_metadata_json)) =
        conn.query_row(
            "SELECT id, structured_root_relpath, source_metadata_json, display_metadata_json
             FROM archived_entries
             WHERE entry_uid = ?1",
            [entry_uid],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .optional()?
    else {
        return Ok(None);
    };

    let summary = list_root_entries(conn)?
        .into_iter()
        .find(|entry| entry.entry_uid == entry_uid)
        .context("entry disappeared while loading detail")?;

    let mut stmt = conn.prepare(
        "SELECT ea.artifact_role, ea.storage_area, ea.relpath, b.byte_size
         FROM entry_artifacts ea
         LEFT JOIN blobs b ON b.id = ea.blob_id
         WHERE ea.entry_id = ?1
         ORDER BY ea.id ASC",
    )?;
    let artifacts = stmt
        .query_map([entry_id], |row| {
            Ok(EntryArtifactSummary {
                artifact_role: row.get(0)?,
                storage_area: row.get(1)?,
                relpath: row.get(2)?,
                byte_size: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    Ok(Some(EntryDetail {
        summary,
        structured_root_relpath,
        source_metadata_json,
        display_metadata_json,
        artifacts,
    }))
}
```

- [ ] **Step 6: Implement `list_runs`**

Add:

```rust
pub fn list_runs(conn: &rusqlite::Connection) -> Result<Vec<RunSummary>> {
    let mut stmt = conn.prepare(
        "SELECT run_uid, started_at, finished_at, status, requested_count,
                discovered_count, completed_count, failed_count, error_summary
         FROM archive_runs
         ORDER BY started_at DESC, id DESC",
    )?;
    let runs = stmt
        .query_map([], |row| {
            Ok(RunSummary {
                run_uid: row.get(0)?,
                started_at: row.get(1)?,
                finished_at: row.get(2)?,
                status: row.get(3)?,
                requested_count: row.get(4)?,
                discovered_count: row.get(5)?,
                completed_count: row.get(6)?,
                failed_count: row.get(7)?,
                error_summary: row.get(8)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    Ok(runs)
}
```

- [ ] **Step 7: Verify query tests**

Run:

```bash
cargo test -p archivr-core archive::
```

Expected: all archive query tests pass.

- [ ] **Step 8: Commit query APIs**

```bash
git add crates/archivr-core/src/archive.rs
git commit -m "feat: add archive query APIs"
```

## Task 6: Add Server Crate And Registry

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/archivr-server/Cargo.toml`
- Create: `crates/archivr-server/src/main.rs`
- Create: `crates/archivr-server/src/registry.rs`

- [ ] **Step 1: Add server dependencies to workspace**

In root `Cargo.toml`, add to `[workspace.dependencies]`:

```toml
axum = "0.7.9"
serde = { version = "1.0.228", features = ["derive"] }
tempfile = "3.13.0"
tokio = { version = "1.41.1", features = ["macros", "rt-multi-thread", "net"] }
toml = "0.8.19"
tower = "0.5.1"
tower-http = { version = "0.6.2", features = ["fs", "trace"] }
```

- [ ] **Step 2: Add server crate to workspace**

Update root `Cargo.toml` workspace members:

```toml
members = [
    "crates/archivr-core",
    "crates/archivr-cli",
    "crates/archivr-server",
]
```

- [ ] **Step 3: Create server manifest**

Create `crates/archivr-server/Cargo.toml`:

```toml
[package]
name = "archivr-server"
version.workspace = true
edition.workspace = true

[dependencies]
anyhow.workspace = true
archivr-core = { path = "../archivr-core" }
axum.workspace = true
serde.workspace = true
tokio.workspace = true
toml.workspace = true
tower-http.workspace = true

[dev-dependencies]
tempfile.workspace = true
```

- [ ] **Step 4: Write registry tests**

Create `crates/archivr-server/src/registry.rs`:

```rust
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{fs, path::{Path, PathBuf}};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MountedArchive {
    pub id: String,
    pub label: String,
    pub archive_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct ServerRegistry {
    pub archives: Vec<MountedArchive>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_round_trips_archives_from_toml() {
        let temp = tempfile::tempdir().unwrap();
        let archive_path = temp.path().join("personal").join(".archivr");
        fs::create_dir_all(&archive_path).unwrap();
        fs::write(archive_path.join("name"), "Personal").unwrap();
        fs::write(archive_path.join("store_path"), temp.path().join("store").display().to_string()).unwrap();

        let registry = ServerRegistry {
            archives: vec![MountedArchive {
                id: "personal".to_string(),
                label: "Personal".to_string(),
                archive_path: archive_path.clone(),
            }],
        };
        let path = temp.path().join("server.toml");
        save_registry(&path, &registry).unwrap();

        let loaded = load_registry(&path).unwrap();

        assert_eq!(loaded, registry);
    }

    #[test]
    fn registry_rejects_duplicate_archive_ids() {
        let registry = ServerRegistry {
            archives: vec![
                MountedArchive {
                    id: "personal".to_string(),
                    label: "Personal".to_string(),
                    archive_path: PathBuf::from("/tmp/a/.archivr"),
                },
                MountedArchive {
                    id: "personal".to_string(),
                    label: "Duplicate".to_string(),
                    archive_path: PathBuf::from("/tmp/b/.archivr"),
                },
            ],
        };

        let err = validate_registry(&registry).unwrap_err().to_string();

        assert!(err.contains("duplicate archive id"));
    }
}
```

- [ ] **Step 5: Run registry tests to verify failure**

Run:

```bash
cargo test -p archivr-server registry::
```

Expected: FAIL because `save_registry`, `load_registry`, and `validate_registry` are not defined.

- [ ] **Step 6: Implement registry functions**

Add above the test module in `registry.rs`:

```rust
pub fn load_registry(path: &Path) -> Result<ServerRegistry> {
    let contents = fs::read_to_string(path)
        .with_context(|| format!("failed to read server registry {}", path.display()))?;
    let registry = toml::from_str::<ServerRegistry>(&contents)
        .with_context(|| format!("failed to parse server registry {}", path.display()))?;
    validate_registry(&registry)?;
    Ok(registry)
}

pub fn save_registry(path: &Path, registry: &ServerRegistry) -> Result<()> {
    validate_registry(registry)?;
    let contents = toml::to_string_pretty(registry)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, contents)?;
    Ok(())
}

pub fn validate_registry(registry: &ServerRegistry) -> Result<()> {
    let mut ids = std::collections::HashSet::new();
    for archive in &registry.archives {
        if archive.id.trim().is_empty() {
            bail!("archive id must not be empty");
        }
        if !ids.insert(archive.id.as_str()) {
            bail!("duplicate archive id: {}", archive.id);
        }
        if !archive.archive_path.ends_with(".archivr") {
            bail!(
                "mounted archive path must point at a .archivr directory: {}",
                archive.archive_path.display()
            );
        }
    }
    Ok(())
}
```

- [ ] **Step 7: Add minimal server main**

Create `crates/archivr-server/src/main.rs`:

```rust
mod registry;
mod routes;

use anyhow::Result;
use std::{net::SocketAddr, path::PathBuf};

#[tokio::main]
async fn main() -> Result<()> {
    let config_path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("archivr-server.toml"));
    let registry = registry::load_registry(&config_path)?;
    let app = routes::app(registry);
    let addr = SocketAddr::from(([127, 0, 0, 1], 8080));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("archivr-server listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}
```

Create temporary `crates/archivr-server/src/routes.rs` so the crate compiles:

```rust
use axum::{routing::get, Router};

use crate::registry::ServerRegistry;

pub fn app(_registry: ServerRegistry) -> Router {
    Router::new().route("/health", get(|| async { "ok" }))
}
```

- [ ] **Step 8: Verify server crate**

Run:

```bash
cargo test -p archivr-server
```

Expected: registry tests pass.

- [ ] **Step 9: Commit server registry**

```bash
git add Cargo.toml Cargo.lock crates/archivr-server
git commit -m "feat: add web server registry"
```

## Task 7: Add Server API Routes

**Files:**
- Modify: `crates/archivr-server/src/routes.rs`
- Modify: `crates/archivr-server/Cargo.toml`

- [ ] **Step 1: Add JSON serialization support**

Add to `crates/archivr-core/Cargo.toml`:

```toml
serde.workspace = true
```

Derive `serde::Serialize` for these core query types in `crates/archivr-core/src/archive.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct EntrySummary {
    pub entry_uid: String,
    pub archived_at: String,
    pub source_kind: String,
    pub entity_kind: String,
    pub title: Option<String>,
    pub visibility: String,
    pub original_url: Option<String>,
    pub artifact_count: i64,
    pub total_artifact_bytes: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct EntryDetail {
    pub summary: EntrySummary,
    pub structured_root_relpath: String,
    pub source_metadata_json: String,
    pub display_metadata_json: Option<String>,
    pub artifacts: Vec<EntryArtifactSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct EntryArtifactSummary {
    pub artifact_role: String,
    pub storage_area: String,
    pub relpath: String,
    pub byte_size: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RunSummary {
    pub run_uid: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub status: String,
    pub requested_count: i64,
    pub discovered_count: i64,
    pub completed_count: i64,
    pub failed_count: i64,
    pub error_summary: Option<String>,
}
```

- [ ] **Step 2: Write API route tests**

In `crates/archivr-server/src/routes.rs`, add tests using `tower::ServiceExt`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    #[tokio::test]
    async fn archives_endpoint_lists_mounted_archives() {
        let registry = ServerRegistry {
            archives: vec![MountedArchive {
                id: "personal".to_string(),
                label: "Personal".to_string(),
                archive_path: std::path::PathBuf::from("/tmp/personal/.archivr"),
            }],
        };
        let response = app(registry)
            .oneshot(Request::builder().uri("/api/archives").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn missing_archive_returns_404() {
        let response = app(ServerRegistry::default())
            .oneshot(Request::builder().uri("/api/archives/missing/entries").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
```

- [ ] **Step 3: Run tests to verify failure**

Run:

```bash
cargo test -p archivr-server routes::
```

Expected: FAIL until API routes exist.

- [ ] **Step 4: Implement API route state and handlers**

Replace `crates/archivr-server/src/routes.rs` with:

```rust
use std::sync::Arc;

use archivr_core::{archive, database};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};

use crate::registry::{MountedArchive, ServerRegistry};

#[derive(Clone)]
pub struct AppState {
    registry: Arc<ServerRegistry>,
}

pub fn app(registry: ServerRegistry) -> Router {
    let state = AppState {
        registry: Arc::new(registry),
    };

    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/api/archives", get(list_archives))
        .route("/api/archives/:archive_id/entries", get(list_entries))
        .route("/api/archives/:archive_id/entries/:entry_uid", get(entry_detail))
        .route("/api/archives/:archive_id/runs", get(list_runs))
        .with_state(state)
}

async fn list_archives(State(state): State<AppState>) -> Json<Vec<MountedArchive>> {
    Json(state.registry.archives.clone())
}

async fn list_entries(
    State(state): State<AppState>,
    Path(archive_id): Path<String>,
) -> Result<Json<Vec<archive::EntrySummary>>, ApiError> {
    let mounted = mounted_archive(&state, &archive_id)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    Ok(Json(archive::list_root_entries(&conn)?))
}

async fn entry_detail(
    State(state): State<AppState>,
    Path((archive_id, entry_uid)): Path<(String, String)>,
) -> Result<Json<archive::EntryDetail>, ApiError> {
    let mounted = mounted_archive(&state, &archive_id)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    let detail = archive::get_entry_detail(&conn, &entry_uid)?
        .ok_or(ApiError::not_found("entry not found"))?;
    Ok(Json(detail))
}

async fn list_runs(
    State(state): State<AppState>,
    Path(archive_id): Path<String>,
) -> Result<Json<Vec<archive::RunSummary>>, ApiError> {
    let mounted = mounted_archive(&state, &archive_id)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    Ok(Json(archive::list_runs(&conn)?))
}

fn mounted_archive<'a>(state: &'a AppState, archive_id: &str) -> Result<&'a MountedArchive, ApiError> {
    state
        .registry
        .archives
        .iter()
        .find(|archive| archive.id == archive_id)
        .ok_or(ApiError::not_found("archive not found"))
}

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn not_found(message: &str) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.to_string(),
        }
    }
}

impl<E> From<E> for ApiError
where
    E: Into<anyhow::Error>,
{
    fn from(error: E) -> Self {
        let error = error.into();
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: format!("{error:#}"),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, self.message).into_response()
    }
}
```

- [ ] **Step 5: Add server dev dependency for route tests**

In `crates/archivr-server/Cargo.toml`, add:

```toml
[dev-dependencies]
tempfile.workspace = true
tower.workspace = true
```

- [ ] **Step 6: Verify route tests**

Run:

```bash
cargo test -p archivr-server
```

Expected: route tests pass.

- [ ] **Step 7: Commit API routes**

```bash
git add Cargo.toml Cargo.lock crates/archivr-core crates/archivr-server
git commit -m "feat: expose archive server APIs"
```

## Task 8: Add Static Web UI

**Files:**
- Modify: `crates/archivr-server/src/routes.rs`
- Create: `crates/archivr-server/static/index.html`
- Create: `crates/archivr-server/static/styles.css`
- Create: `crates/archivr-server/static/app.js`

- [ ] **Step 1: Serve static assets**

In `crates/archivr-server/src/routes.rs`, import:

```rust
use tower_http::services::{ServeDir, ServeFile};
```

Update `app`:

```rust
pub fn app(registry: ServerRegistry) -> Router {
    let state = AppState {
        registry: Arc::new(registry),
    };

    let static_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("static");

    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/api/archives", get(list_archives))
        .route("/api/archives/:archive_id/entries", get(list_entries))
        .route("/api/archives/:archive_id/entries/:entry_uid", get(entry_detail))
        .route("/api/archives/:archive_id/runs", get(list_runs))
        .nest_service("/assets", ServeDir::new(&static_dir))
        .fallback_service(ServeFile::new(static_dir.join("index.html")))
        .with_state(state)
}
```

- [ ] **Step 2: Create HTML shell**

Create `crates/archivr-server/static/index.html`:

```html
<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>Archivr</title>
    <link rel="stylesheet" href="/assets/styles.css">
  </head>
  <body>
    <header class="topbar">
      <div class="brand">Archivr</div>
      <select id="archive-switcher" class="archive-switcher" aria-label="Select archive"></select>
      <nav class="nav">
        <button class="nav-link is-active" data-view="archive">Archive</button>
        <button class="nav-link" data-view="runs">Runs</button>
        <button class="nav-link" data-view="admin">Admin</button>
      </nav>
      <button class="capture-button">+ Capture</button>
    </header>

    <main class="app-shell">
      <section class="workspace">
        <div class="search-row">
          <input id="search" class="search-input" type="search" aria-label="Search archive" value="">
          <div class="search-note">Advanced search later</div>
        </div>

        <section id="archive-view" class="view is-active">
          <table class="entry-table">
            <thead>
              <tr>
                <th>Added</th>
                <th>Title</th>
                <th>Type</th>
                <th>Size</th>
                <th>Original URL</th>
              </tr>
            </thead>
            <tbody id="entries-body"></tbody>
          </table>
        </section>

        <section id="runs-view" class="view">
          <table class="entry-table">
            <thead>
              <tr>
                <th>Started</th>
                <th>Status</th>
                <th>Requested</th>
                <th>Completed</th>
                <th>Failed</th>
              </tr>
            </thead>
            <tbody id="runs-body"></tbody>
          </table>
        </section>

        <section id="admin-view" class="view">
          <div class="admin-note">
            Mounted archive configuration will live here.
          </div>
        </section>
      </section>

      <aside class="context-rail">
        <div class="rail-title">Context</div>
        <div id="context-body" class="rail-body">Select an entry to inspect metadata and artifacts.</div>
      </aside>
    </main>

    <script type="module" src="/assets/app.js"></script>
  </body>
</html>
```

- [ ] **Step 3: Create CSS**

Create `crates/archivr-server/static/styles.css`:

```css
:root {
  color-scheme: light;
  --ink: #1f2924;
  --muted: #62665f;
  --paper: #f7f3ea;
  --paper-2: #eee8dc;
  --paper-3: #fffdf8;
  --line: #d4cabb;
  --line-soft: #e3ddd3;
  --accent: #8d3f30;
  --link: #245f72;
}

* {
  box-sizing: border-box;
}

body {
  margin: 0;
  min-height: 100vh;
  background: var(--paper);
  color: var(--ink);
  font-family: ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
}

.topbar {
  height: 56px;
  display: grid;
  grid-template-columns: auto minmax(160px, 240px) 1fr auto;
  align-items: center;
  gap: 18px;
  padding: 0 18px;
  background: #151f1b;
  color: #f5eee1;
  border-bottom: 3px solid var(--accent);
}

.brand {
  font-family: Georgia, "Times New Roman", serif;
  font-size: 28px;
}

.archive-switcher,
.capture-button,
.nav-link {
  font: inherit;
}

.archive-switcher {
  width: 100%;
  border: 1px solid rgba(245, 238, 225, 0.35);
  background: #202c27;
  color: #f5eee1;
  padding: 7px 9px;
}

.nav {
  display: flex;
  gap: 14px;
  justify-content: flex-end;
}

.nav-link {
  border: 0;
  background: transparent;
  color: #d8d0c3;
  cursor: pointer;
  padding: 8px 0;
}

.nav-link.is-active {
  color: #fffaf0;
  border-bottom: 1px solid #fffaf0;
}

.capture-button {
  border: 0;
  background: #f5eee1;
  color: #151f1b;
  padding: 9px 12px;
  cursor: pointer;
}

.app-shell {
  height: calc(100vh - 56px);
  display: grid;
  grid-template-columns: minmax(0, 1fr) 320px;
}

.workspace {
  min-width: 0;
  overflow: auto;
}

.search-row {
  min-height: 76px;
  display: grid;
  grid-template-columns: minmax(0, 1fr) 210px;
  gap: 18px;
  align-items: center;
  padding: 14px 16px;
  background: var(--paper-2);
  border-bottom: 1px solid var(--line);
}

.search-input {
  width: 100%;
  height: 46px;
  border: 2px solid var(--ink);
  background: var(--paper-3);
  color: var(--ink);
  padding: 0 14px;
  font: inherit;
  font-size: 16px;
}

.search-note {
  color: var(--muted);
  font-size: 13px;
}

.view {
  display: none;
}

.view.is-active {
  display: block;
}

.entry-table {
  width: 100%;
  border-collapse: collapse;
  table-layout: fixed;
  font-size: 13px;
}

.entry-table th {
  position: sticky;
  top: 0;
  z-index: 1;
  text-align: left;
  padding: 10px;
  background: #e1dacd;
  color: #5d625e;
  border-bottom: 1px solid #d2cabe;
  font-size: 11px;
  text-transform: uppercase;
}

.entry-table td {
  padding: 11px 10px;
  border-bottom: 1px solid var(--line-soft);
  vertical-align: top;
}

.entry-table tr:nth-child(even) td {
  background: #f5f1e8;
}

.entry-table tr:nth-child(odd) td {
  background: var(--paper-3);
}

.entry-table tr.is-selected td {
  background: #efe8dc;
  border-top: 2px solid var(--accent);
  border-bottom: 2px solid var(--accent);
}

.entry-title {
  color: var(--link);
  font-weight: 750;
}

.url-cell {
  color: #555;
  word-break: break-all;
}

.context-rail {
  border-left: 1px solid var(--line);
  background: #f0ebe2;
  padding: 18px;
  overflow: auto;
}

.rail-title {
  color: var(--accent);
  font-weight: 800;
  margin-bottom: 10px;
}

.rail-body {
  color: var(--muted);
  font-size: 14px;
  line-height: 1.6;
}

.admin-note {
  padding: 24px;
  color: var(--muted);
}

@media (max-width: 900px) {
  .topbar {
    grid-template-columns: 1fr auto;
    height: auto;
    min-height: 56px;
    padding: 12px;
  }

  .archive-switcher,
  .nav {
    grid-column: 1 / -1;
  }

  .app-shell {
    height: auto;
    grid-template-columns: 1fr;
  }

  .context-rail {
    border-left: 0;
    border-top: 1px solid var(--line);
  }
}
```

- [ ] **Step 4: Create JavaScript**

Create `crates/archivr-server/static/app.js`:

```js
const state = {
  archives: [],
  archiveId: null,
  entries: [],
};

const archiveSwitcher = document.querySelector("#archive-switcher");
const entriesBody = document.querySelector("#entries-body");
const runsBody = document.querySelector("#runs-body");
const contextBody = document.querySelector("#context-body");
const navButtons = document.querySelectorAll(".nav-link");

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

function text(value) {
  return value ?? "";
}

async function getJson(url) {
  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(`${response.status} ${response.statusText}`);
  }
  return response.json();
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
}

function renderEntries() {
  entriesBody.innerHTML = "";
  for (const entry of state.entries) {
    const row = document.createElement("tr");
    row.tabIndex = 0;
    row.innerHTML = `
      <td>${text(entry.archived_at)}</td>
      <td><span class="entry-title">${text(entry.title) || text(entry.entry_uid)}</span></td>
      <td>${text(entry.entity_kind)}</td>
      <td>${formatBytes(entry.total_artifact_bytes)}</td>
      <td class="url-cell">${text(entry.original_url)}</td>
    `;
    row.addEventListener("click", () => selectEntry(entry, row));
    entriesBody.append(row);
  }
}

async function selectEntry(entry, row) {
  document.querySelectorAll(".entry-table tr.is-selected").forEach((selected) => {
    selected.classList.remove("is-selected");
  });
  row.classList.add("is-selected");

  const detail = await getJson(`/api/archives/${state.archiveId}/entries/${entry.entry_uid}`);
  contextBody.innerHTML = `
    <div><strong>${text(detail.summary.title) || text(detail.summary.entry_uid)}</strong></div>
    <div>Type: ${text(detail.summary.entity_kind)}</div>
    <div>Visibility: ${text(detail.summary.visibility)}</div>
    <div>Artifacts: ${detail.artifacts.length}</div>
    <div>Structured root: ${text(detail.structured_root_relpath)}</div>
  `;
}

async function loadRuns() {
  const runs = await getJson(`/api/archives/${state.archiveId}/runs`);
  runsBody.innerHTML = "";
  for (const run of runs) {
    const row = document.createElement("tr");
    row.innerHTML = `
      <td>${text(run.started_at)}</td>
      <td>${text(run.status)}</td>
      <td>${run.requested_count}</td>
      <td>${run.completed_count}</td>
      <td>${run.failed_count}</td>
    `;
    runsBody.append(row);
  }
}

async function loadEntries() {
  state.entries = await getJson(`/api/archives/${state.archiveId}/entries`);
  renderEntries();
  contextBody.textContent = "Select an entry to inspect metadata and artifacts.";
}

async function loadArchives() {
  state.archives = await getJson("/api/archives");
  state.archiveId = state.archives[0]?.id ?? null;
  renderArchives();
  if (state.archiveId) {
    await loadEntries();
    await loadRuns();
  } else {
    contextBody.textContent = "No archives are mounted.";
  }
}

archiveSwitcher.addEventListener("change", async () => {
  state.archiveId = archiveSwitcher.value;
  await loadEntries();
  await loadRuns();
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
```

- [ ] **Step 5: Verify static route**

Run:

```bash
cargo test -p archivr-server
```

Expected: all server tests still pass.

- [ ] **Step 6: Commit static UI**

```bash
git add crates/archivr-server
git commit -m "feat: add archive table web UI"
```

## Task 9: End-To-End Smoke Verification

**Files:**
- No required source changes unless verification finds a bug.

- [ ] **Step 1: Run all Rust tests**

Run:

```bash
cargo test
```

Expected: all workspace tests pass.

- [ ] **Step 2: Create a temporary archive**

Run:

```bash
tmp=$(mktemp -d)
cargo run -p archivr-cli --bin archivr -- init "$tmp/archive" "$tmp/store" --name Smoke
printf 'archive me' > "$tmp/source.txt"
cd "$tmp/archive"
cargo run --manifest-path /Users/thegeneralist/.codex/worktrees/0dd8/archivr/crates/archivr-cli/Cargo.toml --bin archivr -- archive "file://$tmp/source.txt"
```

Expected: archive initializes and one local file entry is recorded.

- [ ] **Step 3: Create server config for the temporary archive**

Run:

```bash
cat > "$tmp/server.toml" <<EOF
[[archives]]
id = "smoke"
label = "Smoke"
archive_path = "$tmp/archive/.archivr"
EOF
```

- [ ] **Step 4: Start server**

Run:

```bash
cargo run -p archivr-server -- "$tmp/server.toml"
```

Expected: server prints `archivr-server listening on http://127.0.0.1:8080`.

- [ ] **Step 5: Verify API endpoints**

In another terminal:

```bash
curl -s http://127.0.0.1:8080/api/archives
curl -s http://127.0.0.1:8080/api/archives/smoke/entries
curl -s http://127.0.0.1:8080/api/archives/smoke/runs
```

Expected:

- `/api/archives` includes `"id":"smoke"`.
- `/entries` returns one entry.
- `/runs` returns at least one run.

- [ ] **Step 6: Verify browser UI**

Open `http://127.0.0.1:8080` in the in-app browser.

Expected:

- The archive switcher shows `Smoke`.
- The archive table renders one row.
- The table uses dense sans-serif rows.
- The warm archive-ledger shell is visible.
- `+ Capture` is present but secondary.
- Clicking the row populates the context rail.
- Runs tab displays the run history.

- [ ] **Step 7: Commit verification fixes if needed**

If verification required source changes:

```bash
git add crates Cargo.toml Cargo.lock
git commit -m "fix: complete web UI smoke path"
```

If no source changes were needed, do not create an empty commit.

## Final Verification

Before marking implementation complete, run:

```bash
cargo fmt --check
cargo test
cargo run -p archivr-server -- --help
```

If `archivr-server` does not implement `--help`, run:

```bash
cargo run -p archivr-server -- /path/to/server.toml
```

with a real temporary server config and verify `/health`, `/api/archives`, and the static UI.

## Handoff Notes

- Keep commits atomic in the task order above.
- Do not implement production auth in this plan.
- Do not implement advanced search in this plan.
- Do not add a top-level Public page in this plan.
- Do not add a top-level Tags page in this plan.
- Keep table typography sans-serif.
- Use `tags` and `hierarchical tags` in user-facing and code/schema terminology.
