#!/bin/sh
# Exercises content-guard.sh directly (it reads a file path from a JSON stdin
# shaped like a PostToolUse payload). No Claude session needed.
set -u
here=$(cd "$(dirname "$0")" && pwd)
guard="$here/content-guard.sh"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fail=0

# Clean text file -> exit 0.
printf 'hello world\n' > "$tmp/clean.rs"
printf '{"tool_input":{"file_path":"%s/clean.rs"}}' "$tmp" | sh "$guard"
[ $? -eq 0 ] || { echo "FAIL: clean file was blocked"; fail=1; }

# File with a NUL byte -> exit 2.
printf 'a\000b' > "$tmp/nul.rs"
printf '{"tool_input":{"file_path":"%s/nul.rs"}}' "$tmp" | sh "$guard"
[ $? -eq 2 ] || { echo "FAIL: NUL file was not blocked"; fail=1; }

# A file named 'serial' under a tests/fixtures path -> exit 2.
mkdir -p "$tmp/tests/fixtures/hosts/x/dev"
printf '0123456789\n' > "$tmp/tests/fixtures/hosts/x/dev/serial"
printf '{"tool_input":{"file_path":"%s/tests/fixtures/hosts/x/dev/serial"}}' "$tmp" | sh "$guard"
[ $? -eq 2 ] || { echo "FAIL: fixture serial file was not blocked"; fail=1; }

# Scratch/target paths are always skipped even with a NUL.
mkdir -p "$tmp/target"
printf 'a\000b' > "$tmp/target/x.rs"
printf '{"tool_input":{"file_path":"%s/target/x.rs"}}' "$tmp" | sh "$guard"
[ $? -eq 0 ] || { echo "FAIL: target path was not skipped"; fail=1; }

[ "$fail" -eq 0 ] && echo "content-guard tests PASS" || exit 1
