# Archivr Next Work Decision Handoff

> **For future agentic workers:** Start by reading `ARCHIVR-MENTAL-MODEL.md`, then this file. If the user chooses one track below, create a task-level implementation plan before touching code. Use `superpowers:writing-plans` for the chosen track, then implement with `superpowers:subagent-driven-development` or `superpowers:executing-plans`.

**Goal:** Capture the next plausible product and engineering directions after the database, workspace, Nix packaging, and initial multi-archive web UI foundation.

**Current Architecture:** Archivr is a Rust workspace. `archivr-core` owns archive behavior and SQLite access. `archivr-cli` writes archives. `archivr-server` reads mounted archive databases and serves the static browser UI.

**Tech Stack:** Rust, SQLite via `rusqlite`, Axum, static HTML/CSS/JavaScript, Nix flakes.

---

## Current State

The project now has three crates:

| Crate | Role |
|---|---|
| `crates/archivr-core` | Archive/domain logic, database schema, archive queries, downloader helpers |
| `crates/archivr-cli` | Terminal interface for `archivr init` and `archivr archive` |
| `crates/archivr-server` | Web server, mounted archive registry, JSON API, static web UI |

The browser UI currently does these things:

- Mounts one or more archives through `archivr-server.toml`.
- Lists archives.
- Lists root archive entries in a table.
- Supports simple client-side text filtering.
- Fetches entry detail for a selected row.
- Lists archive runs.
- Shows a minimal admin/mounted-archives view.

The browser UI does not yet do these things:

- Open stored artifacts.
- Serve archived files through stable URLs.
- Show rich entry details.
- Search on the server or through SQLite FTS.
- Manage hierarchical tags.
- Capture new material from the browser.
- Provide production auth/session behavior.

## Product Direction Already Decided

These preferences came from the design discussion:

- The first screen should be the archive table, not a stats dashboard.
- Capture should exist as a button/dialog, not as the main landing workflow.
- The UI should remain table-forward and practical.
- The table should use sans-serif typography.
- The right sidebar/detail rail idea is good.
- Search should become a major, powerful feature.
- Do not use "categories" as product language.
- Use "tags" or "hierarchical tags" for the tree-shaped organization model.
- Archives remain self-contained directories with their own `.archivr` database.
- The server can mount many archive directories, but it has its own registry config.

## Recommended Order

1. Make entry details and artifact access useful.
2. Define and implement search v1.
3. Rename/complete hierarchical tags.
4. Add browser capture.
5. Decide auth/session boundaries.

This order makes the UI increasingly useful without forcing organization work or auth decisions too early.

---

# Track 1: Entry Detail And Artifact Access Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:writing-plans` to expand this track before implementation. Steps should use checkbox syntax for tracking.

**Goal:** Let a user select an entry and inspect/open the files Archivr saved for it.

**Architecture:** Add stable artifact-serving routes to `archivr-server`, backed by archive metadata from `archivr-core`. Keep the static UI simple: selected row opens a right-side detail rail with metadata, original URL, structured root, and artifact links.

**Tech Stack:** Rust, Axum, `tower_http::services::ServeFile` or explicit file responses, static JavaScript.

## Files

| File | Responsibility |
|---|---|
| `crates/archivr-core/src/archive.rs` | Resolve an entry artifact to a safe on-disk path under the archive store |
| `crates/archivr-core/src/database.rs` | Add query helpers only if `archive.rs` cannot use existing schema cleanly |
| `crates/archivr-server/src/routes.rs` | Add artifact-serving API route |
| `crates/archivr-server/static/app.js` | Render selected-entry detail and artifact links |
| `crates/archivr-server/static/styles.css` | Make the detail rail readable and table selection obvious |
| `crates/archivr-server/src/routes.rs` tests | Cover missing archive, missing entry, missing artifact, and valid artifact response |

## Proposed API

```text
GET /api/archives/:archive_id/entries/:entry_uid/artifacts/:artifact_index
```

`artifact_index` is the zero-based index from `EntryDetail.artifacts`. This avoids exposing arbitrary relative paths in URLs for v1.

## Acceptance Criteria

- Selecting a row shows title, source kind, entity kind, original URL, visibility, archive timestamp, structured root, and artifacts.
- Artifact links open in a new tab.
- Invalid archive IDs return `404`.
- Invalid entry UIDs return `404`.
- Invalid artifact indexes return `404`.
- Resolved paths cannot escape the configured archive store directory.
- Existing `cargo test` passes.
- A browser smoke test can select a row and see at least one artifact link for an archive with artifacts.

## Key Risk

