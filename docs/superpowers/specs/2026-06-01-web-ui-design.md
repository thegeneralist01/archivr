# Archivr Web UI Design

## Summary

Archivr's first web UI should open directly to the archive itself: a dense, explicit table of saved entries. The interface should take structural inspiration from ArchiveBox's useful index view, but avoid its older visual treatment and noisy control surface. The tone is a warm archive ledger: serious, fast, preservation-oriented, and modern through restraint.

The first implementation is also a foundational refactor. Archivr should become a Rust workspace with reusable archive logic in `archivr-core`, the current command-line behavior in a CLI crate, and a separate `archivr-server` crate for the web server.

## Product Direction

The home surface is the `Archive` view. It shows saved entries immediately, not a dashboard, capture workflow, or triage queue.

The approved UI direction:

- Use an explicit dense table for entries.
- Use a warm archive-ledger shell with modern spacing, color, and interaction details.
- Keep all table text sans-serif.
- Keep `+ Capture` visible but secondary.
- Make search the dominant power object.
- Do not put a tag field beside search.
- Do not put maintenance controls such as re-snapshot, delete, reset, or bulk action strips beside search.
- Use a right-side contextual rail for selected-entry information, not as the primary filter system.
- Leave exact row-click and entry-opening behavior unresolved for now.

The first table should expose practical archive columns such as added time, title, type, size, and original URL. Technical artifact details should be quieter than the entry title and source.

## Search

Search is a major future feature and should be treated as one of Archivr's signature capabilities. For this first slice, search can be a basic server-backed query field or placeholder, but the UI should reserve visual prominence for it.

Search is expected to absorb much of the functionality that might otherwise live in a filter sidebar. The right rail should not become the main filtering interface.

The full search grammar, ranking behavior, saved searches, facets, and advanced search interactions are deferred to a later focused design pass.

## Multi-Archive Model

Archivr should support one web server mounting many independent archives.

Each archive remains portable and self-contained:

```text
some-archive/
  .archivr/
    archivr.sqlite
    store_path
    name
```

The web server has its own registry/config state that points to mounted archive directories:

```text
server state:
  personal -> /path/to/personal/.archivr
  family   -> /path/to/family/.archivr
  research -> /path/to/research/.archivr
```

This means Archivr should not merge all archive entries into one global archive database. Instead, the server chooses a mounted archive, opens that archive's own DB, and queries entries/runs/artifacts from there.

The web UI should include an archive switcher once more than one archive is mounted.

## Workspace Architecture

Move toward a Rust workspace:

```text
crates/
  archivr-core/
  archivr-cli/
  archivr-server/
```

`archivr-core` owns reusable archive behavior:

- archive directory discovery/opening
- archive DB schema and access
- store path handling
- entry, artifact, blob, source identity, run, user, setting, and tag domain operations
- archive-scoped query APIs used by CLI and server

`archivr-cli` owns command-line UX:

- argument parsing
- terminal output
- process exit behavior
- invoking `archivr-core` operations

`archivr-server` owns web-server behavior:

- mounted archive registry/config
- archive switcher data
- HTTP routes
- static frontend assets or frontend build integration
- local development server behavior
- request routing to the selected archive

The server state must stay clearly separate from each mounted archive's DB.

## Navigation

Use a small top navigation model:

```text
[Archive Switcher]  Archive  Runs  Admin        + Capture
```

`Archive` is the main entry table.

`Runs` is archive job history, failures, and progress.

`Admin` is for mounted archive configuration, instance/server settings, and later user management.

`+ Capture` is a button, not a top-level workspace. It can open a capture dialog or page later.

Do not add `Public` as a top-level nav item in the first design. Public/private/unlisted are entry visibility states inside an archive. Public archive publishing UI is deferred.

Do not add top-level `Tags` navigation yet. Tags should be queryable/searchable and appear in entry context, but the first UI does not need a separate tag management workspace.

## Tags

Use the product term `tags` or `hierarchical tags`.

The current `taxonomy` naming should be renamed before UI work builds on it:

```text
taxonomy_nodes             -> tags
entry_taxonomy_assignments -> entry_tag_assignments
```

Proposed tag columns:

```text
tags.id
tags.tag_uid
tags.parent_tag_id
tags.name
tags.slug
tags.full_path
```

Examples:

```text
/sciences/computer-science/compilers
/family/recipes/georgian
/research/ai/evals
```

Avoid user-facing words such as taxonomy and categories. Internally, code and schema should also move to tag terminology while the project is still early.

## Data Flow

The UI talks to `archivr-server`. The server resolves the selected mounted archive, then calls `archivr-core` against that archive's DB and store path.

Initial API shape:

```text
GET /api/archives
GET /api/archives/:archive_id/entries
GET /api/archives/:archive_id/entries/:entry_uid
GET /api/archives/:archive_id/runs
```

The main page loads entries for the selected archive, renders the dense table, and fills the contextual right rail from the selected entry.

Search initially filters or queries entries through the server. The search internals can become more powerful later without changing the high-level UI shape.

## First Implementation Scope

Must have:

- Convert repo toward a Rust workspace.
- Extract reusable archive DB/store/domain logic into `archivr-core`.
- Keep existing CLI behavior working through `archivr-cli` or an equivalent transitional CLI crate.
- Add `archivr-server` as a separate crate.
- Server can register or mount multiple archive directories.
- Web UI can switch archives.
- Main page shows the selected archive's entries in the approved dense table layout.
- Right rail is contextual and can show selected entry metadata/artifacts.
- Runs page shows archive run history.
- Rename taxonomy schema/code language to hierarchical tags.

Should not have yet:

- Full OP search implementation.
- Full browser capture workflow.
- Full production authentication/session/permission model.
- Public archive publishing UI.
- Final row-click/open behavior.
- Full tag management UI.

Authentication is deferred except for whatever minimal local-only guard is needed to run the first web UI safely. The production auth/session/permission model needs a separate design pass covering login, sessions, roles, public/private/unlisted access, and multi-archive permissions.

## Testing

Core tests:

- Move existing archive DB tests into `archivr-core`.
- Test creating hierarchical tag paths.
- Test assigning an entry to the deepest tag.
- Test ancestor tag queries still find assigned entries.
- Test opening an existing archive path returns its DB, store path, and name.

Server tests:

- Mount registry can add/list archives.
- Invalid archive paths are rejected.
- Entry list endpoint queries the selected archive.
- Two mounted archives with separate DBs do not leak entries into each other.

UI/smoke tests:

- Server starts.
- Archive table page loads.
- Archive switcher changes selected archive.
- Selecting an entry can populate the right context rail when that behavior is implemented.
- Runs page loads.

Before claiming UI work complete, verify the table in a browser at desktop and mobile widths for density, legibility, overflow, and right-rail behavior.

## Open Decisions

- Exact row-click/open behavior.
- Exact search grammar and advanced search interactions.
- Browser capture workflow.
- Public archive publishing UI.
- Production authentication/session/permission model.
- Final visual polish.

## Risks

- The workspace refactor will touch many files, so implementation should be staged carefully.
- Renaming taxonomy to tags should happen before UI work to avoid building on the wrong vocabulary.
- Server registry state must stay clearly separate from archive DB state.
- The table can easily feel old if the implementation copies ArchiveBox styling too closely instead of using the approved archive-ledger direction.
