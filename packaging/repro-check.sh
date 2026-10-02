#!/bin/sh
# M9-AC-08: builds the committed state twice – in two independent clones with
# their own build directories, package caches and llama.cpp checkouts – and
# compares every shipped file (path, size, mode, SHA-256) and the archives.
#
#   packaging/repro-check.sh [<work-dir>]
set -eu

root=$(cd "$(dirname "$0")/.." && pwd -P)
w=${1:-$(mktemp -d)}
mkdir -p "$w"
work=$(cd "$w" && pwd -P)
commit=$(git -C "$root" rev-parse HEAD)
echo "reproducibility check of $commit in $work"

for side in a b; do
    rm -rf "$work/$side"
    git clone --quiet --no-hardlinks "$root" "$work/$side"
    git -C "$work/$side" checkout --quiet "$commit"
    (
        cd "$work/$side"
        export CARGO_TARGET_DIR="$work/$side/target"
        export npm_config_cache="$work/$side/.npm"
        packaging/package.sh "$work/$side/dist" > "$work/$side.log" 2>&1
    ) || { echo "build $side failed – see $work/$side.log" >&2; exit 1; }
done

status=0
if ! diff -u "$work/a/dist/MANIFEST-aarch64-apple-darwin.txt" "$work/b/dist/MANIFEST-aarch64-apple-darwin.txt"; then
    echo "shipped files differ" >&2
    status=1
fi
for f in "$work"/a/dist/*.tar.gz; do
    g="$work/b/dist/$(basename "$f")"
    if ! cmp -s "$f" "$g"; then
        echo "archives differ: $(basename "$f")" >&2
        status=1
    fi
done
if [ $status -eq 0 ]; then
    echo "reproducible: $(wc -l < "$work/a/dist/MANIFEST-aarch64-apple-darwin.txt" | tr -d ' ') files identical, archive $(shasum -a 256 "$work"/a/dist/*.tar.gz | cut -d' ' -f1)"
fi
exit $status
