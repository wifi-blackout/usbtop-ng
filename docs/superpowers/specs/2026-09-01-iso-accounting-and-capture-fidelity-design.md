# Isochronous accounting and capture fidelity — design

**Date:** 2026-09-01
**Status:** approved for planning
**Wave:** 1 of the 2026-09 series (known bugs). Later waves: troubleshooting
export + support bundle; one row per physical connector; cable and port
diagnostics; parked minors.

## Goal

Close the two isochronous-accounting bugs the roadmap carries, fix the
capture-fidelity defect the investigation exposed in the fixture harness,
prune the roadmap entries that are already fixed, and bring user-facing
text under a written rule set. Every claim below rests on a measurement
taken 2026-09-01 on `mainrag` (this host, AMD xHCI, kernel 7.0.0-30) and
`asus`, or on the v7.0 kernel source.

## Background: what the spike established

### The binary interface does not undercount isochronous transfers

The roadmap section "Discovered: the usbmon binary reader undercounts
high-bandwidth isochronous transfers" predates the ring-size ladder. A
four-source comparison of one 100-frame raw YUYV 640x480 stream (exactly
61,440,000 frame bytes, streamed by `v4l2-ctl` inside a 45 s window) from
the Chicony webcam on `mainrag` bus 1, endpoint 1 IN, alt setting 3x1020:

| Source | Bytes on the iso endpoint | Ratio to frame bytes |
|---|---|---|
| mmap ring (default build) | 62,397,736 | 1.0156 |
| eBPF build | 62,397,736 | 1.0156 |
| Frame bytes + one 12-byte UVC header per packet (79,968 packets) | 62,399,616 | 1.0156 |

mmap and eBPF agree byte for byte, and both sit exactly one UVC payload
header per packet above the frame data. Kernel source (v7.0
`drivers/usb/mon/mon_bin.c`, lines 512-513 and 581) confirms the binary
header's length field is `urb->actual_length` for callback events, the same
quantity the eBPF program sums. The old undercount was the ring overflow the
ladder fixed. Outcome: the roadmap section closes with this table; no code
change to the binary path.

### The text interface can be estimated to within about 1%

