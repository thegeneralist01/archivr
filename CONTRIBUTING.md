# Contributing to Archivr

Archivr's [architecture guide](ARCHIVR-MENTAL-MODEL.md) explains where behavior lives. The [user guide](docs/README.md) covers supported inputs and setup. Keep changes focused and describe the resulting behavior and checks actually run when proposing a change.

Before starting and before finishing a user-facing or API change, use the [ecosystem change checklist](docs/ecosystem-change-checklist.md). It asks whether the Chrome extension and MCP server need companion changes, and also covers permissions, docs, and branding. Record a reason when a project is not applicable; name a follow-up for applicable work that remains open. This applies to direct commits and agent tasks as well as PRs.

For code changes, run relevant Rust tests with `cargo test -p <crate>` or `cargo test`, and frontend tests with `bun test` from `frontend/`. Report any tests you did not run and why. If you open a PR, use the PR template and link related changes across repositories. A PR is not required to use the checklist.
