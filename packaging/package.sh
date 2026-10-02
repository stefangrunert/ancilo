#!/bin/sh
# Builds the release packages from a clean checkout:
#   dist/ancilo-<version>-<target>.tar.gz   CLI + daemon + llama.cpp (Homebrew formula)
#   dist/stage/                              the files the app bundle ships (for `tauri build`)
#   dist/MANIFEST-<target>.txt               path, size, mode, SHA-256 of every shipped file
#
#   packaging/package.sh [<out-dir>]
#
# Reproducible: fixed toolchains (rust-toolchain.toml, package-lock.json),
# --locked, SOURCE_DATE_EPOCH from the last commit, build paths remapped,
# archive metadata normalized.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd -P)
out=$(mkdir -p "${1:-$root/dist}" && cd "${1:-$root/dist}" && pwd -P)
target=aarch64-apple-darwin
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)
export SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-$(git -C "$root" log -1 --format=%ct)}
export ZERO_AR_DATE=1
cargo_home=${CARGO_HOME:-$HOME/.cargo}
export RUSTFLAGS="--remap-path-prefix=$root=/ancilo --remap-path-prefix=$cargo_home=/cargo ${RUSTFLAGS:-}"
export CFLAGS="-ffile-prefix-map=$root=/ancilo -ffile-prefix-map=$cargo_home=/cargo ${CFLAGS:-}"
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$root/target}

echo "== web UI"
(cd "$root/app" && npm ci --no-audit --no-fund --silent && npx vite build --logLevel warn)

echo "== ancilo $version"
cargo build --release --locked -p ancilo --manifest-path "$root/Cargo.toml"

echo "== llama.cpp"
llama_out="$out/llama"
if [ ! -x "$llama_out/llama-server" ]; then
    "$root/packaging/build-llama.sh" "$llama_out" "$out/work"
fi

echo "== third-party notices"
cargo run -q --locked -p xtask --manifest-path "$root/Cargo.toml" -- notices > "$out/THIRD_PARTY_NOTICES.txt"

name="ancilo-$version-$target"
stage="$out/$name"
rm -rf "$stage"
mkdir -p "$stage/bin" "$stage/libexec/ancilo" "$stage/share/doc/ancilo"
install -m 0755 "$CARGO_TARGET_DIR/release/ancilo" "$stage/bin/ancilo"
install -m 0755 "$llama_out/llama-server" "$stage/libexec/ancilo/llama-server"
install -m 0644 "$root/LICENSE" "$root/README.md" "$root/PRIVACY.md" "$out/THIRD_PARTY_NOTICES.txt" "$llama_out/LICENSE-llama.cpp" "$stage/share/doc/ancilo/"

# Local builds are ad-hoc signed so they start on this Mac; releases are
# signed with the Developer ID afterwards (packaging/sign.sh).
codesign --force --sign - "$stage/bin/ancilo" "$stage/libexec/ancilo/llama-server" 2>/dev/null || true

# Normalized archive: sorted entries, fixed times, owner and permissions.
find "$stage" -exec touch -h -d "@$SOURCE_DATE_EPOCH" {} + 2>/dev/null || find "$stage" -exec touch -h -t "$(date -r "$SOURCE_DATE_EPOCH" +%Y%m%d%H%M.%S)" {} +
(cd "$out" && find "$name" -print | LC_ALL=C sort > "$out/files.txt")
(cd "$out" && tar --uid 0 --gid 0 --uname root --gname wheel -n -c -f - -T "$out/files.txt" | gzip -n -9 > "$out/$name.tar.gz")
rm "$out/files.txt"

# The manifest of every shipped file – compared by the reproducibility check.
(cd "$stage" && find . -type f -o -type l | LC_ALL=C sort | while read -r f; do
    printf '%s %s %s %s\n' "$f" "$(stat -f %z "$f")" "$(stat -f %Lp "$f")" "$(shasum -a 256 "$f" | cut -d' ' -f1)"
done) > "$out/MANIFEST-$target.txt"

echo "$out/$name.tar.gz"
shasum -a 256 "$out/$name.tar.gz"
