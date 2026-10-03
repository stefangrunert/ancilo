#!/usr/bin/env bash
# Builds the text recognition helper (macOS, Vision): packaging/build-ocr.sh <out-file>
# Used by the release packages and – for debug builds and tests – by
# crates/docs/build.rs. Needs the Xcode command line tools (swiftc).
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd -P)
out=${1:?usage: build-ocr.sh <out-file>}
mkdir -p "$(dirname "$out")"
xcrun swiftc -O -target arm64-apple-macos13 -o "$out" "$root/crates/docs/ocr/main.swift"
