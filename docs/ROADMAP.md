# Roadmap

Ideas and follow-up work for usbtop-ng. Nothing here is committed work or a
schedule. Items move to [CHANGELOG.md](../CHANGELOG.md) when they ship.

## Feature ideas

- Document file export. `--batch --json > capture.ndjson` already covers
  it; a native `--output PATH` only earns its place if users ask for
  append or rotation semantics.
- One row per physical connector, using the sysfs port `peer` links. Today
  the USB2 side and the USB3 side of one connector list as sibling buses.
  A connector pairs individual hub ports, not whole buses, and `peer`
  links can be absent or wrong, so this needs a design investigation and
  real dock fixtures before code.
- Plugin system for custom monitors. Deferred: the versioned NDJSON
  stream is already the right boundary for external analysis tools.
- Monitoring of remote systems over the network. `ssh -t host sudo
  usbtop-ng` and `ssh host sudo usbtop-ng --batch --json` already cover
  the common cases; document those before building a network service.

## Cable and port diagnostics

A diagnostic layer over the Type-C sysfs classes, evaluated 2026-08-22
against a cable-diagnostic engine study. usbtop-ng's edge: it measures
delivered throughput, so cable claims can be corroborated by observed
delivery instead of negotiation state alone.

Worth building, in dependency order:

- Cable identity from `/sys/class/typec/portX-cable/identity`: decode the
  e-marker VDOs in userspace (passive or active, rated speed, rated
  current, spec-anomaly flags such as a passive header with active-only
  bits). Stable sysfs, no root, no new dependencies. Availability is
  firmware-bound: kernel PD stacks that run the state machine expose the
  VDOs, firmware-managed ports often expose nothing, and the tool must
  say which case it found rather than guess.
- Advertised power capabilities from `/sys/class/usb_power_delivery` and
  DP-Alt pin assignment from the typec class, as informational rows.
- Thunderbolt fabric link speed and lanes from `/sys/bus/thunderbolt`,
  enriching the controller grouping.
- The distinctive verdict: claim versus measured delivery. A cable rated
  10 Gbps carrying a sustained measured rate near a slower tier's ceiling
  is a finding only a traffic monitor can make. The exact-speed model it
  needs shipped in 2026-08. Verdict doctrine: measured beats claimed,
  exonerate confidently, convict only a uniquely limiting party, and say
  nothing where attribution is ambiguous.

Parked until Linux exposes them, not dropped. No stable kernel interface
carries these today, and each becomes buildable the moment a mainline ABI
or capable hardware lands:

- Per-port power-out metering (delivered volts and amps per connector).
- The system DC-in rail, which also unlocks a whole-path resistance
  estimate from a V over I regression.
- The negotiated PD contract. Advertised capabilities are readable now,
  the struck contract is not.
- Connector fault counters (overcurrent trips, replug storms).
- Liquid and corrosion detection, defined in the connector spec and
  implemented in PD controller silicon, with no kernel attribute yet.

Revisit this list on kernel upgrades. Display diagnostics stay out by
charter, not by gap.

