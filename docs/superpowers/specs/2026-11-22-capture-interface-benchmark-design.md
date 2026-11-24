# Capture interface benchmark — design

**Date:** 2026-11-22
**Status:** approved in the design session (the owner's "yes, dive in"); the
calls marked *builder's call* below were made while writing this up and are
the owner's to overrule; the adversarial three-engine review of the build
(Claude reviewer, Codex, Antigravity) folded in the same day, see "What the
review changed" at the end

## Goal

usbtop-ng reads usbmon through one of three interfaces: the mmap ring,
read(2) on the same node, and the debugfs text file. The docs say what each
one loses and decline to rank their cost, because nothing in the tree
measures it (`docs/ARCHITECTURE.md`, "What this section does not claim").
This change adds the measurement: a repeatable benchmark that runs each
interface against the same webcam workload and reports how long the capture
took and what the CPU did during it, so the docs can state figures with the
conditions they were taken under.

The owner's requirements, as stated:

- the length of time of each test, a test being "capture 3000 video frames";
- the CPU usage through that capture;
- webcams as the traffic source, because they generate constant traffic
  where storage does not.

## What the hardware shows

Checked on the development workstation, 2026-11-22, kernel 7.0:

- `v4l2-ctl` (v4l-utils 1.26.1) stops after an exact frame count
  (`--stream-count`), and with `--verbose` prints one line per frame:
  `cap dqbuf: 0 seq: 4 bytesused: 614400 ts: …`. The sequence numbers show
  frames the driver dropped; `bytesused` shows the frame size.
- The built-in webcam (USB 2 high speed, bus 1) offers uncompressed YUYV up
  to 640x480 at a nominal 30 fps. Every YUYV frame is 614400 bytes whatever
  the camera sees. Its *delivered* rate was 19.6 fps: the camera has
  `exposure_dynamic_framerate` on and lengthens exposure in a dim room.
  MJPEG at the same size delivered the same 19.6 fps.
- The external camera (USB 2 high speed, bus 3, behind hubs) offers MJPEG to
  1920x1080 at a nominal 60 fps. A three-second trial averaged about 53 fps
  while it came up to speed; over a full run it delivers 59.6.
- **The camera sets the pace, not the reader.** A usbmon reader watches
  traffic and never slows it, so the time to deliver 3000 frames is the same
  for every interface. The elapsed time is reported as the owner asked, and
  it serves as the control that every run saw the same workload; the
  interfaces differ in CPU used and in events lost.
- The buses the cameras sit on also carry HID devices. A moving mouse adds
  events of its own, at a rate that can exceed the camera's.
- All three readers poll: a reader that finds nothing to read sleeps one
  `POLL_INTERVAL` (50 ms), so each wakes about 20 times a second and handles
  a batch.

## Design

### Where it lives

A new module `src/usbmon/bench.rs`, compiled for tests only.

- The pure parts (workload settings, the run schedule, parsing
  `/proc/stat` and the `v4l2-ctl` log, resolving a camera to its USB bus,
  the arithmetic and the tables) are plain `#[cfg(test)]` code with hermetic
  unit tests, so the default `cargo test` covers them in CI.
- The live part is a submodule gated on the `integration` feature, holding
  one test, `capture_interface_benchmark`, marked `#[ignore]`. No gate and
  no CI job runs it; it runs only when named with `--ignored`.

No new feature, no new crate, nothing in the shipped binary.

*Builder's call:* an explicit run that lacks a prerequisite (root, a
readable usbmon node, debugfs, `v4l2-ctl`, the camera) **fails with a
message naming what is missing**. The other live tests print a skip and
pass, because the ordinary `--features integration` run includes them; this
one is only ever run on purpose, and a benchmark that prints "ok" without
measuring anything misleads. This follows `bless_named_bundle`, the tree's
other operator-run ignored test.

### The workload

Settings come from environment variables, each with a default:

