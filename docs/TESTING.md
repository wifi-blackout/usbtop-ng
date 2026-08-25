# Hardware Testing

How to validate usbtop-ng on a physical machine: what to plug in, in what
order, what traffic to generate, and what to record. One pass of this
document on one platform produces one entry set for the tested-hardware
log. The platforms in scope are the x86 reference laptop and the ARM
boards on the roadmap: Raspberry Pi Zero, Pi 4, Pi 400, Pi 5, Radxa
ROCK 5C, and SOPHGO Fogwise AirBox.

## Test inventory

### On hand

Verified working in captures on the reference laptop.

| Device | ID | Speed | Exercises |
| --- | --- | --- | --- |
| Genesys USB2 hubs, 3 units | 05e3:0610 | 480 | chains, dual-personality USB2 side |
| Genesys USB3 hub | 05e3:0620 | 5000 | USB3 side |
| Genesys USB3.2 hub tier | 05e3:0625, 0626 | 10000 uplink | SuperSpeedPlus link display |
| VIA dual-personality hub | 2109:2822, 0822 | 480 + 5000 | sibling-bus pairing |
| HD webcam | 04f2:b71a | 480 | isochronous IN, the estimate markers |
| Industrial cameras, 2 units | 1409:3270 | 5000 | USB3 bulk, needs vendor SDK |
| Gigabit Ethernet adapter | 0b95:1790 | 5000 | sustained bidirectional bulk |
| Card reader | 0bda:0316 | 5000 | usb-storage bulk reads |
| Edge AI accelerator | 1a6e:089a | 5000 | bulk, needs its runtime |
| Stream Deck | 0fd9:006d | 480 | high-speed HID interrupt |
| Game controller | 2dc8:310a | 12 | full-speed interrupt |
| Flight-sim button panel | 044f:b352 | 12 | HID behind a hub |
| 3-button mouse | 0430:0100 | 1.5 | low-speed display |
| Bluetooth radio | 8087:0029 | 12 | interrupt + bulk mix, scan traffic |
| Keyboard controllers, 2 units | 048d:ce00, 6005 | 12 | internal HID |

### To acquire

Each fills a hole no on-hand device covers.

| Item | Fills |
| --- | --- |
| USB 3.2 Gen 2 SSD enclosure (10 Gbps) | high-rate bulk near a real device ceiling |
| USB 3.2 Gen 2x2 enclosure (20 Gbps) | the exact-speed model at 20000 |
| USB4 or Thunderbolt dock | tunneled topology, the validation matrix |
| USB2-only flash drive | high-speed bulk without USB3 wiring |
| Powered USB2 OTG hub + micro-B adapter | the Pi Zero's single port |
| E-marked USB-C cables, 3 A and 5 A rated | the future cable diagnostics |
| SD card for the reader | makes the card reader a bulk source |

## Traffic generators

One per transfer type. Record the generator's own number next to
usbtop-ng's.

1. Isochronous: `ffmpeg -f v4l2 -i /dev/video0 -t 10 -f null -`.
   Expected bytes = delivered frames × frame size, plus about 5% packet
   headers. ffmpeg prints the frame count.
2. Bulk storage: `sudo dd if=/dev/sdX of=/dev/null bs=4M count=256
   iflag=direct`. dd prints its own MB/s.
3. Bulk network: `iperf3 -c <peer>` through the Ethernet adapter, then
   `-R` for the other direction. Checks the rx/tx split.
4. Interrupt: move the mouse or a controller stick during the window.
   Presence and attribution, not rate.
5. Bluetooth: `bluetoothctl scan on` during the window.

## The topology ladder

Run the stages in order. Each stage's capture is one
`sudo usbtop-ng --once --window 10 --json > stageN.json` alongside the
generator, plus a look at the TUI for the display-only checks.

1. Bare board. No external devices. Pass: every root hub and internal
   device enumerates, speeds exact, all rates zero.
2. Snapshot. Run `usbtop-ng --snapshot-internal` now, before anything
   external attaches. Pass: the capture lists exactly the internal set.
3. One device, direct. A single leaf on a root port. Generate its
   traffic. Pass: rate lands on the right row within tolerance, nothing
   bleeds elsewhere, the Port cell stays uncolored, internal rows blue.
4. One hub, one device. Pass: the leaf's port chain gains the hub level,
   traffic attribution unchanged.
5. Deep chain. Three hub levels, mixed speeds on the leaves, the mouse at
   the deepest port. Pass: port-chain sort matches the physical wiring,
   the 1.5 Mbps leaf renders exactly.
6. Dual-personality split. The VIA or Genesys USB3 hub with a USB2 leaf
   and a USB3 leaf attached. Pass: the two sides list as sibling buses
   under one controller, each leaf on its matching side.
7. Saturation. The fastest bulk source, direct, then behind the deepest
   hub position. Pass: measured rate within 10% of the generator's
   number in both placements, `dropped` stays 0.
8. Churn. During a `--batch` capture: hot-plug a device, move it to a
   different depth, unplug it mid-transfer. Pass: rows appear, move, and
   age out through the disconnect grace, no stale phantom rows, the
   session survives.

Attachment order inside every stage: hubs first, leaves second, deepest
level last. Record each device's port path at attach time. Detach in
reverse.

## Per-platform notes

- Reference x86 laptop: run the full ladder. This is the baseline every
  board compares against.
- Pi Zero: one OTG port, so stages 3 through 8 all run behind the powered
  OTG hub. Also test the TUI over SSH and over the serial console, and
  watch `dropped` — it is the slowest core in the fleet.
- Pi 4 and Pi 400: two host stacks. Run stage 3 once on a blue USB3 port
  and once on the USB2 path, and confirm both controllers group
  correctly.
- Pi 5: repeat stage 6 on both front ports.
- ROCK 5C: include the OTG-capable port in stage 3 and note its role.
- Fogwise AirBox: stage 0 gate first — confirm the vendor kernel ships
  usbmon and mounts debugfs before running anything else. Record the
  kernel config findings either way.
- Every ARM board: record which usbmon interface the run used (the
  startup log prints it) and whether `/dev/usbmon<N>` nodes exist.

## What to record

One directory per platform per session:
`tmp/hw-results/<board>-<date>/`, never committed. Each stage keeps its
`stageN.json`, the generator's printed number, and a `notes.md` with:

1. Board, OS image, kernel version, `usbtop-ng --version`.
2. Controllers seen (from the JSON's `controller` fields).
3. Capture backend used (`source` field) and any load prompts hit.
4. Per-stage verdict: pass, or what deviated.

The tested-hardware log on the roadmap aggregates these directories into
its normalized schema once that schema is settled. Until then the raw
directories are the record.

## Pass tolerances

- Bulk: usbtop-ng within 10% of the generator's sustained number.
- Isochronous: within 10% of frames × frame size + 5% headers, on the
  binary source. On the text source the rate carries the `~` estimate
  marker instead, and the exact check does not apply.
- Speeds: exact string match against sysfs (`1.5`, `480`, `10000`), no
  rounding anywhere.
- Enumeration: device count equals `lsusb` line count on every stage.
