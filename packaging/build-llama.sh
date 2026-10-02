#!/bin/sh
# Builds llama.cpp's `llama-server` for the release package from the pinned
# source: a fixed commit (content-addressed – the commit id is the integrity
# check), static, Metal shaders embedded, portable CPU code, reproducible paths.
#
#   packaging/build-llama.sh <out-dir> [<work-dir>]
#
# Result: <out-dir>/llama-server (+ LICENSE-llama.cpp)
set -eu

TAG=b11270
COMMIT=748d4225b9016b17ce4bcfa69fdc2c39f473a965

out=$(mkdir -p "$1" && cd "$1" && pwd)
work=${2:-$(mktemp -d)}
mkdir -p "$work"
src="$work/llama.cpp-$TAG"

if [ ! -d "$src/.git" ]; then
    git clone --quiet --depth 1 --branch "$TAG" https://github.com/ggml-org/llama.cpp "$src"
fi
actual=$(git -C "$src" rev-parse HEAD)
if [ "$actual" != "$COMMIT" ]; then
    echo "llama.cpp $TAG is $actual, expected $COMMIT – refusing to build" >&2
    exit 1
fi
if [ -n "$(git -C "$src" status --porcelain)" ]; then
    echo "llama.cpp source tree is modified – refusing to build" >&2
    exit 1
fi

export SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-$(git -C "$src" log -1 --format=%ct)}
# Build paths must not end up in the binary – in every spelling (`/tmp` is
# `/private/tmp` on macOS).
map=""
for w in "$work" "$(cd "$work" && pwd -P)" "${work#/private}"; do
    map="$map -ffile-prefix-map=$w/llama.cpp-$TAG=/llama.cpp -ffile-prefix-map=$w=/work"
done
cmake -S "$src" -B "$src/build" -G Ninja \
    -DCMAKE_BUILD_TYPE=Release \
    -DBUILD_SHARED_LIBS=OFF \
    -DGGML_NATIVE=OFF \
    -DGGML_METAL=ON \
    -DGGML_METAL_EMBED_LIBRARY=ON \
    -DLLAMA_CURL=OFF \
    -DLLAMA_OPENSSL=OFF \
    -DLLAMA_BUILD_TESTS=OFF \
    -DLLAMA_BUILD_EXAMPLES=OFF \
    -DCMAKE_OSX_DEPLOYMENT_TARGET=13.0 \
    -DCMAKE_C_FLAGS="$map" \
    -DCMAKE_CXX_FLAGS="$map" \
    > "$work/cmake.log"
cmake --build "$src/build" --target llama-server -j > "$work/build.log"
# Only system libraries: the binary must run on any Mac.
if otool -L "$src/build/bin/llama-server" | tail -n +2 | grep -vqE '^\s+(/System/Library/|/usr/lib/)'; then
    echo "llama-server links non-system libraries:" >&2
    otool -L "$src/build/bin/llama-server" >&2
    exit 1
fi
if strings "$src/build/bin/llama-server" | grep -q "$work"; then
    echo "llama-server contains build paths" >&2
    exit 1
fi
install -m 0755 "$src/build/bin/llama-server" "$out/llama-server"
cp "$src/LICENSE" "$out/LICENSE-llama.cpp"
echo "$out/llama-server ($TAG, $COMMIT)"
