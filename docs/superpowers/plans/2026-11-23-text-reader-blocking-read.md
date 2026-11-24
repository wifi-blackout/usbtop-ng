# Text Reader Blocking Read Implementation Plan

> **Outcome (2026-11-23):** executed in the working tree the same day. Two
> things changed the build after the first complete version, and the spec
> records both: measuring it (a reader woken for every event cost several
> times the CPU of the one it replaced, so it now pauses between batches by
> the event rate), and the review (see the spec's "What the review
> changed"). The build differs from the task text below accordingly:
> `pause_for`, `LoopCounts` and the `Step` enum in `reader.rs`; the signal
> mask restored by `Unblocked` in `tick.rs`; and the tests the review asked
> for.
>
> Checked on eight hosts besides the development one. The test suite, built
> with `integration`, passed on six of them: aarch64 with glibc on kernels
> 6.1 (`rock5c`), 6.6 with 16 KiB pages (`pi-5`) and 6.18 (`pi-4`,
> `pi-400`), aarch64 with static musl on 5.4 (`bm1684x`) and ARMv6 with
> static musl on a single core (`pi-zero`). On `cezanne` (x86-64) only the
> live tests and the reader's and the tick's own were run, since its binary
> carries the build host's fixture path. The tests that depend on timing ran
> six times on each of the seven without a failure. On every one of them with
> usbmon (`bm1684x` has none) the shipped program read the mmap ring and,
> with its device nodes hidden, the text interface, and reported the same
> traffic on both. `tgl-tb4` and `pi-400` gave the figures in
> `docs/BENCHMARK.md`.

**Goal:** the debugfs text reader stops losing events to its 50 ms sleep.

**Spec:** `../specs/2026-11-23-text-reader-blocking-read-design.md`

**Architecture:** a new `src/usbmon/tick.rs` holding the per-thread interval
timer, and a rewritten read loop in `src/usbmon/reader.rs`. No caller
changes: `read_packets` keeps its signature and its shutdown contract.

**Tech stack:** `libc` (already a dependency) for `sigaction`,
`pthread_sigmask`, `timer_create`, `timer_settime`, `timer_delete`, `gettid`
and `fcntl`. No new crates.

Every task ends with `cargo fmt`, clippy on all four configurations and the
three test runs green. No `#[allow]`, no `#[expect]`.

## Task 1: the failing test

- [ ] In `reader.rs`, a test that writes fifteen lines to a FIFO, each after
  an idle spell longer than `POLL_INTERVAL`, and asserts the median delay to
  delivery is under 10 ms. Confirm it fails on the sleeping reader, and that
  it fails on the delay.

## Task 2: `tick.rs`

- [ ] Verify against timer_create(2), timer_settime(2), timer_delete(2),
  sigevent(3type), sigaction(2), signal(7), pthread_sigmask(3), and
  `good_sigevent` in `kernel/time/posix-timers.c`.
- [ ] Tests: a blocked pipe read returns `Interrupted` under a ticker; after
  the ticker is dropped a blocked read is not interrupted; a ticker on one
  thread does not interrupt a read blocked on another.
- [ ] `Ticker::start`, `Drop`.

## Task 3: the read loop

- [ ] Tests: line assembly (several lines in one read, a split line, a last
  line with no newline, a non-UTF-8 line, an over-long line); a reader
  blocked on an idle FIFO stops when asked; the polling fallback delivers and
  stops.
- [ ] `read_packets`: ticker, then `O_NONBLOCK` cleared; `File::read` and a
  line buffer; `EINTR`, `WouldBlock` and end of file each handled.
- [ ] Task 1's test passes, unedited.

## Task 4: live verification and the docs

- [ ] Release build; the benchmark's text runs on `tgl-tb4` and on the
  development host: no `text_lost`, the ring readers' camera event count, and
  the reader's CPU beside the old figure.
- [ ] `docs/BENCHMARK.md`, `README.md`, the man page section,
  `docs/ARCHITECTURE.md`, `CHANGELOG.md`.
- [ ] All gates, MSRV, `bash evals/run.sh`.

## Task 5: review

- [ ] Three-engine review per `REVIEW.md` (adversarial: unsafe FFI and
  signals), reconcile, fix, scoped re-review.
