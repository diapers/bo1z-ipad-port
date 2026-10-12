#!/usr/bin/env bash
# Install the Rust toolchain + C toolchain inside WSL.
# Non-interactive, idempotent.
set -euo pipefail

export DEBIAN_FRONTEND=noninteractive
echo "### apt packages"
sudo apt-get update -qq
sudo apt-get install -y -qq build-essential clang lld cmake ninja-build unzip pkg-config libssl-dev >/dev/null

echo "### rustup"
if ! command -v rustup >/dev/null 2>&1; then
  curl -sSf https://sh.rustup.rs -o /tmp/rustup.sh
  sh /tmp/rustup.sh -y --profile minimal --default-toolchain stable >/dev/null
fi
# shellcheck disable=SC1091
. "$HOME/.cargo/env"

echo "### versions"
rustc --version
cargo --version
clang --version | head -1
cmake --version | head -1

echo "### rust targets installed"
rustup target list --installed

# iOS target is installable from Linux (the SDK/linker is the hard part, not the target).
echo "### adding aarch64-apple-ios target (std only)"
rustup target add aarch64-apple-ios 2>&1 | tail -2 || echo "target add failed (non-fatal for the Windows-side experiment)"

echo "### done"
