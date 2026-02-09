#!/usr/bin/env bash
#
# setup_wsl.sh — Prepare WSL2 for cross-compiling Setu to Windows.
#
# Usage:  chmod +x setup_wsl.sh && ./setup_wsl.sh
#
set -euo pipefail

echo "==> Installing mingw-w64 cross-compiler toolchain..."
sudo apt-get update -qq
sudo apt-get install -y --no-install-recommends \
    gcc-mingw-w64-x86-64 \
    g++-mingw-w64-x86-64 \
    mingw-w64-tools \
    pkg-config

echo "==> Verifying cross-compiler..."
x86_64-w64-mingw32-gcc --version

echo "==> Installing Rust (if not present)..."
if ! command -v rustup &>/dev/null; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    # shellcheck source=/dev/null
    source "$HOME/.cargo/env"
fi

echo "==> Adding Windows GNU target..."
rustup target add x86_64-pc-windows-gnu

echo "==> Verifying target is installed..."
rustup target list --installed | grep -q x86_64-pc-windows-gnu \
    && echo "    ✓ x86_64-pc-windows-gnu target ready" \
    || { echo "    ✗ target installation failed"; exit 1; }

echo "==> Verifying linker is reachable..."
x86_64-w64-mingw32-gcc -v 2>&1 | head -1

echo ""
echo "Environment ready. Build with:"
echo "  cd $(pwd) && cargo build --release"
echo ""
echo "The binary will be at:"
echo "  target/x86_64-pc-windows-gnu/release/setu.exe"
