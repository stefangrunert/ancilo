# Contributing

Thanks for your interest in Ancilo!

## Before you start

- For larger changes, please open an issue first and describe the problem and your idea.
- Ancilo is developed along specs with acceptance criteria. Every criterion has a test that names it (`// covers: M8-AC-03`); the specs themselves are project notes kept outside this repository (`just trace` checks the coverage where they are present).

## Development

```bash
just verify       # the gate for every commit: formatting, clippy, tests, app checks (~5 min, offline)
just app-e2e      # app end-to-end tests (Chromium, WebKit)
```

Tests come in three tiers – the quick ones decide, the slow ones never block a fix:

| Tier | What | When | Time |
|---|---|---|---|
| **Every commit** | `just verify`, `just app-e2e` – deterministic, offline, every external service faked; GitHub CI runs them on every push | always | minutes |
| **Real world** | `just test-real` – real llama.cpp and small real models, Claude Code and Codex delegating for real | regularly, and when touching models, delegation or the API | ~10–15 min |
| **Benchmarks and artifacts** | `just bench-reliability` (~45 min), reproducible builds, a fresh Mac in a VM, Homebrew, every acceptance criterion (`xtask release-gate`) | before larger releases, or when working on that area | hours |

## Releasing

```bash
just release
```

One command, about 20–30 minutes (most of it Apple's notarization): it checks the checkout, builds the CLI archive and the app, signs and notarizes both, waits for green CI on the commit and creates a **draft** release on GitHub, which the maintainer publishes. A quick fix is a commit, a version bump and `just release` – nothing more. Version numbers follow [Semantic Versioning](https://semver.org); 0.x versions are pre-releases.

- Rust: `cargo fmt`, `clippy -D warnings`; tests with `cargo nextest`.
- App: React + TypeScript in `app/`; API types are generated from the daemon (`just app-api`).
- Behaviour that depends on model output is tested with the scriptable fake model (`crates/testkit`), so tests stay deterministic.
- Commit messages follow [Conventional Commits](https://www.conventionalcommits.org) (`feat(m8): …`, `fix: …`).

## Guidelines

- No telemetry, no network access without an explicit user action.
- Every feature is an operation in the registry – reachable from the CLI, the API, MCP, the assistant and the app alike.
- Keep the UI simple: advanced areas appear only when they matter.

By contributing you agree that your contributions are licensed under the Apache License 2.0.
