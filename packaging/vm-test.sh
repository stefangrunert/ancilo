#!/bin/sh
# M9-AC-01: a fresh Mac is ready in minutes. Clones a clean macOS VM (tart),
# installs the finished package without any developer environment, starts
# Ancilo, runs a small real model, connects Claude Code and delegates a task.
#
#   packaging/vm-test.sh <package.tar.gz> <model.gguf>
#
# Needs: tart (https://tart.run) and a base image with Claude Code logged in
# (ANCILO_TART_IMAGE, default ghcr.io/cirruslabs/macos-sequoia-base:latest –
# the login step is manual once per image). Prints timings as JSON.
set -eu

archive=$(cd "$(dirname "$1")" && pwd -P)/$(basename "$1")
model=$(cd "$(dirname "$2")" && pwd -P)/$(basename "$2")
image=${ANCILO_TART_IMAGE:-ghcr.io/cirruslabs/macos-sequoia-base:latest}
vm="ancilo-test-$$"
share=$(mktemp -d)
cp "$archive" "$share/ancilo.tar.gz"
cp "$model" "$share/model.gguf"

tart clone "$image" "$vm"
trap 'tart stop "$vm" 2>/dev/null || true; tart delete "$vm" 2>/dev/null || true; rm -rf "$share"' EXIT
tart run --no-graphics --dir="pkg:$share" "$vm" &
ip=$(tart ip --wait 120 "$vm")
ssh_vm() { sshpass -p admin ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null "admin@$ip" "$@"; }
until ssh_vm true 2>/dev/null; do sleep 2; done

start=$(date +%s)
ssh_vm 'set -e
  pkg="/Volumes/My Shared Files/pkg"
  mkdir -p ~/ancilo && tar -xzf "$pkg/ancilo.tar.gz" -C ~/ancilo --strip-components 1
  export PATH=$HOME/ancilo/bin:$PATH
  unset ANCILO_BIN ANCILO_LLAMA_SERVER
  ancilo --version
  ancilo add "$pkg/model.gguf"
  ancilo connect claude
  mkdir -p ~/demo && cd ~/demo && git init -q && echo "# Demo" > README.md && git add -A && git -c user.name=t -c user.email=t@x commit -qm init
  claude -p "Use the ancilo delegate tool to create hello.txt containing the word hello in $HOME/demo. Then reply DONE." --allowedTools "mcp__ancilo__delegate"
  grep -qi hello ~/demo/hello.txt'
end=$(date +%s)
printf '{"ready_s": %s}\n' "$((end - start))"