Kernel source (v7.0 `drivers/usb/mon/mon_text.c`, lines 218-247 and
590-606): for an isochronous callback the printed length is
`urb->transfer_buffer_length` ("Wasteful, but simple to understand: ISO 'C'
is sparse"), the printed descriptor count is the URB's full
`number_of_packets`, and the first `min(number_of_packets, 5)` descriptors
print as `status:offset:length` with `length = actual_length`.

Scaling the printed descriptors' actual-length sum by
`number_of_packets / printed_count` measured against the exact mmap total of
the same window:

| Stream | Buffer size (today) | Printed-5 sum (roadmap idea) | Sampled mean (this design) |
|---|---|---|---|
| `asus`, internal UVC webcam, MJPEG, 7.8 fps, 200 frames | 15.42x | 0.16x | 0.9999x |
| `mainrag`, Chicony webcam, YUYV, 10 fps, 100 frames | 3.98x | 0.16x | 1.011x |

The estimate is exact whenever a URB carries five or fewer packets, because
then every descriptor prints.

### The fixture capturer's binary reader drops most events under load

The capturer reads `/dev/usbmonN` with plain `read()` on the kernel's
default ~300 KiB ring and never requests the ladder the mmap reader uses.
An isochronous callback event occupies up to `ring_size / 5` bytes of ring,
so five events fill it. Measured: the capturer kept 32.4% of the iso URBs
the concurrently captured text trace held, on both `mainrag` and `asus`. The
committed `asus-2026-08-31/stage2` binary golden is therefore drop-starved,
and its note claiming a "~9.8x" text-versus-binary overcount is not an
accuracy statement. `pi400-2026-08-31/stage2` (bulk) shows the *text* side
short instead: its text queue is a few dozen events deep (`EVENT_MAX` in
`mon_text.c`) and has no drop counter at all. Goldens pin pipeline
determinism, so CI is unaffected; the coverage claims are what is wrong.

The runtime read()-based fallback reader (`src/usbmon/binary.rs`) has the
same gap: no ladder, and no kernel-drop counter feeding `kdropped:`.

### Already fixed, roadmap stale

- "A root-owned /dev/usbmon node reads as absent for a plain user" —
  `src/usbmon/mod.rs` distinguishes permission-denied from absent and the
  Unreleased changelog records it.
- "Search filtering waits for the next refresh tick" — shipped, Unreleased
  changelog "Fixed".

## Requirements

### R1. Text-interface isochronous estimate

In `src/usbmon/parser.rs`, for a `C` (callback) event whose address word is
`Z*` (isochronous, either direction):

- Parse the printed descriptors instead of skipping them. Each is
  `status:offset:length`; only `length` is used.
- Let `n` = the printed descriptor count word, `k` = the number of
  descriptor words actually present (0 ≤ k ≤ 5), `s` = the sum of their
  lengths, `L` = the length word.
- If `k == 0`, `data_length = L` (today's behavior; also what a synthetic
  seed line yields).
- Otherwise `data_length = round(s * n / k)`, computed in `f64` and
  converted to `u32`. When `k == n` this equals `s` exactly.
- `S` and `E` events are unchanged (submissions are not counted; their
  descriptors carry requested, not actual, lengths).
- The `estimated` flag semantics do not change: a device with isochronous
  traffic under an active text source is still marked `estimated`, because
  the value is a sample-scaled estimate. The TUI's `~` marker and the
  JSON field are unchanged.

Tests: unit tests built from real lines captured during the spike (an idle
URB with five 12-byte headers, a partial URB, a full URB), an exact case
with `n <= 5`, the `k == 0` fallback, and a check that submissions are not
affected. The `asus-2026-08-31/stage2` text golden is re-blessed (its iso
total changes from the buffer-size figure to the estimate; the recapture in
R4 replaces the whole bundle anyway).

### R2. Ring ladder and drop counter for every read()-based consumer

Move the usbmon binary-interface ioctl surface out of `mmap_ring.rs` into a
shared module (`src/usbmon/ring.rs`): the ioctl-number derivation,
`IoctlRequest`, `RING_SIZE_LADDER`, `set_ring_size`, `ring_size`,
`MonBinStats` and `stats`. `MmapReader` keeps its behavior and uses the
shared items. Then:

- `BinaryReader::read_packets` requests the ladder immediately after
  opening the device, best-effort and debug-logged exactly as the mmap
  reader does, and reads `MON_IOCG_STATS` periodically, folding the
  kernel's `dropped` count into the shared `kernel_dropped` counter with the
  same cadence and same signature shape as `MmapReader::read_packets`.
  `monitor.rs` passes the counter through. A kernel without the ioctls
  (`ENOTTY`) leaves the default ring and adds nothing, never fails.
- The capturer's `capture_until` requests the ladder after opening and reads
  the stats once when the deadline passes. It returns the bytes plus
  `Option<u64>` kernel drops: `Some(n)` when the stats ioctl works (the
  binary device), `None` when it does not (the debugfs text file, or an old
  kernel). `capture_pair` stays generic over that return type.

Tests: on a regular temp file the ladder and stats calls fail with `ENOTTY`
and are ignored (`None` drops, bytes still read); the `BinaryReader`
signature change is covered by its existing fixture-driven tests plus one
that passes a counter and asserts it stays zero on a fixture file.

### R3. Bundles declare their completeness

- `meta.toml` gains `binary_kernel_dropped = <u64>` written only when the
  binary source reported a count. The harness `Meta` struct reads it as
  `Option<u64>` with a serde default, so every existing bundle still parses.
- The capturer prints a warning to stderr when the count is non-zero,
  naming the count and saying the binary golden is incomplete for accuracy
  purposes.
- The strict corpus check does not fail on drops. A bundle with drops is a
  valid pipeline pin; the count is documentation for coverage claims.
- Text-side drops are unmeasurable; TESTING.md says so and tells the
  capturer to keep event rates modest for text-inclusive stages.

### R4. Recapture and the characterization bundle

With the fixed capturer:

- Recapture `asus-2026-08-31/stage2` in place (same generator: `v4l2-ctl
  --stream-mmap` on the internal UVC webcam, sized so the stream ends inside
  the window; the camera runs near 8 fps in room light). Replace goldens;
  rewrite the `[generator]` note with the real figures: binary drops
  recorded, text buffer-size ratio, and the estimator's ratio.
- Recapture `pi400-2026-08-31/stage2` in place (same `dd` generator).
  Record drops. Note that the text side is expected short on bulk.
- Add `mainrag-<date>/stage1` (ambient) and `stage2` (the ground-truth iso
  stage): `[generator]` records the `v4l2-ctl` command, the frame count and
  frame bytes, the mmap and eBPF totals, and the UVC-header arithmetic.
  This is the committed characterization fixture the roadmap asked for.
- After every recapture: `cargo test fixture_corpus` green, corpus strict
  check green, SEC-1 and SEC-2 unchanged.

### R5. Documentation

- `docs/ROADMAP.md`: replace the "Discovered: ... undercounts" section with
  a "Resolved" section carrying the four-source table; rewrite the
  engineering follow-up on the text overcount as shipped, with the
  estimator table; delete the sudo-remedy and search-tick bullets; mark the
  error/log-string item done; adjust the Linux 7.3 tracking paragraph to say
  "re-run the four-source check", not "re-characterize the undercount".
- `docs/SCRIPTING.md` "The `estimated` field": describe the sampled
  estimate, its measured accuracy, and that it is exact at five or fewer
  packets per URB; drop the 3.6x sentence.
- `docs/TESTING.md` "Capturing hardware fixtures": the drop counter and
  warning, text-side drops being unmeasurable, and sizing the window to the
  camera's real frame rate (a webcam in room light may deliver a third of
  its nominal rate).
