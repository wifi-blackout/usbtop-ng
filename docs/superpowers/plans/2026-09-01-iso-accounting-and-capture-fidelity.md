# Isochronous Accounting and Capture Fidelity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the text usbmon interface's isochronous byte counts a ~1% estimate instead of a 4-15x overcount, give every read()-based usbmon consumer the same enlarged kernel ring and drop counter the mmap reader has, make fixture bundles declare their own kernel drops, recapture the drop-starved bundles plus a ground-truth iso bundle and the new asus hub stages, close the stale roadmap entries with the evidence, and put user-facing text under a written rule.

**Architecture:** The text parser (`src/usbmon/parser.rs`) stops skipping the printed iso descriptors and scales their actual-length sum by the URB's full packet count for callbacks. The usbmon binary-interface ioctl surface (ring ladder, size, stats, drop folding) moves out of `mmap_ring.rs` into a new `src/usbmon/ring.rs` shared by the mmap reader, the read()-based `BinaryReader`, and the fixture capturer's raw `capture_until`. The capturer threads the kernel drop count through `CapturedTrace` into `meta.toml`. Recaptures and docs follow, with numbers from the 2026-09-01 spike and the recapture runs.

**Tech Stack:** Rust 1.88 (MSRV), `libc` ioctls verified against kernel v7.0 source, serde/toml for `meta.toml`, the in-crate `capture-fixture` feature and `fixture_corpus` harness, `v4l2-ctl` and `dd` as traffic generators on the fleet.

**Spec:** `docs/superpowers/specs/2026-09-01-iso-accounting-and-capture-fidelity-design.md`

## Global Constraints

- MSRV 1.88; zero `#[allow(...)]`; `cargo fmt`; `cargo clippy --all-targets -- -D warnings` on the default build and on `--features capture-fixture`, `--features integration`, `--features ebpf`.
- Kernel FFI and format semantics are verified against kernel source, cited by file and line (v7.0 `drivers/usb/mon/mon_text.c` lines 218-247, 455-459, 590-606; `drivers/usb/mon/mon_bin.c` lines 512-513, 581), never against a quiet live device.
- The private reference project is never named in the repo, this plan included. `PRIVATE_NAME` below is that name, supplied by the controller in each dispatch prompt and exported in the shell (`export PRIVATE_NAME=...`); before every commit `git grep -i -e "$PRIVATE_NAME"` must print nothing.
- Bundles stay payload-free (SEC-1) and path-contained (SEC-2); every per-record `len_cap` is zero.
- `#[cfg]` lattice unchanged: `capture` module feature-only, `fixture_replay` under `any(test, feature = "capture-fixture")`, `fixture_corpus` test-only. New shared code in `usbmon/ring.rs` is in the default build.
- Commit messages: conventional prefix (`fix:`, `feat:`, `refactor:`, `test:`, `docs:`), body explains why, and the trailer block:
  ```
  Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
  Claude-Session: https://claude.ai/code/session_011Q8hG1q7GtEWzYuSRDyb1t
  ```
- `cargo` is not on PATH in this environment: prefix every cargo command with `export PATH="$HOME/.cargo/bin:$PATH";`.
- Test runs: `cargo test` (default), `cargo test --features capture-fixture`, and `cargo test --features ebpf` must all stay green after every task; the ebpf feature builds on this x86-64 host (clang and libbpf-dev are installed).

## File structure

| Path | Responsibility after this plan |
|---|---|
| `src/usbmon/parser.rs` | Text-line parsing; now parses iso descriptors and computes the callback estimate |
| `src/usbmon/ring.rs` (new) | usbmon binary-interface ioctl surface: request numbers, structs, `IoctlRequest`, ring ladder, `ring_size`, `set_ring_size`, `request_ring_ladder`, `stats`, `add_kernel_drops` |
| `src/usbmon/mmap_ring.rs` | mmap-ring reader only; imports the ioctl surface from `ring.rs` |
| `src/usbmon/binary.rs` | read()-based reader; requests the ladder, folds kernel drops |
| `src/usbmon/monitor.rs` | passes the drop counter to the binary reader |
| `src/usbmon/mod.rs` | declares `pub mod ring;` |
| `src/capture/mod.rs` | `capture_until` requests the ladder and returns drops; `CapturedTrace.kernel_dropped`; warning at capture time |
| `src/capture/meta.rs` | writes `binary_kernel_dropped` |
| `src/fixture_corpus.rs` | `bless_named_bundle`, meta key type check, ground-truth bundle check |
| `src/fixture_replay.rs` | binary replay passes a drop counter (signature change only) |
| `tests/fixtures/hosts/**` | recaptured asus stage2, new asus stage3/stage4, recaptured pi400 stage2, new mainrag stage1/stage2 |
| `docs/ROADMAP.md`, `docs/SCRIPTING.md`, `docs/TESTING.md`, `docs/CONTRIBUTING.md`, `CHANGELOG.md` | R5 and R6 documentation |

Ruling recorded here so executors see it: the spec's R3 says the harness `Meta` struct reads `binary_kernel_dropped` as `Option<u64>`. `Meta` is `#[cfg(test)]` and a field nothing reads trips the dead-code lint under `-D warnings`. serde already ignores unknown keys, so old bundles parse without any change. The plan therefore validates the key's type through `toml::Value` in a corpus test (Task 4) and pins the ground-truth bundle's zero-drop value (Task 6) instead of adding an unread field.

---

### Task 1: Text-interface isochronous estimate

**Files:**
- Modify: `src/usbmon/parser.rs:321-347` (the iso descriptor branch and the length word)
- Modify: `src/usbmon/parser.rs:490-504` (existing test `parses_iso_events_with_descriptors`)
- Modify: `src/fixture_corpus.rs` (add `bless_named_bundle` beside `bless_seed_goldens`)
- Modify: `tests/fixtures/hosts/asus-2026-08-31/stage2/golden.text.json` (re-blessed)

**Interfaces:**
- Consumes: `parse_usbmon_text_line(&str) -> Result<UsbPacket>`; `UsbPacket.data_length: u32`; `UrbType::{Submission, Callback, Error}`.
- Produces: no signature change. `data_length` on an isochronous callback is now the estimate described below. `bless_named_bundle` (ignored test) regenerates one named bundle's goldens from `USBTOP_NG_BLESS_BUNDLE=<host-dir>/<stage-dir>`.

Kernel facts this task encodes (v7.0 `drivers/usb/mon/mon_text.c`): line 218-219 sets the length to `actual_length` for callbacks, then lines 245-247 override it with `transfer_buffer_length` for any isochronous URB with packets ("ISO 'C' is sparse"); lines 233-243 copy the first `min(number_of_packets, 5)` descriptors with `actual_length` for callbacks; lines 455-459 print, per event, either the status set alone (`E`) or the iso status word then the descriptors (any other type on an isochronous endpoint); lines 595-606 print the *full* `numdesc` then up to 5 `status:offset:length` words.

- [ ] **Step 1: Write the failing tests**

Append inside `mod tests` in `src/usbmon/parser.rs`:

```rust
    /// Real lines from the 2026-09-01 spike captures (asus internal UVC
    /// webcam, MJPEG; mainrag Chicony webcam, YUYV on a 3x1020 endpoint).
    /// The kernel prints the first 5 of a URB's descriptors with their
    /// *actual* lengths plus the full descriptor count, while the length
    /// word is the whole buffer (mon_text.c v7.0 lines 245-247). The
    /// estimate is the printed sum scaled by count / printed.
    #[test]
    fn iso_callback_length_is_the_printed_descriptors_scaled_by_the_full_count() {
        // Idle URB: 32 packets, each printed one a 12-byte UVC header.
        // 60 * 32 / 5 = 384, not the 73,728-byte buffer.
        let idle = "ffff8b93d7026800 2391376079 C Zi:3:002:1 0:1:23720:0 32 0:0:12 0:2304:12 0:4608:12 0:6912:12 0:9216:12 73728 <";
        assert_eq!(parse_usbmon_text_line(idle).unwrap().data_length, 384);

        // Partial fill: 956+284+1932+684+660 = 4516; 4516 * 32 / 5 = 28902.4.
        let partial = "ffff8b94d1a62c00 2391380055 C Zi:3:002:1 0:1:23752:0 32 0:0:956 0:2304:284 0:4608:1932 0:6912:684 0:9216:660 73728 <";
        assert_eq!(parse_usbmon_text_line(partial).unwrap().data_length, 28902);

        // Every packet full: 5 * 3060 * 32 / 5 = 97920 == the buffer size.
        let full = "ffff89c8fe10e800 2366937589 C Zi:1:004:1 0:1:16:0 32 0:0:3060 0:3060:3060 0:6120:3060 0:9180:3060 0:12240:3060 97920 <";
        assert_eq!(parse_usbmon_text_line(full).unwrap().data_length, 97920);
    }

    /// With five or fewer packets every descriptor prints, so the estimate
    /// is exact: 3 x 2048 moved, not the 12288-byte buffer.
    #[test]
    fn iso_callback_with_all_descriptors_printed_is_exact() {
        let line = "ffff88005bd8b100 2189040992 C Zo:1:005:2 0:1:1810:0 3 0:0:2048 0:2048:2048 0:4096:2048 12288 >";
        assert_eq!(parse_usbmon_text_line(line).unwrap().data_length, 6144);
    }

    /// A descriptor count with no descriptor words (the synthetic seed
    /// fixtures do this) keeps the length word, exactly as before.
    #[test]
    fn iso_callback_without_descriptor_words_keeps_the_length_word() {
        let line = "ffff0000cccc0001 200 C Zi:2:004:1 0:1:6672:0 32 27000 <";
        assert_eq!(parse_usbmon_text_line(line).unwrap().data_length, 27000);
    }

    /// Submissions carry *requested* lengths in their descriptors and are
    /// never counted; their length word stays untouched.
    #[test]
    fn iso_submission_length_is_not_estimated() {
        let line = "ffff88005bd8b100 2189039971 S Zo:1:005:2 -115:1:1810 3 -18:0:2048 -18:2048:2048 -18:4096:2048 12288 >";
        assert_eq!(parse_usbmon_text_line(line).unwrap().data_length, 12288);
    }

    /// `E` events print only a status word (mon_text.c v7.0 lines 455-459):
    /// no descriptor count, no descriptors, and here no length either.
    #[test]
    fn iso_error_event_has_no_descriptors() {
        let p = parse_usbmon_text_line("ffff88006fff3800 2453805583 E Zi:1:004:1 -2").unwrap();
        assert_eq!(p.urb_type, UrbType::Error);
        assert_eq!(p.data_length, 0);
    }
```

