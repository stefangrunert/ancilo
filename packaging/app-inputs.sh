#!/usr/bin/env bash
# What the app bundle takes from a package (packaging/package.sh): the
# binaries next to the app's own (externalBin) and the license notices
# (bundle resources) – for the release and a local build alike.
#
#   packaging/app-inputs.sh <package dir> <target triple>
set -euo pipefail
pkg=$1
target=$2
tauri="$(cd "$(dirname "$0")/.." && pwd -P)/app/src-tauri"
mkdir -p "$tauri/binaries"
cp "$pkg/bin/ancilo" "$tauri/binaries/ancilo-$target"
cp "$pkg/libexec/ancilo/llama-server" "$tauri/binaries/llama-server-$target"
cp "$pkg/libexec/ancilo/ancilo-ocr" "$tauri/binaries/ancilo-ocr-$target"
# Ancilo's license and every third-party notice (Rust crates of the CLI and
# the app, npm packages, llama.cpp) go into Ancilo.app/Contents/Resources/licenses.
rm -rf "$tauri/licenses"
mkdir -p "$tauri/licenses"
cp "$pkg/share/doc/ancilo/LICENSE" "$pkg/share/doc/ancilo/THIRD_PARTY_NOTICES.txt" \
    "$pkg/share/doc/ancilo/LICENSE-llama.cpp" "$tauri/licenses/"