Path safety matters. Do not concatenate request path strings into filesystem paths. Resolve artifact paths from database rows, join them against the trusted store path, canonicalize where possible, and reject paths outside the store.

---

# Track 2: Search V1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:brainstorming` first if changing query language behavior. Then use `superpowers:writing-plans` before implementation.

**Goal:** Replace simple client-side filtering with a real search API that can become Archivr's "OP search" foundation.

**Architecture:** Start with server-side structured filtering over existing SQLite columns. Keep query parsing small and explicit. Defer full SQLite FTS until the fields and syntax feel right.

**Tech Stack:** Rust, Axum query extractors, SQLite queries with bound parameters, static JavaScript.

## Files

| File | Responsibility |
|---|---|
| `crates/archivr-core/src/archive.rs` | Add `SearchEntriesQuery` and `search_entries` |
| `crates/archivr-server/src/routes.rs` | Add query parameters to entries endpoint or add `/search` endpoint |
| `crates/archivr-server/static/app.js` | Debounce search input and call server |
| `crates/archivr-server/static/index.html` | Keep one prominent search bar |
| `crates/archivr-server/static/styles.css` | Style search states without making the UI feel like a dashboard |

## Recommended API

```text
GET /api/archives/:archive_id/entries/search?q=...&source_kind=...&entity_kind=...&from=...&to=...
```

Keep `/entries` as the default unfiltered archive table.

## Query Language V1

Support plain text first:

```text
polymarket
```

Then add explicit prefixes:

```text
source:x
type:tweet
url:medium.com
title:"resume templates"
after:2026-01-01
before:2026-04-01
```

Do not implement fuzzy search, stemming, ranking, or boolean logic in v1.

## Acceptance Criteria

- Empty search returns the same rows as `/entries`.
- Plain text searches title, original URL, entry UID, source kind, entity kind, and visibility.
- Prefix filters are parsed deterministically.
- Unknown prefixes return `400` with a helpful message.
- Search uses SQL parameters, not string interpolation.
- The UI clearly distinguishes loading, no results, and error states.
- Existing `cargo test` passes.

## Key Risk

Search can sprawl. Keep v1 boring and correct, then iterate toward FTS/ranking after the query language feels right.

---

# Track 3: Hierarchical Tags Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:writing-plans` before implementation.

**Goal:** Make the existing tree organization model use product language: tags and hierarchical tags, not taxonomy/categories.

**Architecture:** Rename remaining domain types and APIs from taxonomy language to tag language where doing so does not break migrations unnecessarily. Keep the database table rename decision explicit: either preserve existing table names internally for compatibility, or migrate to `tags` and `entry_tag_assignments`.

**Tech Stack:** Rust, SQLite migrations/schema initialization, static JavaScript.

## Files

| File | Responsibility |
|---|---|
| `crates/archivr-core/src/database.rs` | Rename schema helpers/types/tests from taxonomy to tags |
| `crates/archivr-core/src/archive.rs` | Expose tag-tree and entry-tag APIs |
| `crates/archivr-server/src/routes.rs` | Add tag tree and entry assignment endpoints |
| `crates/archivr-server/static/app.js` | Render tag filters in the sidebar/detail rail |
| `crates/archivr-server/static/styles.css` | Style tag tree compactly |
| `PLAN.md` | Update old design language so future agents do not reintroduce "taxonomy" product language |

## Naming Decision

Preferred product names:

- `Tag`
- `TagNode` if a code type needs to emphasize tree structure
- `EntryTagAssignment`
- `full_path`, such as `/sciences/computer-science/compilers`

Avoid product-visible names:

- category
- taxonomy
- collection

## Acceptance Criteria

- Public/user-facing docs say "tags" or "hierarchical tags".
- No UI text says "taxonomy" or "categories".
- A tag can have a parent.
- An entry can be assigned to the most specific tag.
- Filtering by an ancestor tag can include descendant tags.
- Existing database tests continue to pass.
- New tests cover parent, child, and ancestor query behavior.

## Key Risk

Do not turn tags into first-screen triage work. The archive table remains the first screen; tags support filtering and detail context.

---

# Track 4: Browser Capture Button Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:writing-plans` before implementation.

**Goal:** Add a small browser capture workflow that starts archive runs from the web UI without making capture the landing page.

**Architecture:** Add a POST endpoint in `archivr-server` that delegates to `archivr-core` capture/archive logic. The UI gets a compact Capture button and dialog. Runs appear in the Runs tab.

**Tech Stack:** Rust, Axum POST routes, Serde JSON, existing downloader code, static JavaScript.

## Files