Then change the second half of the existing `parses_iso_events_with_descriptors` test: its `C Zo` line now yields the exact descriptor sum, so replace `assert_eq!(p.data_length, 12288);` on the callback with `assert_eq!(p.data_length, 6144); // 3 printed of 3: exact`. Leave the submission assertion at 12288.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test iso_ -- --nocapture 2>&1 | tail -30`
Expected: `iso_callback_length_is_the_printed_descriptors_scaled_by_the_full_count`, `iso_callback_with_all_descriptors_printed_is_exact`, `iso_error_event_has_no_descriptors`, and `parses_iso_events_with_descriptors` FAIL (wrong `data_length`, and the `E` line errors with "Missing isochronous descriptor count"); the other two pass already.

- [ ] **Step 3: Implement the estimate**

Replace the block from the comment `// Word 6 (isochronous transfers only, ...` through the end of the `let data_length: u32 = match tokens.next() { ... };` statement with:

```rust
    // Word 6 (isochronous transfers only, when word 5 wasn't a setup and the
    // event is not an `E`, which prints only its status set -- see
    // mon_text_read_u, drivers/usb/mon/mon_text.c v7.0 lines 455-459): the
    // URB's *full* descriptor count, then up to 5 `status:offset:length`
    // words. For a callback those printed lengths are actual lengths while
    // the length word further on is the whole buffer ("ISO 'C' is sparse",
    // lines 245-247), so the printed sample scaled by the full count is the
    // best available estimate of the bytes moved: measured at 0.9999x and
    // 1.011x of the exact binary total on two cameras (2026-09-01 spike),
    // and exact whenever every descriptor printed (five or fewer packets).
    let mut iso_estimate: Option<u32> = None;
    if transfer_type == 'Z' && !is_setup && urb_type != UrbType::Error {
        let ndesc_token = tokens
            .next()
            .ok_or_else(|| anyhow!("Missing isochronous descriptor count"))?;
        let ndesc: u32 = ndesc_token
            .parse()
            .map_err(|_| anyhow!("Invalid isochronous descriptor count: {}", ndesc_token))?;
        let mut printed_lengths: Vec<u32> = Vec::with_capacity(5);
        for _ in 0..ndesc.min(5) {
            match tokens.peek() {
                Some(word) if word.contains(':') => {
                    let length_field = word.rsplit(':').next().unwrap_or("");
                    let length: u32 = length_field
                        .parse()
                        .map_err(|_| anyhow!("Invalid isochronous descriptor: {}", word))?;
                    printed_lengths.push(length);
                    tokens.next();
                }
                _ => break,
            }
        }
        if urb_type == UrbType::Callback && !printed_lengths.is_empty() {
            let printed = printed_lengths.len() as f64;
            let sum: u64 = printed_lengths.iter().map(|&l| u64::from(l)).sum();
            iso_estimate = Some((sum as f64 * f64::from(ndesc) / printed).round() as u32);
        }
    }

    // Word 7: data length. `E` events may omit it entirely. An isochronous
    // callback's estimate (above) replaces the buffer-size word.
    let length_word: u32 = match tokens.next() {
        Some(word) => word
            .parse()
            .map_err(|_| anyhow!("Invalid data length: {}", word))?,
        None if urb_type == UrbType::Error => 0,
        None => {
            return Err(anyhow!(
                "Invalid usbmon text line format: missing data length"
            ))
        }
    };
    let data_length = iso_estimate.unwrap_or(length_word);
```

- [ ] **Step 4: Run the parser tests**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test usbmon::parser 2>&1 | tail -5`
Expected: all parser tests PASS.

- [ ] **Step 5: Run the whole default suite and see the one expected corpus failure**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test 2>&1 | grep -E 'FAILED|failed|test result' | head`
Expected: exactly one failure, `fixture_corpus::every_bundle_replays_to_its_golden`, on `asus-2026-08-31/stage2` text (its iso total is now the estimate, not 368,418,816). `seed-iso-2026-08-30` is unaffected (its line prints no descriptor words).

- [ ] **Step 6: Add `bless_named_bundle` to `src/fixture_corpus.rs`**

Insert directly after the `bless_seed_goldens` test:

```rust
/// Bless helper for one named *real* bundle after an intentional pipeline
/// change (a parser fix that alters what a committed trace replays to).
/// Regenerates only the goldens -- never a trace -- of the bundle named by
/// `USBTOP_NG_BLESS_BUNDLE=<host-dir>/<stage-dir>`, relative to
/// `tests/fixtures/hosts`. Not run in CI:
///   USBTOP_NG_BLESS_BUNDLE=asus-2026-08-31/stage2 cargo test bless_named_bundle -- --ignored --nocapture
#[test]
#[ignore]
fn bless_named_bundle() {
    let name = std::env::var("USBTOP_NG_BLESS_BUNDLE")
        .expect("set USBTOP_NG_BLESS_BUNDLE=<host-dir>/<stage-dir>");
    let bundle = discover_bundles()
        .into_iter()
        .find(|b| b.dir.ends_with(&name))
        .unwrap_or_else(|| panic!("no bundle named {name} under {}", fixtures_root().display()));
    for source in sources_of(&bundle) {
        let report = replay_fixture(&bundle.dir, source).unwrap();
        std::fs::write(
            bundle.dir.join(source.golden_filename()),
            report_to_golden_json(&report).unwrap(),
        )
        .unwrap();
    }
    eprintln!("blessed {}", bundle.dir.display());
}
```

- [ ] **Step 7: Re-bless the asus stage2 goldens and verify the corpus**

Run:
```bash
export PATH="$HOME/.cargo/bin:$PATH"
USBTOP_NG_BLESS_BUNDLE=asus-2026-08-31/stage2 cargo test bless_named_bundle -- --ignored --nocapture 2>&1 | grep blessed
git diff --stat tests/fixtures
python3 -c "import json; r=json.load(open('tests/fixtures/hosts/asus-2026-08-31/stage2/golden.text.json')); print([e['total_bytes'] for b in r['buses'] for d in b['devices'] for e in d['endpoints'] if e['transfer_type']=='iso'])"
cargo test 2>&1 | grep 'test result'
```
Expected: only `golden.text.json` changes; the iso endpoint total prints near 116,450,000 (the spike's estimator sum on this trace; the exact figure differs by per-line rounding and goes in the commit message); every `test result` line is `ok`.

- [ ] **Step 8: Gates and commit**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo fmt && cargo clippy --all-targets -- -D warnings && cargo clippy --all-targets --features capture-fixture -- -D warnings && cargo test --features capture-fixture 2>&1 | grep 'test result' && git grep -i -e "$PRIVATE_NAME"`
Expected: clean, all ok, the grep prints nothing.

```bash
git add src/usbmon/parser.rs src/fixture_corpus.rs tests/fixtures/hosts/asus-2026-08-31/stage2/golden.text.json
git commit -m "fix(parser): estimate iso callback bytes from the printed descriptors

The text interface prints the whole transfer buffer as an isochronous
callback's length (mon_text.c v7.0 lines 245-247) but also the first five
descriptors with their actual lengths and the full packet count. Scaling
the printed sum by count / printed measured 0.9999x (asus MJPEG) and
1.011x (mainrag YUYV) of the exact binary total, against 15.4x and 4.0x
for the buffer size. Exact when five or fewer packets print. E events
print no descriptors and now parse. The asus stage2 text golden is
re-blessed via the new bless_named_bundle helper: iso total <exact value>.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_011Q8hG1q7GtEWzYuSRDyb1t"
```
(Replace `<exact value>` with the number printed in Step 7.)

---

### Task 2: Extract the usbmon ring ioctl surface into `src/usbmon/ring.rs`

**Files:**
- Create: `src/usbmon/ring.rs`
- Modify: `src/usbmon/mmap_ring.rs` (remove the moved items; import them; replace the ladder loop)
- Modify: `src/usbmon/mod.rs:8-14` (declare the module)

**Interfaces:**
- Consumes: nothing new.
- Produces (all `pub(crate)`, in `crate::usbmon::ring`): `type IoctlRequest`; `const MON_IOCQ_RING_SIZE: u32`, `MON_IOCT_RING_SIZE`, `MON_IOCH_MFLUSH`, `MON_IOCG_STATS`; `fn mon_iocx_mfetch() -> u32`; `struct MonBinMfetch { offvec: *mut u32, nfetch: u32, nflush: u32 }`; `struct MonBinStats { queued: u32, dropped: u32 }`; `const RING_SIZE_LADDER: [usize; 4]`; `fn ring_size(fd: RawFd) -> io::Result<usize>`; `fn set_ring_size(fd: RawFd, bytes: usize) -> io::Result<()>`; `fn request_ring_ladder(fd: RawFd, path: &Path)`; `fn stats(fd: RawFd) -> io::Result<MonBinStats>`; `fn add_kernel_drops(counter: &AtomicU64, dropped: u32)`. Tasks 3 and 4 call `request_ring_ladder`, `stats`, and `add_kernel_drops`.

This is a move, not a rewrite: every function body, doc comment, and `// SAFETY:` block travels verbatim from `mmap_ring.rs` (lines 70-160 for the ioctl derivation, constants, and structs; 318-374 for `IoctlRequest`, `ring_size`, `set_ring_size`; 432-475 for `stats` and `add_kernel_drops`; 39-65 for `RING_SIZE_LADDER`). `mfetch`, `mflush`, `OFFSETS_CAP`, `RingMapping`, and the ring walk stay in `mmap_ring.rs`. The only new code is `request_ring_ladder` and its test.

- [ ] **Step 1: Write the failing test for the new helper**

Create `src/usbmon/ring.rs` with a module doc, the moved items (see Step 3), and this test module at the bottom:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::io::AsRawFd;

    /// A regular file has no usbmon ring: every request in the ladder fails
    /// with ENOTTY, the helper swallows each one, and the size query fails
    /// the same way afterward. This is the fixture-file path every
    /// read()-based test exercises, so it must be silent and harmless.
    #[test]
    fn request_ring_ladder_is_a_no_op_on_a_regular_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("not-usbmon");
        std::fs::write(&path, []).unwrap();
        let file = std::fs::File::open(&path).unwrap();
        let fd = file.as_raw_fd();

        request_ring_ladder(fd, &path);

        let err = ring_size(fd).unwrap_err();
        assert_eq!(err.raw_os_error(), Some(libc::ENOTTY));
        let err = stats(fd).unwrap_err();
        assert_eq!(err.raw_os_error(), Some(libc::ENOTTY));
    }
}
```

Also move these four tests verbatim from `mmap_ring.rs`'s test module into this one: `add_kernel_drops_sums_every_read_even_when_the_value_repeats`, `add_kernel_drops_handles_a_lower_read_without_wrapping`, `add_kernel_drops_from_multiple_readers_sums_into_one_counter`, `ioctl_numbers_match_the_verified_constants`. Add whatever `use` lines they need (`AtomicU64`, `Ordering`).

- [ ] **Step 2: Declare the module and run the test to verify it fails to compile**

In `src/usbmon/mod.rs` add `pub mod ring;` after `pub mod parser;`.

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test usbmon::ring 2>&1 | tail -5`
Expected: compile error, `request_ring_ladder` (and the moved items) not found until Step 3 lands.

