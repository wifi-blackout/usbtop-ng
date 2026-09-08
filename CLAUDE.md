# usbtop-ng — agent guide

Linux USB bandwidth monitor with a terminal UI. Binary-only crate. This file is
the one-page contract every session reads; the deeper docs are linked, not
copied here.

## Build, test, verify

cargo is not on `PATH` by default in this project's environment; prefix Rust
commands with `export PATH="$HOME/.cargo/bin:$PATH"`.

The gates, exactly as CI runs them:

- `cargo fmt --all -- --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo clippy --all-targets --features capture-fixture -- -D warnings`
- `cargo clippy --all-targets --features integration -- -D warnings`
- `cargo clippy --all-targets --features ebpf -- -D warnings`
- `cargo test --all-targets`
- `cargo test --features capture-fixture`
- `cargo test --features integration`

The `ebpf` feature builds and runs its hermetic tests in CI (it needs clang and
libbpf); its live tests need root and BTF, so run them only on a capable host.
The `integration` feature's live tests (fchown, live support-bundle, live
usbmon ring) self-skip without root and usbmon, so CI exercises only its
hermetic tests; the privileged paths are verified on the fleet, not in GitHub CI.

Regression floor: `bash evals/run.sh` (see `evals/`).

## Non-negotiable gates

- clippy `-D warnings` clean on all four configs: default, `capture-fixture`,
  `integration`, `ebpf`.
- Zero `#[allow(...)]` and zero `#[expect(...)]`. This is a binary crate, so
  `pub` does not exempt an item from `dead_code`; make every item reachable from
  `main` or a test in each feature config, or add a test — never suppress.
- MSRV 1.88, edition 2021. Verify with `cargo +1.88.0 check --all-targets`.
- `cargo fmt` clean.

## Disciplines

- Verify kernel and syscall FFI against man pages or kernel source, never a
  single quiet live run.
- The committed golden fixture corpus under `tests/fixtures/hosts/` is
  authoritative. A capture-to-report change that alters a golden is a signal to
  investigate, not a golden to bless away.
- Privacy: never collect or publish anything that identifies the host — no host
  serial, MAC address, or machine-id. USB and Thunderbolt device details,
  including device serials, are in scope and are kept.
- Tooling trap: always write the word NUL, never a backslash-u or high
  backslash-x escape — the Write/Edit tools decode those into a real NUL byte
  that corrupts the source file. Write binary test data as `&[u8]` byte arrays,
  never as string escapes.
- Testing: a change arrives with its tests, `cargo fmt`, and clippy already
  green. For a bug, write the failing test first, confirm it fails for the
  stated reason, then fix without editing the test. Tests assert specific
  behavior or values, never tautologies.

## Reviews

The review policy — the passes every change gets, the severity levels, when
the Codex and Antigravity reviews run and in which variant, and that the
session's Claude reconciles all three engines' findings as the final
arbiter — is in `REVIEW.md`.

## Commit trailers

End every commit message with a `Co-Authored-By:` trailer crediting the
assisting Claude model, and the session's `Claude-Session:` line when one is
provided.

## Where things are

- `docs/ARCHITECTURE.md` — module map and data flow.
- `docs/CONTRIBUTING.md` — full contributor guide.
- `docs/TESTING.md` — the fixture corpus and how to capture one.
- `docs/superpowers/specs/` and `docs/superpowers/plans/` — the spec then plan
  flow every non-trivial change follows.