- `CHANGELOG.md` Unreleased: Fixed (text iso estimate; read()-fallback ring
  and drop counter; capturer ring), Added (`binary_kernel_dropped`, the
  `mainrag` bundle), Changed (recaptured bundles).
- `docs/CONTRIBUTING.md`: a "User-facing text" subsection under Code style
  (R6).

### R6. User-facing text rules and sweep

Rules, added to CONTRIBUTING and applied across `src/`:

- Error messages (`anyhow!`, `bail!`, `warn!`, `error!`, and errors printed
  with `eprintln!`): start lowercase, no trailing period, name the subject
  and the offending value, chain causes with `: `, say "could not" rather
  than "Failed to", no exclamation marks.
- Remedies and prompts printed to the user (`println!`/`eprintln!` guidance
  text): sentence case, imperative, one action per line, exact commands on
  their own line.
- Log lines (`info!`/`debug!`): lowercase, present tense, name the
  interface and bus they concern.

The sweep rewrites existing strings to match and updates the tests that
assert on them. No behavior changes.

## Non-goals

- Changing the eBPF backend or the mmap reader's accounting.
- Publishing a lower-bound field or an interval for text-mode iso rates.
- Parsing per-descriptor lengths from the binary ring; `actual_length` is
  already exact there.
- Any TUI change beyond none; the `~` marker keeps its meaning.

## Global constraints

- MSRV 1.88; zero `#[allow(...)]`; `cargo fmt`; `cargo clippy -D warnings`
  on the default, `capture-fixture`, `integration`, and `ebpf` configs.
- Kernel FFI and format semantics are verified against kernel source, cited
  by file and line, never against a quiet live device.
- The private reference project is never named in the repo.
- Bundles stay payload-free (SEC-1) and path-contained (SEC-2).
- `#[cfg]` lattice unchanged: `capture` module feature-only,
  `fixture_replay` under `any(test, feature)`, `fixture_corpus` test-only.

## Verification

- Unit and corpus suites green on every config above.
- A live re-run of the four-source comparison on `mainrag` after R1 and R2:
  mmap and eBPF still byte-identical; the text estimate within 2% of them;
  capturer drops zero on the iso stream.
- `cargo test fixture_corpus` green with the three recaptured or new
  bundles; the strict corpus check accepts the new meta key.