- [ ] **Step 3: Write `ring.rs` and slim `mmap_ring.rs`**

`src/usbmon/ring.rs` top:

```rust
//! The usbmon binary interface's ioctl surface (`drivers/usb/mon/mon_bin.c`):
//! request numbers derived the way the kernel derives them, the argument
//! structs in kernel layout, the ring-size ladder, and the drop counter.
//! Shared by the mmap-ring reader ([`super::mmap_ring`]), the read()-based
//! reader ([`super::binary`]), and the fixture capturer, so every consumer of
//! `/dev/usbmonN` asks for the same enlarged ring and reports kernel drops
//! the same way. The read()-based path is not exempt: the ring is the same
//! per-open buffer whichever way it is drained, and on the default ~300 KiB
//! ring one isochronous callback can occupy a fifth of it.
//!
//! The ioctl numbers and struct layouts were verified against a live
//! `/dev/usbmon1`; [`tests::ioctl_numbers_match_the_verified_constants`]
//! pins them.

use std::io;
use std::mem::size_of;
use std::os::unix::io::RawFd;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use log::debug;
```

Then, in this order, the moved items with their original docs: `RING_SIZE_LADDER`; `IOC_NONE`, `IOC_WRITE`, `IOC_READ`, `USBMON_IOC_MAGIC`, `ioc`; `MON_IOCQ_RING_SIZE`, `MON_IOCT_RING_SIZE`, `MON_IOCH_MFLUSH`, `MON_IOCG_STATS`, `mon_iocx_mfetch`; `MonBinMfetch`, `MonBinStats`; `IoctlRequest`; `ring_size`, `set_ring_size`; then the new helper:

```rust
/// Step [`RING_SIZE_LADDER`] down largest-first on `fd`, stopping at the
/// first size this kernel accepts. Best-effort: each refusal is debug-logged
/// and the ring is left at whatever size it had -- the default, on a kernel
/// without the ioctl (`ENOTTY`, which is also what a regular fixture file
/// answers) or one that denies the request. Must run before `mmap` on a
/// reader that maps the ring and before the first `read(2)` on one that does
/// not: the kernel reallocates the ring on an accepted request.
pub(crate) fn request_ring_ladder(fd: RawFd, path: &Path) {
    for &target in &RING_SIZE_LADDER {
        match set_ring_size(fd, target) {
            Ok(()) => return,
            Err(e) => debug!("MON_IOCT_RING_SIZE({target}) on {}: {e}", path.display()),
        }
    }
}
```

then `stats` and `add_kernel_drops`. Every moved `const`, `type`, `struct`, field, and `fn` that another module uses becomes `pub(crate)` (fields of `MonBinMfetch` and `MonBinStats` included; `mfetch` in `mmap_ring.rs` constructs one and `stats` returns one).

In `src/usbmon/mmap_ring.rs`: delete the moved items and the moved tests; replace the imports of `size_of`, `AtomicU64`/`Ordering` with what still compiles; add

```rust
use super::ring::{
    self, add_kernel_drops, mon_iocx_mfetch, ring_size, stats, IoctlRequest, MonBinMfetch,
    MON_IOCH_MFLUSH,
};
```

and replace the ladder loop inside `read_packets` (the `for &target in &RING_SIZE_LADDER { ... }` block, keeping the comment above it but pointing at `ring::RING_SIZE_LADDER`) with:

```rust
        ring::request_ring_ladder(fd, &self.path);
```

Update the module doc's sentence "The ioctl numbers and struct layouts below were verified ..." to point at `super::ring` instead of "below".

- [ ] **Step 4: Run the tests**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test usbmon:: 2>&1 | grep -E 'test result|FAILED'`
Expected: all ok, including the moved tests under `usbmon::ring::tests` and `request_ring_ladder_is_a_no_op_on_a_regular_file`.

- [ ] **Step 5: Gates and commit**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo fmt && cargo clippy --all-targets -- -D warnings && cargo clippy --all-targets --features capture-fixture -- -D warnings && cargo clippy --all-targets --features ebpf -- -D warnings && cargo test 2>&1 | grep 'test result' && git grep -i -e "$PRIVATE_NAME"`
Expected: clean; the grep prints nothing. (No `dead_code` warnings: every `pub(crate)` item is used by `mmap_ring.rs` or, after Tasks 3-4, by `binary.rs` and `capture/`. If clippy flags `set_ring_size` or `RING_SIZE_LADDER` as unused at this commit, it is because only `request_ring_ladder` uses them and it is `pub(crate)` too; that is not a warning. If a genuinely unused item appears, it was moved by mistake: put it back.)

```bash
git add src/usbmon/ring.rs src/usbmon/mmap_ring.rs src/usbmon/mod.rs
git commit -m "refactor(usbmon): share the ring ioctl surface across readers

Move the usbmon binary-interface ioctl numbers, structs, ring ladder,
size/stats calls, and drop folding out of the mmap reader into
usbmon/ring.rs so the read()-based reader and the fixture capturer can
request the same enlarged ring and report kernel drops the same way.
Pure move plus one helper, request_ring_ladder, that steps the ladder
best-effort.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_011Q8hG1q7GtEWzYuSRDyb1t"
```

---

### Task 3: Ring ladder and kernel drops for the read()-based reader

**Files:**
- Modify: `src/usbmon/binary.rs:1-9` (imports), `:81-119` (`read_packets`), test call sites at `:299`, `:338`, `:411`, `:430`, `:461`
- Modify: `src/usbmon/monitor.rs:534` (dispatch) and the `PacketSource` doc at `:93-101`
- Modify: `src/fixture_replay.rs:197` (binary replay call site)
- Modify: `src/usbmon/mmap_ring.rs:554-560` (doc sentence claiming the read() reader cannot see kernel drops)

**Interfaces:**
- Consumes: `ring::request_ring_ladder`, `ring::stats`, `ring::add_kernel_drops` from Task 2; `POLL_INTERVAL` from `super`.
- Produces: `BinaryReader::read_packets(&self, shutdown: &AtomicBool, kernel_dropped: &AtomicU64, callback: F) -> Result<()>` -- the same shape as `MmapReader::read_packets`.

- [ ] **Step 1: Write the failing test**

Append to `mod tests` in `src/usbmon/binary.rs`:

```rust
    /// A regular file has no usbmon ring: the ladder and stats ioctls fail
    /// with ENOTTY and are ignored, so a fixture-driven read still delivers
    /// every event and reports zero kernel drops through the counter.
    #[test]
    fn fixture_reads_report_zero_kernel_drops() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("usbmon1");
        let mut stream = Vec::new();
        stream.extend(event(b'C', 0x81, 3, 1, 0, 512, &[]));
        stream.extend(event(b'C', 0x81, 3, 1, 0, 256, &[]));
        std::fs::write(&path, &stream).unwrap();

        let reader = BinaryReader::with_path(1, path, false);
        let kernel_dropped = AtomicU64::new(0);
        let mut delivered = 0;
        reader
            .read_packets(&AtomicBool::new(false), &kernel_dropped, |_| {
                delivered += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!(delivered, 2);
        assert_eq!(kernel_dropped.load(Ordering::Relaxed), 0);
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test fixture_reads_report_zero_kernel_drops 2>&1 | tail -5`
Expected: compile error, `read_packets` takes 2 arguments but 3 were supplied.

- [ ] **Step 3: Implement**

Imports at the top of `src/usbmon/binary.rs` become:

```rust
use anyhow::{anyhow, Result};
use log::{debug, error};
use std::fs::File;
use std::io::{ErrorKind, Read};
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use super::parser::{TransferType, UrbType, UsbPacket};
use super::ring;
use super::{open_nonblocking, POLL_INTERVAL};
```

`read_packets` becomes:

