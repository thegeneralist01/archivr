# X Article, Vision Summaries, and Summary Search Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make X Articles summarize their archived article text, let a user explicitly attach eligible archived images to a requested summary, and search the latest completed summary text (including its JSON tags).

**Architecture:** Keep `archivr-core` synchronous and make the summary input the single carrier of both reduced text and an opt-in, bounded list of image descriptors. The image-selection policy is serialized into the existing `input_sha256` preimage, preserving the existing `entry_summaries` uniqueness key without a migration. Extend the existing server-side entry search query with a correlated latest-completed-summary predicate; the endpoint response and frontend search transport stay unchanged.

**Tech Stack:** Rust 2024, `anyhow`, `rusqlite`, `reqwest` blocking HTTP, `serde_json`, Axum, React JSX, and plain CSS.

---

## File map and interfaces

| File | Responsibility |
| --- | --- |
| `crates/archivr-core/src/summarizer.rs` | X Article reducer, image candidate policy, `SummaryBuildOptions`, `SummaryImage`, cache digest preimage, provider request payloads, Codex invocation, and unit tests. |
| `crates/archivr-server/src/routes.rs` | Parse `include_images`, reject an unsupported Claude CLI vision request before a job is created, and pass build options to both cache lookup and the background worker. |
| `frontend/src/api.js` | Send the explicit `include_images` boolean in the existing summary POST. |
| `frontend/src/components/ContextRail.jsx` | Per-generation checkbox, provider-specific disabled Claude state, privacy/cap warning, and request wiring. |
| `frontend/src/styles.css` | Dedicated summary-image option layout and disabled-note treatment. |
| `crates/archivr-core/src/archive.rs` | Correlated SQL predicate for the latest completed summary and search tests. |
| `docs/README.md`, `AGENTS.md`, `ARCHIVR-MENTAL-MODEL.md` | User, contributor, and architectural documentation after the implementation is complete. |

Define the following core interfaces before server or UI tasks use them. `archive_file` is an absolute local path derived from `ArchivePaths.store_path` plus the stored artifact `relpath`; it is never returned from an API.

```rust
pub const MAX_SUMMARY_IMAGES: usize = 4;
pub const MAX_SUMMARY_IMAGE_BYTES: u64 = 5 * 1024 * 1024;
pub const MAX_SUMMARY_IMAGE_TOTAL_BYTES: u64 = 12 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SummaryBuildOptions {
    pub include_images: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryImage {
    pub sha256: String,
    pub mime_type: String,
    pub byte_size: u64,
    pub archive_file: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryRequest {
    pub entry_uid: String,
    pub title: Option<String>,
    pub source_kind: String,
    pub entity_kind: String,
    pub content: String,
    pub images: Vec<SummaryImage>,
}

pub fn build_summary_input(
    paths: &ArchivePaths,
    entry_uid: &str,
    options: SummaryBuildOptions,
) -> Result<SummaryInput>;

pub fn summarize_entry(
    archive_paths: &ArchivePaths,
    entry_uid: &str,
    options: SummaryBuildOptions,
    provider: &dyn SummaryProvider,
    prompt_version: &str,
) -> Result<database::EntrySummaryRecord>;
```

Use one deterministic digest preimage: `content` bytes, then `"\0images="`, then `include_images` as `"0"` or `"1"`, followed by each selected image in query order as `"\0" + sha256 + "\0" + mime_type + "\0" + byte_size`. Hash that complete byte sequence with the existing `hash::hash_bytes`. This means an unchecked request has no images and a different digest from a checked request even when no candidate qualifies.

### Task 1: Add the X Article reducer with test-first precedence

**Files:**

