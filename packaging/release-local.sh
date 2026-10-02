#!/usr/bin/env bash
# A release built on the maintainer's Mac – the same steps as a release job,
# without a self-hosted runner attached to a public repository.
#
#   packaging/release-local.sh <phase>…
#
# Phases, in order (each can be run on its own):
#   tests    just verify, just test-real, just app-e2e – results kept per commit
#   package  CLI archive with llama.cpp, signed and notarized
#   app      Ancilo.app + DMG, update archive signed with the updater key
#            (asks for the key's password – run it in your own terminal),
#            then notarized and checked like a download
#   checks   tests of the finished artifacts (network, reproducibility, trust)
#   gate     every criterion passed on this commit (a pre-release may name
#            known gaps in packaging/known-gaps-<version>.txt)
#   draft    a draft release on GitHub – published by the maintainer
#
# Needs: a clean checkout at the commit to release, the Developer ID in the
# login keychain, the notarization profile `ancilo-notary`, the updater key
# ~/.tauri/ancilo.key. Never releases unsigned.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd -P)
cd "$root"
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
export APPLE_SIGNING_IDENTITY="${APPLE_SIGNING_IDENTITY:-Developer ID Application: Stefan Grunert (7JSYYWD345)}"
export ANCILO_NOTARY_PROFILE="${ANCILO_NOTARY_PROFILE:-ancilo-notary}"
repo="${ANCILO_REPO:-stefangrunert/ancilo}"

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
tag="v$version"
commit=$(git rev-parse HEAD)
results="$root/results"
dist="$root/dist"
# 0.x and versions with a suffix (1.0.0-rc.1) are pre-releases.
prerelease=false
[[ "$version" == 0.* || "$version" == *-* ]] && prerelease=true
gaps="$root/packaging/known-gaps-$version.txt"

say() { printf '\n== %s\n' "$*"; }

clean_tree() {
  if [ -n "$(git status --porcelain)" ]; then
    echo "the checkout has changes – commit them first, a release is built from a commit" >&2
    git status --short >&2
    exit 1
  fi
}

keep() { # keep <name> <junit.xml>
  mkdir -p "$results/$1"
  if [ -f "$2" ]; then cp "$2" "$results/$1/junit.xml"; else echo "no results for $1 ($2)" >&2; fi
  echo "$commit" > "$results/$1/COMMIT"
}

phase_tests() {
  clean_tree
  rm -rf "$results" && mkdir -p "$results"
  # A red test does not stop the run: everything is kept, the gate decides.
  say "just verify";   just verify   || true; keep verify target/nextest/ci/junit.xml
  say "just test-real"; just test-real || true; keep real target/nextest/real/junit.xml
  say "just app-e2e";  just app-e2e  || true; keep e2e app/test-results/junit.xml
}

phase_package() {
  clean_tree
  rm -rf "$dist"
  say "CLI archive"
  just package "$dist"
  say "sign and notarize the CLI"
  packaging/sign.sh "$dist" cli
}

