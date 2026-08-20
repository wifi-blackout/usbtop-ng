# Roadmap

Ideas and follow-up work for usbtop-ng. Nothing here is committed work or a
schedule. Items move to [CHANGELOG.md](../CHANGELOG.md) when they ship.

## Feature ideas

- Interactive `/` search in the device table. `--filter` covers the
  command-line case; the table itself has no live search yet.
- Endpoint rows in the TUI device list. The JSON output already carries
  per-endpoint detail; the table shows only device totals.
- Export of bandwidth data to a file.
- One row per physical connector, using the sysfs port `peer` links. Today
  the USB2 side and the USB3 side of one connector list as sibling buses.
- Bus discovery without debugfs, so the binary interface stands alone. Today
  usbtop-ng finds buses through debugfs even when it reads `/dev/usbmon<bus>`.
- Plugin system for custom monitors.
- Monitoring of remote systems over the network.

## eBPF backend

A third packet source built on kprobes, prototyped on 2026-08-19 with
bpftrace 0.20.2 on kernel 7.0.0-29-generic. Two probes cover all USB
traffic: `usb_submit_urb` and `__usb_hcd_giveback_urb`. The kernel BTF at
`/sys/kernel/btf/vmlinux` resolved every struct field with no header files.
In-kernel maps aggregated bytes keyed by bus, device, endpoint, direction,
and transfer type. usbtop-ng would read the maps once per refresh tick.

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
- Headline feature: per-process attribution. `usb_submit_urb` runs with task
  context, which usbmon never records. The prototype also showed the trap:
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

- A warning color for the `dropped:` and `shed:` counters. The palette has no
  warning hue, so both render like ordinary stats.
- An ellipsis on truncated table cells. Truncation is silent today.
- No empty parens in the bus header when the bus speed is unknown.
- One constant for the 60-second window. The device chart bounds and
  `RATE_HISTORY_WINDOW` state it separately.
- Error and log strings brought under the documentation style guide.
- The usbmon text fallback overcounts isochronous transfers. Its length
  column holds the buffer size, not the bytes moved. On a camera stream the
  overcount was 3.6x. The text format prints 5 of 32 descriptors per URB, so
  no exact count exists in text mode. The binary interface reports true
  bytes and is already the preferred source. Candidate fix: mark text-mode
  rates as estimates in the UI.

## Testing follow-ups

- A committed pty harness for the wedged-terminal checks. The checks run by
  hand today and are recorded in review reports only.
- A pipe-based regression guard proving the terminal-restore bytes bypass
  stdio buffering.
- Age-based tests that do not assume 2 minutes of machine uptime.
- Thunderbolt 3 and newer devices are untested.
- USB 4 and newer devices are untested.
- Generate a log with a normalized schema of hardware devices that have
  been tested. Ideas for this would include the date the test was performed
  along with the conditions (kernel version, relevant drivers, attached port
  chipsets, hubs). Then come up with an option to gather the data which
  could then be submitted as a contribution. But the methodology of this is
  open at this time.


## Notes