| Variable | Default | Meaning |
|---|---|---|
| `USBTOP_NG_BENCH_CAMERA` | `/dev/video0` | the V4L2 capture node |
| `USBTOP_NG_BENCH_FRAMES` | `3000` | frames per run |
| `USBTOP_NG_BENCH_ROUNDS` | `3` | times each interface is run |
| `USBTOP_NG_BENCH_FORMAT` | `YUYV` | V4L2 pixel format code |
| `USBTOP_NG_BENCH_WIDTH` | `640` | frame width |
| `USBTOP_NG_BENCH_HEIGHT` | `480` | frame height |
| `USBTOP_NG_BENCH_FPS` | `30` | frame rate asked of the camera |
| `USBTOP_NG_BENCH_INTERFACES` | all four | which of `none`, `mmap-ring`, `binary`, `text` to run |

A value that does not parse, or a zero count, fails the run with the
variable's name and the offending value.

The camera's bus and device number are read from sysfs: the canonical path
of `/sys/class/video4linux/<node>/device` is the USB interface, and its
parent directory holds `busnum`, `devnum`, `speed` and `product`. The
readers open that bus's own node (`/dev/usbmonN`, `…/usbmon/Nu`), not the
aggregate bus 0, so other buses' traffic stays out of the measurement.

The harness does not change camera controls. A camera whose frame rate
follows exposure should have that pinned by the operator before the run;
the harness reports each run's delivered frame rate so drift between runs is
visible.

### One run

For one interface, in order:

1. Start the reader on its own thread, exactly the production reader
   (`MmapReader::new`, `BinaryReader::new`, `UsbmonReader::new`) with a
   callback that only counts. Wait four poll intervals; a reader that has
   already returned failed its setup, and the run fails with its error.
2. Read the reader's CPU time and event counts, then `/proc/stat`.
3. Run `v4l2-ctl --verbose --stream-mmap --stream-count=<frames>` with the
   format, size and rate, to completion, timing it by the wall clock.
4. Read `/proc/stat`; wait two poll intervals, so the reader has read what
   the kernel still held for it; read the reader's CPU time and event counts
   again; then stop and join the reader.

Every figure of a run is therefore taken over the capture: the reader's
setup before it and teardown after it are outside, and reported separately.

The callback is a counter, not the production channel, so the figure is the
cost of reading and parsing, which is the part that differs between the
interfaces. The downstream cost (the channel and the accounting) is the same
for all three and is not measured. The callback passes each packet through
`std::hint::black_box`, as the production callback's channel send would
force every field to exist.

A fourth "interface", **none**, runs steps 2 to 4 with no reader. *Builder's
call:* it was not in the design the owner approved, and it adds a quarter to
the run time. Without it the whole-machine figure cannot be read, because it
includes the camera driver, `v4l2-ctl` and everything else the machine is
doing.

A run fails, and the benchmark with it, when `v4l2-ctl` fails or delivers a
different number of frames, when a frame line cannot be read, when the
reader returns an error, when the reader's loop returned before it was asked
to stop (the read() and text readers return `Ok` after a read error), and
when the reader saw no event from the camera's device number.

### What is measured

Per run:

| Figure | Source |
|---|---|
| elapsed time | wall clock around the `v4l2-ctl` run |
| frames delivered, frames the driver skipped, frame rate, MB/s | the `cap dqbuf` lines: their count, the gaps in `seq`, count over elapsed, the sum of `bytesused` |
| bus events, camera events | the callback, over the capture: every packet, and those whose device number is the camera's |
| reader CPU | the reader thread's `/proc/thread-self/schedstat`, opened by that thread and read by the benchmark's at the two moments above |
| system share, CPU outside the capture | `getrusage(RUSAGE_THREAD)` in the reader thread, at its start and its end |
| kernel drops | over the reader's whole life: the reader's `kernel_dropped` counter (`MON_IOCG_STATS`) on the two ring interfaces; on text the growth of the bus's `text_lost` in `/sys/kernel/debug/usb/usbmon/<bus>s` (`drivers/usb/mon/mon_stat.c`) |
| machine CPU | the busy share of the first line of `/proc/stat` over the capture, in CPU-seconds |

