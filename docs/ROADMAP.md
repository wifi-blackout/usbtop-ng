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

Prerequisite: none of the typec, power-delivery, or thunderbolt classes
exist on the current development host, so even the buildable tier needs
hardware that exposes them before any of it can be built honestly.

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
  the backend needs a delta seam. That seam SHIPPED on 2026-08-29: the
  manager accounts backend-neutral `TrafficDelta { bus, dev, endpoint,
  dir, transfer type, bytes }` values through `apply_delta`, and the
  usbmon packet path is a thin adapter over it. The eBPF source will read
  its kernel map (monotonic per-CPU counters diffed against snapshots,
  never read-and-clear) and feed `apply_delta` directly. Building it needs
  clang installed for the CO-RE object; the first backend is throughput-
  only, with per-process attribution deferred to separate research below.
- Headline feature: per-process attribution, as separate research first.
  `usb_submit_urb` often has task context, but not always, and usbmon
  never records it. The prototype also showed the trap:
  drivers resubmit periodic URBs from interrupt context, so the camera
  stream logged 41 submissions under ffmpeg and 1,538 under idle-task and
  kworker contexts. Attribution needs an owner map written at stream start,
  not the submitter name.
- Costs: libbpf-rs and libbpf-sys dependencies (libelf, zlib), a clang BPF
  toolchain at build time, kernel BTF at runtime, and CI that cannot attach
  kprobes unprivileged.

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

## Engineering follow-ups

These came out of code review. Each is small and none blocks a release.

- Error and log strings brought under the documentation style guide.
- A root-owned /dev/usbmon node reads as absent for a plain user, so the
  remedy says no node was found. Distinguishing permission-denied from
  not-found in the probe would give the sharper sudo remedy.
- Search filtering waits for the next refresh tick, up to 1 second at the
  default rate. Pulling the tick forward on a search keystroke would make
  filter-as-you-type feel immediate.
- The usbmon text fallback overcounts isochronous transfers. Its length
  column holds the buffer size, not the bytes moved. On a camera stream the
  overcount was 3.6x. The text format prints 5 of 32 descriptors per URB, so
  no exact count exists in text mode. The binary interface reports true
  bytes and is already the preferred source. The UI and the JSON output now
  mark affected rates as estimates. The remaining idea: sum the printed
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

## Testing follow-ups

- Thunderbolt and USB4 hardware validation. usbtop-ng observes USB URBs
  behind such hosts and docks, never PCIe or fabric traffic. A useful
  matrix covers dock controllers, hub topology, hotplug, suspend and
  resume, and peer links.
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

## Notes
