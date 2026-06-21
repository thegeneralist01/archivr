# Track 1: Entry Detail And Artifact Access — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a user select an archive entry and inspect/open the files Archivr saved for it, served through a safe stable URL.

**Architecture:** Add `resolve_artifact_path` to `archivr-core` for safe path resolution, add a `serve_artifact` handler in `archivr-server` that streams the resolved file via `tower_http::services::ServeFile` (handles HTTP Range requests, ETags, and streaming — no full-file buffering), and expand the static UI's context rail to render all entry metadata and clickable artifact links.

**Tech Stack:** Rust, Axum 0.7, `tower_http::services::ServeFile`, static HTML/CSS/JavaScript. No new crate dependencies — `tower` moves from dev-dep to dep within the workspace.

---

## Files

| File | Change |
|---|---|
| `crates/archivr-server/Cargo.toml` | Move `tower` from `[dev-dependencies]` to `[dependencies]` |
| `crates/archivr-core/src/archive.rs` | Add `resolve_artifact_path(store_path, artifact)` returning a safe `PathBuf` |
| `crates/archivr-server/src/routes.rs` | Add `serve_artifact` handler, register route, add 3 tests |
| `crates/archivr-server/static/app.js` | Expand `renderContextDetail` to show all fields + artifact links |
| `crates/archivr-server/static/styles.css` | Add `.artifact-list`, `.artifact-link`, `.rail-section` styles |

---

### Task 1: `resolve_artifact_path` in archivr-core

**Files:**
- Modify: `crates/archivr-core/src/archive.rs`

- [ ] **Step 1: Write the failing tests in the existing `mod tests` block**

  Add to the `mod tests` block at the bottom of `crates/archivr-core/src/archive.rs`:

  ```rust
  #[test]
  fn resolve_artifact_path_returns_absolute_path_within_store() {
      let dir = tempfile::tempdir().unwrap();
      let store_path = dir.path();
      std::fs::create_dir_all(store_path.join("raw/a/b")).unwrap();
      let artifact_file = store_path.join("raw/a/b/abc.pdf");
      std::fs::write(&artifact_file, b"data").unwrap();

      let artifact = EntryArtifactSummary {
          artifact_role: "primary".to_string(),
          storage_area: "raw".to_string(),
          relpath: "raw/a/b/abc.pdf".to_string(),
          byte_size: Some(4),
      };
      let resolved = resolve_artifact_path(store_path, &artifact).unwrap();
      assert_eq!(resolved, artifact_file.canonicalize().unwrap());
  }

  #[test]
  fn resolve_artifact_path_rejects_traversal() {
      let dir = tempfile::tempdir().unwrap();
      let store_path = dir.path();
      let artifact = EntryArtifactSummary {
          artifact_role: "primary".to_string(),
          storage_area: "raw".to_string(),
          relpath: "../escaped.txt".to_string(),
          byte_size: None,
      };
      assert!(resolve_artifact_path(store_path, &artifact).is_err());
  }
  ```

  Note: `tempfile` is already a dev-dependency in `archivr-core`. Check that `use super::*;` is already at the top of the `mod tests` block (it is).

- [ ] **Step 2: Run the tests to see them fail**

  ```bash
  cargo test -p archivr-core resolve_artifact 2>&1
  ```

  Expected: compile error — `resolve_artifact_path` not defined yet.

