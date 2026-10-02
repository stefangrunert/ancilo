#!/bin/sh
# Signs and notarizes the release artifacts with the owner's Developer ID.
# Without the credentials it stops – Ancilo is never released unsigned.
#
#   packaging/sign.sh <dist-dir> [cli|app]   (default: both)
#
# Two ways to run it:
#
#   Locally (this Mac): certificate and notarization credentials stay in the
#   login keychain – nothing secret passes through the environment.
#     APPLE_SIGNING_IDENTITY  "Developer ID Application: <Name> (<TEAM ID>)"
#     ANCILO_NOTARY_PROFILE   keychain profile from `xcrun notarytool store-credentials`
#
#   In CI (release job, secrets):
#     APPLE_SIGNING_IDENTITY, APPLE_CERTIFICATE (base64 .p12),
#     APPLE_CERTIFICATE_PASSWORD, APPLE_ID, APPLE_TEAM_ID, APPLE_APP_PASSWORD
set -eu

dist=$(cd "$1" && pwd -P)
what=${2:-all}
if [ -z "${APPLE_SIGNING_IDENTITY:-}" ]; then
    echo "missing APPLE_SIGNING_IDENTITY – signing needs the Developer ID ; not releasing unsigned" >&2
    exit 1
fi

keychain=""
if [ -n "${ANCILO_NOTARY_PROFILE:-}" ]; then
    notary() { xcrun notarytool submit "$1" --keychain-profile "$ANCILO_NOTARY_PROFILE" --wait; }
else
    for v in APPLE_CERTIFICATE APPLE_CERTIFICATE_PASSWORD APPLE_ID APPLE_TEAM_ID APPLE_APP_PASSWORD; do
        eval "val=\${$v:-}"
        if [ -z "$val" ]; then
            echo "missing $v – signing needs the Developer ID ; not releasing unsigned" >&2
            exit 1
        fi
    done
    # A temporary keychain for this run only.
    keychain="$dist/signing.keychain-db"
    kc_pass=$(uuidgen)
    security create-keychain -p "$kc_pass" "$keychain"
    trap 'security delete-keychain "$keychain" 2>/dev/null || true' EXIT
    security set-keychain-settings -lut 3600 "$keychain"
    security unlock-keychain -p "$kc_pass" "$keychain"
    printf '%s' "$APPLE_CERTIFICATE" | base64 --decode > "$dist/cert.p12"
    security import "$dist/cert.p12" -k "$keychain" -P "$APPLE_CERTIFICATE_PASSWORD" -T /usr/bin/codesign
    rm -f "$dist/cert.p12"
    security set-key-partition-list -S apple-tool:,apple: -s -k "$kc_pass" "$keychain" >/dev/null
    security list-keychains -d user -s "$keychain" $(security list-keychains -d user | tr -d '"')
    notary() { xcrun notarytool submit "$1" --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" --password "$APPLE_APP_PASSWORD" --wait; }
fi

sign() {
    if [ -n "$keychain" ]; then
        codesign --force --timestamp --options runtime --keychain "$keychain" --sign "$APPLE_SIGNING_IDENTITY" "$@"
    else
        codesign --force --timestamp --options runtime --sign "$APPLE_SIGNING_IDENTITY" "$@"
    fi
}

# CLI archive: sign the binaries, re-pack, notarize (zip), verify.
for pkg in "$dist"/ancilo-*-aarch64-apple-darwin; do
    [ -d "$pkg" ] && [ "$what" != app ] || continue
    sign "$pkg/libexec/ancilo/llama-server"
    sign "$pkg/bin/ancilo"
    (cd "$dist" && ditto -c -k --keepParent "$(basename "$pkg")" "$pkg.zip")
    notary "$pkg.zip"
    codesign --verify --strict --verbose=2 "$pkg/bin/ancilo" "$pkg/libexec/ancilo/llama-server"
    # The archive users download carries the signed binaries (same normalization as package.sh).
    name=$(basename "$pkg")
    epoch=${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct)}
    find "$pkg" -exec touch -h -t "$(date -r "$epoch" +%Y%m%d%H%M.%S)" {} +
    (cd "$dist" && find "$name" -print | LC_ALL=C sort > "$dist/files.txt" \
        && tar --uid 0 --gid 0 --uname root --gname wheel -n -c -f - -T "$dist/files.txt" | gzip -n -9 > "$pkg.tar.gz")
    rm -f "$dist/files.txt" "$pkg.zip"
done

# App: Tauri signs the bundle with APPLE_SIGNING_IDENTITY during `tauri build`;
# here the result is notarized, stapled and checked like a download would be.
for dmg in "$dist"/*.dmg; do
    [ -f "$dmg" ] && [ "$what" != cli ] || continue
    notary "$dmg"
    xcrun stapler staple "$dmg"
    mnt=$(mktemp -d)
    hdiutil attach -nobrowse -readonly -mountpoint "$mnt" "$dmg" >/dev/null
    codesign --verify --deep --strict --verbose=2 "$mnt/Ancilo.app"
    spctl --assess --type execute --verbose "$mnt/Ancilo.app"
    xcrun stapler validate "$dmg"
    hdiutil detach "$mnt" >/dev/null
done
echo "signed, notarized and verified"