- Modify: `crates/archivr-core/src/summarizer.rs`
- Test: `crates/archivr-core/src/summarizer.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Write failing tests for each per-status X Article precedence rule.**

  Add assertions against `extract_tweet_text` using values that retain a link-only top-level tweet body:

  ```rust
  #[test]
  fn extract_tweet_text_prefers_x_article_plain_text_over_tco_body() {
      let tweet = serde_json::json!({
          "full_text": "https://t.co/article",
          "article": { "title": "Skin guide", "plain_text": "Use sunscreen daily." }
      });
      assert_eq!(extract_tweet_text(&tweet).as_deref(),
          Some("Skin guide\n\nUse sunscreen daily."));
  }

  #[test]
  fn extract_tweet_text_uses_article_blocks_when_plain_text_is_empty() {
      let tweet = serde_json::json!({"article": {
          "title": "Blocks", "plain_text": " ",
          "blocks": [{"text": "First"}, {"children": [{"text": "Second"}]}]
      }});
      assert_eq!(extract_tweet_text(&tweet).as_deref(), Some("Blocks\n\nFirst\n\nSecond"));
  }

  #[test]
  fn extract_tweet_text_falls_back_from_article_to_link_only_tweet_body() {
      let tweet = serde_json::json!({
          "full_text": "https://t.co/fallback",
          "article": {"title": "Preview", "preview_text": "Preview copy", "summary_text": "Later"}
      });
      assert_eq!(extract_tweet_text(&tweet).as_deref(), Some("Preview\n\nPreview copy"));
  }
  ```

- [ ] **Step 2: Run the focused test target and observe it fail.**

  Run: `cargo test -p archivr-core extract_tweet_text_`

  Expected: FAIL because the current reducer returns the top-level `full_text` or does not descend into `article.blocks`.

- [ ] **Step 3: Implement `article_text` and deterministic block flattening.**

  Add private helpers before `extract_tweet_text`:

  ```rust
  fn nonempty_string(v: &serde_json::Value, key: &str) -> Option<String>;
  fn flatten_article_blocks(v: &serde_json::Value, out: &mut Vec<String>);
  fn article_text(status: &serde_json::Value) -> Option<String>;
  ```

  `article_text` must inspect the status's `article` object before ordinary fields. With a nonempty title, format each successful source as `title + "\n\n" + body`; if title is empty, return only `body`. Select the body in this exact order: nonblank `plain_text`; recursive text leaves from `blocks` in JSON array/object encounter order; nonblank `preview_text`; nonblank `summary_text`. `flatten_article_blocks` must collect only textual scalar values from conventional textual keys (`text`, `plain_text`, `content`, `body`, `title`, `heading`) and recursively visit arrays and objects; it must not stringify IDs, URLs, booleans, media metadata, or arbitrary scalar fields. Join block leaves with `"\n\n"`.

- [ ] **Step 4: Integrate the helper into all tweet-status paths.**

  Make `one(v)` call `article_text(v).or_else(|| normal_tweet_text(v))`, where `normal_tweet_text` preserves the existing `full_text`, `text`, `content`, `body` sequence. Keep support for a top-level `{ "tweet": ... }` wrapper and for embedded `thread`, `tweets`, and `replies` members.

- [ ] **Step 5: Add the thread-artifact regression test and run the focused tests.**

  Add a fixture archive with two `raw_tweet_json` artifacts, where each JSON status has article text, then assert `build_summary_input(..., SummaryBuildOptions::default())?.request.content` contains both article bodies separated by `"\n\n---\n\n"`. Run: `cargo test -p archivr-core extract_tweet_text_ build_summary_input_`

  Expected: PASS, including the existing wrapped/thread tweet tests.

- [ ] **Step 6: Commit the atomic reducer change.**

  ```bash
  git add crates/archivr-core/src/summarizer.rs
  git commit -m "fix: summarize X Article text"
  ```

### Task 2: Model and select explicit image inputs in core

**Files:**

- Modify: `crates/archivr-core/src/summarizer.rs`
- Test: `crates/archivr-core/src/summarizer.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Write failing candidate-selection and digest tests.**

  Build a temporary archive entry containing `media` artifacts for valid `jpg`, `png`, `webp`, `gif`, and `avif`, plus `avatar`, `video`, `audio`, unsupported `svg`, one 5 MiB + 1 byte image, and enough valid images to exceed both the four-image and 12 MiB limits. Assert only role `media`, allowed MIME/extension pairs, at most four descriptors, no descriptor over 5 MiB, and total selected bytes at most 12 MiB. Also assert:

  ```rust
  let text_only = build_summary_input(&paths, &uid, SummaryBuildOptions { include_images: false })?;
  let visual = build_summary_input(&paths, &uid, SummaryBuildOptions { include_images: true })?;
  assert!(text_only.request.images.is_empty());
  assert!(!visual.request.images.is_empty());
  assert_ne!(text_only.input_sha256, visual.input_sha256);
  ```

- [ ] **Step 2: Run the new core tests and observe failure.**

  Run: `cargo test -p archivr-core summary_image_`

  Expected: FAIL because `SummaryRequest` has no images and input construction has no image-selection mode.