```rust
    /// Read loop over the usbmon binary interface. Runs to completion on the
    /// calling thread; callers that want this alongside other work should spawn
    /// it on a dedicated thread.
    ///
    /// `shutdown` is polled whenever the device has nothing to give — between
    /// events, mid-header, and mid-drain — so a caller can stop the loop within
    /// [`POLL_INTERVAL`] and join the thread.
    ///
    /// The kernel ring is enlarged before the first read (see
    /// [`ring::request_ring_ladder`]): the ring is the same per-open buffer
    /// whether it is drained by `read(2)` or by mmap, and on its ~300 KiB
    /// default five isochronous callbacks fill it. `MON_IOCG_STATS` is read at
    /// most once per [`POLL_INTERVAL`] and once more at exit, and each
    /// read-and-clear `dropped` count is summed into `kernel_dropped` via
    /// [`ring::add_kernel_drops`], exactly as the mmap reader does. On a
    /// regular file (fixtures) both ioctls answer `ENOTTY` and are ignored.
    ///
    /// Events of unknown type are skipped (their payload is still drained). A
    /// callback `Err` stops the loop early and still returns `Ok(())`.
    pub fn read_packets<F>(
        &self,
        shutdown: &AtomicBool,
        kernel_dropped: &AtomicU64,
        mut callback: F,
    ) -> Result<()>
    where
        F: FnMut(UsbPacket) -> Result<()>,
    {
        debug!(
            "Starting binary packet capture from {}",
            self.path.display()
        );

        let mut file = open_nonblocking(&self.path)
            .map_err(|e| anyhow!("Failed to open {}: {}", self.path.display(), e))?;
        let fd = file.as_raw_fd();
        ring::request_ring_ladder(fd, &self.path);
        let mut header = [0u8; HEADER_LEN];
        let mut last_stats_at = Instant::now();

        loop {
            if shutdown.load(Ordering::Relaxed) {
                break;
            }

            // Bounded to at most once per `POLL_INTERVAL` regardless of event
            // rate, matching the mmap reader.
            if last_stats_at.elapsed() >= POLL_INTERVAL {
                last_stats_at = Instant::now();
                match ring::stats(fd) {
                    Ok(s) => ring::add_kernel_drops(kernel_dropped, s.dropped),
                    Err(e) => debug!("MON_IOCG_STATS on {}: {}", self.path.display(), e),
                }
            }

            // A partial header followed by a clean EOF (the tail of a truncated
            // capture) ends the loop here without reporting an error.
            if let Fill::Stopped = self.fill(&mut file, &mut header, shutdown) {
                break;
            }

            let parsed = parse_binary_header(&header);
            // Read from the header rather than `parsed`: skipped events carry a
            // payload too, and the next header only starts once it is consumed.
            if let Fill::Stopped = self.drain(&mut file, len_cap(&header), shutdown) {
                break;
            }

            if let Some((packet, _)) = parsed {
                if let Err(e) = callback(packet) {
                    debug!("Packet callback error: {}", e);
                    break;
                }
            }
        }

        // One more read-and-clear at exit, so whatever the kernel lost since
        // the last periodic read is not lost from `kernel_dropped` too.
        match ring::stats(fd) {
            Ok(s) => ring::add_kernel_drops(kernel_dropped, s.dropped),
            Err(e) => debug!("MON_IOCG_STATS on {}: {}", self.path.display(), e),
        }

        Ok(())
    }
```

Update every existing call site to pass a counter:

- `src/usbmon/binary.rs` tests at the five sites: insert `&AtomicU64::new(0),` as the second argument (e.g. `.read_packets(&shutdown, &AtomicU64::new(0), |p| {`). `AtomicU64` is now imported at the top of the file, so `use super::*;` in the test module brings it in.
- `src/usbmon/monitor.rs:534`: `PacketSource::Binary(reader) => reader.read_packets(shutdown, kernel_dropped, send),`
- `src/fixture_replay.rs:197`: `BinaryReader::with_path(0, trace, false).read_packets(&shutdown, &AtomicU64::new(0), |packet| {` -- add `use std::sync::atomic::AtomicU64;` beside the existing `AtomicBool` import in that file.

Docs: in `src/usbmon/monitor.rs` lines 93-101 the `PacketSource` doc names which sources feed `MonitorHandle::kernel_dropped`; make it say the binary and mmap sources both do (text has no counter). In `src/usbmon/mmap_ring.rs` lines 554-560 delete the parenthetical "kernel-side drops the `read()`-based reader has no way to see" and say "as `BinaryReader` does".

- [ ] **Step 4: Run the tests**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test 2>&1 | grep -E 'test result|FAILED' && cargo test --features capture-fixture 2>&1 | grep -E 'test result|FAILED'`
Expected: all ok on both.

- [ ] **Step 5: Gates and commit**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo fmt && cargo clippy --all-targets -- -D warnings && cargo clippy --all-targets --features capture-fixture -- -D warnings && cargo clippy --all-targets --features ebpf -- -D warnings && cargo clippy --all-targets --features integration -- -D warnings && git grep -i -e "$PRIVATE_NAME"`
Expected: clean; the grep prints nothing.

```bash
git add src/usbmon/binary.rs src/usbmon/monitor.rs src/usbmon/mmap_ring.rs src/fixture_replay.rs
git commit -m "fix(usbmon): enlarge the ring and count kernel drops on the read() reader

The read()-based fallback reader drained the kernel's default ~300 KiB
ring and never queried MON_IOCG_STATS, so under load it lost events
silently: five isochronous callbacks fill that ring. It now requests the
same size ladder as the mmap reader before its first read and folds the
kernel's read-and-clear drop count into the shared kernel_dropped counter
at the same cadence, so kdropped: is live on this path too.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_011Q8hG1q7GtEWzYuSRDyb1t"
```

---

### Task 4: Fixture capturer requests the ring, records kernel drops in `meta.toml`

**Files:**
- Modify: `src/capture/mod.rs` (`CapturedTrace`, `run_capture_fixture`, `capture_pair`, `capture_until`, `assemble_bundle`, tests)
- Modify: `src/capture/meta.rs` (`MetaOut`, `build_meta`, tests)
- Modify: `src/fixture_corpus.rs` (add the meta key type check)

**Interfaces:**
- Consumes: `ring::request_ring_ladder`, `ring::stats` (Task 2).
- Produces: `pub struct RawCapture { pub bytes: Vec<u8>, pub kernel_dropped: Option<u64> }`; `fn capture_until(path, deadline, stop) -> io::Result<RawCapture>`; `fn capture_pair<T: Send, F>(...) -> (io::Result<T>, io::Result<T>)`; `CapturedTrace { source, bytes, kernel_dropped: Option<u64> }`; `meta::build_meta(report, sources, stage_id, binary_kernel_dropped: Option<u64>) -> Result<String>`; the `meta.toml` key `binary_kernel_dropped` (u64, present only when the binary source reported a count).

- [ ] **Step 1: Write the failing tests**

In `src/capture/meta.rs` tests, change the existing `build_meta_emits_the_three_keys_the_harness_reads` call to `build_meta(&report, &[FixtureSource::Binary, FixtureSource::Text], Some(7), None)` and add, right after its `stage_id = 7` assertion:

```rust
        assert!(
            !toml_text.contains("binary_kernel_dropped"),
            "no count reported: the key must be absent, not zero: {toml_text}"
        );
```

Add a new test to the same module:

```rust
    /// The capturer writes the kernel's drop count for the binary source so
    /// a bundle declares its own completeness; a bundle captured without the
    /// stats ioctl (old kernel) simply lacks the key.
    #[test]
    fn build_meta_records_the_binary_kernel_drop_count_when_reported() {
        let temp = tempfile::tempdir().unwrap();
        let mgr = DeviceManager::with_sysfs_base(temp.path().to_path_buf());
        let baseline = Baseline::capture(&mgr);
        let report = build_report(
            &mgr,
            &baseline,
            FIXED_ELAPSED,
            "binary",
            0,
            false,
            &FilterSet::default(),
        );
        let toml_text = build_meta(&report, &[FixtureSource::Binary], None, Some(1_621)).unwrap();
        assert!(
            toml_text.contains("binary_kernel_dropped = 1621"),
            "{toml_text}"
        );
        let value: toml::Value = toml::from_str(&toml_text).unwrap();
        assert_eq!(
            value.get("binary_kernel_dropped").and_then(toml::Value::as_integer),
            Some(1_621)
        );
    }
```

In `src/capture/mod.rs` tests, add `kernel_dropped: None,` to every `CapturedTrace { ... }` literal (three tests), and add:

```rust
    /// The binary trace's kernel drop count lands in meta.toml so a bundle
    /// declares its own completeness.
    #[test]
    fn assemble_bundle_records_binary_kernel_drops_in_meta() {
        let temp = tempfile::tempdir().unwrap();
        build_src_sysfs(temp.path());
        let outdir = temp.path().join("bundle");
        let traces = vec![CapturedTrace {
            source: FixtureSource::Binary,
            bytes: one_binary_event(),
            kernel_dropped: Some(7),
        }];
        assemble_bundle(
            &temp.path().join("devices"),
            &outdir,
            &traces,
            &BaselineSource::CaptureFrom(temp.path().join("devices")),
            Some(2),
        )
        .unwrap();
        let meta = std::fs::read_to_string(outdir.join("meta.toml")).unwrap();
        assert!(meta.contains("binary_kernel_dropped = 7"), "{meta}");
    }

    /// `capture_until` on a regular file: the ring ladder and the stats
    /// ioctl both answer ENOTTY, so the bytes are read and no drop count is
    /// reported (`None`), which is also what the debugfs text file yields.
    #[test]
    fn capture_until_reads_a_regular_file_and_reports_no_drop_count() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("usbmon1");
        std::fs::write(&path, one_binary_event()).unwrap();
        let raw = capture_until(&path, Instant::now() + Duration::from_secs(5), &AtomicBool::new(false)).unwrap();
        assert_eq!(raw.bytes, one_binary_event());
        assert_eq!(raw.kernel_dropped, None);
    }
```

