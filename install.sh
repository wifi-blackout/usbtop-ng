#!/usr/bin/env bash
# Build usbtop-ng and install it to PREFIX (default /usr/local/bin).
# Also create a `usbtop` symlink, so `usbtop` and `sudo usbtop` both work.
# Usage: ./install.sh
# The script refuses to replace a `usbtop` command it does not own.
# Set FORCE_ALIAS=1 to replace one anyway.
set -euo pipefail

cd "$(dirname "$0")" || exit 1

PREFIX="${PREFIX:-/usr/local/bin}"
FORCE_ALIAS="${FORCE_ALIAS:-0}"

if [ ! -f Cargo.toml ]; then
    echo "Run this from the usbtop-ng repository root." >&2
    exit 1
fi

if ! command -v cargo >/dev/null 2>&1; then
    echo "cargo not found. Install Rust 1.88 or later from https://rustup.rs/." >&2
    exit 1
fi

# Refuse to clobber a usbtop that is not ours. Checked before any sudo
# action, so a refused install changes nothing.
alias_path="$PREFIX/usbtop"
if [ "$FORCE_ALIAS" != "1" ]; then
    if [ -L "$alias_path" ]; then
        target="$(readlink "$alias_path")"
        if [ "$target" != "usbtop-ng" ] && [ "$target" != "$PREFIX/usbtop-ng" ]; then
            echo "$alias_path is a symlink to '$target', not to usbtop-ng." >&2
            echo "Remove it yourself, or rerun with FORCE_ALIAS=1 to replace it." >&2
            exit 1
        fi
    elif [ -e "$alias_path" ]; then
        echo "$alias_path already exists and is not a usbtop-ng symlink." >&2
        echo "Remove it yourself, or rerun with FORCE_ALIAS=1 to replace it." >&2
        exit 1
    fi
fi

cargo build --release

sudo install -m 0755 target/release/usbtop-ng "$PREFIX/usbtop-ng"
sudo ln -sfn usbtop-ng "$alias_path"

echo "Installed:"
echo "  $PREFIX/usbtop-ng"
echo "  $PREFIX/usbtop -> usbtop-ng"
echo "Run: sudo usbtop"
