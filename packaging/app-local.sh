#!/bin/sh
# Ancilo.app for this Mac – the same bundle as a release (CLI, daemon and llama.cpp inside),
# without the release checks, notarization or update archive.
#
#   packaging/app-local.sh            build → app/src-tauri/target/release/bundle/macos/Ancilo.app
#   packaging/app-local.sh install    build, replace /Applications/Ancilo.app, link `ancilo` into
#                                     ~/.local/bin (ANCILO_BIN_DIR), open the app
#
# Built on this Mac, so macOS does not quarantine it and notarization is not needed. Signed with
# the Developer ID from the keychain when there is one (macOS then keeps granted permissions across
# rebuilds), otherwise ad hoc.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd -P)
out="$root/target/app-local"
target=aarch64-apple-darwin
app="$root/app/src-tauri/target/release/bundle/macos/Ancilo.app"

"$root/packaging/package.sh" "$out"

pkg=$(ls -d "$out"/ancilo-*-$target)
mkdir -p "$root/app/src-tauri/binaries"
cp "$pkg/bin/ancilo" "$root/app/src-tauri/binaries/ancilo-$target"
cp "$pkg/libexec/ancilo/llama-server" "$root/app/src-tauri/binaries/llama-server-$target"

if [ -z "${APPLE_SIGNING_IDENTITY:-}" ]; then
    APPLE_SIGNING_IDENTITY=$(security find-identity -v -p codesigning 2>/dev/null |
        sed -n 's/.*"\(Developer ID Application: .*\)"/\1/p' | head -1)
    [ -n "$APPLE_SIGNING_IDENTITY" ] || APPLE_SIGNING_IDENTITY=-
fi
echo "== app (signing: $APPLE_SIGNING_IDENTITY)"
# No notarization credentials in the environment: Tauri signs but does not notarize.
cd "$root/app"
env -u APPLE_ID -u APPLE_API_KEY APPLE_SIGNING_IDENTITY="$APPLE_SIGNING_IDENTITY" \
    npx tauri build --bundles app \
    --config src-tauri/tauri.release.conf.json \
    --config '{"bundle":{"createUpdaterArtifacts":false}}'
codesign --verify --deep --strict "$app"
echo "== built $app"

[ "${1:-}" = install ] || exit 0

echo "== install /Applications/Ancilo.app"
osascript -e 'tell application "Ancilo" to quit' 2>/dev/null || true
# The running daemon may come from the previous copy; the new app starts its own.
"$app/Contents/MacOS/ancilo" daemon stop 2>/dev/null || true
rm -rf /Applications/Ancilo.app
ditto "$app" /Applications/Ancilo.app
# The `ancilo` command in the terminal: a link into the app, so every install updates it.
bindir=${ANCILO_BIN_DIR:-$HOME/.local/bin}
mkdir -p "$bindir"
if [ -e "$bindir/ancilo" ] && [ ! -L "$bindir/ancilo" ]; then
    echo "!! $bindir/ancilo exists and is not a link – left alone"
else
    ln -sfn /Applications/Ancilo.app/Contents/MacOS/ancilo "$bindir/ancilo"
    case ":$PATH:" in *":$bindir:"*) ;; *) echo "!! $bindir is not on PATH – add it to use \`ancilo\`" ;; esac
fi
# Opening it points the login item (LaunchAgent) at the installed copy.
open /Applications/Ancilo.app
echo "== installed"