- [ ] **Step 3: Implement `resolve_artifact_path`**

  Add this function to `crates/archivr-core/src/archive.rs`, after `list_runs` and before `#[cfg(test)]`:

  ```rust
  /// Resolves an artifact to its absolute on-disk path under `store_path`.
  ///
  /// `artifact.relpath` is a store-relative path (e.g. `raw/a/b/abc.pdf`).
  /// The returned path is canonicalized. Returns an error if the resolved path
  /// escapes `store_path` (path traversal protection) or if the file does not exist.
  pub fn resolve_artifact_path(
      store_path: &Path,
      artifact: &EntryArtifactSummary,
  ) -> Result<PathBuf> {
      let joined = store_path.join(&artifact.relpath);
      let canonical_store = store_path
          .canonicalize()
          .with_context(|| format!("failed to canonicalize store path: {}", store_path.display()))?;
      let canonical_artifact = joined
          .canonicalize()
          .with_context(|| format!("artifact path does not exist: {}", joined.display()))?;
      if !canonical_artifact.starts_with(&canonical_store) {
          bail!(
              "artifact path escapes store: {}",
              canonical_artifact.display()
          );
      }
      Ok(canonical_artifact)
  }
  ```

- [ ] **Step 4: Run the tests to see them pass**

  ```bash
  cargo test -p archivr-core resolve_artifact 2>&1
  ```

  Expected: both tests PASS.

- [ ] **Step 5: Run full core test suite to confirm no regressions**

  ```bash
  cargo test -p archivr-core 2>&1
  ```

  Expected: all tests PASS.

- [ ] **Step 6: Commit**

  ```bash
  git add crates/archivr-core/src/archive.rs
  git commit -m "feat(core): add resolve_artifact_path with path traversal protection"
  ```

---

### Task 2: `serve_artifact` route in archivr-server

**Files:**
- Modify: `crates/archivr-server/Cargo.toml`
- Modify: `crates/archivr-server/src/routes.rs`

Route: `GET /api/archives/:archive_id/entries/:entry_uid/artifacts/:artifact_index`

`artifact_index` is the zero-based index into `EntryDetail.artifacts`. This avoids any raw filesystem path in the URL. `ServeFile` streams the file and handles HTTP Range requests — browsers can seek in `<video>` elements and large files never buffer in memory.

- [ ] **Step 1: Move `tower` to production dependencies**

  In `crates/archivr-server/Cargo.toml`, move `tower` from `[dev-dependencies]` to `[dependencies]`:

  ```toml
  [dependencies]
  anyhow.workspace = true
  archivr-core = { path = "../archivr-core" }
  axum.workspace = true
  serde.workspace = true
  tokio.workspace = true
  toml.workspace = true
  tower.workspace = true
  tower-http.workspace = true

  [dev-dependencies]
  tempfile.workspace = true
  tower.workspace = true
  ```

  (`tower` stays in `[dev-dependencies]` too so `tower::ServiceExt` remains available in tests without a redundant import path.)

- [ ] **Step 2: Write the failing tests**

  Add to the existing `mod tests` block in `crates/archivr-server/src/routes.rs`, after the `missing_archive_returns_404` test:

  ```rust
  #[tokio::test]
  async fn artifact_missing_archive_returns_404() {
      let response = app(ServerRegistry::default())
          .oneshot(
              Request::builder()
                  .uri("/api/archives/nope/entries/entry_abc/artifacts/0")
                  .body(Body::empty())
                  .unwrap(),
          )
          .await
          .unwrap();
      assert_eq!(response.status(), StatusCode::NOT_FOUND);
  }

  #[tokio::test]
  async fn artifact_missing_entry_returns_404() {
      let dir = tempfile::tempdir().unwrap();
      archivr_core::archive::initialize_archive(
          dir.path(),
          &dir.path().join("store"),
          "test",
          false,
      )
      .unwrap();
      let archive_path = dir.path().join(".archivr");
      let registry = ServerRegistry {
          archives: vec![MountedArchive {
              id: "test".to_string(),
              label: "Test".to_string(),
              archive_path,
          }],
      };
      let response = app(registry)
          .oneshot(
              Request::builder()
                  .uri("/api/archives/test/entries/entry_doesnotexist/artifacts/0")
                  .body(Body::empty())
                  .unwrap(),
          )
          .await
          .unwrap();
      assert_eq!(response.status(), StatusCode::NOT_FOUND);
  }

  #[tokio::test]
  async fn artifact_out_of_range_index_returns_404() {
      let dir = tempfile::tempdir().unwrap();
      archivr_core::archive::initialize_archive(
          dir.path(),
          &dir.path().join("store"),
          "test",
          false,
      )
      .unwrap();
      let archive_path = dir.path().join(".archivr");
      let registry = ServerRegistry {
          archives: vec![MountedArchive {
              id: "test".to_string(),
              label: "Test".to_string(),
              archive_path,
          }],
      };
      let response = app(registry)
          .oneshot(
              Request::builder()
                  .uri("/api/archives/test/entries/entry_doesnotexist/artifacts/99")
                  .body(Body::empty())
                  .unwrap(),
          )
          .await
          .unwrap();
      assert_eq!(response.status(), StatusCode::NOT_FOUND);
  }
  ```

