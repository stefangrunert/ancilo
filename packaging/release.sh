#!/usr/bin/env bash
# A release – also a quick fix – in one command, on the maintainer's Mac:
#
#   just release        (or packaging/release.sh)
#
# 1. preflight   clean checkout, pushed, version not released yet, changelog section
# 2. build       CLI archive and Ancilo.app/DMG – signed, notarized, checked like a
#                download; the update archive signed with the updater key (asks
#                for its password)
# 3. CI          GitHub's CI on this commit must be green (it runs on every push,
#                in parallel with the build – usually it is done by now)
# 4. draft       a draft release on GitHub – published by the maintainer
#
# About 20–30 minutes, most of it Apple's notarization. The full verification
# (real models, reproducibility, fresh Mac, Homebrew, every acceptance
# criterion) is no release step: `just verify-full` and the artifact checks,
# run regularly and before larger releases (see CONTRIBUTING.md).
#
# Needs: the Developer ID in the login keychain, the notarization profile
# `ancilo-notary`, the updater key ~/.tauri/ancilo.key. Never releases unsigned.
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
dist="$root/dist"
# Only versions with a suffix (1.0.0-rc.1) are pre-releases: the updater reads
# releases/latest, which GitHub never points at a pre-release – 0.x versions
# are normal releases, or they would never reach anyone as an update.
prerelease=false
[[ "$version" == *-* ]] && prerelease=true

say() { printf '\n== %s\n' "$*"; }
fail() { echo "release stopped: $*" >&2; exit 1; }

preflight() {
  say "preflight: $tag from ${commit:0:7}"
  [ -z "$(git status --porcelain)" ] || { git status --short >&2; fail "the checkout has changes – commit them"; }
  git fetch -q origin
  git merge-base --is-ancestor "$commit" origin/main || fail "${commit:0:7} is not pushed to origin/main"
  if gh release view "$tag" --repo "$repo" >/dev/null 2>&1; then
    fail "$tag exists already – raise the version in Cargo.toml (and app/src-tauri/tauri.conf.json)"
  fi
  local app_version
  app_version=$(node -p 'require("./app/src-tauri/tauri.conf.json").version')
  [ "$app_version" = "$version" ] || fail "the app says $app_version, Cargo.toml $version"
  grep -q "^## $version" CHANGELOG.md || fail "CHANGELOG.md has no section '## $version'"
  test -f "${TAURI_SIGNING_KEY_FILE:-$HOME/.tauri/ancilo.key}" || fail "updater key missing – users could never update"
}

build() {
  # A build of this commit is reused (e.g. after the CI was not done yet).
  if [ "$(cat "$dist/COMMIT" 2>/dev/null)" = "$commit" ] && ls "$dist"/*.dmg >/dev/null 2>&1; then
    say "build: reusing the build of ${commit:0:7}"
    return
  fi
  rm -rf "$dist"
  say "CLI archive (with llama.cpp)"
  just package "$dist"
  say "sign and notarize the CLI"
  packaging/sign.sh "$dist" cli

  local key="${TAURI_SIGNING_KEY_FILE:-$HOME/.tauri/ancilo.key}" pkg bundle=app/src-tauri/target/release/bundle
  pkg=$(ls -d "$dist"/ancilo-*-aarch64-apple-darwin)
  if [ -z "${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-}" ]; then
    read -r -s -p "Password of the updater key ($key): " TAURI_SIGNING_PRIVATE_KEY_PASSWORD
    echo
  fi
  say "Ancilo.app and DMG"
  mkdir -p app/src-tauri/binaries
  cp "$pkg/bin/ancilo" app/src-tauri/binaries/ancilo-aarch64-apple-darwin
  cp "$pkg/libexec/ancilo/llama-server" app/src-tauri/binaries/llama-server-aarch64-apple-darwin
  (cd app && npm ci --no-audit --no-fund --silent \
    && TAURI_SIGNING_PRIVATE_KEY="$(cat "$key")" TAURI_SIGNING_PRIVATE_KEY_PASSWORD="$TAURI_SIGNING_PRIVATE_KEY_PASSWORD" \
       npx tauri build --config src-tauri/tauri.release.conf.json)
  unset TAURI_SIGNING_PRIVATE_KEY_PASSWORD
  cp "$bundle"/dmg/*.dmg "$dist/"
  cp "$bundle/macos/Ancilo.app.tar.gz" "$dist/"
  # The manifest the updater reads (releases/latest/download/latest.json).
  node -e '
    const fs = require("fs");
    const [version, repo, sigFile, out] = process.argv.slice(1);
    fs.writeFileSync(out, JSON.stringify({
      version, pub_date: new Date().toISOString(),
      platforms: { "darwin-aarch64": { signature: fs.readFileSync(sigFile, "utf8").trim(),
        url: `https://github.com/${repo}/releases/download/v${version}/Ancilo.app.tar.gz` } }
    }, null, 2));
  ' "$version" "$repo" "$bundle/macos/Ancilo.app.tar.gz.sig" "$dist/latest.json"
  say "notarize, staple and check the app like a download"
  packaging/sign.sh "$dist" app
  # The notarized DMG once more under a name that never changes: the website
  # links to releases/latest/download/Ancilo.dmg and never needs an update.
  cp "$(ls "$dist"/Ancilo_*.dmg)" "$dist/Ancilo.dmg"
  echo "$commit" > "$dist/COMMIT"
}

ci_green() {
  say "GitHub CI on ${commit:0:7}"
  local id status conclusion
  id=$(gh run list --repo "$repo" --commit "$commit" --workflow CI --limit 1 --json databaseId --jq '.[0].databaseId // empty')
  [ -n "$id" ] || fail "no CI run for ${commit:0:7} – push it first"
  status=$(gh run view "$id" --repo "$repo" --json status --jq .status)
  if [ "$status" != "completed" ]; then
    echo "CI still running – waiting for it"
    gh run watch "$id" --repo "$repo" --exit-status >/dev/null || true
  fi
  conclusion=$(gh run view "$id" --repo "$repo" --json conclusion --jq .conclusion)
  [ "$conclusion" = "success" ] || fail "CI on ${commit:0:7} is $conclusion: https://github.com/$repo/actions/runs/$id"
  echo "green"
}

draft() {
  local notes="$dist/notes.md"
  awk -v v="$version" '$0 ~ "^## " v {on=1; next} on && /^## / {exit} on' CHANGELOG.md > "$notes"
  local flags=(--draft --target "$commit" --title "Ancilo $version" --notes-file "$notes")
  $prerelease && flags+=(--prerelease)
  say "draft release $tag"
  gh release create "$tag" --repo "$repo" "${flags[@]}" \
    "$dist"/ancilo-*.tar.gz "$dist"/Ancilo_*.dmg "$dist/Ancilo.dmg" "$dist/Ancilo.app.tar.gz" "$dist/latest.json" \
    "$dist"/MANIFEST-*.txt "$dist/THIRD_PARTY_NOTICES.txt"
  # A draft may come back as "untagged-…": the tag must be the version.
  local id
  id=$(gh api "repos/$repo/releases" --jq ".[] | select(.draft and .target_commitish == \"$commit\") | .id" | head -1)
  [ -n "$id" ] && gh api -X PATCH "repos/$repo/releases/$id" -f tag_name="$tag" --jq '"tag: \(.tag_name)"'
  echo
  echo "Draft ready: https://github.com/$repo/releases – check it, then publish it."
}

preflight
build
ci_green
draft