The first field of `schedstat` is the thread's time on a CPU in nanoseconds
(`Documentation/scheduler/sched-stats.rst`; `proc_pid_schedstat` in
`fs/proc/base.c` prints `se.sum_exec_runtime` as stored, so a thread on a
CPU at that instant is behind, normally by under a tick, without bound on a
`nohz_full` CPU). `RUSAGE_THREAD` is the
calling thread's usage (getrusage(2), Linux 2.6.26 and later); its user and
system times are scaled to add up to the same `sum_exec_runtime`, and the
split between them is tick-sampled (`cputime_adjust` in
`kernel/sched/cputime.c`). In `/proc/stat` (proc_stat(5)) busy is user +
nice + system + irq + softirq + steal, in units of `sysconf(_SC_CLK_TCK)`;
guest time is left out because the kernel adds it to user as well
(`account_guest_time`).

Reader CPU misses the work usbmon does when a transfer is submitted or
completes, which runs in the kernel outside the reader thread, and misses
more of it for the ring than for text: `mon_bin_event` copies the header and
the captured payload into the ring there, while `mon_text_event` keeps at
most 32 bytes and leaves the formatting to the reader's `read()`. The
machine figure includes that work and is noisy. Both are reported, the
machine figure also as the difference from the **none** runs.

Two tables are printed. The first has one row per run. The summary gives,
per interface: the mean over the rounds of elapsed time, frame rate, camera
events per second, reader CPU as a share of one core, reader CPU per
thousand bus events, the system share and the CPU outside the capture; the
mean, lowest and highest of reader CPU seconds and of machine CPU seconds;
the machine's share of a core and its difference from **none**; and the
totals of skipped frames and kernel drops.

### Order of runs

Each round runs none, mmap-ring, binary and text once, and each round starts
one place further along, so no interface always runs first. Four rounds put
every interface in every position once; the default stays at the three the
owner approved.

### Output