phase_app() {
  local pkg key
  pkg=$(ls -d "$dist"/ancilo-*-aarch64-apple-darwin)
  key="${TAURI_SIGNING_KEY_FILE:-$HOME/.tauri/ancilo.key}"
  test -f "$key" || { echo "updater key $key missing – without it users never get updates" >&2; exit 1; }
  node -e 'const c=require("./app/src-tauri/tauri.conf.json"); if(!c.plugins.updater.pubkey){console.error("updater pubkey missing");process.exit(1)}'
  if [ -z "${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-}" ]; then
    read -r -s -p "Password of the updater key ($key): " TAURI_SIGNING_PRIVATE_KEY_PASSWORD
    echo
  fi
  export TAURI_SIGNING_PRIVATE_KEY_PASSWORD
  TAURI_SIGNING_PRIVATE_KEY=$(cat "$key")
  export TAURI_SIGNING_PRIVATE_KEY
  say "Ancilo.app and DMG"
  mkdir -p app/src-tauri/binaries
  cp "$pkg/bin/ancilo" app/src-tauri/binaries/ancilo-aarch64-apple-darwin
  cp "$pkg/libexec/ancilo/llama-server" app/src-tauri/binaries/llama-server-aarch64-apple-darwin
  (cd app && npm ci --no-audit --no-fund --silent && npx tauri build --config src-tauri/tauri.release.conf.json)
  unset TAURI_SIGNING_PRIVATE_KEY TAURI_SIGNING_PRIVATE_KEY_PASSWORD
  local bundle=app/src-tauri/target/release/bundle
  cp "$bundle"/dmg/*.dmg "$dist/"
  cp "$bundle/macos/Ancilo.app.tar.gz" "$dist/"
  # The manifest the updater reads (releases/latest/download/latest.json).
  node -e '
    const fs = require("fs");
    const [version, repo, sigFile, out] = process.argv.slice(1);
    const sig = fs.readFileSync(sigFile, "utf8").trim();
    fs.writeFileSync(out, JSON.stringify({
      version, pub_date: new Date().toISOString(),
      platforms: { "darwin-aarch64": { signature: sig, url: `https://github.com/${repo}/releases/download/v${version}/Ancilo.app.tar.gz` } }
    }, null, 2));
  ' "$version" "$repo" "$bundle/macos/Ancilo.app.tar.gz.sig" "$dist/latest.json"
  say "notarize, staple and check the app like a download"
  packaging/sign.sh "$dist" app
}

phase_checks() {
  ANCILO_PACKAGE=$(ls -d "$dist"/ancilo-*-aarch64-apple-darwin)
  ANCILO_ARCHIVE=$(ls "$dist"/ancilo-*.tar.gz)
  ANCILO_DMG=$(ls "$dist"/*.dmg)
  export ANCILO_PACKAGE ANCILO_ARCHIVE ANCILO_DMG
  say "artifact checks"
  cargo nextest run --workspace --profile artifact --run-ignored only \
    -E 'test(the_package_talks_only_to_this_machine) | test(reproducible_release_builds) | test(a_fresh_mac_is_ready_in_minutes) | test(macos_trusts_the_downloads)' || true
  keep artifact target/nextest/artifact/junit.xml
  (cd app/src-tauri && cargo nextest run --profile artifact --run-ignored all) || true
  keep app app/src-tauri/target/nextest/artifact/junit.xml
}

phase_gate() {
  say "release gate for $commit"
  local accept=()
  if [ -f "$gaps" ]; then
    $prerelease || { echo "$gaps: only a pre-release may have known gaps" >&2; exit 1; }
    accept=(--accept "$gaps")
  fi
  cargo run -q -p xtask -- release-gate "$commit" "${accept[@]}" $(find "$results" -name junit.xml) | tee "$results/gate.txt"
}

phase_draft() {
  clean_tree
  test -f "$results/gate.txt" || { echo "run the gate first" >&2; exit 1; }
  grep -q "^release gate ok" "$results/gate.txt" || { echo "the gate did not pass" >&2; exit 1; }
  local notes="$results/notes.md"
  # This version's section of the changelog, then the known gaps.
  awk -v v="$version" '$0 ~ "^## " v {on=1; next} on && /^## / {exit} on' CHANGELOG.md > "$notes"
  if grep -q "^accepted (pre-release)" "$results/gate.txt"; then
    { echo; echo "### Known gaps of this pre-release"; echo
      sed -n 's/^accepted (pre-release): /- /p' "$results/gate.txt"; } >> "$notes"
  fi
  local flags=(--draft --target "$commit" --title "Ancilo $version" --notes-file "$notes")
  $prerelease && flags+=(--prerelease)
  say "draft release $tag"
  gh release create "$tag" --repo "$repo" "${flags[@]}" \
    "$dist"/ancilo-*.tar.gz "$dist"/*.dmg "$dist/Ancilo.app.tar.gz" "$dist/latest.json" \
    "$dist"/MANIFEST-*.txt "$dist/THIRD_PARTY_NOTICES.txt"
  echo "draft: https://github.com/$repo/releases – check it and publish it"
}

[ $# -gt 0 ] || { sed -n '2,20p' "$0"; exit 1; }
for p in "$@"; do
  case "$p" in
    tests | package | app | checks | gate | draft) "phase_$p" ;;
    *) echo "unknown phase $p" >&2; exit 1 ;;
  esac
done
