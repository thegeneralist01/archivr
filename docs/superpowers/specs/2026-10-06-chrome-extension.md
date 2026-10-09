# Archivr Chrome extension implementation

Approved 2026-10-06. Chrome Manifest V3 first; Firefox later.

## Contract
- Server: GET /api/captures/options (ROLE_USER), flat capture defaults/availability plus title_providers [{kind,label}].
- Server: POST /api/archives/:archive_id/captures/text/title, {body,provider} -> {title}; ROLE_USER, no capture or DB mutation.
- Frontend CaptureDialog: optional api adapter (same methods as frontend api.js), initialItems (URL items {locator}; text items {kind:'text',title,body,mime}), persistenceKey (default captureItems), requestKey (changing seeds a separate new request), allowFileUpload (default true). Extension disables file upload in v1 unless working bridge provided.
- Frontend deep link: /?archive=<id>&capture=<encoded-locator>, opens once after login/archive resolution; removes capture param; no submission.
- Extension: worker owns credentials/network/jobs. runtime messages {type, ...} return {ok:true,data} or {ok:false,error,status}; capture API calls mediated by typed methods, never arbitrary fetch URLs.
- Content UI: shared CaptureDialog rendered inside Shadow DOM, api injected, no token available to content scripts. Dialog prefills target, advanced options identical to Archivr. Text/body editable with a first-line 80-char title suggestion and manual AI generation.
- Setup: URL+Connect, existing same-origin server login used to mint named API token, select archive. Credentials in storage.local TRUSTED_CONTEXTS. One server; origin-bound requests; no site-cookie transfer.
- Context: X action-row/detail buttons; YouTube target link beats page identity, video stays in tab and channels/playlists open server deep link; selected text right-click and ordinary page right-click.
- Persist jobs with server/archive identity, track after overlay close/worker restart; no automatic capture-submit retries.

## Work streams
1. Core/server endpoints, tests.
2. Shared capture frontend adapter/seeding/title UI/deep links, tests.
3. Chrome extension worker, setup/settings, context scripts, shared UI build, tests/docs/artifacts.
4. Integrator independently builds/tests and performs spec review then code-quality/security review; resolve defects before delivery.
