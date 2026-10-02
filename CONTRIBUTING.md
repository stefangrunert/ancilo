# Contributing

Thanks for your interest in Ancilo!

## Before you start

- For larger changes, please open an issue first and describe the problem and your idea.
- Ancilo is developed along specs with acceptance criteria. Every criterion has a test that names it (`// covers: M8-AC-03`); the specs themselves are project notes kept outside this repository (`just trace` checks the coverage where they are present).

## Development

```bash
just verify       # the gate for every commit: formatting, clippy, tests, app checks, traceability
just app-e2e      # app end-to-end tests (Chromium, WebKit)
```

- Rust: `cargo fmt`, `clippy -D warnings`; tests with `cargo nextest`.
- App: React + TypeScript in `app/`; API types are generated from the daemon (`just app-api`).
- Behaviour that depends on model output is tested with the scriptable fake model (`crates/testkit`), so tests stay deterministic.
- Commit messages follow [Conventional Commits](https://www.conventionalcommits.org) (`feat(m8): …`, `fix: …`).

## Guidelines

- No telemetry, no network access without an explicit user action.
- Every feature is an operation in the registry – reachable from the CLI, the API, MCP, the assistant and the app alike.
- Keep the UI simple: advanced areas appear only when they matter.

By contributing you agree that your contributions are licensed under the Apache License 2.0.
