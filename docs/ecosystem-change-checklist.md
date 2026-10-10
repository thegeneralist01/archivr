# Ecosystem change checklist

Archivr, the [Chrome extension](https://github.com/thegeneralist01/archivr-extension), and the [MCP server](https://github.com/thegeneralist01/archivr-mcp) form one user-facing ecosystem. This is the shared review for changes that affect behavior, public API contracts, capture options or defaults, shared UI, permissions, documentation, or branding. Use it for direct agent work as well as pull requests. An internal change also needs review when it can change one of those surfaces.

Equivalent capability is the goal where a project's role makes it appropriate. The extension does not need every administrative or local-file feature; the MCP server does not need a visual control. Explain an intentional exception rather than adding a misleading control or tool.

## Record the impact

At the start of work, copy this table into the task, issue, or PR and identify the affected projects. Revisit it before calling the feature complete. For direct commits, include the finished record in the task's completion report. A project marked **No** needs a reason; a **Yes** with unfinished work needs a named follow-up in that project's issue or task tracker.

| Project | Applies? (Yes/No) | Change made, or reason it does not apply | Verification | PR, commit, or follow-up |
| --- | --- | --- | --- | --- |
| Archivr (core, API, web UI, CLI) | | | | |
| Chrome extension | | | | |
| MCP server | | | | |

Check these questions for each applicable project:

- **Archivr:** Does the core behavior, server request/response and defaults, web UI, or CLI need to change? Are authorization, visibility, persistence, and user docs accurate? Update the API contract when clients depend on it.
- **Extension:** Does the shared Capture dialog show the behavior? Does the worker's explicit request allowlist forward it? Check the bridge, browser permissions and privacy boundaries, tests, and the Archivr source revision pinned by extension CI. A visible dialog control alone does not prove the request reaches the server.
- **MCP:** Can an AI client express the operation through the appropriate tool? Check input validation, request forwarding, response schema, role and read-only behavior, tool descriptions, tests, and the README catalogue. The Archivr server remains the authority for access and validation.
- **Across all three:** Do names, defaults, error messages, help text, documentation, and branding still describe the same capability? Start from Archivr's `docs/branding/` for visual identity and the extension's `docs/ui-consistency.md` for browser surfaces. Record deliberate differences.


## Coordinate and finish

When PRs are used, cross-link the related PRs and include the table in each. When work is committed directly, link the related commits in the completion report. Land compatible server API additions first; test the extension and MCP changes against that server revision before landing them. For a breaking contract, support old and new clients during the transition and remove the old shape only after companion changes are ready.

The originating work is complete when every **Yes** row has an implemented and verified change, or the record gives a concrete reason for deferral and a named follow-up. Do not silently treat an unchecked project as **No**. This review is a human and agent practice; it does not require a PR, trigger another PR, or impose a CI gate.
