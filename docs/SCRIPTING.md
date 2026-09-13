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
   findings: none
   chokepoints: none
   ```
   The first line carries the window's timestamp, length, packet source, the
   channel drop count, and the kernel-side ring drop count (`kdropped`,
   nonzero only when the mmap ring reader dropped packets). One bus header
   follows per bus, then one indented row per device: `bus:address`, a
   1-wide origin cell (`i` when the device matches an internal-device
   snapshot, blank otherwise — see
   [The `internal` field](#the-internal-field)), `vendor_id:product_id`, link
   speed, rx and tx rate, and the vendor/product string. Two sections close
   the report: `findings:`, `none` or a count and one indented line per
   device linked below the speed it supports (see
   [The findings list](#the-findings-list)), then `chokepoints:`, `none` or
   a count and one indented line per hub whose link is asked for more than
   it can carry (see [The chokepoints list](#the-chokepoints-list)). This
   capture ran on an idle bus with nothing to call out, hence the all-zero
   rates and the empty sections; a device moving data reports its rate here
   instead.

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
- `--window`, `--json`, and `--output` all require `--once` or `--batch`.
  Passing any of the three without one of those two flags is an error,
  exit code 2:
  ```
  error: --json, --window, and --output need --once or --batch
  ```
- `--window` is also accepted by `--support`, which needs neither `--once`
  nor `--batch`: there it sets the capture window instead of a report
  window, with its own default and floor -- 5 seconds and 0.1 seconds, the
  capture rule, not the report rule above.

## `--demand BASIS`: the choke-point basis

`--demand link` (the default) or `--demand capability` picks the rate the
choke-point model assumes every device pushes, for `--once` and `--batch`;
the chosen basis rides in the report as `demand_basis`, and any other value
is rejected. Unlike `--window`, `--json`, and `--output`, it is accepted
without `--once` or `--batch` and simply has nothing to act on there: the
TUI always starts at the link basis, and its `c` key toggles the basis live.
See [The chokepoints list](#the-chokepoints-list).

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
| `version` | u32 | report schema version, currently 1; fields are only added, never renamed or removed, and an added field does not bump it |
| `timestamp` | f64 | Unix time the report was built, seconds |
| `window_seconds` | f64 | the sample window's length, seconds |
| `source` | string | `"binary"` or `"text"`, the usbmon interface read |
| `dropped_packets` | u64 | packets lost to a full channel this session |
| `kernel_dropped_packets` | u64 | packets the kernel's usbmon ring dropped before a reader saw them, from `MON_IOCG_STATS`; always 0 unless the mmap ring reader is in use |
| `total_rx_bps` | f64 | sum of every bus's `rx_bps` |
| `total_tx_bps` | f64 | sum of every bus's `tx_bps` |
| `buses` | array | one entry per bus, sorted by bus number |
| `findings` | array | devices linked below the speed they support, sorted by (bus, address); empty when there is nothing to call out. See [The findings list](#the-findings-list) |
| `demand_basis` | string | `"link"` or `"capability"`, the rate the choke-point model assumed every device pushes; follows `--demand`. See [The chokepoints list](#the-chokepoints-list) |
| `choke_floor` | f64 | the breathing room applied, `1.25`: a hub is listed only when the devices below it ask at least this many times its link's capacity |
| `chokepoints` | array | hubs whose links are asked for more than they can carry, worst first; empty when none reaches `choke_floor`. See [The chokepoints list](#the-chokepoints-list) |

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
| `capability_mbps` | f64? | the highest link rate the device says it supports, in Mbps; `null` when unknown. See [The findings list](#the-findings-list) |
| `capability_source` | string? | `"bos"` when that figure was decoded from the device's BOS, `"bcd_usb"` when it is the bcdUSB floor; `null` when `capability_mbps` is |
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

`findings[]`, one entry per device linked below the speed it supports:

| Field | Type | Meaning |
| --- | --- | --- |
| `bus` | u8 | bus number |
| `address` | u8 | USB device number |
| `path` | string | the device's sysfs name, `5-1.2` |
| `port` | string? | the kernel port object the device is on, `5-1-port2`; `null` when the connector index does not know it |
| `link_mbps` | f64 | the rate the link actually came up at, in Mbps |
| `capability_mbps` | f64 | the rate the device says it supports, in Mbps |
| `capability_source` | string | `"bos"` or `"bcd_usb"`, as on the device row |
| `cause` | string? | `"superspeed_side_empty"`, `"usb2_only_host_port"`, `"usb2_only_port"`, `"upstream_hub_link"`, `"host_port_max"`, or `"upstream_permits"`; `null` when the topology attributes none |
| `peer_port` | string? | the empty SuperSpeed port, `6-1-port2`; set by `superspeed_side_empty` only, `null` otherwise |
| `upstream` | string? | the hub above the device, `3-1.4`; set by `upstream_hub_link` and `usb2_only_port`, `null` otherwise |
| `limit_mbps` | f64? | the rate that limits the link: the upstream hub's own link for `upstream_hub_link`, the host port's ceiling for `host_port_max`; `null` for every other cause, `usb2_only_port` included — the port is USB 2 only, the hub above it is not slow |
| `message` | string | the one sentence the text report and the TUI show, `linked at 480M, supports 10G: <reason>` |

A cause never sets a field another cause owns, so a consumer reads `cause`
first and then only the fields that tag defines; everything else is `null`.

`chokepoints[]`, one entry per hub whose link is asked for more than it can
carry, ordered by `ratio` descending and then by `path`:

| Field | Type | Meaning |
| --- | --- | --- |
| `bus` | u8 | bus number |
| `address` | u8 | the hub's USB device number |
| `path` | string | the hub's sysfs name, `3-1` |
| `port` | string? | the kernel port object the hub's own link is, `usb3-port1` — the same key `findings[].port` carries, so a script can join the two lists; `null` when the connector index does not know it |
| `capacity_mbps` | f64 | what the hub's link can carry, in Mbps, after the class efficiency factor |
| `demand_mbps` | f64 | what the devices below it would ask of that link, in Mbps, after the same factor |
| `ratio` | f64 | `demand_mbps` divided by `capacity_mbps`; at or above `choke_floor` on every entry |
| `devices` | u64 | how many devices sit below the hub, nested hubs counted |
| `top` | array | the largest contributors, at most three, by demand descending and then by path |
| `message` | string | the one sentence the text report's `chokepoints:` section shows, `384M carries 9 devices asking 1.17G: 3.05x`; the TUI's connector heading builds a shorter suffix from the same numbers |

`chokepoints[].top[]`, one entry per contributing device:

| Field | Type | Meaning |
| --- | --- | --- |
| `path` | string | the device's sysfs name, `3-1.4.1` |
| `demand_mbps` | f64 | what that device alone asks of the hub's link, in Mbps |

### Example document

A representative document, trimmed to two buses with one device each,
matching the field names and shapes above; it comes from the corpus's
Thunderbolt 4 dock bundle, where both of those devices are linked below the
speed they support, so the report carries two findings. That bundle also has
three choke points; the first of them is kept here and the other two
trimmed. Pretty-printed here for readability: `--once --json` prints each
report as a single compact line:

```json
{
  "version": 1,
  "timestamp": 1787199564.855,
  "window_seconds": 1.0,
  "source": "binary",
  "dropped_packets": 0,
  "kernel_dropped_packets": 0,
  "total_rx_bps": 720.0,
  "total_tx_bps": 0.0,
  "buses": [
    {
      "bus": 3,
      "speed_mbps": 480.0,
      "controller": "0000:00:14.0",
      "rx_bps": 720.0,
      "tx_bps": 0.0,
      "devices": [
        {
          "bus": 3,
          "address": 51,
          "port": "1.4.5",
          "vendor_id": "1409",
          "product_id": "3270",
          "vendor": "Camera Manufacturer",
          "product": "USB 3.0 Camera",
          "speed_mbps": 480.0,
          "rx_bps": 720.0,
          "tx_bps": 0.0,
          "total_rx_bytes": 720,
          "total_tx_bytes": 0,
          "estimated": false,
          "internal": false,
          "capability_mbps": 5000.0,
          "capability_source": "bos",
          "endpoints": [
            {
              "endpoint": 1,
              "direction": "in",
              "transfer_type": "bulk",
              "bps": 720.0,
              "total_bytes": 720
            }
          ]
        }
      ]
    },
    {
      "bus": 5,
      "speed_mbps": 480.0,
      "controller": "0000:2e:00.0",
      "rx_bps": 0.0,
      "tx_bps": 0.0,
      "devices": [
        {
          "bus": 5,
          "address": 5,
          "port": "1.2",
          "vendor_id": "0bda",
          "product_id": "9210",
          "vendor": "SSK",
          "product": "SSK Storage",
          "speed_mbps": 480.0,
          "rx_bps": 0.0,
          "tx_bps": 0.0,
          "total_rx_bytes": 0,
          "total_tx_bytes": 0,
          "estimated": false,
          "internal": false,
          "capability_mbps": 10000.0,
          "capability_source": "bos",
          "endpoints": []
        }
      ]
    }
  ],
  "findings": [
    {
      "bus": 3,
      "address": 51,
      "path": "3-1.4.5",
      "port": "3-1.4-port5",
      "link_mbps": 480.0,
      "capability_mbps": 5000.0,
      "capability_source": "bos",
      "cause": "upstream_hub_link",
      "peer_port": null,
      "upstream": "3-1.4",
      "limit_mbps": 480.0,
      "message": "linked at 480M, supports 5G: the hub above it (3-1.4) is linked at 480M; move it to a USB 3 port"
    },
    {
      "bus": 5,
      "address": 5,
      "path": "5-1.2",
      "port": "5-1-port2",
      "link_mbps": 480.0,
      "capability_mbps": 10000.0,
      "capability_source": "bos",
      "cause": "superspeed_side_empty",
      "peer_port": "6-1-port2",
      "upstream": null,
      "limit_mbps": null,
      "message": "linked at 480M, supports 10G: the SuperSpeed side of this connector (6-1-port2) is empty, so the link came up at USB 2 speed; check the cable or the port"
    }
  ],
  "demand_basis": "link",
  "choke_floor": 1.25,
  "chokepoints": [
    {
      "bus": 3,
      "address": 2,
      "path": "3-1",
      "port": "usb3-port1",
      "capacity_mbps": 384.0,
      "demand_mbps": 1172.25,
      "ratio": 3.052734375,
      "devices": 9,
      "top": [
        {
          "path": "3-1.4.1",
          "demand_mbps": 384.0
        },
        {
          "path": "3-1.4.5",
          "demand_mbps": 384.0
        },
        {
          "path": "3-1.4.7.3",
          "demand_mbps": 384.0
        }
      ],
      "message": "384M carries 9 devices asking 1.17G: 3.05x"
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

## `--output PATH`: write to a file

`--output PATH` sends every report to `PATH` instead of stdout, in the active
format (text, or NDJSON with `--json`). The file is created or truncated when
the run starts; there is no append and no rotation (redirect stdout if you
want either). One line on stderr at exit says how many reports were written
and where. A write error on the file is fatal with a non-zero exit. `PATH`
itself must not be a symbolic link: the file is opened without following a
link at its final component, so a link planted in a shared directory cannot
redirect a root run's output onto some other file (a symlinked parent
directory is fine). Such a path is refused at start with an error that says
so. `/dev/stdout`, `/dev/stderr`, and `/dev/fd/N` are symlinks on Linux but
name descriptors the process already holds, so they keep working: the sink
duplicates the descriptor instead of opening a path. The directories on the
way to `PATH` are trusted as given: do not point `--output` into a directory
another user can modify, since a root run writes there with root's
authority.

```bash
sudo usbtop-ng --batch --json --window 1 --output run.ndjson
```

A file export starts with a run record so the file describes the run it
came from. In JSON it is the first line:

```json
{"record":"run","usbtop_ng":"1.7.0","features":[],"started_unix":1788354946,"window_seconds":1.0,"batch":true,"filters":[],"command":["usbtop-ng","--batch","--json","--window","1","--output","run.ndjson"],"backend":"mmap","kernel":"7.0.0-30-generic","os":"Linux Mint 22.3","arch":"x86_64","buses":[0,1,2,3,4]}
```

| Field | Type | Meaning |
| --- | --- | --- |
| `record` | string | always `"run"`; report lines never carry this key |
| `usbtop_ng` | string | the version that wrote the file |
| `features` | array | cargo features compiled in, sorted (`capture-fixture`, `ebpf`, `integration`) |
| `started_unix` | u64 | Unix time the run started, seconds |
| `window_seconds` | f64 | the requested window |
| `batch` | bool | `true` for `--batch`, `false` for `--once` |
| `filters` | array | the `--filter` terms as given |
| `command` | array | the command line as run |
| `backend` | string | the source selected at start: `ebpf`, `mmap`, `binary`, `text`, or `none`; each report's own `source` stays authoritative |
| `kernel` | string | kernel release |
| `os` | string | the OS pretty name |
| `arch` | string | target architecture |
| `buses` | array | the usbmon buses available at start |

The report lines that follow are unchanged, schema version 1 — a version
an added field does not bump, so a reader written against an older release
keeps working and simply does not read the new keys. A consumer
that only wants reports skips the record by key:

```bash
jq -c 'select(.record != "run") | .total_rx_bps' run.ndjson
```

In text mode the same fields lead the file as a `# key: value` block, one
per line, before the first report. Stdout never carries the run record, so
`--batch --json | jq` scripts need no change.

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

## The findings list

`findings` is the top-level list of devices linked below the speed they
support, with the cause where the topology proves one. It sits at the top
level rather than nested under each device, so a script can ask
`.findings | length` without walking the tree.

The capability behind it is the device's own statement, read from the sysfs
`bos_descriptors` attribute — the device's Binary device Object Store,
exposed by Linux 6.9 and later. A SuperSpeedPlus capability there gives the
largest sublink rate it advertises (10000 or 20000), a SuperSpeed capability
alone gives 5000, and neither gives no capability at all: `capability_mbps`
stays `null` and the device is never called out. Where the file does not
exist — an older kernel, or a device with no BOS — bcdUSB 3.x stands in as
a 5000 floor and nothing higher, so a device linked at 5 Gbps on such a host
is never called out for a 10 Gbps ability the tool cannot see.
`capability_source` says which of the two it was, and the `message` appends
`(from bcdUSB)` for the floor.

Two consequences worth knowing before scripting on this:

- The BOS states lane rates, not lane counts, so `capability_mbps` is the
  per-lane rate. A dual-lane 20 Gbps device linked single-lane at 10 Gbps
  produces no finding.
- A hub's USB 2 half advertises SuperSpeed even when it is working
  perfectly, because its USB 3 half is a separate device on the peer bus.
  Findings are therefore decided per connector, from the kernel's port
  `peer` links and the device tree, not from the capability figure alone,
  and a case the topology cannot attribute yields a finding with a `null`
  `cause` or no finding at all rather than a guess.

`--filter` narrows `findings` the same way it narrows the device rows: a
device filtered out of `buses[].devices` is filtered out of `findings` too.

```bash
sudo usbtop-ng --once --json | jq -c '.findings[] | {path, cause, message}'
```

## The chokepoints list

`chokepoints` is the top-level list of hubs whose links are asked for more
than they can carry. It is a model of the topology, not a measurement: no
traffic is read for it. Every end device contributes what it would push at
its own rate, each hub above it adds that contribution into its own subtree
sum, and the sum is divided by what the hub's own link can carry. Both
sides are practical rates -- the link rate times the class efficiency factor
the `%busy` denominators use, so a 480 Mbps hub reads 384 -- and the
quotient is `ratio`. A device of unknown rate contributes nothing, a
disconnected one is left out, and an internal device counts like any other,
because it is a traffic source too.

A hub's own link is the only stage. A root hub is therefore never an entry:
everything below a hub crosses that hub's link, and the root port above it
would always carry the identical number. The USB 2 and USB 3 halves of one
physical hub are two devices in sysfs with two links, so their subtrees are
summed separately without any pairing: a USB 2 mouse under a USB 3 hub
loads the 480M half, a 5 Gbps camera the SuperSpeed one.

`choke_floor` is the breathing room, `1.25`. A hub is listed only when its
subtree asks at least 1.25 times its capacity; below that the model is
noise, since a 480M hub carrying a flash drive and a mouse already reads
1.03x. The floor rides in every report so a script sees the one that was
applied rather than assuming it.

`demand_basis` says which rate each device was assumed to push, and
`--demand` picks it:

- `link`, the default, uses the rate every device has now, on both sides.
  It answers "is the tree as it stands oversubscribed".
- `capability` uses the rate each device says it could link at -- the same
  BOS figure the findings use -- bounded by the capacity of every hub above
  it, and takes a SuperSpeed hub's capacity as the larger of its link and
  its own capability, so a 10 Gbps hub linked at 5 Gbps counts as 10 Gbps.
  The bound is what keeps the view honest: nothing below a USB 2 half can
  push more than 480 Mbps whatever its own BOS advertises, so a 10 Gbps
  drive plugged into one asks 480 of it and not 10000 -- moving it is the
  call-out the findings already make, not something this model simulates.
  A USB 2 half's own capacity is likewise its link and never its BOS
  figure, because the SuperSpeed capability such a hub advertises belongs
  to its other half, a different sysfs hub. Where every device is already
  linked at what it supports, the two bases give the same list.

`--filter` narrows `chokepoints` the way it narrows `findings`, at the list
and not in the model: the walk always covers the whole device tree, so a
device filtered out of `buses[].devices` still counts toward the hub above
it, but an entry whose own hub row was filtered out is dropped from the
list.

```bash
sudo usbtop-ng --once --json | jq -c '.chokepoints[] | [.path, .ratio]'
```

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
