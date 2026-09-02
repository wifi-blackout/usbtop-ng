# Scripting usbtop-ng

`--once` and `--batch` print a bandwidth report and exit, or print one report
per window until interrupted. Neither mode opens the TUI or prompts for
anything, so both are safe inside a script or a cron job.

## `--once`: one report and exit

1. Run:
   ```bash
   sudo usbtop-ng --once
   ```
2. usbtop-ng samples a 5 second window (see [Window length](#window-length)),
   then prints one report to stdout and exits 0:
   ```
   ts=1787199783.511 window=5.00s source=binary dropped=0 kdropped=0
   bus 1 (480 Mbps) rx 0.00 MB/s tx 0.00 MB/s
     1:1     1d6b:0002  480 Mbps  rx 0.00 MB/s  tx 0.00 MB/s  Linux 7.0.0-29-generic xhci-hcd xHCI Host Controller
     1:3     05e3:0610  480 Mbps  rx 0.00 MB/s  tx 0.00 MB/s  GenesysLogic USB2.1 Hub
     1:4  i  04f2:b71a  480 Mbps  rx 0.00 MB/s  tx 0.00 MB/s  SunplusIT Inc HD Webcam
   ```
   The first line carries the window's timestamp, length, packet source, the
   channel drop count, and the kernel-side ring drop count (`kdropped`,
   nonzero only when the mmap ring reader dropped packets). One bus header
   follows per bus, then one indented row per device: `bus:address`, a
   1-wide origin cell (`i` when the device matches an internal-device
   snapshot, blank otherwise — see
   [The `internal` field](#the-internal-field)), `vendor_id:product_id`, link
   speed, rx and tx rate, and the vendor/product string. This capture ran on
   an idle bus, hence the all-zero rates; a device moving data reports its
   rate here instead.

## `--batch`: one report per window, repeated

1. Run:
   ```bash
   sudo usbtop-ng --batch --json
   ```
2. usbtop-ng samples 1 second windows (the `--batch` default) and prints one
   report after each, forever, until `Ctrl-C` or a signal ends it (see
   [Exit behavior](#exit-behavior)).

`--once` and `--batch` are mutually exclusive.

## Window length

- `--window SECONDS` sets the sample window. It defaults to 5 seconds with
  `--once` and 1 second with `--batch`.
- The value floors at 0.25 seconds; anything lower is raised to it.
- `--window` and `--json` both require `--once` or `--batch`. Passing either
  without one of those two flags is an error, exit code 2:
  ```
  error: --json and --window need --once or --batch
  ```

## `--json`

`--json` prints each report as one JSON document instead of the text table.
Add it to either mode:

```bash
sudo usbtop-ng --once --json
```

### Field list

Report, the top-level document:

| Field | Type | Meaning |
| --- | --- | --- |
| `version` | u32 | report schema version, currently 1 |
| `timestamp` | f64 | Unix time the report was built, seconds |
| `window_seconds` | f64 | the sample window's length, seconds |
| `source` | string | `"binary"` or `"text"`, the usbmon interface read |
| `dropped_packets` | u64 | packets lost to a full channel this session |
| `kernel_dropped_packets` | u64 | packets the kernel's usbmon ring dropped before a reader saw them, from `MON_IOCG_STATS`; always 0 unless the mmap ring reader is in use |
| `total_rx_bps` | f64 | sum of every bus's `rx_bps` |
| `total_tx_bps` | f64 | sum of every bus's `tx_bps` |
| `buses` | array | one entry per bus, sorted by bus number |

`buses[]`, one entry per bus:

| Field | Type | Meaning |
| --- | --- | --- |
| `bus` | u8 | bus number |
| `speed_mbps` | f64 | bus link speed in Mbps, 0 if unknown |
| `controller` | string? | host controller sysfs name, `null` if unresolved |
| `rx_bps` | f64 | sum of the bus's devices' `rx_bps` |
| `tx_bps` | f64 | sum of the bus's devices' `tx_bps` |
| `devices` | array | one entry per device, in port order |

`buses[].devices[]`, one entry per device on that bus:

| Field | Type | Meaning |
| --- | --- | --- |
| `bus` | u8 | bus number (repeats the parent) |
| `address` | u8 | USB device number |
| `port` | string? | port chain joined by `.`; `""` for a root hub; `null` if sysfs did not resolve the device |
| `vendor_id` | string? | 4 hex digit vendor ID, `null` if unread |
| `product_id` | string? | 4 hex digit product ID, `null` if unread |
| `vendor` | string? | vendor name, from a usb.ids database if one resolved it, else the sysfs string; `null` if unread |
| `product` | string? | product name, from a usb.ids database if one resolved it, else the sysfs string; `null` if unread |
| `speed_mbps` | f64 | device link speed in Mbps |
| `rx_bps` | f64 | bytes in over the window, divided by `window_seconds` |
| `tx_bps` | f64 | bytes out over the window, divided by `window_seconds` |
| `total_rx_bytes` | u64 | cumulative bytes received this session |
| `total_tx_bytes` | u64 | cumulative bytes transmitted this session |
| `estimated` | bool | see [The `estimated` field](#the-estimated-field), below |
| `internal` | bool? | `true` when the device matches the internal-device snapshot, `false` when it doesn't, `null` when no snapshot exists |
| `endpoints` | array | one entry per endpoint seen, ordered by (number, direction) |

`buses[].devices[].endpoints[]`, one entry per endpoint the device has carried traffic on:

| Field | Type | Meaning |
| --- | --- | --- |
| `endpoint` | u8 | endpoint number, 0 through 15 |
| `direction` | string | `"in"` or `"out"` |
| `transfer_type` | string | `"control"`, `"iso"`, `"bulk"`, or `"interrupt"` |
| `bps` | f64 | bytes over the window, divided by `window_seconds` |
| `total_bytes` | u64 | cumulative bytes on this endpoint |

Every rate (`rx_bps`, `tx_bps`, `bps` at every level) is computed from the
exact byte delta across the sample window, not from the TUI's 10 second
sliding-window rate. A `--window 1` report and a `--window 30` report each
report their own window's true average.

### Example document

A representative document with one bus and one active isochronous device,
matching the field names and shapes above, pretty-printed here for
readability. `--once --json` prints each report as a single compact line:

```json
{
  "version": 1,
  "timestamp": 1787199564.855,
  "window_seconds": 1.0,
  "source": "binary",
  "dropped_packets": 0,
  "kernel_dropped_packets": 0,
  "total_rx_bps": 20480.0,
  "total_tx_bps": 0.0,
  "buses": [
    {
      "bus": 1,
      "speed_mbps": 480.0,
      "controller": "0000:06:00.3",
      "rx_bps": 20480.0,
      "tx_bps": 0.0,
      "devices": [
        {
          "bus": 1,
          "address": 4,
          "port": "4",
          "vendor_id": "04f2",
          "product_id": "b71a",
          "vendor": "SunplusIT Inc",
          "product": "HD Webcam",
          "speed_mbps": 480.0,
          "rx_bps": 20480.0,
          "tx_bps": 0.0,
          "total_rx_bytes": 20480,
          "total_tx_bytes": 0,
          "estimated": false,
          "internal": true,
          "endpoints": [
            {
              "endpoint": 1,
              "direction": "in",
              "transfer_type": "iso",
              "bps": 20480.0,
              "total_bytes": 20480
            }
          ]
        }
      ]
    }
  ]
}
```

### NDJSON in `--batch`

`--batch --json` prints one JSON document per line, newline-delimited
(NDJSON). Pipe it into a line-oriented JSON reader, e.g.:

```bash
sudo usbtop-ng --batch --json | jq -c '.total_rx_bps'
```

## Exit behavior

- `Ctrl-C` (`SIGINT`) or `SIGTERM` ends `--batch` after its current window's
  report has printed, and exits 0. `--once` also honors both signals: they
  end the sample window early, and the report that prints carries the true,
  shorter `window_seconds` that was actually measured — not the nominal
  `--window` value — so its rates stay accurate.
- If the reader on the other end of stdout goes away — the common case is
  piping into `head` or a script that closes early — usbtop-ng exits 0
  instead of reporting a broken-pipe error. A script that only wants the
  first report can safely do `usbtop-ng --batch --json | head -n 1`.
- If every usbmon reader stops mid-run — capture failed, so nothing new can
  arrive — usbtop-ng prints an error to stderr and exits 1 instead of
  reporting zeros. `--force` on a host with no detected buses is the
  exception: no capture was expected, so its empty reports print normally.

## The `estimated` field

`estimated` is `true` when both of these hold:

- usbtop-ng is reading the debugfs text interface (`source: "text"` in the
  same report), not the binary `/dev/usbmonN` interface.
- The device has carried isochronous traffic (webcams, some audio devices).

The text interface prints only the first 5 of an isochronous URB's
descriptors (up to 32 on a webcam) and reports the whole buffer as the
URB's length. usbtop-ng estimates the bytes moved by scaling the printed
descriptors' actual lengths by the URB's full packet count. Measured
against the binary interface on the same window, the estimate landed at
0.9999x on a sparse MJPEG webcam stream and 1.011x on a continuous YUYV
stream, where the buffer size had read 15.4x and 3.98x; it is exact
whenever a URB carries five or fewer packets. It is still a sample-based
estimate, so the report says so. usbtop-ng prefers the binary interface
and only falls back to text when the binary nodes cannot be opened.
Non-isochronous devices are never marked `estimated`, on either
interface.

## The `internal` field

`internal` reflects the internal-device snapshot recorded by
`--snapshot-internal`: `true` when a device's sysfs port and IDs match a
snapshot entry, `false` when a snapshot exists but the device doesn't match
it, and `null` when no snapshot file exists at all — so a script can tell
"known external" apart from "origin unknown". The text report's row carries
the same information as a 1-wide cell between the address and the
`vendor_id:product_id` columns: `i` for internal, blank otherwise (see the
example row above). `--filter internal=yes` (or `no`) narrows on this field;
see the README's Filtering section.

## Filters apply the same way

`--filter` narrows both modes exactly as it narrows the TUI's device table:
a device that does not match is left out of `buses[].devices` entirely (and
a bus with no matching devices left is dropped from `buses`), and packets
that do not match a filter term do not count toward the rates or totals of a
device that does show. See the README's Filtering section for the full key
list.

```bash
sudo usbtop-ng --once --json --filter type=iso
```
