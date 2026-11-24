# Read() Reader One Read Per Event Implementation Plan

> **Outcome (2026-11-23):** executed in the working tree the same day, in
> the order below. The
> spec's Results section has what was measured, and `docs/BENCHMARK.md` the
> tables. Besides the docs the plan lists, the startup warning for the
> read() fallback changed by a word or two ("a system call of its own",
> `fallback_notice` in `src/usbmon/monitor.rs`), since it described the old
> loop. The review (Task 5) added a stop check before each delivery, moved
> the final read of the statistics into `read_from`, and added tests; the
> spec's "What the review changed" has the list.

**Goal:** the read() fallback reader takes an event, header and captured
payload, in one system call.

**Spec:** `../specs/2026-11-23-binary-reader-one-read-per-event-design.md`

**Architecture:** `src/usbmon/binary.rs` only. A `Frames` type owns one
128 KiB buffer and cuts what the reads return into events; `read_from` is
the loop, generic over `Read`. `read_packets` keeps its signature and its
contract, so no caller changes.

**Tech stack:** nothing new.

Every task ends with `cargo fmt`, clippy on all four configurations and the
three test runs green. No `#[allow]`, no `#[expect]`.

## Task 1: confirm the cause

- [ ] `mon_bin_read`, `mon_bin_wait_event` and the `len_cap` bound in
  `mon_bin_event`, read in `drivers/usb/mon/mon_bin.c`.
- [ ] Count the reader's `read()` calls over a camera run (`strace -c`),
  beside the events it delivered.

## Task 2: the failing test

- [ ] Make the loop generic over `Read` with no change of behaviour, so a
  test can stand in for the device.
- [ ] A stand-in that gives one read the bytes of one event and counts its
  reads. Three events, one with a 3,000-byte payload, must take four reads.
  Confirm it fails on the old loop, and on the count.

## Task 3: the loop

- [ ] `READ_LEN`, `Frames` (`space`, `filled`), `read_from`. Remove `fill`,
  `drain`, `Fill`, `DRAIN_CHUNK`.
- [ ] Task 2's test passes, unedited.
- [ ] Tests: an exact fit; a payload longer than the buffer, skipped and
  delivered; a stream cut into reads of many sizes; an event cut short by
  the end of a file; a stop seen by a source that never runs dry; the
  statistics read by the clock.
- [ ] Break the code three ways and see a test fail each time.

## Task 4: measure, and the docs

- [ ] The buffer size: 64 KiB, 128 KiB and 1 MiB on the camera.
- [ ] The benchmark's read() runs on both webcam hosts, four rounds each,
  beside the old figures.
- [ ] A bus whose transfers are longer than the buffer: the shipped program
  held to read() on `pi-400` while its USB drive is read, old loop and new.
- [ ] The test suite on an aarch64 host and on the 32-bit one.
- [ ] `docs/BENCHMARK.md`, `README.md`, the man page section,
  `docs/ARCHITECTURE.md`, `docs/ROADMAP.md`, `CHANGELOG.md`.
- [ ] All gates, MSRV, `bash evals/run.sh`.

## Task 5: review

- [ ] Codex and Antigravity review the change, the session's Claude
  reconciles their findings as the final arbiter, fixes what holds, and a
  scoped re-review follows the fixes.