Prerequisite, now met by the test fleet: the buildable tier needs a host
that exposes the typec, power-delivery, and thunderbolt classes, and the
2026-08-30 fleet probe found them on the two x86 hosts -- `asus` (typec +
power-delivery + thunderbolt) and `judge` (typec + power-delivery, no
thunderbolt). This is the seam where the fleet work feeds this feature:
the [port capability matrix](TESTING.md#port-capability-matrix) names the
host and the free USB-C port to build and test each row against, and the
feature reads that host's sysfs. `judge`'s USB-C/PD port is free today;
both of `asus`'s show a connected partner, so free one before testing
there. The development host itself still exposes none of these classes,
so this work is done on the fleet, not locally.

## eBPF backend

A third packet source built on kprobes, prototyped on 2026-08-19 with
bpftrace 0.20.2 on kernel 7.0.0-29-generic. Two probes cover all USB
traffic: `usb_submit_urb` and `__usb_hcd_giveback_urb`. The kernel BTF at
`/sys/kernel/btf/vmlinux` resolved every struct field with no header files.
In-kernel maps aggregated bytes keyed by bus, device, endpoint, direction,
and transfer type. Each consumer polls the maps on its own cadence: the
TUI tick and a headless window differ.

Prototype numbers, from a 6-second camera stream (Chicony IR camera, bus 1
device 4, 61 frames of 614,400 bytes each):

- bpftrace summed `urb->actual_length` at giveback: 39,309,824 bytes on the
  isochronous endpoint. That is the pixel data plus 5% UVC packet headers.
- The usbmon binary interface reported 39,309,824 bytes for the same window.
  The two sources match to the byte.
- The usbmon text interface reported 154,028,160 bytes in a matched run, a
  3.6x overcount. Its length column holds the 97,920-byte buffer size for
  every isochronous completion. See the engineering follow-up below.

Design notes:

- Startup probes a backend chain: eBPF, then usbmon binary, then usbmon
  text. Any BTF, attach, or lockdown failure degrades to the next source.
  Kprobe attach points are not a stable kernel interface, so eBPF ships
  as an explicit opt-in first, not the automatic default.
- Aggregated maps do not fit the per-packet `PacketSource` contract, so
  the backend needs a delta seam. That seam shipped on 2026-08-29: the
  manager accounts backend-neutral `TrafficDelta { bus, dev, endpoint,
  dir, transfer type, bytes }` values through `apply_delta`, and the
  usbmon packet path is a thin adapter over it. The eBPF source itself
  shipped the same day, behind the opt-in `ebpf` cargo feature: it reads
  its kernel map (monotonic counts diffed against per-key snapshots,
  never read-and-clear) and feeds `apply_delta` directly. The first
  backend is throughput-only, with per-process attribution deferred to
  separate research below.
- Headline feature: per-process attribution, as separate research first.
  `usb_submit_urb` often has task context, but not always, and usbmon
  never records it. The prototype also showed the trap:
  drivers resubmit periodic URBs from interrupt context, so the camera
  stream logged 41 submissions under ffmpeg and 1,538 under idle-task and
  kworker contexts. Attribution needs an owner map written at stream start,
  not the submitter name.
- Costs, paid only when the opt-in `ebpf` feature is built, never by the
  default build: `libbpf-rs` and `libbpf-cargo` dependencies, a clang BPF
  toolchain, libbpf-dev headers, a Rust ≥ 1.82 floor for the feature
  (documented, not enforced on the default Rust 1.88 build), kernel BTF at
  runtime, and CI that builds and hermetic-tests the feature but cannot
  attach kprobes unprivileged.
- Fixed: the kernel hash map that aggregates bytes is bounded at 4096
  keys -- ample for realistic device and endpoint counts. A map-full
  insert was a silent loss; it is now surfaced. The BPF program counts the
  dropped URBs in a single-slot counter map, and the poller folds that
  cumulative total into the same `kernel_dropped` counter the usbmon
  backends feed, shown as `kdropped:` in the header and
  `kernel_dropped_packets` in JSON. The map is not enlarged or evicted --
  the loss stays bounded, but it is visible now rather than silent.

### The mmap middle path: shipped

The cheaper middle path floated above shipped on 2026-08-28. `MmapReader`
(`src/usbmon/mmap_ring.rs`) reads the usbmon binary interface through its
mmap ring and `MON_IOCX_MFETCH`, copying only the 48-byte event header per
packet and never the captured payload. `start_monitoring` prefers it on
any kernel that supports it, falling back per bus to the read()-based
binary reader, then the debugfs text reader, logging which interface it
chose.

Measured on this host over one ~4-second camera stream, 2,500 events: the
read()-based reader drained and discarded 81,942,719 bytes (81.9 MB) of
payload it never used; the mmap reader copied 0 payload bytes, headers
only. Both readers selected correctly at startup and attributed the
traffic identically.

That changes the eBPF go/no-go. The mmap path already captured the
throughput win the middle path above targeted, with no new dependencies.
eBPF's one remaining unique advantage over usbmon (mmap included) is
per-process attribution: `usb_submit_urb` has task context, at least some of
the time (see the trap above), and usbmon never records one at all. So eBPF
is no longer gated on performance; it is gated on whether that attribution
is worth its cost — the libbpf-rs and libbpf-sys dependencies, the clang
BPF toolchain, the kernel BTF requirement, and the unprivileged-CI gap
listed above. That cost is pre-approved for a later wave, behind an
optional cargo feature, if and when per-process attribution earns its
place.

### Shipped: the throughput-only backend

The eBPF backend above shipped as an opt-in `ebpf` cargo feature on
2026-08-29, ahead of the attribution question the paragraph above leaves
open: throughput alone earned its place once the controller live-verified
it against physical ground truth on a high-bandwidth isochronous webcam
(bus 1). The eBPF backend's measured rate, 0.935 MB/s, tracked the
camera's own captured MJPEG payload rate, 0.825 MB/s, with the 1.13x
difference accounted for by the expected UVC/iso packet-header overhead.
Per-process attribution stays the deferred future work described above;
this backend is throughput-only. See
[INSTALL.md](INSTALL.md#building-the-ebpf-backend) for the build and
runtime requirements.

One known limitation: the BPF program hand-writes an x86-64 `pt_regs` for
the kprobe entry context, so `--features ebpf` builds on x86-64 only today
(it fails loudly on other architectures rather than miscounting). A
per-architecture `pt_regs` is the follow-up that would let it build on the
ARM boards below; usbmon capture there needs none of this and works now.

### Discovered: the usbmon binary reader undercounts high-bandwidth isochronous transfers

Live-verifying the eBPF backend against ground truth surfaced a separate,
pre-existing issue, distinct from the text-interface overcount tracked in
[Engineering follow-ups](#engineering-follow-ups) below: the usbmon binary
interface -- usbtop-ng's normally preferred, exact-byte source --
undercounts high-bandwidth isochronous transfers (endpoints with a 2x or
3x `wMaxPacketSize` transaction multiplier) by roughly 3x, and drops
packets outright under sustained isochronous load. `src/usbmon/binary.rs`
is unchanged by this wave (byte-identical against `main`), so the bug
predates it. Candidate future fix wave: compare the event header's
`length` field against the sum of the per-iso-packet descriptors, and
characterize the drop behavior under load. The eBPF backend already
measures these transfers correctly.

### Fixed: usbmon dropped badly under high throughput; the ring is now enlarged

The iso undercount above turned out to be one symptom of a broader,
pre-existing throughput problem. An exact-byte bulk test -- a read-only
`dd` of 4 GiB from a USB3 SSD (5 Gbps link, 336 MB/s), with both backends
observing the same transfer over a window that contained it -- measured:

| Source | Captured | vs. the exact 4 GiB | Kernel drops |
|---|---|---|---|
| eBPF | 4,291,952,544 | 0.9993x (byte-exact) | 0 |
| usbmon (mmap ring), before | 537,421,984 | 0.125x (-87.5%) | 40,046 |
| usbmon (mmap ring), after | 4,293,525,472 | 0.9997x (byte-exact) | 0 |

usbmon's accuracy problem was not iso-specific: at USB3 rates it dropped
the great majority of any high-throughput stream, bulk included. Root
cause: usbtop-ng ran the mmap ring reader on the kernel's *default* ~300 KiB
usbmon ring and never enlarged it -- it read `MON_IOCQ_RING_SIZE` but never
called the `MON_IOCT_RING_SIZE` setter, so the small default ring overflowed
continuously and the kernel dropped whole events. Fixed by requesting a large
ring (a 64 MiB..8 MiB step-down ladder, since the kernel rejects an over-max
request outright rather than clamping) before `mmap`; best-effort, so a
kernel without the ioctl falls back to the default ring. Live-verified to
drop zero packets across a 4 GiB read at 5 Gbps and a 12 GiB read at 10 Gbps,
matching the eBPF capture and the exact `dd` count (numbers above). The eBPF
backend aggregates in-kernel and was immune throughout -- the strongest
validation yet of that backend for high-throughput monitoring.

## Engineering follow-ups

These came out of code review. Each is small and none blocks a release.

- Error and log strings brought under the documentation style guide.
- A root-owned /dev/usbmon node reads as absent for a plain user, so the
  remedy says no node was found. Distinguishing permission-denied from
  not-found in the probe would give the sharper sudo remedy.
- Fixed: the report `source` field mislabeled the mmap ring reader as
  `"binary"`. `capture_source_label` (src/headless/mod.rs) knew only
  `ebpf`/`text`/`binary`, so an active mmap reader printed `"binary"`. The
  monitor now raises an `mmap_active` flag (bundled with `text_active` into
  `SourceFlags`) for the interface actually running, and the label reads
  `"mmap"`. Verified live alongside the ring-size fix above.
- Search filtering waits for the next refresh tick, up to 1 second at the
  default rate. Pulling the tick forward on a search keystroke would make
  filter-as-you-type feel immediate.
- The usbmon text fallback overcounts isochronous transfers. Its length
  column holds the buffer size, not the bytes moved. On a camera stream the
  overcount was 3.6x. The text format prints 5 of 32 descriptors per URB, so
  no exact count exists in text mode. The binary interface reports true
  bytes for ordinary transfers and is already the preferred source -- but
  not for high-bandwidth isochronous ones; see "Discovered: the usbmon
  binary reader undercounts high-bandwidth isochronous transfers" in the
  eBPF backend section above, a separate bug. The UI and the JSON output
  now mark affected rates as estimates. The remaining idea: sum the printed
  descriptors to tighten the estimate. Caveat before building it: only up
  to 5 descriptors print, so a sum can under-estimate as badly as the
  length over-estimates, and the kernel's usbmon document claims callback
  lengths are actual values, which the measurement above contradicts on
  this kernel. Commit a raw trace alongside any fix.

## ARM board support

In-depth research and testing of usbtop-ng on small ARM hosts: Raspberry
Pi Zero, Pi 4, Pi 400, and Pi 5, plus the Radxa ROCK 5C and the SOPHGO
Fogwise AirBox. usbmon is architecture-independent, so the questions are
builds, vendor kernels, and controller behavior, not core capture logic.
(The optional `ebpf` feature is the one exception: its BPF program has an
x86-64-only `pt_regs` today and needs a per-architecture one before it
builds on these boards -- see "Bringing the eBPF backend to ARM" below.
The default usbmon build carries no such restriction.)

The fleet is now provisioned and probed (2026-08-30). All six ARM
targets exist as named, SSH-reachable hosts -- `rattler` (Pi 4), `pi400`
(Pi 400), `pi58` (Pi 5), `enviro` (Pi Zero W, armv6l), `rock-32`
(ROCK 5C), and `airbox` (AirBox) -- alongside two x86 hosts, `asus`
(Intel Tiger Lake, with Thunderbolt 4) and `judge` (AMD Cezanne, USB 3.1
Gen 2). Their kernels, controllers, and confirmed usbmon and eBPF status
are in the [Test hosts](TESTING.md#test-hosts) table. Three findings
already shape the work: `rock-32` ships usbmon built in with
`/dev/usbmon0..8` live, so the binary-only vendor-kernel path is testable
today; `airbox`'s 5.4 vendor kernel has no usbmon module at all
(`modprobe` fails) and no BTF, so it cannot capture with either backend
until its kernel gains `CONFIG_USB_MON`; and BTF
(`/sys/kernel/btf/vmlinux`) is absent on every ARM board, which blocks
the CO-RE eBPF path independently of the x86-64-only `pt_regs` above --
so the two x86 hosts are the only ones that can run the `ebpf` feature
today.

Research first:

- Build targets. 64-bit boards (Pi 4, Pi 400, Pi 5, ROCK 5C, AirBox on a
  64-bit OS) need `aarch64-unknown-linux-gnu`. The original Pi Zero is
  ARMv6 and needs `arm-unknown-linux-gnueabihf` on a 32-bit OS. Verify
  the MSRV toolchain exists for both, then extend the release workflow
  with an aarch64 artifact (cross-compile or an ARM runner).
- Vendor kernels. Confirm each ships usbmon (module or built-in), whether
  debugfs mounts by default, and whether `/dev/usbmon<N>` nodes appear.
  Built-in detection and debugfs-free startup shipped in 2026-08, so a
  binary-only vendor kernel is a supported shape to test.
- Controller matrix. Each board exercises a different host stack: the
  Zero's single OTG controller, the Pi 4 and Pi 400's PCIe xHCI for the
  USB3 ports plus the OTG port, the Pi 5's RP1 southbridge, the ROCK 5C's
  Rockchip RK3588-class OTG plus xHCI mix, and the AirBox's SOPHGO
  BM1684X. Verify capture, bus numbering, topology, and speeds on each.
- Small-core behavior. The Zero is a single slow core: measure the reader
  thread, channel bound, and TUI refresh there, and the TUI over SSH and
  a serial console.

Then record every board in the tested-hardware log below, with kernel,
OS image, controller, and capture backend per entry. The procedure,
device inventory, and pass criteria live in [TESTING.md](TESTING.md).

### Bringing the eBPF backend to ARM

The `ebpf` feature is x86-64-only today; the 2026-08-30 fleet probe (see
[Test hosts](TESTING.md#test-hosts)) pinned down exactly why and what it
would take. Two independent blockers, both solvable on arm64.

First, the x86-only `pt_regs`. The program's argument handling is already
portable -- `src/bpf/usbrate.bpf.c` uses libbpf's `BPF_KPROBE` macro and
`BPF_CORE_READ` throughout. The one arch-specific piece is the
hand-written `struct pt_regs` in `src/bpf/vmlinux.h` (the x86-64 register
layout: `di`/`si`/`dx`/...), which `BPF_KPROBE` reads argument registers
from. Adding arm64 means making that struct conditional -- an
`#if defined(__TARGET_ARCH_arm64)` block with the arm64 layout
(`unsigned long regs[31]; unsigned long sp, pc, pstate;`) -- and passing
`-D__TARGET_ARCH_arm64` when `build.rs` compiles the BPF object for an ARM
target, so `bpf_tracing.h`'s `PT_REGS_PARM1` resolves to `regs[0]`. The C
in `usbrate.bpf.c` does not change; only the header and the build.

Second, missing BTF. CO-RE needs `/sys/kernel/btf/vmlinux`, and the probe
found it absent on every ARM board (present only on the x86 hosts `asus`
and `judge`). `CONFIG_BPF_SYSCALL`, `CONFIG_KPROBES`, and the
`__usb_hcd_giveback_urb` symbol are all present on the ARM boards, so BTF
is the sole load-time gap. Three ways to supply it, none touching the
program: an external BTF via libbpf's `btf_custom_path`, generated with
`pahole -J vmlinux` from the board kernel's DWARF debuginfo (or pulled
from BTFHub where it carries the kernel -- it will not have the `rpt`,
Rockchip, or SOPHGO vendor kernels); a kernel rebuilt with
`CONFIG_DEBUG_INFO_BTF=y`; or a per-kernel non-CO-RE build, which is
brittle and not worth it.

Per host: `rock-32` (RK3588S2, arm64, 6.1) is the natural first target --
arm64 with kprobe and BPF confirmed, so it needs only the arm64 `pt_regs`
and an external BTF. `pi58`, `rattler`, and `pi400` (arm64, rpt kernels)
follow the same recipe, sourcing BTF from a matching debuginfo build or a
custom kernel. `enviro` (armv6l, Pi Zero) is out of scope: 32-bit ARM
eBPF and CO-RE tooling are immature, and usbmon is the right backend
there. `airbox` (BM1684x, 5.4 vendor) is the worst case -- no usbmon and
no BTF on an old kernel; it needs a rebuild with both `CONFIG_USB_MON`
and `CONFIG_DEBUG_INFO_BTF` before either backend runs.

Worth doing, not urgent: usbmon now captures byte-exact at high
throughput on every ARM board except `airbox`, so the eBPF backend's ARM
upside is mainly the isochronous-undercount case and future per-process
attribution. The concrete next step is a spike on `rock-32` -- add the
conditional arm64 `pt_regs`, compile with `-D__TARGET_ARCH_arm64`,
generate BTF with `pahole -J`, and attempt a verify-load and a short
capture.

## Testing follow-ups

- Thunderbolt and USB4 hardware validation. usbtop-ng observes USB URBs
  behind such hosts and docks, never PCIe or fabric traffic. A useful
  matrix covers dock controllers, hub topology, hotplug, suspend and
  resume, and peer links. The x86 `asus` host (Intel Tiger Lake-LP,
  Thunderbolt 4 plus a 10 Gbps USB bus) is now the concrete platform for
  this matrix; see the [Test hosts](TESTING.md#test-hosts) table.
- Generate a log with a normalized schema of hardware devices that have
  been tested. Ideas for this would include the date the test was performed
  along with the conditions (kernel version, relevant drivers, attached port
  chipsets, hubs). Then come up with an option to gather the data which
  could then be submitted as a contribution. But the methodology of this is
  open at this time.


## USB troubleshooting and performance notes

General Linux USB knowledge worth capturing, gathered from working with a
USB3 Vision / UVC machine-vision camera (an Imaging Source 37UX273-ML).
The kernel-buffer point below is directly relevant to usbtop-ng: a
too-small usbfs buffer is exactly what makes a fast device drop frames,
which is what the `kdropped:` counter now surfaces.

### The usbfs buffer limit (the one that bites)

The kernel caps the usbfs memory a userspace capture can pin at 16 MB
across all USB devices by default. That is too small for a USB3 Vision
camera at full rate, and the symptom is dropped frames. Raise it:

```bash
echo 1000 | sudo tee /sys/module/usbcore/parameters/usbfs_memory_mb   # test now
```

Make it permanent on the kernel command line with
`usbcore.usbfs_memory_mb=1000` (`/boot/firmware/cmdline.txt` on a
Raspberry Pi, `GRUB_CMDLINE_LINUX_DEFAULT` on x86). When usbtop-ng shows
a rising `kdropped:` under a high-rate device, this limit is the first
thing to check.

### UVC versus USB3 Vision, and what you install

A UVC-compliant camera is claimed by the in-kernel `uvcvideo` module with
zero installation: it appears as `/dev/video0` on any current 64-bit
Raspberry Pi OS or x86 distro, and `cv2.VideoCapture(0, cv2.CAP_V4L2)`
works immediately. There is no out-of-tree module to build and no DKMS
package for a modern UVC camera. What you install is userspace, and only
because a feature like GenICam trigger mode is a camera-side property
plain UVC cannot reach.

Two userspace stacks for such a camera, pick one:

- **tiscamera** (the vendor Linux SDK): a GStreamer source plus a
  properties tool and a capture GUI. Prebuilt packages for amd64 and
  arm64. 32-bit ARM (armhf) is legacy and was dropped as of Ubuntu 24.04,
  so use a 64-bit Raspberry Pi OS. The vendor has announced end-of-life
  for tiscamera on 2029-04-01.
- **IC4 SDK**: a GenTL producer (a `.deb`) plus Python bindings
  (`pip install imagingcontrol4`), on Ubuntu 22.04+ x64 and ARM64. Its
  GenTL producer talks to the camera over libusb rather than `uvcvideo`,
  so the same Python code runs unchanged on a Pi and on x86.

### udev rules for non-root access

Both packages ship udev rules so the device is reachable without root.
Confirm the rules actually landed after install, or you will be forced to
run as root — the same non-root-access problem usbtop-ng itself documents
for the usbmon interfaces.

### Raspberry Pi bandwidth reality

A 1440x1080 Mono8 stream at full rate is about 370 MB/s (roughly 3 Gbps).
How that lands per board, useful context for the ARM board testing above:

- **Pi 5**: USB 3.0 through the RP1 southbridge, the best option, should
  sustain near full rate.
- **Pi 4 / 400**: USB3 sits behind a VL805 on a single PCIe Gen2 lane
  (about 4 Gbps shared across all ports). Workable, but keep other USB
  traffic off it.
- **Pi 3 / Zero**: USB 2.0 only, about 40 MB/s realistic (roughly 25 fps
  at that resolution). Fine for triggered single-shot capture, not for a
  camera's full high-frame-rate stream.

## Tracking the kernel: USB/Thunderbolt updates for Linux 7.3

Greg Kroah-Hartman's `[GIT PULL]` for `usb-7.3-rc1` (25 Aug 2026,
against `7.2-rc7`, tag `usb-7.3-rc1`) opens the 7.3 merge window for the
USB and Thunderbolt drivers: 142 files, +3,311/-1,780. usbtop-ng should
track it through the -rc cycle to the 7.3 **final** release and re-verify
against it, because the capture surfaces usbtop-ng depends on live in
this tree. The near-term conclusion is reassuring -- nothing usbtop-ng
reads is broken by this pull -- but two areas want a re-run of the
verification suite once 7.3 ships.

**Capture surfaces are unchanged, so there is nothing to port.** The pull
touches `drivers/usb/core/{config,devices,devio,driver,hub}.c` but not
`drivers/usb/core/hcd.c`, so the eBPF backend's kprobe target
`__usb_hcd_giveback_urb` keeps its signature and the CO-RE field offsets
stay valid on 7.3. No file under `drivers/usb/mon/` changes either (only
the zh_CN translation of `usbmon.rst`), so the usbmon text, binary, and
mmap interfaces -- and the `MON_IOCT_RING_SIZE` ring-enlarge fix above --
are untouched. Confirm both hold when 7.3-final lands; a break in a later
-rc is the signal to re-check the FFI against source, per the standing
practice of verifying kernel semantics against the kernel, not a device.

**Re-verify the isochronous accounting.** Mathias Nyman's xHCI series
reworks isoc scheduling and completion -- "fix frame id calculation and
checks for isoc URBs", "set frame ID field of isoc TRB when starting an
isoch stream", plus several endpoint-recovery-after-disconnect changes,
roughly 500 lines of `xhci-ring.c`. That sits *upstream* of everything
usbtop-ng observes, so it may shift the high-bandwidth isochronous
undercount and drop behavior documented above (see "Discovered: the
usbmon binary reader undercounts high-bandwidth isochronous transfers").
Re-run the iso characterization on a 7.3 kernel before attributing that
bug purely to usbmon.

**Robustness context, not a feature to add.** Several fixes harden the
character-device teardown paths that neighbor our capture: a usbfs
use-after-free on release, devio validation before buffer allocation, and
a probe-versus-dynamic-ID use-after-free. usbtop-ng hits the same class of
races on hotplug; there is nothing to incorporate, but it is useful
ground truth that 7.3 tightens device-teardown handling underneath a
monitor.

**Ecosystem watch, out of current scope.** Not incorporable today, but
worth following: the in-kernel Rust USB abstractions (`rust/kernel/usb.rs`)
keep maturing, relevant only if usbtop-ng ever grows a kernel-side
companion; the Thunderbolt `stream` interface and its
`configfs-thunderbolt_stream` ABI are fabric-level, below the USB URB
layer usbtop-ng measures, so they stay out of scope (see
[Testing follow-ups](#testing-follow-ups)); and the large typec / PD /
UCSI churn is power-delivery negotiation, not data bandwidth -- a possible
future surface for [Cable and port diagnostics](#cable-and-port-diagnostics),
never for the bandwidth path.

**The eventual final features.** When 7.3 releases, re-run the throughput
and iso verification suite on it -- the exact-`dd` cross-checks at 5 and
10 Gbps and the eBPF-versus-usbmon comparison -- to confirm the ring-size
fix and the eBPF backend still measure byte-exact, and to re-characterize
the iso undercount under the new xHCI isoc path. Record the kernel in the
tested-hardware log.

## Notes