- [ ] **Step 3: Run the failing tests**

  ```bash
  cargo test -p archivr-server artifact 2>&1
  ```

  Expected: compile error — `serve_artifact` not defined yet.

- [ ] **Step 4: Update imports in `routes.rs`**

  Replace the existing `use axum` block with:

  ```rust
  use axum::{
      Json, Router,
      body::Body,
      extract::{Path, Request, State},
      http::StatusCode,
      response::{IntoResponse, Response},
      routing::get,
  };
  ```

  Add after the `use tower_http` line:

  ```rust
  use tower::ServiceExt;
  use tower_http::services::{ServeDir, ServeFile};
  ```

  (Replace the existing `use tower_http::services::{ServeDir, ServeFile};` line — just add `use tower::ServiceExt;` above or below it.)

- [ ] **Step 5: Implement `serve_artifact`**

  Add this function to `routes.rs` after `list_runs` and before `mounted_archive`. There is no `content_type_for_path` helper — `ServeFile` infers content type from the file extension automatically:

  ```rust
  async fn serve_artifact(
      State(state): State<AppState>,
      Path((archive_id, entry_uid, artifact_index)): Path<(String, String, usize)>,
      req: Request,
  ) -> Result<Response, ApiError> {
      let mounted = mounted_archive(&state, &archive_id)?;
      let paths = archive::read_archive_paths(&mounted.archive_path)?;
      let conn = database::open_or_initialize(&mounted.archive_path)?;
      let detail = archive::get_entry_detail(&conn, &entry_uid)?
          .ok_or(ApiError::not_found("entry not found"))?;
      let artifact = detail
          .artifacts
          .get(artifact_index)
          .ok_or(ApiError::not_found("artifact index out of range"))?;
      let file_path = archive::resolve_artifact_path(&paths.store_path, artifact)?;
      // ServeFile streams the file, handles Range requests (video seeking),
      // sets Content-Type/ETag/Last-Modified, and returns 404 if missing.
      // Its error type is Infallible — file-not-found becomes a 404 response.
      Ok(ServeFile::new(&file_path)
          .oneshot(req)
          .await
          .unwrap()
          .into_response())
  }
  ```

- [ ] **Step 6: Register the route in `app()`**

  In the `app()` function, add after the `entry_detail` route:

  ```rust
  .route(
      "/api/archives/:archive_id/entries/:entry_uid/artifacts/:artifact_index",
      get(serve_artifact),
  )
  ```

  The full router chain becomes:

  ```rust
  Router::new()
      .route("/health", get(|| async { "ok" }))
      .route("/api/archives", get(list_archives))
      .route("/api/archives/:archive_id/entries", get(list_entries))
      .route(
          "/api/archives/:archive_id/entries/:entry_uid",
          get(entry_detail),
      )
      .route(
          "/api/archives/:archive_id/entries/:entry_uid/artifacts/:artifact_index",
          get(serve_artifact),
      )
      .route("/api/archives/:archive_id/runs", get(list_runs))
      .nest_service("/assets", ServeDir::new(&static_dir))
      .fallback_service(ServeFile::new(static_dir.join("index.html")))
      .with_state(state)
  ```

