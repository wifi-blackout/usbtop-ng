#!/usr/bin/env bash
# Build usbtop-ng and install it to PREFIX (default /usr/local/bin).
# Also create a `usbtop` symlink, so `usbtop` and `sudo usbtop` both work.
# Usage: ./install.sh
set -euo pipefail

cd "$(dirname "$0")" || exit 1

PREFIX="${PREFIX:-/usr/local/bin}"

if [ ! -f Cargo.toml ]; then
    echo "Run this from the usbtop-ng repository root." >&2
    exit 1
fi

if ! command -v cargo >/dev/null 2>&1; then
    echo "cargo not found. Install Rust 1.88 or later from https://rustup.rs/." >&2
    exit 1
fi

cargo build --release

sudo install -m 0755 target/release/usbtop-ng "$PREFIX/usbtop-ng"
sudo ln -sf usbtop-ng "$PREFIX/usbtop"

echo "Installed:"
echo "  $PREFIX/usbtop-ng"
echo "  $PREFIX/usbtop -> usbtop-ng"
echo "Run: sudo usbtop"