- [ ] **Step 3: Define the shared image model and query candidates from existing blobs.**

  Add the constants and `SummaryBuildOptions`, `SummaryImage`, and `SummaryRequest.images` definitions from the file map. Add a private `load_summary_image_candidates(conn, entry_id) -> Result<Vec<SummaryImage>>` querying `entry_artifacts ea JOIN blobs b` for `ea.entry_id = ?1 AND ea.artifact_role = 'media'`, ordered by `ea.id ASC`, selecting `b.sha256`, `b.mime_type`, `b.extension`, `b.byte_size`, and `ea.relpath`.

  Accept a candidate only when both of the following are true: its extension is one of `jpg`, `jpeg`, `png`, `webp`, `gif`, `avif`, and its MIME is the matching `image/jpeg`, `image/png`, `image/webp`, `image/gif`, or `image/avif` family. Resolve `archive_file` under the configured store path and reject a candidate whose canonicalized/normalized path escapes that store root. Stop at the first candidate that would exceed either image count, per-image, or aggregate byte limit; continue scanning later candidates so a too-large or unsupported early artifact cannot hide a valid later one.

- [ ] **Step 4: Build options-aware input and a complete cache digest.**

  Change every current `build_summary_input` call to pass `SummaryBuildOptions::default()` until Task 4 changes the server. Populate `request.images` only if `options.include_images` is true; text extraction and `MAX_INPUT_CHARS` handling remain identical. Replace `hash_bytes(content.as_bytes())` with a private `summary_input_digest(content, include_images, images)` implementing the stated NUL-delimited preimage so the existing database uniqueness constraint continues to distinguish all modes. Do not alter `database.rs`: `input_sha256` already participates in the cache key.

- [ ] **Step 5: Run core regression tests.**

  Run: `cargo test -p archivr-core summary_image_ build_summary_input_`

  Expected: PASS; the text-only request has zero image descriptors, and selection is deterministic by artifact insertion order.

- [ ] **Step 6: Commit the core input model.**

  ```bash
  git add crates/archivr-core/src/summarizer.rs
  git commit -m "feat: model opt-in summary images"
  ```

### Task 3: Make provider transports honor image descriptors

**Files:**