Note for the last test: `capture_until` polls until the deadline (a regular file's EOF sleeps 50 ms and retries), so use a one-second deadline, `Instant::now() + Duration::from_secs(1)`, not five. The test then takes about a second, acceptable for one test.

In `src/fixture_corpus.rs` add, beside `every_stage_dir_is_a_wellformed_bundle`:

```rust
/// `binary_kernel_dropped` is optional documentation of a bundle's own
/// completeness; when a bundle declares it, it must be a non-negative
/// integer. Read through `toml::Value` so the key stays out of `Meta`
/// (which nothing else would read; see the plan's ruling).
#[test]
fn declared_binary_kernel_drops_are_non_negative_integers() {
    for bundle in discover_bundles() {
        let text = std::fs::read_to_string(bundle.dir.join("meta.toml")).unwrap();
        let value: toml::Value = toml::from_str(&text).unwrap();
        if let Some(v) = value.get("binary_kernel_dropped") {
            let n = v
                .as_integer()
                .unwrap_or_else(|| panic!("{}: binary_kernel_dropped is not an integer", bundle.dir.display()));
            assert!(n >= 0, "{}: negative drop count {n}", bundle.dir.display());
        }
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test --features capture-fixture capture:: 2>&1 | tail -8`
Expected: compile errors (`kernel_dropped` field unknown; `build_meta` takes 3 arguments; `capture_until` returns `Vec<u8>`).

- [ ] **Step 3: Implement**

`src/capture/mod.rs`:

```rust
use std::io::Read;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context};

use crate::fixture_replay::{replay_fixture, report_to_golden_json, FixtureSource};
use crate::snapshot::Snapshot;
use crate::usbmon::ring;

/// One sanitized trace ready to be written into a bundle.
pub struct CapturedTrace {
    pub source: FixtureSource,
    pub bytes: Vec<u8>,
    /// Kernel-side events lost during the capture, when the interface could
    /// report them (the binary device; `None` for the text file).
    pub kernel_dropped: Option<u64>,
}

/// Raw bytes read from one usbmon interface, plus the kernel's drop count
/// when the interface has one.
pub struct RawCapture {
    pub bytes: Vec<u8>,
    /// `Some(n)`: `MON_IOCG_STATS` worked and the kernel lost `n` events
    /// during this capture. `None`: no such counter on this interface (the
    /// debugfs text file, or a kernel without the ioctl).
    pub kernel_dropped: Option<u64>,
}
```

In `assemble_bundle`, before the `meta::build_meta` call:

```rust
    let binary_kernel_dropped = traces
        .iter()
        .find(|t| t.source == FixtureSource::Binary)
        .and_then(|t| t.kernel_dropped);
```

and call `meta::build_meta(&report, &sources, stage_id, binary_kernel_dropped)?`.

In `run_capture_fixture`, the two match arms become:

```rust
    match bin_result {
        Ok(raw) => {
            if let Some(n) = raw.kernel_dropped.filter(|&n| n > 0) {
                eprintln!(
                    "warning: the kernel dropped {n} events from {} during the capture; \
                     the binary golden still pins the pipeline but understates the traffic, \
                     so lower the rate or widen the window before citing it for accuracy",
                    bin_dev.display()
                );
            }
            let sanitized =
                trace::sanitize_binary_stream(&mut std::io::Cursor::new(raw.bytes))?;
            traces.push(CapturedTrace {
                source: FixtureSource::Binary,
                bytes: sanitized,
                kernel_dropped: raw.kernel_dropped,
            });
        }
        Err(e) => eprintln!(
            "warning: could not capture {} (binary usbmon interface): {e}",
            bin_dev.display()
        ),
    }
    match text_result {
        Ok(raw) => {
            let sanitized = trace::sanitize_text_stream(&mut std::io::BufReader::new(
                std::io::Cursor::new(raw.bytes),
            ))?;
            traces.push(CapturedTrace {
                source: FixtureSource::Text,
                bytes: sanitized.into_bytes(),
                kernel_dropped: raw.kernel_dropped,
            });
        }
        Err(e) => eprintln!(
            "warning: could not capture {} (text usbmon interface): {e}",
            text_dev.display()
        ),
    }
```

`capture_pair` becomes generic over what `read` returns:

```rust
fn capture_pair<T, F>(
    bin_dev: &Path,
    text_dev: &Path,
    deadline: Instant,
    stop: &AtomicBool,
    read: F,
) -> (std::io::Result<T>, std::io::Result<T>)
where
    T: Send,
    F: Fn(&Path, Instant, &AtomicBool) -> std::io::Result<T> + Sync,
{
    std::thread::scope(|scope| {
        let text_handle = scope.spawn(|| read(text_dev, deadline, stop));
        let bin_result = read(bin_dev, deadline, stop);
        let text_result = text_handle.join().expect("text capture thread panicked");
        (bin_result, text_result)
    })
}
```

(The existing `capture_pair_shares_one_deadline...` test's closure returns `io::Result<Vec<u8>>`, so `T = Vec<u8>` there and it compiles unchanged.)

`capture_until` becomes:

```rust
/// Read raw bytes from a usbmon interface until `deadline`, polling a
/// non-blocking open (idle buses return `WouldBlock`). The raw buffer is
/// framed and sanitized afterward, so no framing happens here. Thin live glue.
///
/// The kernel ring is enlarged first (see [`ring::request_ring_ladder`]): on
/// the default ~300 KiB ring the 2026-09-01 spike measured this reader
/// keeping 32% of an isochronous stream's events. The debugfs text file
/// answers `ENOTTY` to both ioctls, which the helper and the final stats
/// read ignore, so one function serves both interfaces. The drop count is
/// read once at the end: the kernel zeroes it on every read, and nothing
/// else reads it during a capture, so that single read is the whole
/// capture's loss.
fn capture_until(path: &Path, deadline: Instant, stop: &AtomicBool) -> std::io::Result<RawCapture> {
    let mut file = crate::usbmon::open_nonblocking(path)?;
    let fd = file.as_raw_fd();
    ring::request_ring_ladder(fd, path);
    let mut buf = Vec::new();
    let mut chunk = [0u8; 65536];
    while Instant::now() < deadline && !stop.load(Ordering::Relaxed) {
        match file.read(&mut chunk) {
            Ok(0) => std::thread::sleep(Duration::from_millis(50)),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50))
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    let kernel_dropped = ring::stats(fd).ok().map(|s| u64::from(s.dropped));
    Ok(RawCapture {
        bytes: buf,
        kernel_dropped,
    })
}
```

`src/capture/meta.rs`: add to `MetaOut`, after `captured_unix`:

```rust
    /// Kernel-side events lost from the binary source during the capture.
    /// Absent when the source could not report one. Documentation of the
    /// bundle's own completeness; never asserted by the harness.
    #[serde(skip_serializing_if = "Option::is_none")]
    binary_kernel_dropped: Option<u64>,
```

and `build_meta` gains the trailing parameter `binary_kernel_dropped: Option<u64>`, passed straight into `MetaOut`. Extend the doc comment: "`binary_kernel_dropped` is the kernel's drop count for the binary source, when it reported one."

- [ ] **Step 4: Run the tests**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test --features capture-fixture 2>&1 | grep -E 'test result|FAILED' && cargo test 2>&1 | grep -E 'test result|FAILED'`
Expected: all ok on both.

- [ ] **Step 5: Gates and commit**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo fmt && cargo clippy --all-targets --features capture-fixture -- -D warnings && cargo clippy --all-targets -- -D warnings && git grep -i -e "$PRIVATE_NAME"`
Expected: clean; the grep prints nothing.

```bash
git add src/capture/mod.rs src/capture/meta.rs src/fixture_corpus.rs
git commit -m "fix(capture): enlarge the ring and record kernel drops in meta.toml

The fixture capturer read /dev/usbmonN on the kernel's default ring and
kept 32% of an isochronous stream's events without saying so (2026-09-01
spike, mainrag and asus). It now requests the shared ring ladder before
reading, reads MON_IOCG_STATS once at the end, warns when the count is
non-zero, and writes binary_kernel_dropped into meta.toml so every bundle
declares its own completeness. The text file answers ENOTTY to both and
records nothing, which is honest: it has no counter.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_011Q8hG1q7GtEWzYuSRDyb1t"
```

---

### Task 5: User-facing text rules and sweep

**Files:**
- Modify: `docs/CONTRIBUTING.md:98-106` (add a subsection after "Rust guidelines")
- Modify: every `src/**/*.rs` string matched by the greps below; `src/usbmon/parser.rs:444` (test expectation)

**Interfaces:** none (wording only; no behavior change; no signature change).

- [ ] **Step 1: Add the rules to CONTRIBUTING**

Insert after the "Rust guidelines" bullet list and before "### Code organization":

```markdown
### User-facing text

Every string a person can see follows one of three shapes. Tests assert
on a few of them, so a rewording is a code change like any other.

- **Error messages** (`anyhow!`, `bail!`, `warn!`, `error!`, and errors
  printed with `eprintln!`): start lowercase unless the first word is a
  proper name, acronym, or identifier (`SEC-1`, `MON_IOCG_STATS`, `USB`,
  `eBPF`); no trailing period; name the subject and the offending value;
  chain causes with `: ` (`could not open /dev/usbmon1: permission
  denied`); say "could not", never "Failed to"; no exclamation marks.
- **Remedies and prompts** (guidance text printed with `println!` or
  `eprintln!`): sentence case, imperative, one action per line, exact
  commands on their own line.
- **Log lines** (`info!`, `debug!`): lowercase, present tense, and name
  the interface and bus they concern (`using usbmon mmap-ring interface
  on bus 3`).
```

- [ ] **Step 2: Find every string that breaks a rule**

Run:
```bash
grep -rnE '(anyhow|bail|warn|error)!\(\s*"[A-Z]' src --include=*.rs
grep -rnE 'eprintln!\(\s*"[A-Z]' src --include=*.rs
grep -rn 'Failed to' src --include=*.rs
grep -rnE '(info|debug)!\(\s*"[A-Z]' src --include=*.rs
grep -rnE '(anyhow|bail|warn|error|eprintln)!\(.*[.!]"' src --include=*.rs
```
Expected: roughly 60-90 lines. Keep the ones whose first word is a proper name, acronym, or identifier (`SEC-1: ...`, `MON_IOCX_MFETCH on ...`, `USB ...`); every other hit is rewritten in Step 3. Prompts printed by `print_setup_instructions`, `print_permission_remedy`, and the load/unload prompts in `src/usbmon/mod.rs` are remedies: leave their sentence case, only remove any trailing exclamation mark.

- [ ] **Step 3: Rewrite**

Apply these transformations, by hand, one string at a time:

| Before | After |
|---|---|
| `"Failed to open {}: {}"` | `"could not open {}: {}"` |
| `"Failed to read from {}: {}"` | `"could not read {}: {}"` |
| `"Failed to run modprobe -r: {}"` | `"could not run modprobe -r: {}"` |
| `"Invalid bus ID: {}"` | `"invalid bus ID: {}"` |
| `"Missing isochronous descriptor count"` | `"missing isochronous descriptor count"` |
| `"Invalid usbmon text line format: empty line"` | `"invalid usbmon text line: empty line"` |
| `"Cannot watch for signals, so a signal will not restore the terminal: {e}"` | `"could not watch for signals, so a signal will not restore the terminal: {e}"` |
| `"Starting usbtop-ng v{}"` (info) | `"starting usbtop-ng v{}"` |
| `"Available USB buses: {:?} (...)"` (info) | `"available USB buses: {:?} (...)"` |
| `"Starting binary packet capture from {}"` (debug) | `"starting binary packet capture from {}"` |

Lowercase the first letter of every other error or log string the greps found; replace every "Failed to" with "could not"; drop trailing periods and exclamation marks. Do not touch strings inside tests except the assertions that check them, and do not touch `--help` text or the man page.

Update `src/usbmon/parser.rs:444` to `assert!(err.to_string().contains("invalid transfer/address token"));`. Run `grep -rn 'contains("' src --include=*.rs | grep -iE 'failed|invalid|missing|cannot'` and fix any other assertion the sweep broke.

- [ ] **Step 4: Run every suite**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test 2>&1 | grep -E 'test result|FAILED'; cargo test --features capture-fixture 2>&1 | grep -E 'test result|FAILED'; cargo test --features ebpf 2>&1 | grep -E 'test result|FAILED'`
Expected: all ok.

- [ ] **Step 5: Gates and commit**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo fmt && cargo clippy --all-targets -- -D warnings && cargo clippy --all-targets --features capture-fixture -- -D warnings && cargo clippy --all-targets --features ebpf -- -D warnings && cargo clippy --all-targets --features integration -- -D warnings && git grep -i -e "$PRIVATE_NAME"`
Expected: clean; the grep prints nothing.

```bash
git add docs/CONTRIBUTING.md src
git commit -m "style: bring error, prompt, and log strings under one rule

CONTRIBUTING now states the three shapes user-facing text takes (errors
lowercase with chained causes and could-not phrasing; remedies in
sentence case and imperative; log lines lowercase naming interface and
bus), and every string in src follows it. Wording only.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_011Q8hG1q7GtEWzYuSRDyb1t"
```

---

### Task 6: Recaptures, the ground-truth bundle, and the live four-source check

Operator task: it runs against the fleet over SSH and the local webcam, so the controller executes it from the dev host (`mainrag`) rather than a fresh subagent, then reviews the bundles like any other task output. Every command below is exact; the only values read off a run are the figures written into `[generator]` notes and commit messages.

**Files:**
- Delete then recreate: `tests/fixtures/hosts/asus-2026-08-31/stage2/`, `tests/fixtures/hosts/pi400-2026-08-31/stage2/`
- Create: `tests/fixtures/hosts/asus-2026-08-31/stage3/`, `tests/fixtures/hosts/asus-2026-08-31/stage4/`, `tests/fixtures/hosts/mainrag-<capture date>/stage1/`, `.../stage2/`
- Modify: `src/fixture_corpus.rs` (add `the_ground_truth_bundle_declares_zero_binary_drops`)

**Interfaces:**
- Consumes: the `capture-fixture` binary built from Tasks 1-5; `bless_named_bundle` is not needed (fresh captures generate their own goldens).
- Produces: the recaptured and new bundles; `binary_kernel_dropped` present in every new `meta.toml`.

Fleet facts (from `docs/TESTING.md` and the 2026-09-01 probes): `asus` has passwordless sudo and `v4l2-ctl`; its internal UVC webcam is `/dev/video0` on bus 3 and runs near 8 fps in room light; the powered hub's FlashDisk is `/dev/sdb` (`1aa6:0201`, 963 MB) two hubs deep on bus 3. `pi58` is the aarch64 build box (rustup at `~/.cargo/bin`); `pi400`'s RTL9210 UAS NVMe is `/dev/sda`. `mainrag`'s Chicony webcam is `/dev/video0`, bus 1, and delivers 10 fps at 640x480 YUYV (614,400 bytes per frame) in room light. Captured directories are root-owned on the remote hosts, so remove them with `sudo`.

- [ ] **Step 1: Build the three local binaries**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
SCRATCH="$(pwd)/.superpowers/wave1-bin"; mkdir -p "$SCRATCH"   # git-ignored under .superpowers/
cargo build --release --features capture-fixture && cp target/release/usbtop-ng "$SCRATCH/usbtop-capture"
cargo build --release && cp target/release/usbtop-ng "$SCRATCH/usbtop-default"
cargo build --release --features ebpf && cp target/release/usbtop-ng "$SCRATCH/usbtop-ebpf"
```

- [ ] **Step 2: asus -- recapture stage2 (iso) with a complete binary side**

```bash
scp "$SCRATCH/usbtop-capture" asus:~/usbtop-ng-capture
scp tests/fixtures/hosts/asus-2026-08-31/stage1/internal-devices.toml asus:/tmp/asus-baseline.toml
ssh asus 'sudo rm -rf ~/fixtures/asus-2026-08-31/stage2 ~/fixtures/asus-2026-08-31/stage3 ~/fixtures/asus-2026-08-31/stage4; sudo mkdir -p ~/fixtures/asus-2026-08-31
sudo ~/usbtop-ng-capture --capture-fixture ~/fixtures/asus-2026-08-31/stage2 --bus 3 --window 40 --baseline /tmp/asus-baseline.toml > /tmp/cap2.log 2>&1 &
sleep 4; v4l2-ctl -d /dev/video0 --stream-mmap --stream-count=200 --stream-to=/dev/null 2>&1 | grep -o "[0-9.]* fps" | tail -1; wait; cat /tmp/cap2.log'
```
Expected: the fps line (about 8), then `captured fixture bundle at ...` with no `warning: the kernel dropped` line.

- [ ] **Step 3: asus -- stage3 (deep chain + dual-personality split, ambient) and stage4 (saturation through the hub)**

```bash
ssh asus 'sudo ~/usbtop-ng-capture --capture-fixture ~/fixtures/asus-2026-08-31/stage3 --window 20 --baseline /tmp/asus-baseline.toml 2>&1 | tail -2'
ssh asus 'lsblk -S -o NAME,VENDOR,MODEL,SIZE | grep -i flashdisk'
ssh asus 'sudo ~/usbtop-ng-capture --capture-fixture ~/fixtures/asus-2026-08-31/stage4 --bus 3 --window 30 --baseline /tmp/asus-baseline.toml > /tmp/cap4.log 2>&1 &
sleep 3; sudo timeout 24 dd if=/dev/sdb of=/dev/null bs=4M iflag=direct 2>&1 | tail -1; wait; cat /tmp/cap4.log'
```
Expected: stage3 captures with no warning; the `lsblk` line names `sdb` (if it does not, substitute the FlashDisk's node in the `dd`); stage4's `dd` prints its byte count and rate (about 30 MB/s) and the capture prints no drop warning.

- [ ] **Step 4: Pull the asus bundles and annotate them**

```bash
rm -rf tests/fixtures/hosts/asus-2026-08-31/stage2
ssh asus 'sudo tar -C ~/fixtures -cf - asus-2026-08-31/stage2 asus-2026-08-31/stage3 asus-2026-08-31/stage4' | tar -C tests/fixtures/hosts -xf -
grep -H 'binary_kernel_dropped\|speed_classes\|controllers' tests/fixtures/hosts/asus-2026-08-31/stage{2,3,4}/meta.toml
python3 - <<'PY'
import json
for src in ("binary","text"):
    r=json.load(open(f"tests/fixtures/hosts/asus-2026-08-31/stage2/golden.{src}.json"))
    tot=sum(e["total_bytes"] for b in r["buses"] for d in b["devices"] for e in d["endpoints"] if b["bus"]==3 and e["transfer_type"]=="iso")
    print(src, tot)
PY
```
Expected: `binary_kernel_dropped = 0` in all three; stage3's `speed_classes` lists `1.5`, `12`, `480`, and `5000`; the stage2 text iso total is within 2% of the binary iso total (the estimator against a now-complete binary side).

Append to `tests/fixtures/hosts/asus-2026-08-31/stage2/meta.toml`:

```toml

[generator]
kind = "uvc-stream"
note = "v4l2-ctl --stream-mmap --stream-count=200 from the internal UVC webcam (13d3:56eb, bus 3 ep1 iso, MJPEG, ~8 fps) inside the window. Recaptured 2026-09 with the enlarged kernel ring after the 2026-08-31 capture was found to have kept 32% of its URBs; binary_kernel_dropped above is the proof. The text golden's iso total is the sampled-descriptor estimate (docs/SCRIPTING.md, 'The estimated field') and sits within 2% of the binary total; the buffer-size figure it replaced was 15x. The powered hub attached on 2026-09-01 is present in sysfs but idle."
```

Append to `stage3/meta.toml`:

```toml

[generator]
kind = "ambient"
note = "Ladder stages 5 and 6 in one capture: root port 1 -> RTS5411 (0bda:5411) -> Terminus FE 2.1 7-port (1a40:0201) -> second FE 2.1 -> Fujitsu 4-port (0430:100e) -> Fujitsu keyboard (0430:00a2); a Fujitsu mouse at 1.5 Mbps, a Bus Pirate and card reader at 12 Mbps, a FlashDisk and an IDS camera at 480 Mbps on the USB2 side, and the same RTS5411's USB3 side on bus 4 carrying two IDS USB 3.0 cameras (1409:3270) at 5 Gbps. Ambient traffic only. The sysfs snapshot carries the port peer links (3-1-port1/peer -> usb4/4-1/4-1:1.0/4-1-port1) the connector-row design investigation reads."
```

Append to `stage4/meta.toml` (fill the two figures from the `dd` line):

```toml

[generator]
kind = "bulk-dd"
note = "dd if=/dev/sdb of=/dev/null bs=4M iflag=direct from the FlashDisk (1aa6:0201, 963 MB, 480 Mbps) two hubs deep on bus 3 (root -> RTS5411 -> FE 2.1), ladder stage 7 behind-the-hub placement; <bytes> bytes at <rate> MB/s inside the window."
```

- [ ] **Step 5: Corpus green, then commit the asus bundles**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test fixture_corpus 2>&1 | grep -E 'test result|FAILED'
git grep -i -e "$PRIVATE_NAME"
git add tests/fixtures/hosts/asus-2026-08-31
git commit -m "test(fixtures): recapture asus stage2 with a complete binary side; add hub stages 3 and 4

The 2026-08-31 stage2 binary golden kept 32% of its URBs on the default
kernel ring. Recaptured with the ring ladder: binary_kernel_dropped = 0,
and the text golden's estimated iso total lands within 2% of it. Stage3
is the powered hub's four-deep chain plus the RTS5411's dual-personality
split (1.5/12/480/5000 Mbps); stage4 is bulk through two hubs.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_011Q8hG1q7GtEWzYuSRDyb1t"
```

- [ ] **Step 6: pi400 -- build on pi58, recapture stage2**

```bash
rsync -a --delete --exclude target --exclude .git --exclude .superpowers ./ pi58:~/usbtop-ng-wave1/
ssh pi58 'cd ~/usbtop-ng-wave1 && ~/.cargo/bin/cargo build --release --features capture-fixture 2>&1 | tail -1'
scp pi58:~/usbtop-ng-wave1/target/release/usbtop-ng "$SCRATCH/usbtop-capture-aarch64"
scp "$SCRATCH/usbtop-capture-aarch64" pi400:~/usbtop-ng-capture
scp tests/fixtures/hosts/pi400-2026-08-31/stage1/internal-devices.toml pi400:/tmp/pi400-baseline.toml
python3 -c "import json; r=json.load(open('tests/fixtures/hosts/pi400-2026-08-31/stage2/golden.binary.json')); print(sorted({b['bus'] for b in r['buses'] for d in b['devices'] if d['total_rx_bytes']>0}))"
```
The last line prints the bus the UAS device sits on; substitute that number for the literal `BUS` in the next block.

```bash
ssh pi400 'lsblk -S -o NAME,VENDOR,MODEL,SIZE | grep -i -E "rtl|nvme|ssk"'
ssh pi400 "sudo rm -rf ~/fixtures/pi400-2026-08-31/stage2; sudo mkdir -p ~/fixtures/pi400-2026-08-31
sudo ~/usbtop-ng-capture --capture-fixture ~/fixtures/pi400-2026-08-31/stage2 --bus BUS --window 30 --baseline /tmp/pi400-baseline.toml > /tmp/cap.log 2>&1 &
sleep 3; sudo timeout 24 dd if=/dev/sda of=/dev/null bs=4M iflag=direct 2>&1 | tail -1; wait; cat /tmp/cap.log"
rm -rf tests/fixtures/hosts/pi400-2026-08-31/stage2
ssh pi400 'sudo tar -C ~/fixtures -cf - pi400-2026-08-31/stage2' | tar -C tests/fixtures/hosts -xf -
grep binary_kernel_dropped tests/fixtures/hosts/pi400-2026-08-31/stage2/meta.toml
```
Expected: `lsblk` names `sda` as the RTL9210 (substitute if not); `dd` reports about 49 MB/s; `binary_kernel_dropped = 0`. Append to its `meta.toml` (fill the two figures):

```toml

[generator]
kind = "bulk-dd"
note = "raw read of the RTL9210 UAS NVMe (dd if=/dev/sda of=/dev/null bs=4M iflag=direct) through the VL805, bulk IN on the two UAS data endpoints; <bytes> bytes at <rate> MB/s inside the window. Recaptured 2026-09 with the enlarged kernel ring (binary_kernel_dropped above). The text side is expected to be short on a bulk stream: the debugfs text queue is a few dozen events deep and has no drop counter, so the text golden pins the pipeline, not the traffic."
```

```bash
export PATH="$HOME/.cargo/bin:$PATH"; cargo test fixture_corpus 2>&1 | grep -E 'test result|FAILED'; git grep -i -e "$PRIVATE_NAME"
git add tests/fixtures/hosts/pi400-2026-08-31
git commit -m "test(fixtures): recapture pi400 stage2 with a complete binary side

Same drop-starved-ring history as asus stage2; recaptured with the ring
ladder and binary_kernel_dropped recorded. The text side is documented
as short on bulk: its kernel queue has no counter.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_011Q8hG1q7GtEWzYuSRDyb1t"
```

- [ ] **Step 7: mainrag -- stage1 (ambient) and stage2 (ground-truth iso, four sources)**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
DATE=$(date +%F); H="tests/fixtures/hosts/mainrag-$DATE"
sudo modprobe usbmon
sudo "$SCRATCH/usbtop-capture" --capture-fixture "$H/stage1" --window 10 && sudo chown -R "$USER" "$H"
sudo "$SCRATCH/usbtop-capture" --capture-fixture "$H/stage2" --bus 1 --window 45 --baseline "$H/stage1/internal-devices.toml" > "$SCRATCH/cap2.log" 2>&1 &
sudo "$SCRATCH/usbtop-default" --once --json --window 45 > "$SCRATCH/mmap.json" 2>/dev/null &
sudo "$SCRATCH/usbtop-ebpf" --once --json --window 45 > "$SCRATCH/ebpf.json" 2>/dev/null &
sleep 4
v4l2-ctl -d /dev/video0 --set-fmt-video=width=640,height=480,pixelformat=YUYV --set-parm=30 --stream-mmap --stream-count=100 --stream-to="$SCRATCH/frames.yuv" 2>&1 | grep -o '[0-9.]* fps' | tail -1
stat -c %s "$SCRATCH/frames.yuv"
wait; cat "$SCRATCH/cap2.log"; sudo chown -R "$USER" "$H"
python3 - "$SCRATCH" "$H" <<'PY'
import json,sys
S,H=sys.argv[1],sys.argv[2]
def iso(p):
    r=json.load(open(p)); return r["source"], sum(e["total_bytes"] for b in r["buses"] for d in b["devices"] for e in d["endpoints"] if b["bus"]==1 and e["transfer_type"]=="iso")
rows=[iso(f"{S}/mmap.json"), iso(f"{S}/ebpf.json"), iso(f"{H}/stage2/golden.binary.json"), iso(f"{H}/stage2/golden.text.json")]
mm=rows[0][1]
for src,tot in rows: print(f"{src:7s} {tot:>12,}  x{tot/mm:.4f} of mmap  x{tot/61440000:.4f} of frame bytes")
PY
grep binary_kernel_dropped "$H/stage2/meta.toml"
```
Expected: `10.00 fps`; `61440000`; no drop warning; `mmap` and `ebpf` equal to within 0.01%; `binary` within 0.1% of mmap; `text` within 2% of mmap; all near 1.016x of frame bytes; `binary_kernel_dropped = 0`.

Append to `$H/stage2/meta.toml` (fill the four totals from the table):

```toml

[generator]
kind = "uvc-ground-truth"
note = "v4l2-ctl -d /dev/video0 --set-fmt-video=width=640,height=480,pixelformat=YUYV --set-parm=30 --stream-mmap --stream-count=100 --stream-to=frames.yuv from the Chicony webcam (04f2:b71a, bus 1 ep1 iso, alt setting 3x1020, 10 fps in room light) inside the window: exactly 100 x 614,400 = 61,440,000 frame bytes. Concurrent captures of the same window: mmap ring <mmap> bytes, eBPF <ebpf> bytes, this bundle's binary golden <binary> bytes, text golden (sampled-descriptor estimate) <text> bytes. mmap and eBPF agree to the byte and sit 1.56% above the frame bytes, which is one 12-byte UVC payload header per packet (79,968 packets); this is the corpus's accuracy anchor and must keep declaring binary_kernel_dropped = 0."
```

Append to `$H/stage1/meta.toml`:

```toml

[generator]
kind = "ambient"
note = "Bare-board ambient capture of the development host (AMD xHCI 0000:06:00.3 and 0000:06:00.4, four buses, kernel 7.0.0-30-generic) for the stage2 baseline."
```

- [ ] **Step 8: Pin the anchor in the corpus and commit**

Add to `src/fixture_corpus.rs` beside `declared_binary_kernel_drops_are_non_negative_integers`:

```rust
/// The mainrag ground-truth iso bundle (see its `[generator]` note) is the
/// corpus's accuracy anchor: captured with the enlarged ring, its binary
/// golden matched a concurrent eBPF capture and the v4l2 frame bytes. It
/// must keep declaring zero kernel drops.
#[test]
fn the_ground_truth_bundle_declares_zero_binary_drops() {
    let bundle = discover_bundles()
        .into_iter()
        .find(|b| {
            b.dir.ends_with("stage2")
                && b.dir
                    .parent()
                    .and_then(|host| host.file_name())
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("mainrag-"))
        })
        .expect("the mainrag ground-truth bundle is committed");
    let text = std::fs::read_to_string(bundle.dir.join("meta.toml")).unwrap();
    let value: toml::Value = toml::from_str(&text).unwrap();
    assert_eq!(
        value
            .get("binary_kernel_dropped")
            .and_then(toml::Value::as_integer),
        Some(0),
        "{}",
        bundle.dir.display()
    );
}
```

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test 2>&1 | grep -E 'test result|FAILED'; cargo test --features capture-fixture 2>&1 | grep -E 'test result|FAILED'
cargo clippy --all-targets -- -D warnings; cargo fmt; git grep -i -e "$PRIVATE_NAME"
git add src/fixture_corpus.rs "tests/fixtures/hosts/mainrag-$DATE"
git commit -m "test(fixtures): mainrag ground-truth iso bundle and the corpus accuracy anchor

Stage2 captures 100 raw YUYV frames (61,440,000 bytes) from the Chicony
webcam with the fixture capturer while a default build (mmap ring) and an
ebpf build observe the same window: mmap and eBPF agree to the byte and
sit one UVC header per packet above the frame bytes, the binary golden
matches them, and the text golden's estimate lands within 2%. The bundle
declares binary_kernel_dropped = 0 and a corpus test keeps it so.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_011Q8hG1q7GtEWzYuSRDyb1t"
```

---

### Task 7: Documentation

**Files:**
- Modify: `docs/ROADMAP.md:184-197` (the "Discovered" section), `:226-256` (engineering follow-ups), `:473-482` (7.3 tracking, iso paragraph)
- Modify: `docs/SCRIPTING.md:207-223` (the `estimated` section)
- Modify: `docs/TESTING.md:13-52` (hosts table row), `:204-256` (capture procedure)
- Modify: `CHANGELOG.md:8-24` (Unreleased)

**Interfaces:** none.

- [ ] **Step 1: ROADMAP**

Replace the whole section headed `### Discovered: the usbmon binary reader undercounts high-bandwidth isochronous transfers` (through the paragraph ending "The eBPF backend already measures these transfers correctly.") with:

```markdown
### Resolved: the binary interface is exact for isochronous transfers

Live-verifying the eBPF backend once surfaced an apparent ~3x undercount
of high-bandwidth isochronous transfers on the binary interface. It was
the ring overflow fixed below, not an accounting bug. A four-source
comparison on 2026-09-01 (`mainrag`, Chicony webcam, 100 raw YUYV
640x480 frames streamed by `v4l2-ctl` inside a 45 s window, exactly
61,440,000 frame bytes):

| Source | Bytes on the iso endpoint | Ratio to frame bytes |
|---|---|---|
| mmap ring (default build) | 62,397,736 | 1.0156 |
| eBPF build | 62,397,736 | 1.0156 |
| Frame bytes + one 12-byte UVC header per packet (79,968 packets) | 62,399,616 | 1.0156 |

mmap and eBPF agree byte for byte and sit exactly one UVC payload header
per packet above the frame data. The kernel source agrees: the binary
header's length field is `urb->actual_length` for callback events
(`drivers/usb/mon/mon_bin.c`, v7.0 lines 512-513 and 581), the same
quantity the eBPF program sums. The committed
`tests/fixtures/hosts/mainrag-*/stage2` bundle is this measurement, and
a corpus test keeps it declaring zero kernel drops.
```

In `## Engineering follow-ups`: delete the bullet beginning "A root-owned /dev/usbmon node reads as absent" and the bullet beginning "Search filtering waits for the next refresh tick" (both shipped; see the Unreleased changelog). Replace the bullet "Error and log strings brought under the documentation style guide." with "Fixed: error, prompt, and log strings follow the three shapes in [CONTRIBUTING](CONTRIBUTING.md#user-facing-text)." Replace the bullet beginning "The usbmon text fallback overcounts isochronous transfers." with:

```markdown
- Fixed: the usbmon text fallback's isochronous counts. The kernel prints
  the whole transfer buffer as an isochronous callback's length, but also
  the first five descriptors with their actual lengths and the full packet
  count (`drivers/usb/mon/mon_text.c`, v7.0 lines 218-247 and 590-606).
  The parser now scales the printed sum by count over printed. Against
  the exact mmap total of the same window: 0.9999x on a sparse MJPEG
  stream (`asus`, where the buffer size read 15.4x) and 1.011x on a
  continuous YUYV stream (`mainrag`, buffer size 3.98x); exact whenever
  five or fewer packets print. It stays flagged `estimated`, because it is
  one. The plain printed-descriptor sum that was floated here measured
  0.16x and is not used.
```

In `## Tracking the kernel: USB/Thunderbolt updates for Linux 7.3`, replace the paragraph beginning "**Re-verify the isochronous accounting.**" with:

```markdown
**Re-run the four-source check.** Mathias Nyman's xHCI series reworks
isoc scheduling and completion ("fix frame id calculation and checks for
isoc URBs", "set frame ID field of isoc TRB when starting an isoch
stream", plus endpoint-recovery-after-disconnect changes, roughly 500
lines of `xhci-ring.c`). That sits upstream of everything usbtop-ng
observes. On a 7.3 kernel, repeat the `mainrag` stage2 procedure in
[TESTING.md](TESTING.md#capturing-hardware-fixtures): mmap, eBPF, the
fixture capturer, and the text estimate against `v4l2-ctl` frame bytes.
The binary path is exact today (see "Resolved" above); confirm it stays
so and that the text estimate stays within 2%.
```

Also in the paragraph "**The eventual final features.**" replace "and to re-characterize the iso undercount under the new xHCI isoc path" with "and to repeat the four-source iso check".

- [ ] **Step 2: SCRIPTING**

Replace the two paragraphs under `## The `estimated` field` after the bullet list with:

```markdown
The text interface prints only the first 5 of an isochronous URB's
descriptors (up to 32 on a webcam) and reports the whole buffer as the
URB's length. usbtop-ng estimates the bytes moved by scaling the printed
descriptors' actual lengths by the URB's full packet count. Measured
against the binary interface on the same window, the estimate landed at
0.9999x on a sparse MJPEG webcam stream and 1.011x on a continuous YUYV
stream, where the buffer size had read 15.4x and 4.0x; it is exact
whenever a URB carries five or fewer packets. It is still a sample-based
estimate, so the report says so. usbtop-ng prefers the binary interface
and only falls back to text when the binary nodes cannot be opened.
Non-isochronous devices are never marked `estimated`, on either
interface.
```

- [ ] **Step 3: TESTING**

Add a row to the `### Test hosts` table (match the existing columns; `mainrag` is the development host, not one of the eight fleet machines, so add one sentence before the table saying it appears because it contributes the ground-truth bundle):

```markdown
| `mainrag` | Development host, AMD Ryzen 9 5900HX (Cezanne), xHCI 0000:06:00.3 and .4 | x86_64 | 7.0.0-30-generic | Linux Mint 22.3 | module | Chicony webcam on bus 1 (the ground-truth iso bundle); BTF present, eBPF runs |
```

In `### Capturing hardware fixtures`, append after item 6:

```markdown
7. The capturer enlarges the kernel ring before reading and records the
   kernel's drop count for the binary source as `binary_kernel_dropped`
   in `meta.toml`, warning on stderr when it is not zero. A bundle with
   drops is still a valid pipeline pin, but do not cite its totals for
   accuracy; lower the rate or widen the window and recapture. The
   debugfs text side has no counter and its kernel queue is a few dozen
   events deep, so on a bulk stream the text trace is expected to be
   short; keep text-inclusive stages at modest event rates.
8. Size the window to the generator's real rate, not its nominal one. A
   webcam in room light may deliver a third of its advertised frame rate:
   the `mainrag` Chicony ran at 10 fps and the `asus` internal webcam at
   8 fps, so 100 or 200 frames need 10 to 26 s and the window must end
   after the stream does. Run the whole stream inside the window or the
   totals describe only part of it.
9. The `mainrag` stage2 bundle is the corpus's accuracy anchor: its
   `[generator]` note records the `v4l2-ctl` command, the exact frame
   bytes, and the concurrent mmap and eBPF totals. Repeat that procedure
   on any kernel that changes the xHCI isochronous path.
```

- [ ] **Step 4: CHANGELOG**

Under `## [Unreleased]` add to `### Fixed`:

```markdown
- Isochronous rates on the usbmon text fallback are now a sample-based estimate within about 1% of the binary interface, instead of the buffer size, which overcounted by 4x to 15x on webcams. The report still marks them `estimated`. See [docs/SCRIPTING.md](docs/SCRIPTING.md#the-estimated-field).
- The read()-based binary fallback reader now requests the same enlarged kernel ring as the mmap reader and feeds the `kdropped:` counter; on the default ring it lost most of a fast stream silently.
- `--capture-fixture` no longer captures on the default kernel ring (it kept about a third of an isochronous stream's events) and records the kernel's drop count as `binary_kernel_dropped` in each bundle's `meta.toml`.
```

Add to `### Added` (after the harness bullet):

```markdown
- Fixture bundles for the development host's ground-truth isochronous stream (`mainrag`), and for the powered-hub deep chain and saturation-through-hub stages on `asus`.
```

Add a `### Changed` bullet:

```markdown
- The `asus` and `pi400` stage2 bundles were recaptured with the enlarged ring; their earlier binary goldens had kept about a third of their URBs.
```

- [ ] **Step 5: Verify and commit**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test 2>&1 | grep -E 'test result|FAILED'
git grep -i -e "$PRIVATE_NAME"
git grep -n 'undercounts high-bandwidth\|3.6x' docs README.md CHANGELOG.md
git add docs/ROADMAP.md docs/SCRIPTING.md docs/TESTING.md CHANGELOG.md
git commit -m "docs: close the iso accounting entries with the measurements

Roadmap: the binary undercount section becomes a Resolved section with
the four-source table; the text overcount follow-up is marked fixed
with the estimator figures; the two already-shipped follow-ups are
pruned; the 7.3 tracking asks for a re-run of the check. SCRIPTING
describes the estimate. TESTING gains the drop counter, window sizing,
and the accuracy-anchor procedure. CHANGELOG records all of it.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_011Q8hG1q7GtEWzYuSRDyb1t"
```
Expected before the commit: tests ok; the first grep prints nothing; the second grep prints nothing (no stale "undercounts" claim or 3.6x figure survives).