| File | Responsibility |
|---|---|
| `crates/archivr-core/src/archive.rs` | Factor CLI archive operation into reusable core function if needed |
| `crates/archivr-cli/src/main.rs` | Keep CLI as a thin adapter over core capture logic |
| `crates/archivr-server/src/routes.rs` | Add `POST /api/archives/:archive_id/captures` |
| `crates/archivr-server/static/index.html` | Add Capture button and dialog markup |
| `crates/archivr-server/static/app.js` | Submit capture request and refresh entries/runs |
| `crates/archivr-server/static/styles.css` | Style dialog and disabled/loading states |

## Proposed API

```http
POST /api/archives/:archive_id/captures
Content-Type: application/json

{
  "locator": "tweet:1234567890"
}
```

Successful v1 response:

```json
{
  "run_uid": "run_...",
  "status": "completed"
}
```

## Acceptance Criteria

- Capture is a button, not the primary page.
- Empty locator returns `400`.
- Unsupported locator returns a useful error, not a panic.
- Successful capture creates a run and at least one entry.
- After success, the UI refreshes entries and runs.
- The server route reuses core archive logic rather than duplicating CLI behavior.
- Existing `cargo test` passes.

## Key Risk

Long-running captures will eventually need async job tracking. For v1, it is acceptable for the request to run synchronously if the UI clearly shows loading and errors.

---

# Track 5: Local Auth And Session Boundary Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:brainstorming` before implementation because this affects product/security boundaries.

**Goal:** Decide and document what "local development auth" means before adding public/admin behavior.

**Architecture:** Prefer a local-first model until remote/public hosting is real. Avoid half-building production auth. Document which endpoints are trusted-local and which are public-safe.

**Tech Stack:** Rust, Axum middleware later if needed, browser local UI.

## Files

| File | Responsibility |
|---|---|
| `ARCHIVR-MENTAL-MODEL.md` | Document local-only assumption or chosen auth boundary |
| `docs/README.md` | Tell users whether to expose `archivr-server` to a network |
| `crates/archivr-server/src/main.rs` | Bind address configuration if needed |
| `crates/archivr-server/src/routes.rs` | Add middleware only after the model is chosen |

## Recommended Decision For Now

Keep `archivr-server` local-only:

```text
127.0.0.1:8080
```

Do not advertise it as safe to expose publicly.

## Acceptance Criteria

- Docs clearly say whether the server is local-only.
- The default bind address remains loopback.
- If bind address becomes configurable, docs explain the risk of using `0.0.0.0`.
- No public-sharing feature is implemented before auth/session requirements are chosen.

## Key Risk

Public archive visibility exists in the database model, but that is not the same thing as a secure public web server.

---

# Track 6: Plan Cleanup Implementation Plan

> **For agentic workers:** This track can be done without product brainstorming. Use `superpowers:executing-plans` if implementing.

**Goal:** Remove stale planning ambiguity so future threads do not confuse old database plans with current roadmap.

**Architecture:** Keep root docs small and explicit. `PLAN.md` is currently an older database design plan; either rename it or add a status note at the top.

**Tech Stack:** Markdown only.

## Files

| File | Responsibility |
|---|---|
| `PLAN.md` | Mark as historical database design plan, or rename to `DATABASE-DESIGN-PLAN.md` |
| `ARCHIVR-MENTAL-MODEL.md` | Link to this handoff and any retained historical plan |
| `docs/README.md` | Link to current docs only if useful for users |

## Acceptance Criteria

- A future agent can tell which document is current architecture, which is historical design, and which is next-work planning.
- No recreated `docs/superpowers/` directory.
- No `.gitignore` exception is added for planning docs unless the user explicitly asks.

## Key Risk

Do not delete useful database context. The old plan contains schema rationale that may still be valuable.

---

## Suggested First New Thread Prompt

Use this in the next thread if the goal is to continue product development:

```text
Read ARCHIVR-MENTAL-MODEL.md and NEXT.md. I want to implement Track 1: Entry Detail And Artifact Access. Create a task-level implementation plan first, then wait for approval.
```

If the goal is search:

```text
Read ARCHIVR-MENTAL-MODEL.md and NEXT.md. I want to design Search V1. Start with brainstorming the query language and API boundary, then write an implementation plan.
```

If the goal is tags:

```text
Read ARCHIVR-MENTAL-MODEL.md and NEXT.md. I want to implement hierarchical tags and remove taxonomy/category language. Write the implementation plan first.
```

## Self-Review

- Spec coverage: This handoff covers the next UI usefulness work, search, hierarchical tags, web capture, auth boundary, and plan cleanup.
- Placeholder scan: No unresolved implementation slots are used as required work.
- Type consistency: The document uses current crate names and current API concepts from `ARCHIVR-MENTAL-MODEL.md`.
