# Roadmap

Ideas and follow-up work for usbtop-ng. Nothing here is committed work or a
schedule. Items move to [CHANGELOG.md](../CHANGELOG.md) when they ship.

## Feature ideas

- Interactive `/` search in the device table. `--filter` covers the
  command-line case; the table itself has no live search yet.
- Endpoint rows in the TUI device list, expand/collapse on the selected
  device. The accounting and JSON output already carry per-endpoint
  detail; the table shows only device totals, and `WindowCounter::bps`
  waits test-gated for this consumer. Best value-to-effort item on this
  list (2026-08-22 review).
- Document file export. `--batch --json > capture.ndjson` already covers
  it; a native `--output PATH` only earns its place if users ask for
  append or rotation semantics.
- One row per physical connector, using the sysfs port `peer` links. Today
  the USB2 side and the USB3 side of one connector list as sibling buses.
  A connector pairs individual hub ports, not whole buses, and `peer`
  links can be absent or wrong, so this needs a design investigation and
  real dock fixtures before code.
- Bus discovery without debugfs, so the binary interface stands alone. Today
  usbtop-ng finds buses through debugfs even when it reads `/dev/usbmon<bus>`.
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
  is a finding only a traffic monitor can make. Blocked on the
  exact-speed model fix below. Verdict doctrine: measured beats claimed,
  exonerate confidently, convict only a uniquely limiting party, and say
  nothing where attribution is ambiguous.

Out of scope, no stable Linux interface: per-port power-out metering, the
system DC-in rail, the negotiated PD contract, connector fault counters,
and liquid detection. Display diagnostics are out of charter.

Prerequisite: none of the typec, power-delivery, or thunderbolt classes
exist on the current development host, so this work needs hardware that
exposes them before any of it can be built honestly.

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
- Aggregated maps do not fit the per-packet `PacketSource` contract. The
  backend needs a delta seam: capture backends emit (key, bytes) updates,
  the manager accounts them, and `apply_packet` becomes the usbmon
  adapter. Monotonic per-CPU counters diffed against snapshots, never
  read-and-clear.
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
- A cheaper middle path exists inside usbmon: the mmap ring with
  MON_IOCX_MFETCH batch header fetches. It would end the payload copies the
  binary reader makes and discards today, 39 MB over the 6-second stream.

## Engineering follow-ups

These came out of code review. Each is small and none blocks a release.

- A semantic warning color for the `dropped:` and `shed:` counters. Both
  already render orange and bold, but they share `SECONDARY_COLOR` with
  the Peak figure, so warning and statistic look alike.
- An ellipsis on truncated table cells. Truncation is silent today.
- No empty parens in the bus header when the bus speed is unknown.
- One constant for the 60-second window. Three places state it today:
  `RATE_HISTORY_WINDOW` in stats, `HISTORY_WINDOW_SECS` in ui, and the
  device chart's hard-coded -60.0 axis bound.
- Error and log strings brought under the documentation style guide.
- SUDO_USER-aware config-dir resolution, so preferences, the usb.ids home
  copy, the internal snapshot, and `--create-alias` follow the invoking
  user under sudo. Today each resolves against root's home there, and
  only sudo -E bridges the two. One coherent change, and created files
  must land owned by the invoking user, not root.
- The exact-speed model. `20000` parses to SuperSpeedPlus, which reports
  10,000 Mbps everywhere, so a 20 Gbps bus shows half its speed and
  %busy roughly doubles. Store exact Mbps with a separate display class
  instead of the lossy enum. A correctness bug, not polish.
- Built-in usbmon detection. Module status reads /proc/modules only, so a
  kernel with usbmon compiled in reads as not loaded and can draw a
  pointless load prompt when debugfs is unmounted.
- Text reports round device speeds to whole Mbps, so a 1.5 Mbps low-speed
  device prints as 2 Mbps. The JSON output carries the exact value.
- A first pull with no home copy has no date floor, so a replayed
  older-but-valid usb.ids payload could install and shadow a newer distro
  copy. A hardening pass could floor on the newer of the replaced copy
  and the active source.
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

## Testing follow-ups

- A committed pty harness for the wedged-terminal checks. The checks run by
  hand today and are recorded in review reports only.
- A pipe-based regression guard proving the terminal-restore bytes bypass
  stdio buffering.
- Age-based tests that do not assume machine uptime: the 70-second
  assumption in stats and the 120-second one in ui. Extract eviction
  helpers that take a caller-supplied now.
- Thunderbolt and USB4 hardware validation. usbtop-ng observes USB URBs
  behind such hosts and docks, never PCIe or fabric traffic. A useful
  matrix covers dock controllers, hub topology, hotplug, suspend and
  resume, and peer links. Blocked on the exact-speed model fix above for
  20 Gbps links.
- Generate a log with a normalized schema of hardware devices that have
  been tested. Ideas for this would include the date the test was performed
  along with the conditions (kernel version, relevant drivers, attached port
  chipsets, hubs). Then come up with an option to gather the data which
  could then be submitted as a contribution. But the methodology of this is
  open at this time.


## Notes
