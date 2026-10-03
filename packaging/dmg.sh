#!/usr/bin/env bash
# The DMG users download: packaging/dmg.sh <Ancilo.app> <out.dmg>
#
# A styled window (packaging/dmg/settings.py, background by
# packaging/dmg/background.swift), written by dmgbuild – no Finder scripting,
# so it runs unattended. dmgbuild lives in a venv of its own (pinned).
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd -P)
app=${1:?usage: dmg.sh <Ancilo.app> <out.dmg>}
out=${2:?usage: dmg.sh <Ancilo.app> <out.dmg>}
work="$root/target/dmg"
venv="$work/venv"
mkdir -p "$work"

if [ ! -x "$venv/bin/dmgbuild" ]; then
    python3 -m venv "$venv"
    "$venv/bin/pip" install --quiet --disable-pip-version-check "dmgbuild==1.6.7"
fi

# One HiDPI TIFF: sharp on Retina displays, right on all others.
tiffutil -cathidpicheck "$root/packaging/dmg/background.png" "$root/packaging/dmg/background@2x.png" \
    -out "$work/background.tiff" >/dev/null 2>&1

rm -f "$out"
"$venv/bin/dmgbuild" -s "$root/packaging/dmg/settings.py" \
    -D app="$app" -D background="$work/background.tiff" \
    -D volicon="$app/Contents/Resources/icon.icns" \
    "$(basename "$app" .app)" "$out"
# Signed like the app inside (a release: the Developer ID); notarized by sign.sh.
if [ -n "${APPLE_SIGNING_IDENTITY:-}" ] && [ "$APPLE_SIGNING_IDENTITY" != - ]; then
    codesign --force --timestamp --sign "$APPLE_SIGNING_IDENTITY" "$out"
fi
echo "$out"