- [ ] **Step 7: Run the new tests**

  ```bash
  cargo test -p archivr-server artifact 2>&1
  ```

  Expected: all three new tests PASS.

- [ ] **Step 8: Run full server test suite**

  ```bash
  cargo test -p archivr-server 2>&1
  ```

  Expected: all tests PASS.

- [ ] **Step 9: Commit**

  ```bash
  git add crates/archivr-server/Cargo.toml crates/archivr-server/src/routes.rs
  git commit -m "feat(server): add streaming serve_artifact route via ServeFile"
  ```

---

### Task 3: Expand the context rail UI

**Files:**
- Modify: `crates/archivr-server/static/app.js`

The current `renderContextDetail` (lines 133–152) shows: title, type, visibility, artifact count (as number), structured root. Replace it with the full implementation showing all metadata fields and clickable artifact links.

- [ ] **Step 1: Replace `renderContextDetail` entirely**

  Find the function at approximately line 133 and replace it with:

  ```js
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
  ```

- [ ] **Step 2: Verify JS parses without errors**

  ```bash
  node --input-type=module < crates/archivr-server/static/app.js 2>&1 | grep -v "ReferenceError\|Cannot find\|is not defined" || true
  ```

  Expected: only DOM-related errors (which happen at runtime, not parse time); no `SyntaxError`.

- [ ] **Step 3: Commit**

  ```bash
  git add crates/archivr-server/static/app.js
  git commit -m "feat(ui): expand context rail with all metadata fields and artifact links"
  ```

---

### Task 4: Style the context rail additions

**Files:**
- Modify: `crates/archivr-server/static/styles.css`

- [ ] **Step 1: Append the new rules to `styles.css`**

  Add before the closing `@media` block (or at the very end of the file):

  ```css
  .rail-entry-title {
    display: block;
    font-size: 15px;
    font-weight: 700;
    color: var(--ink);
    margin-bottom: 12px;
    line-height: 1.4;
  }

  .rail-section {
    margin-bottom: 18px;
  }

  .rail-section-heading {
    font-size: 11px;
    font-weight: 800;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--accent);
    margin-bottom: 6px;
  }

  .rail-label {
    font-weight: 600;
    color: var(--ink);
  }

  .rail-url-link {
    color: var(--accent);
    word-break: break-all;
    font-size: 13px;
  }

  .artifact-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .artifact-link {
    display: block;
    padding: 6px 8px;
    background: var(--paper-3);
    border: 1px solid var(--line);
    color: var(--accent);
    text-decoration: none;
    font-size: 13px;
    border-radius: 3px;
  }

  .artifact-link:hover {
    background: var(--line);
    text-decoration: underline;
  }
  ```

- [ ] **Step 2: Run the full test suite one final time**

  ```bash
  cargo test 2>&1
  ```

  Expected: all tests in all three crates PASS.

- [ ] **Step 3: Commit**

  ```bash
  git add crates/archivr-server/static/styles.css
  git commit -m "feat(ui): style artifact list and context rail sections"
  ```

---

## Acceptance Criteria Checklist

- [ ] Selecting a row shows: title, source kind, entity kind, original URL (as link), visibility, archived_at, structured root, artifact list.
- [ ] Artifact links open in a new tab at `/api/archives/:id/entries/:uid/artifacts/:index`.
- [ ] Invalid archive ID returns `404`.
- [ ] Invalid entry UID returns `404`.
- [ ] Invalid artifact index (out of range) returns `404`.
- [ ] `relpath` containing `../` is rejected by `resolve_artifact_path`.
- [ ] `cargo test` passes across all three crates.

## Key Risk Note

`resolve_artifact_path` canonicalizes the joined path and verifies it starts with the canonicalized store root. This handles `..` traversal in `relpath`. The user only ever controls the numeric `artifact_index` in the URL — the actual `relpath` comes from the database row, so this is defense-in-depth, not the primary input guard.
