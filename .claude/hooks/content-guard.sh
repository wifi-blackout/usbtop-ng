#!/bin/sh
# PostToolUse content guard: ADVISORY fast feedback, not a hard gate. It fires
# AFTER a Write/Edit, inspects the just-written file, and exits 2 with a message
# on stderr so the agent fixes a NUL byte / fixture serial / denylisted term
# right away. It cannot prevent the write (PostToolUse runs after it) and never
# stages or commits, so it is NOT a security boundary. The ENFORCED gate is the
# tracked-file scan in evals/run.sh, which CI runs and which fails the build on
# any tracked NUL byte or fixture serial. Fast and non-destructive: reads only.
set -u
input=$(cat)
if command -v jq >/dev/null 2>&1; then
  file=$(printf '%s' "$input" | jq -r '.tool_input.file_path // .tool_input.filePath // empty' 2>/dev/null)
else
  file=$(printf '%s' "$input" | sed -n 's/.*"file_path"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')
fi
[ -n "$file" ] && [ -f "$file" ] || exit 0

# Never inspect generated or scratch trees. (Not a blanket /tmp/* skip: that
# would also exempt this guard's own test fixtures, which mktemp -d places
# under /tmp, making the checks below untestable no-ops. The agent scratchpad
# this rule is meant to protect already matches */scratchpad/*.)
case "$file" in
  */target/*|*/.git/*|*/scratchpad/*) exit 0 ;;
  *.bin) exit 0 ;;   # sanitized usbmon trace fixtures are legitimately binary
  */.claude/hooks/denylist.local) exit 0 ;;   # the denylist names its own terms
esac

msg=""
if LC_ALL=C grep -qaP '\x00' "$file" 2>/dev/null; then
  msg="$msg
- NUL byte in $file: a backslash-u or high backslash-x escape was decoded into a real NUL. Rewrite the passage describing NUL in words."
fi

# A file literally named 'serial' inside the committed fixture corpus almost
# always means a captured device serial leaked in.
case "$file" in
  *tests/fixtures/*/serial|*tests/fixtures/*/*serial)
    msg="$msg
- $file looks like a device serial under the fixture corpus; fixtures must be serial-free." ;;
esac

# Owner-only denylist: one whole-word, case-insensitive term per line,
# untracked. Absent -> skipped.
root=$(git -C "$(dirname "$file")" rev-parse --show-toplevel 2>/dev/null)
dl="$root/.claude/hooks/denylist.local"
if [ -n "$root" ] && [ -f "$dl" ]; then
  while IFS= read -r term; do
    [ -n "$term" ] || continue
    case "$term" in \#*) continue ;; esac
    if grep -qiwF "$term" "$file" 2>/dev/null; then
      msg="$msg
- $file contains a denylisted term."
    fi
  done < "$dl"
fi

if [ -n "$msg" ]; then
  printf 'content-guard blocked:%s\n' "$msg" >&2
  exit 2
fi
exit 0
