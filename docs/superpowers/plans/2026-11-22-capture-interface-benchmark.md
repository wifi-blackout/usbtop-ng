# Capture Interface Benchmark Implementation Plan

> **Outcome (2026-11-22):** executed in the working tree the same day. The
> adversarial review changed the method after the first full run (see the
> spec's "What the review changed"), so the build differs from the task text
> below in these names and additions: `busy_ticks` and `on_cpu_time` in
> place of `CpuTicks::parse`; `render_progress`, `render_runs` and
> `render_summary` in place of `render_run`; reader CPU taken from the
> reader thread's `schedstat` over the capture, with `getrusage` kept for the
> system share; `ReaderExit::verdict` and `ReaderFigures::saw_the_camera`;
> and hermetic tests of the system calls under `integration`. The spec is
> the authority for what was built.

**Goal:** A repeatable benchmark of the three usbmon interfaces against a
webcam workload, reporting elapsed time and CPU per run, and the docs that
carry its first results.

**Spec:** `../specs/2026-11-22-capture-interface-benchmark-design.md`

**Architecture:** One new test-only module, `src/usbmon/bench.rs`. Its pure
helpers are `#[cfg(test)]` and unit-tested hermetically; its `live`
submodule is gated on the `integration` feature and holds the one ignored
test that drives the production readers against a real camera.

**Tech stack:** Rust 1.88 (edition 2021), `libc` (already a dependency) for
`getrusage` and `sysconf`, `tempfile` (dev). No new crates, no new feature.

Every task ends with `cargo fmt`, clippy on all four configurations, and the
three test runs green. No `#[allow]`, no `#[expect]`: a helper is either
used by a unit test in every configuration or lives in the gated `live`
submodule.

## Task 1: workload settings and the run schedule

- [ ] Tests: defaults when no variable is set; each variable overrides its
  default; a non-number, a zero count and a format that is not four
  characters each fail naming the variable and the value; the schedule for
  three rounds is twelve runs, every interface once per round, each round
  starting one place further along.
- [ ] `Interface` (none, mmap-ring, binary, text, with the labels the
  startup log line uses), `Workload::from_lookup` taking a lookup closure so
  no test touches the real environment, `schedule(rounds)`.

## Task 2: the camera's USB address

- [ ] Tests over a fake sysfs tree in a temp directory: a video node whose
  `device` link points at a USB interface resolves to the parent's `busnum`,
  `devnum`, `speed` and `product`; a missing node, a missing `busnum` and a
  bus number that is not a number each fail with the path in the message.
- [ ] `Camera::from_sysfs(class_dir, node)`.

## Task 3: the two parsers

- [ ] Tests: the first line of a real `/proc/stat` gives the busy and total
  tick counts by the proc_stat(5) rule (iowait is not busy, guest is not
  counted twice); a file with no `cpu ` line is `None`; a real `v4l2-ctl
  --verbose` excerpt gives the frame count, the skipped-frame count from
  the `seq` gaps, and the bytes; an excerpt with no frame lines gives zero
  frames.
- [ ] `CpuTicks::parse`, `StreamLog::parse`.

## Task 4: the arithmetic and the tables

- [ ] Tests on hand-built runs with exact expected strings: one run line;
  the summary's mean and range per interface; reader CPU as a share of one
  core and per thousand bus events; machine CPU and its difference from the
  no-reader runs; text's kernel drops shown as not available; a debug build
  flagged.
- [ ] `Run`, `render_header`, `render_run`, `render_summary`.

## Task 5: the live benchmark

- [ ] `live` submodule under `integration`: `thread_cpu()` over
  `getrusage(RUSAGE_THREAD)`, `clock_ticks_per_second()` over
  `sysconf(_SC_CLK_TCK)`, the reader thread, the `v4l2-ctl` child, and the
  ignored test `capture_interface_benchmark`.
- [ ] Verify the FFI against getrusage(2), sysconf(3) and proc_stat(5).
- [ ] Smoke run with 60 frames and one round, as root, on the development
  host; confirm every interface reports events and plausible CPU.

## Task 6: the measurement and the docs

- [ ] Release build; pin the camera's dynamic frame rate; the full run on
  the built-in camera (YUYV 640x480, 3000 frames, three rounds); the heavy
  pass on the external camera (MJPEG 1920x1080 at 60); restore the camera
  control.
- [ ] `docs/BENCHMARK.md`; the pointers in `docs/ARCHITECTURE.md`,
  `README.md`, the man page section and `docs/CONTRIBUTING.md`;
  `CHANGELOG.md`.
- [ ] All gates, MSRV, `bash evals/run.sh`.

## Task 7: review

- [ ] Three-engine review per `REVIEW.md` (adversarial: new subsystem with
  FFI), reconcile, fix, scoped re-review.