- Modify: `crates/archivr-core/src/summarizer.rs`
- Test: `crates/archivr-core/src/summarizer.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Write failing payload, command, and capability tests.**

  Construct a `SummaryRequest` with one tiny fixture `SummaryImage` and assert `anthropic_request_body` puts a text block and `{ "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "..." } }` in `messages[0].content`. Assert `openai_request_body` emits a text content part plus `{ "type": "image_url", "image_url": { "url": "data:image/png;base64,..." } }`. Unit-test a pure Codex argument builder so its primary arguments contain `exec`, `--image`, the fixture path, `--output-last-message`, output path, and final `-`; test its positional fallback also keeps `--image`. Assert `ClaudeCliProvider::summarize` returns an error containing `Claude CLI cannot attach local images` when `request.images` is nonempty.

- [ ] **Step 2: Run the provider tests and observe failure.**

  Run: `cargo test -p archivr-core "anthropic_request_body|openai_request_body|codex.*image|claude.*images"`

  Expected: FAIL because HTTP bodies are string-only, Codex has no `--image`, and Claude silently accepts the request.

- [ ] **Step 3: Encode images for each HTTP protocol.**

  Add `read_image_base64(image: &SummaryImage) -> Result<String>` which reads only the already bounded selected file and uses `base64::Engine` with the existing dependency or workspace dependency. Make `anthropic_request_body` and `openai_request_body` return their present text-only JSON shapes when `request.images.is_empty()` and their documented content-part arrays otherwise. Preserve `SYSTEM_PROMPT`, `build_user_prompt`, provider URL, headers, and response parsing.

- [ ] **Step 4: Extend Codex safely and reject Claude at the provider boundary.**

  Refactor `codex::run` to accept `&[SummaryImage]`; add `--image <archive_file>` once per selected image before `--output-last-message` in both primary stdin and positional-prompt forms. Keep the existing last-message temporary-file contract and cleanup behavior. Have `ClaudeCliProvider::summarize` `bail!("Claude CLI cannot attach local images; choose an HTTP provider or Codex CLI")` before spawning when images are supplied.

- [ ] **Step 5: Thread image-aware options through synchronous orchestration.**

  Change `summarize_entry` to accept `SummaryBuildOptions` and call the options-aware builder before provider invocation. This is a synchronous core function; do not introduce Tokio, async traits, or new database fields.

- [ ] **Step 6: Run provider and existing summary tests.**

  Run: `cargo test -p archivr-core summarizer::tests`

  Expected: PASS, with all four providers retaining their text-only behavior when `images` is empty.

- [ ] **Step 7: Commit the provider implementation.**

  ```bash
  git add crates/archivr-core/src/summarizer.rs
  git commit -m "feat: attach opted-in images to summaries"
  ```

### Task 4: Expose the opt-in flag through the server API

**Files:**

- Modify: `crates/archivr-server/src/routes.rs`
- Test: `crates/archivr-server/src/routes.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Write failing route tests.**

  Add authenticated POST tests that deserialize a request without `include_images` and assert it takes the text-only build path, and with `{ "provider": "claude_cli", "include_images": true }` assert status `400 BAD_REQUEST` and an error containing `Claude CLI cannot attach local images`. Add a successful non-Claude request test with `include_images: true` using a configured test provider and assert the returned cache key differs from the equivalent text-only request. Keep GET assertions unchanged: it remains `{ "entry_uid", "summary" }`.

- [ ] **Step 2: Run the focused route tests and observe failure.**

  Run: `cargo test -p archivr-server "summary.*include_images|claude.*images"`

  Expected: FAIL because the request body does not accept the field and no capability check occurs.

- [ ] **Step 3: Add the backward-compatible request field and capability guard.**

  Extend `SummaryRequestBody` exactly as follows:

  ```rust
  #[derive(Debug, serde::Deserialize)]
  struct SummaryRequestBody {
      provider: String,
      #[serde(default)]
      force: bool,
      #[serde(default)]
      include_images: bool,
  }
  ```

  After resolving `provider_cfg` and before cache lookup/upsert/spawn, return `ApiError::bad_request("Claude CLI cannot attach local images; choose an HTTP provider or Codex CLI")` when `body.include_images && matches!(provider_cfg, ProviderConfig::ClaudeCli(_))`. Construct `SummaryBuildOptions { include_images: body.include_images }` once and pass it to the preflight builder and cloned into `spawn_blocking` for `summarize_entry`.

- [ ] **Step 4: Verify cache and lifecycle consistency.**

  Ensure preflight `build_summary_input` and background `summarize_entry` receive the same options, so `find_entry_summary` and `upsert_pending_entry_summary` use the same digest. Do not change `entry_summaries`, `latest_entry_summary`, or the GET route; the existing input-hash uniqueness constraint is sufficient.

- [ ] **Step 5: Run the focused server tests.**

  Run: `cargo test -p archivr-server "summary.*include_images|claude.*images"`

  Expected: PASS; omitting the new field is text-only and a Claude image request produces no pending summary row.

- [ ] **Step 6: Commit the API wiring.**

  ```bash
  git add crates/archivr-server/src/routes.rs
  git commit -m "feat: accept image summary requests"
  ```

### Task 5: Add the explicit, provider-aware image consent control

**Files:**

- Modify: `frontend/src/api.js`
- Modify: `frontend/src/components/ContextRail.jsx`
- Modify: `frontend/src/styles.css`
- Test: manual browser smoke test (no frontend test harness exists)

- [ ] **Step 1: Inspect the existing provider selector and write the manual failure script.**

  In a locally authenticated entry detail, select an HTTP provider and verify the Summary section currently has no `Include attached images` checkbox; select Claude CLI and verify there is no capability explanation. Record this as the observed pre-implementation failure. Do not add a frontend test framework.

- [ ] **Step 2: Change the API client contract.**

  Change the function signature and POST body only:

  ```js
  export async function requestEntrySummary(
    archiveId, entryUid, { provider, force = false, includeImages = false } = {}
  ) {
    // existing fetch and error parsing
    body: JSON.stringify({ provider, force, include_images: includeImages })
  }
  ```

  Keep all `fetch` calls inside `frontend/src/api.js`; do not add an inline fetch in the component.

- [ ] **Step 3: Implement the local per-generation control.**

  Add `const [includeSummaryImages, setIncludeSummaryImages] = useState(false)` beside the summary provider state. In `handleGenerateSummary`, pass `includeImages: includeSummaryImages`. Reset this state to `false` whenever `detail?.summary?.entry_uid` changes, so a consent choice cannot carry to another entry. Do not persist the checkbox in `sessionStorage`; the consent is per generation and defaults off.

- [ ] **Step 4: Render clear consent, scope, and Claude capability states.**

  In `.rail-summary-controls`, below the provider `<select>`, render a labeled checkbox with exact visible label `Include attached images`. Its help text must say that selected archived images are sent to the chosen provider and that only up to four supported images (5 MiB each, 12 MiB total) can be attached; unsupported or oversized artifacts are skipped. When `summaryProvider === 'claude_cli'`, render the checkbox disabled, force `includeSummaryImages` to false via an effect or provider-change handler, and show `Claude CLI cannot attach local images. Choose an HTTP provider or Codex CLI.` Do not submit a silently dropped image choice.

- [ ] **Step 5: Add scoped plain-CSS rules.**

  Add `.rail-summary-image-option`, `.rail-summary-image-option__label`, `.rail-summary-image-option__note`, and `.rail-summary-image-option--disabled` under the existing summary rail CSS. Use the project variables (`--muted`, `--line`, `--paper`) and preserve keyboard focus and normal checkbox semantics; do not use a generic row class or inline layout styles.

- [ ] **Step 6: Run the manual success script and build verification.**

  Run: `bun run build`

  Expected: successful production bundle in `crates/archivr-server/static`. Then manually verify: unchecked generation sends `include_images:false`; checked Anthropic/OpenAI/Codex generation sends `true`; changing to Claude unchecks/disables the control and shows the exact explanation; server errors still appear through existing `summaryError` handling.

- [ ] **Step 7: Commit source files, not generated static output.**

  ```bash
  git add frontend/src/api.js frontend/src/components/ContextRail.jsx frontend/src/styles.css
  git commit -m "feat: add summary image consent control"
  ```

### Task 6: Search latest completed summary JSON without changing API shape

**Files:**

- Modify: `crates/archivr-core/src/archive.rs`
- Test: `crates/archivr-core/src/archive.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Write failing search tests with real cache rows.**

  Extend `make_test_db_with_entries` or add a focused fixture helper that inserts summary rows through `database::upsert_pending_entry_summary` and `database::update_entry_summary_status`. Add assertions for all of the following:

  ```rust
  // A completed JSON string with {"tags":["skincare","dermatology"]} matches skincare.
  // An unrelated query yields no result.
  // Two completed rows: the newer completed row is searched; the older one is not.
  // A completed older row remains matched while a newer row is pending or failed.
  // source:, entity:, url:, title:, after:, before:, tag: and collection/visibility scope keep their current behavior.
  ```

  Set distinct `updated_at` values (or insert/transition rows in distinct timestamp order) so "latest completed" is unambiguous. The tag assertion must match `summary_text` itself, not `entry_tag_assignments`.

- [ ] **Step 2: Run the focused tests and observe failure.**

  Run: `cargo test -p archivr-core search_.*summary`

  Expected: FAIL because free text only checks entry and source identity fields.

- [ ] **Step 3: Add a parameter-bound latest-completed summary predicate.**

  In the existing unqualified `query.q` block in `search_entries`, preserve every present `LOWER(...) LIKE ?{n}` condition and add this clause using the same single bound `term`:

  ```sql
  OR LOWER(COALESCE((
      SELECT s.summary_text
      FROM entry_summaries s
      WHERE s.entry_id = e.id
        AND s.status = 'completed'
        AND s.summary_text IS NOT NULL
      ORDER BY s.completed_at DESC, s.updated_at DESC, s.id DESC
      LIMIT 1
  ), '')) LIKE ?N
  ```

  Use the existing numbered parameter construction (`?{n}`) and push `term` once, so user text is never concatenated into SQL. `completed_at` ordering means only completed rows participate, and a later pending/failed row cannot displace an older completed row. Do not add an archive method, schema column, migration, route parameter, or frontend response field.

- [ ] **Step 4: Run core search tests.**

  Run: `cargo test -p archivr-core search_`

  Expected: PASS, including old prefix-filter behavior and JSON tag substring matches.

- [ ] **Step 5: Verify the existing API transport needs no change.**

  Inspect `crates/archivr-server/src/routes.rs::search_entries_handler`, `frontend/src/api.js::searchEntries`, and `frontend/src/App.jsx` search call sites. Confirm the server still calls `archive::search_entries` with the same `SearchEntriesQuery` and returns `Vec<EntrySummary>`; record no source edit for these files unless the inspection reveals a type break. Add no client-side filtering.

- [ ] **Step 6: Commit the search change.**

  ```bash
  git add crates/archivr-core/src/archive.rs
  git commit -m "feat: search completed summary tags"
  ```

### Task 7: Document, bundle, and verify the completed feature set

**Files:**

- Modify: `docs/README.md`
- Modify: `AGENTS.md`
- Modify: `ARCHIVR-MENTAL-MODEL.md`
- Generated (do not hand-edit): `crates/archivr-server/static/`

- [ ] **Step 1: Update user documentation.**

  In `docs/README.md`, document that summaries are manual, text-only by default, and the explicit `Include attached images` option sends at most four eligible local images to the selected provider. State the allowed formats and byte limits, identify Anthropic/OpenAI-compatible/Codex support, state Claude CLI cannot attach local images, and state free-text search includes the latest completed summary text and its generated tags.

- [ ] **Step 2: Update contributor constraints.**

  In `AGENTS.md`, record the `SummaryBuildOptions`/digest rule, the image candidate role and limits, the provider capability matrix, and the latest-completed-only search semantic. Preserve the rule that core remains synchronous and that generated static files are not hand-edited.

- [ ] **Step 3: Update the architectural data-flow documentation.**

  In `ARCHIVR-MENTAL-MODEL.md`, extend the LLM Summary section to show explicit UI consent flowing into image selection, cache hashing, provider transport, and the existing row lifecycle. Add that entry search reads only the latest completed `summary_text`, retaining a prior completed result while newer work is pending or failed.

- [ ] **Step 4: Build and test the final implementation.**

  Run:

  ```bash
  cargo test
  bun --cwd frontend run build
  cargo build
  ```

  Expected: all Rust tests pass, frontend build succeeds, and the generated static bundle contains the checkbox UI. Do not hand-edit generated files; include them in a commit only if this repository currently tracks frontend bundle changes after `bun run build`.

- [ ] **Step 5: Run the end-to-end manual smoke test.**

  Start the server with a test archive and verify: an X Article whose normal tweet text is only a t.co URL summarizes article body text; a text-only generation remains unchanged; a checked vision-capable request attaches only bounded eligible images; Claude has a disabled explanatory option and a direct API request is rejected; a `skincare` search finds completed summary JSON tags; pending/failed rows do not hide a previous completed match.

- [ ] **Step 6: Commit documentation and any tracked generated bundle.**

  ```bash
  git add docs/README.md AGENTS.md ARCHIVR-MENTAL-MODEL.md crates/archivr-server/static
  git commit -m "docs: explain image summaries and summary search"
  ```

### Task 8: Final implementation review before integration

**Files:**

- Review: `crates/archivr-core/src/summarizer.rs`
- Review: `crates/archivr-core/src/archive.rs`
- Review: `crates/archivr-server/src/routes.rs`
- Review: `frontend/src/api.js`
- Review: `frontend/src/components/ContextRail.jsx`
- Review: `frontend/src/styles.css`
- Review: `docs/README.md`, `AGENTS.md`, `ARCHIVR-MENTAL-MODEL.md`

- [ ] **Step 1: Perform the approved-design coverage review.**

  Verify A is covered by Tasks 1 and 2 (plain text, blocks, preview/summary, normal tweet fallback, and every thread JSON artifact); B by Tasks 2–5 (explicit default-off consent, selection policy/caps, input hash, all four provider outcomes, POST flag, and compatible GET); and C by Task 6 (latest completed summary JSON/tags, pending/failed semantics, prefix-filter preservation, server-side architecture).

- [ ] **Step 2: Scan the plan and implementation for unfinished markers and type drift.**

  Run: `rg -n -i '\\bt[o]do\\b|\\bt[b]d\\b|placehold[e]r|implement[[:space:]]later' docs/superpowers/plans/2026-08-23-x-article-vision-search.md crates/archivr-core/src/summarizer.rs crates/archivr-core/src/archive.rs crates/archivr-server/src/routes.rs frontend/src`

  Expected: no newly introduced unfinished markers in the changed feature code or plan. Confirm every use of `SummaryBuildOptions`, `SummaryImage`, options-aware `build_summary_input`, and options-aware `summarize_entry` matches the Task 2 definitions.

- [ ] **Step 3: Review commits and working tree.**

  Run: `git log --oneline --decorate -8` and `git status --short`

  Expected: atomic commits cover the reducer, core image model/providers, server API, frontend control, search, and docs; no unintended artifacts or source edits remain.
