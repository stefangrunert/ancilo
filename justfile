# Ancilo task runner. `just --list` shows all recipes.

# rustup (Homebrew, keg-only) must win over older system toolchains.
export PATH := "/opt/homebrew/opt/rustup/bin:" + env_var_or_default("HOME", "") + "/.cargo/bin:" + env_var("PATH")

default:
    @just --list

# Deterministic verification: offline, no GPU. The gate for every commit.
verify: fmt-check lint test app-check trace

# Everything including real models, evals and agent integration (needs GPU runner).
verify-full: verify test-real

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

lint:
    cargo clippy --workspace --all-targets -- -D warnings

# Node packages for the SDK contract tests (official OpenAI/Anthropic SDKs).
deps:
    @test -d tests/contract/node_modules/openai || npm ci --prefix tests/contract --no-audit --no-fund

# Unit, integration, contract tests (deterministic).
test: deps
    cargo build -q -p ancilo-testkit --bins
    cargo nextest run --workspace --no-fail-fast --profile ci

# Tests against real llama.cpp and real (small) models.
test-real:
    cargo build -q -p ancilo-testkit --bins
    cargo nextest run --workspace --no-fail-fast --profile real --run-ignored only -E 'not (binary(fresh_mac) | binary(homebrew) | binary(network) | binary(trusted) | test(reproducible_release_builds))'

# Every acceptance criterion of an active milestone needs a test.
trace:
    cargo run -q -p xtask -- trace
    cargo run -q -p xtask -- links

# Progress towards the complete application: state of every acceptance criterion.
status:
    cargo run -q -p xtask -- status

build:
    cargo build --release -p ancilo

# Run the daemon in the foreground (development).
daemon *ARGS:
    cargo run -q -p ancilo -- daemon run {{ARGS}}

# Run the CLI (development).
ancilo *ARGS:
    cargo run -q -p ancilo -- {{ARGS}}

# App (web UI served by the daemon, see app/): packages, types, unit tests.
app-deps:
    @test -d app/node_modules/react || npm ci --prefix app --no-audit --no-fund

# Type check and unit tests of the app (Vitest).
app-check: app-deps
    cd app && npx tsc -b && npx vitest run

# Build the app's web UI (served by the daemon under /app/).
app-build: app-deps
    cd app && npx vite build

# Regenerate the app's API types from the daemon's OpenAPI document.
app-api: app-deps
    cd app && node scripts/gen-api.mjs

# End-to-end tests of the app in Chromium and WebKit against a real daemon.
app-e2e: app-build
    cargo build -q -p ancilo-e2e -p ancilo -p ancilo-testkit --bins
    cd app && npx playwright test

# Native shell smoke test (desktop session needed).
app-smoke:
    cargo build -q -p ancilo
    cd app/src-tauri && cargo test -- --nocapture

# Ancilo.app for this Mac (CLI, daemon and llama.cpp inside) – no release needed.
app-bundle:
    packaging/app-local.sh

# Build Ancilo.app and install it to /Applications (replaces the previous copy, restarts it).
app-install:
    packaging/app-local.sh install

# Release packages (CLI archive with llama.cpp, files for the app bundle) – reproducible.
package *OUT:
    packaging/package.sh {{OUT}}

# Two independent builds of the same commit must ship identical files (M9-AC-08).
repro-check:
    packaging/repro-check.sh
