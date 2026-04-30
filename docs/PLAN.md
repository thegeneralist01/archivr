# Archivr Database Design Plan

## Summary
Design the first database as a `SQLite` metadata/index layer for the existing file-based archive store, while making the schema multi-user and public-archive ready from day one. The filesystem remains the source of truth for bytes and rendered archive output; the database becomes the source of truth for users, roles, archive runs, archived entries, visibility, hierarchy, blob reuse, and organization.

Each successfully archived thing becomes its own archived entry. Re-archiving the same source creates a new archived entry row, while deduplicated raw files continue to reuse the same blob rows underneath.

## Key Changes
### Identity, access, and visibility
- `users`
  - Columns: stable public `user_uid`, `username`, `email` nullable, `password_hash`, `status`, `role`, `created_at`, `last_login_at` nullable.
  - Roles: `admin`, `user`.
- `instance_settings`
  - Global booleans for `public_index_enabled`, `public_entry_content_enabled`, `public_archive_submission_enabled`.
  - Defaults all `false`.
- `archived_entries`
  - Add `created_by_user_id`, `owned_by_user_id`, `visibility`.
  - `visibility` values: `private`, `unlisted`, `public`.
- `archive_runs`
  - Add `created_by_user_id`.
- Do not add groups or per-entry ACL tables in v1; keep the schema portable enough to add them later.

### Core archive model
- `archive_runs`
  - One user-started archive operation.
  - Columns: stable public `run_uid`, `created_by_user_id`, `started_at`, `finished_at`, `status`, `requested_count`, `discovered_count`, `completed_count`, `failed_count`, `error_summary`.
- `archive_run_items`
  - One requested or discovered work item inside an archive run.
  - Columns: `run_id`, stable `item_uid`, `parent_item_id` nullable, `ordinal`, `requested_locator`, `canonical_locator` nullable, `source_kind`, `entity_kind`, `status`, `error_text`, `produced_entry_id` nullable.
  - Supports batch requests and container expansion with progress like `0/14`.
- `source_identities`
  - Canonical identity of the thing being archived across re-archives.
  - Columns: `source_kind`, `entity_kind`, `external_id` nullable, `canonical_url` nullable, `normalized_locator`, `identity_key`.
  - Unique constraint on `identity_key`.
- `archived_entries`
  - One archived thing shown in the archive.
  - Columns: stable public `entry_uid`, `source_identity_id`, `archive_run_id`, `parent_entry_id` nullable, `root_entry_id`, `created_by_user_id`, `owned_by_user_id`, `source_kind`, `entity_kind`, `title` nullable, `visibility`, `archived_at`, `original_published_at` nullable, `structured_root_relpath`, `representation_kind`, `source_metadata_json`, `display_metadata_json` nullable.
  - `structured_root_relpath` is required and points to one root under `structured/<entry_uid>/`.
  - Main archive view queries only rows with `parent_entry_id IS NULL`.
  - Child entries remain first-class rows but are nested under the parent in the main view.
- `blobs`
  - One deduplicated raw file in `raw/`.
  - Columns: `sha256`, `byte_size`, `mime_type` nullable, `extension` nullable, `raw_relpath`, `created_at`.
- `entry_artifacts`
  - Selective file pointers attached to an archived entry.
  - Columns: `entry_id`, `artifact_role`, `storage_area`, `relpath`, `blob_id` nullable, `logical_path` nullable, `metadata_json` nullable.
  - `storage_area`: `raw`, `raw_tweets`, `structured`.
  - Store important files only: primary media, raw tweet JSON, avatar, subtitle, thumbnail, manifest, cover image.

### Organization and extensibility
- `taxonomy_nodes`
  - Hierarchical organization tree.
  - Columns: stable `node_uid`, `parent_id` nullable, `name`, `slug`, `full_path`.
  - `full_path` unique, example `/sciences/computer-science/compilers`.
- `entry_taxonomy_assignments`
  - Many-to-many link between archived entries and taxonomy nodes.
  - Assign the most specific node; ancestor membership is derived via recursive queries.
- Keep shared fields relational and source-specific details in `source_metadata_json`.
  - YouTube examples: `video_id`, `channel_id`, duration, playlist membership.
  - Tweet examples: `tweet_id`, `author_handle`, conversation ID, text summary fields.
  - Do not create per-source tables in v1.

### Public/archive access behavior implied by schema
- Public archive browsing is controlled by both instance settings and entry visibility.
- `public` entries are eligible for anonymous listing/viewing only when instance-level public settings allow it.
- `unlisted` entries are not shown in public indexes but can be directly served later by URL/token design.
- `private` entries are visible only to authorized users.
- Ownership is recorded now even if the first UI only exposes simple admin/user behavior.

## Public APIs / Interfaces
- `archivr init`
  - Create the SQLite database and schema alongside the existing archive metadata directory.
  - Keep existing store directories.
- `archivr archive`
  - Start one `archive_run` owned by a user.
  - Insert one or more `archive_run_items`.
  - On success, create one or more `archived_entries`.
  - Link reused raw files through `blobs` and `entry_artifacts`.
  - Record the entry’s `structured_root_relpath`, visibility, and source metadata JSON.
- New persisted domain types
  - `User`
  - `ArchiveRun`
  - `ArchiveRunItem`
  - `ArchivedEntry`
  - `SourceIdentity`
  - `Blob`
  - `EntryArtifact`
  - `TaxonomyNode`
  - `InstanceSettings`

## Test Plan
- Re-archiving the same YouTube video creates two `archived_entries`, one shared `source_identity`, and one shared primary `blob`.
- Archiving a tweet/thread creates one archived entry, records the raw tweet JSON as an `entry_artifact` in `raw_tweets`, and links downloaded media/avatar blobs correctly.
- Archiving a playlist/channel creates one top-level parent entry plus child entries; the main archive query returns only the parent.
- A single archive run with multiple requested locators records multiple run items and correct progress counters.
- A normal user can create entries but cannot manage other users or instance-wide public settings.
- An admin can manage users and instance-wide public settings.
- A `public` entry is still hidden from anonymous users when `public_index_enabled` or `public_entry_content_enabled` is disabled at the instance level.
- A `private` entry never appears in anonymous/public queries.
- Assigning `/sciences/computer-science/compilers` makes the item discoverable through ancestor queries for `sciences` and `computer-science`.
- A website-style entry can be represented as one archived entry with one structured root and no per-asset DB explosion.

## Assumptions
- SQLite is the only target for the first implementation, but the schema should avoid SQLite-only modeling that would block a later Postgres migration.
- The database indexes archive metadata; archive bytes stay on disk.
- Every archived entry gets a stable public ID used for `structured/<entry_uid>/`; timestamps are metadata, not identity.
- `raw_tweets/` remains a valid sibling storage area and is referenced through `entry_artifacts`.
- Titles are optional and nullable.
- Search, FTS, subtitles, transcript indexing, groups, and per-entry ACL sharing are deferred.
- Organization uses hierarchical taxonomy only for now; free-form tags are out of scope.
- The first permissions model matches the simpler ArchiveBox-style shape: admins, normal users, and optional public visibility, without custom group policy in v1.