A header (kernel release, core count, build profile, the workload, the
camera's product name, bus, device and speed), one line per run as it
finishes, then the summary table. A debug build says so in the header and
again above the summary: its CPU figures do not describe the shipped binary.

Nothing that identifies the host is printed: no hostname, no serial.

### Running it

```bash
cargo test --release --features integration --no-run
sudo target/release/deps/usbtop_ng-<hash> capture_interface_benchmark \
    --ignored --nocapture
```

Built as the user and run as root, so `target/` does not fill with
root-owned files.

### Documentation

- `docs/BENCHMARK.md` (new): what is measured and what is not, how to run
  it, how to read the tables, and the dated results with the kernel, the
  hardware-role label from `docs/TESTING.md`, and the workload.
- `docs/ARCHITECTURE.md`: "No benchmark exists in this tree" becomes a
  pointer to the one that now does, and what it still does not cover.
- `README.md` and the man page's CAPTURE INTERFACES section: the sentence
  saying nothing benchmarks the fallbacks is replaced by what was measured,
  with its conditions, and a pointer to `docs/BENCHMARK.md`.
- `docs/CONTRIBUTING.md`: the `integration` section gains the benchmark.
- `CHANGELOG.md`.

## Approaches considered

1. **An ignored in-crate test under `integration`, one interface per run
   (chosen).** It reuses the production readers through their public
   constructors, needs no new feature (so no fifth configuration to gate),
   and nothing reaches the shipped binary. One interface at a time lets the
   machine's CPU be attributed. The cost is settings by environment variable
   and running a test binary under `sudo`.
2. **All three readers at once on the same bus.** Identical traffic for
   all three by construction, and a third of the run time. Rejected: the
   owner asked for the time and CPU of *each test*, the machine figure could
   not be split between the readers, and a webcam makes separate runs
   comparable anyway.
3. **A hidden subcommand in the binary under a new `bench` feature.**
   Nicer flags, and it would measure the installed release binary. Rejected:
   a fifth feature configuration for every gate, and dead-code obligations
   in each, for a tool run a few times a year.
4. **criterion over recorded fixtures.** Runs in CI and needs no hardware.
   Rejected: a replay from a file measures the parser and nothing of the
   system calls, the ring or the kernel, which is where the interfaces
   differ.

## Non-goals

- The eBPF backend. It is a different capture path with its own feature and
  its own prerequisites; adding it is a later change.
- Storage or any bursty workload.
- Memory use.
- A pass/fail threshold. The benchmark reports; it asserts only that each
  run delivered the frames asked for and that a reader did not fail.
- Running in CI.

## Limits to state wherever a figure is published

- One host, one kernel, one camera, one workload. The ranking can differ
  elsewhere, most of all on slow hardware.
- Reader thread time omits the kernel's work at transfer submission and
  completion, and omits more of it for the mmap ring and read() than for
  text, so it does not rank text against the other two.
- The benchmark runs the test build of the readers, which does a little
  per-event work in the two ring readers that the shipped binary does not.
- Machine CPU includes everything else the machine was doing.
- Other devices on the camera's bus add events; the operator should leave
  the mouse and keyboard alone during a run, or use a camera on a quiet bus.

## Left to the builder

Table layout, function names, and how the summary rounds its figures.

## What the review changed

The adversarial review (2026-11-22) found the first build's method wanting
in ways that moved a figure, and the build was changed and rerun:

- Reader CPU and event counts covered the reader thread's whole life while
  the elapsed time covered only the capture; ring setup alone was about a
  tenth of the mmap ring's figure. They are now taken over the capture, from
  `schedstat`, and the rest is reported as "outside".
- A reader whose loop ended by itself part of the way through was accepted.
  It now fails the run, as does a reader that saw nothing from the camera.
- A frame line without a sequence number or a size was counted as a frame.
  It is now an error.
- A failing `MON_IOCG_STATS` would have printed as zero drops. The ioctl is
  now checked before the first run.
- The counting callback let the optimiser skip work the production callback
  forces. It now uses `black_box`.
- The system calls and `/proc` readers ran in no gate. They now have
  hermetic tests under the `integration` feature.
- The published sentences said "nothing was lost" and ranked text against
  the mmap ring on reader CPU alone. They now say what was observed and
  where reader CPU cannot rank.

The reader's CPU alone could not rank text against the mmap ring, so
usbmon's kernel-side work was timed separately with `bpftrace` and the two
added in `docs/BENCHMARK.md`. That measurement is outside the benchmark: the
script and the command are in the document.

The scoped re-review of the same day confirmed those and added four that
were taken: the kernel-side trace had pooled the mmap ring's run with
read()'s, which take different paths in `mon_bin_event`, so
`USBTOP_NG_BENCH_INTERFACES` now lets one interface run alone and each is
traced on its own; text does have a kernel loss count, the bus's
`text_lost`, which the benchmark now reads; the schedstat test now runs the
benchmark's own thread start and skips only where the kernel provably keeps
no statistics; and the "one tick" bound on a schedstat reading is stated as
the normal case, not a guarantee.

Declined, with the reason: a signal from the reader that it is ready or has
caught up, in place of the two fixed waits (the production readers have no
such signal and the benchmark must not change them; the docs name the two
columns that show a wait was too short); carrying a failed `MON_IOCG_STATS`
out of the reader (`mon_bin_ioctl` fails that request only with `EFAULT`, on
a bad pointer, and the ioctl is proven on the node before the first run); a
timeout on `v4l2-ctl` (an operator-run tool;
Ctrl-C stops it, and the docs say so); a guard against a zero elapsed time
(a spawned process that delivered frames cannot have taken none); counting
frames the driver flagged as damaged (the flag's spelling in the `v4l2-ctl`
log could not be checked without a camera that produces one; the docs state
the limit).
