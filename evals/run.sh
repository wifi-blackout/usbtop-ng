#!/bin/sh
# Tier-1 deterministic evals: the regression floor. No API key, no LLM.
# Each check names itself and its pass condition; the first failure exits non-zero.
set -u
export PATH="$HOME/.cargo/bin:$PATH"
root=$(git rev-parse --show-toplevel)
cd "$root" || exit 1
fail=0
ok()   { echo "PASS: $1"; }
bad()  { echo "FAIL: $1"; fail=1; }

echo "== gates =="
cargo fmt --all -- --check >/dev/null 2>&1 && ok "fmt" || bad "fmt"
cargo clippy --all-targets -- -D warnings >/dev/null 2>&1 && ok "clippy default" || bad "clippy default"

echo "== tracked-file scan (the ENFORCED content gate; the PostToolUse hook is only advisory) =="
# The content-guard hook fires after a write and can only warn the agent; this
# scan is the real boundary — it runs in CI over every tracked file and fails
# the build on a committed NUL byte or a device-serial file in the corpus.
scan_fail=0
# `.bin` fixtures (sanitized usbmon traces) are legitimately binary and contain
# NUL bytes by design; skip them. The bug this catches is an accidental NUL in a
# file that should be text (source, docs, config), so an extension allowlist is
# right — git's own binary detection would wrongly skip a corrupted .rs too,
# since a stray NUL makes git classify it as binary.
nul_hits=$(git ls-files | while IFS= read -r f; do
  case "$f" in *.bin) continue ;; esac
  [ -f "$f" ] && LC_ALL=C grep -qaP '\x00' "$f" 2>/dev/null && echo "$f"
done)
[ -z "$nul_hits" ] || { echo "  NUL byte in:"; printf '%s\n' "$nul_hits" | sed 's/^/    /'; scan_fail=1; }
ser_hits=$(git ls-files -- tests/fixtures 2>/dev/null | grep -E '(^|/)[^/]*serial$' || true)
[ -z "$ser_hits" ] || { echo "  fixture serial file:"; printf '%s\n' "$ser_hits" | sed 's/^/    /'; scan_fail=1; }
# Owner-only denylist (untracked, gitignored; absent in CI): the same file the
# content-guard hook reads, one case-insensitive fixed-string term per line,
# `#` comments allowed. When present, no tracked text file may contain a term.
dl="$root/.claude/hooks/denylist.local"
if [ -f "$dl" ]; then
  # Whole-word (-w), case-insensitive, fixed-string terms, read from a temp
  # file so grep's stdin stays free for the file being scanned.
  dlt=$(mktemp)
  grep -v '^[[:space:]]*#' "$dl" | grep -v '^[[:space:]]*$' > "$dlt" || true
  if [ -s "$dlt" ]; then
    dl_hits=$(git ls-files | while IFS= read -r f; do
      case "$f" in *.bin) continue ;; esac
      [ -f "$f" ] && grep -qiwF -f "$dlt" "$f" 2>/dev/null && echo "$f"
    done)
    [ -z "$dl_hits" ] || { echo "  denylisted term in:"; printf '%s\n' "$dl_hits" | sed 's/^/    /'; scan_fail=1; }
    # Paths are content too: a fixture directory named after a host discloses
    # it as surely as a sentence would.
    dl_paths=$(git ls-files | grep -iwF -f "$dlt" || true)
    [ -z "$dl_paths" ] || { echo "  denylisted term in a tracked path:"; printf '%s\n' "$dl_paths" | sed 's/^/    /'; scan_fail=1; }
  fi
  rm -f "$dlt"
fi
[ "$scan_fail" -eq 0 ] && ok "tracked-file scan (no NUL, no fixture serial, no denylisted term)" || bad "tracked-file scan"

echo "== hermetic behavioral evals (reuse the test suite) =="
cargo test redact >/dev/null 2>&1 && ok "redaction" || bad "redaction"
cargo test 'diag::inventory::tests::attr_dump' >/dev/null 2>&1 && ok "support-notes (attr walk)" || bad "support-notes"
cargo test --features capture-fixture sec1 >/dev/null 2>&1 && ok "fixture SEC-1" || bad "fixture SEC-1"
cargo test run_support_without_capture_writes_a_consistent_static_bundle >/dev/null 2>&1 \
  && ok "fixture-replay / static bundle" || bad "fixture-replay"

echo "== live support-bundle invariants (no root; static bundle) =="
cargo build --release >/dev/null 2>&1 || { bad "release build"; echo "aborting live evals"; [ "$fail" -eq 0 ]; exit 1; }
bin="$root/target/release/usbtop-ng"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
"$bin" --support "$work" >/dev/null 2>&1
dir=$(find "$work" -maxdepth 1 -type d -name 'usbtop-ng-support-*' | head -1)
arc=$(find "$work" -maxdepth 1 -name 'usbtop-ng-support-*.tar.gz' | head -1)
if [ -n "$dir" ] && [ -n "$arc" ]; then
  ( cd "$dir" && find . \( -type f -o -type l \) | sed 's|^\./||' | sort ) > "$work/disk"
  grep -E '^[[:space:]]*path[[:space:]]*=' "$dir/manifest.toml" | sed -E 's/.*"([^"]+)".*/\1/' | sort > "$work/mani"
  tar tzf "$arc" | grep -v '/$' | sed -E 's|^usbtop-ng-support-[^/]+/||' | sort > "$work/tar"
  grep -vx 'manifest.toml' "$work/disk" > "$work/disk_nm"
  if diff -q "$work/mani" "$work/disk_nm" >/dev/null && diff -q "$work/disk" "$work/tar" >/dev/null; then
    ok "support-consistency (manifest == archive == disk)"
  else bad "support-consistency"; fi
  leak=0
  for pat in "$(hostname)" "$(cat /etc/machine-id 2>/dev/null)" "$(id -un)" "/proc/self/fd"; do
    [ -n "$pat" ] || continue
    if grep -rIlq -- "$pat" "$dir" 2>/dev/null; then echo "  leak: $pat"; leak=1; fi
  done
  [ "$leak" -eq 0 ] && ok "support-privacy (no host identity / proc-fd)" || bad "support-privacy"
else
  bad "support-bundle produced"
fi

[ "$fail" -eq 0 ] && { echo "ALL EVALS PASS"; exit 0; } || { echo "EVALS FAILED"; exit 1; }
