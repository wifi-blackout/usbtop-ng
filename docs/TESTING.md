# Hardware Testing

How to validate usbtop-ng on a physical machine: what to plug in, in what
order, what traffic to generate, and what to record. One pass of this
document on one platform produces one entry set for the tested-hardware
log. The platforms in scope are the x86 hosts and the ARM boards on the
roadmap: Raspberry Pi Zero, Pi 4, Pi 400, Pi 5, Radxa ROCK 5C, and
SOPHGO BM1684x board. All are now provisioned as SSH-reachable hosts;
see [Test hosts](#test-hosts) below for the concrete fleet.

## Test inventory

### Test hosts

Nine SSH-reachable hosts, eight probed 2026-08-30 and `tgl-x360` on
2026-09-03. These are the concrete
machines the [per-platform notes](#per-platform-notes) below refer to;
each is referred to by a hardware-role label; the ssh names and accounts
behind the labels live in an owner-only index outside the repository.
The `usbmon` column records the probe result: `built-in`, `module` (loads
on `modprobe usbmon`, nodes appear), `absent` (not in this kernel), or
`module?` (present but the load is unconfirmed because the host lacks
passwordless sudo). Kernels span 5.4 through 7.0 and the arch column
spans armv6l, aarch64, and x86_64 -- the matrix the roadmap wants.

`devhost` also appears below, outside the nine-host fleet, because it is
the development host that contributes the ground-truth isochronous bundle
(see [Capturing hardware fixtures](#capturing-hardware-fixtures)).

| host | Board / SoC | Arch | Kernel | OS | usbmon | Notable |
| --- | --- | --- | --- | --- | --- | --- |
| `pi-4` | Raspberry Pi 4 Model B (BCM2711) | aarch64 | 6.18.39+rpt-rpi-v8 | Debian 13 | module | VL805 USB3 over PCIe + BCM2711 USB2; AR9271 Wi-Fi leaf; kernel upgraded from 6.12.75 since the first probe |
| `pi-400` | Raspberry Pi 400 (BCM2711) | aarch64 | 6.18.39+rpt-rpi-v8 | Debian 13 | module | VL805; RTL9210 NVMe over UAS on USB3, a built-in saturation source; newest kernel in the fleet |
| `pi-5` | Raspberry Pi 5 (BCM2712) | aarch64 | 6.6.31+rpt-rpi-2712 | Debian 12 | module | RP1-southbridge xHCI, two USB2 + two USB3 buses; the best Pi USB path |
| `pi-zero` | Raspberry Pi Zero W (BCM2835) | **armv6l** | 6.12.96+rpt-rpi-v6 | Raspbian 12 | module | single core, one `dwc_otg` OTG port at 480M; the 32-bit `gnueabihf` build target |
| `rock5c` | Radxa ROCK 5C (Rockchip RK3588S2) | aarch64 | 6.1.84-8-rk2410 (vendor) | Debian 12 | built-in | usbmon built in, `/dev/usbmon0..8` live; eight buses (xhci + ehci/ohci-platform); the binary-only vendor-kernel path |
| `bm1684x` | SOPHGO BM1684x | aarch64 | 5.4.217-bm1684 (vendor, dirty) | Ubuntu 20.04 | **absent** | no usbmon module in this kernel and no BTF -- not capturable as-is; Imaging Source 37UX273-ML camera attached; oldest kernel |
| `tgl-tb4` | Intel Tiger Lake-LP, i5-1135G7 | x86_64 | 7.0.0-30-generic | Linux Mint 22.3 | module | Thunderbolt 4 (NHI + xHCI) plus a 10 Gbps USB bus; two IDS `1409:3270` USB3 cameras on a 10 Gbps hub (`usbfs`); eBPF-ready (BTF present) |
| `cezanne` | AMD Ryzen 9 5900HX (Cezanne) | x86_64 | 7.0.0-30-generic | Linux Mint 22.3 | module | AMD Renoir/Cezanne USB 3.1, two 10 Gbps + two 480M buses; eBPF-ready (BTF present); AMD, not Thunderbolt/USB4 |
| `tgl-x360` | HP Pavilion x360 14-dw1xxx, Intel Tiger Lake-LP i5-1135G7 | x86_64 | 7.0.12+kali-amd64 | Kali GNU/Linux Rolling | module? | Tiger Lake TB4 USB controller (0000:00:0d.0) plus a 500-series 10 Gbps xHCI; Type-C/PD port free (no partner) but no Thunderbolt domain exposed; BTF present; **no passwordless sudo**, so it is the fleet's non-root test case; zsh login shell; HP webcam, Elan touch, AX201 BT internal |
| `devhost` | Development host, AMD Ryzen 9 5900HX (Cezanne), xHCI 0000:06:00.3 and .4 | x86_64 | 7.0.0-30-generic | Linux Mint 22.3 | module | Chicony webcam on bus 1 (the ground-truth iso bundle); BTF present, eBPF runs |

usbmon confirmed on 2026-08-30: `modprobe usbmon` loads the module and
populates `/dev/usbmon<N>` plus the debugfs text interface on `pi-4`,
`pi-400`, `pi-zero`, `pi-5`, `tgl-tb4`, and `cezanne`; `rock5c` has it built in
with nodes already live. The one exception is `bm1684x`: its 5.4 vendor
kernel carries no usbmon module (`modprobe: FATAL: Module usbmon not
found`), so it cannot capture until its kernel gains `CONFIG_USB_MON` --
its stage-0 gate fails today. `tgl-x360` ships the module
(`usbmon.ko.xz`) but the account has no passwordless sudo, so the load is
unconfirmed from here (`module?`); run `sudo modprobe usbmon` on the host
before a capture stage.

eBPF backend readiness: the CO-RE prerequisite `/sys/kernel/btf/vmlinux`
(from `CONFIG_DEBUG_INFO_BTF=y`) is present only on the x86_64 hosts:
`tgl-tb4` and `cezanne` (both also carry `bpftool`, `CONFIG_BPF_SYSCALL=y`,
and `CONFIG_KPROBES=y`) and `tgl-x360` (BTF and the kprobe symbol
confirmed; no `bpftool`, kernel config unprobed). The kprobe target `__usb_hcd_giveback_urb`
resolves in `/proc/kallsyms` on every host, so the symbol is never the
blocker. Every ARM board ships without BTF, which blocks CO-RE
independently of the eBPF program's current x86-64-only `pt_regs`; two
separate reasons the `ebpf` feature stays x86-only. `tgl-tb4` and `cezanne`
are the hosts that can run it today; `tgl-x360` should, once the
feature build lands there.

### Port capability matrix

Per-host, per-root-hub USB capability, probed 2026-08-30, so it is clear
which additional test devices each host has room for. Speed is the root
hub's max link rate; "free" counts downstream ports with nothing attached
(ports behind an internal hub are called out). The `C/PD/TB` column reads
the sysfs `typec` / `usb_power_delivery` / `thunderbolt` classes; the ARM
boards do not expose these classes at all, so a physical USB-C connector
there is blank rather than a confirmed absence.

| Host | Root hub | Driver | Ports | Speed | C/PD/TB | Free / attached |
|---|---|---|---|---|---|---|
| `pi-4` | bus1 | xhci | 1 | 480M | — | internal 4p hub (ath9k on 1); 3 hub ports free |
| `pi-4` | bus2 | xhci | 4 | 5G | — | 4 free |
| `pi-400` | bus1 | xhci | 1 | 480M | — | internal 4p hub (kbd, RTL-SDR, HID); 1 hub port free |
| `pi-400` | bus2 | xhci | 4 | 5G | — | UAS SSD on 1; 3 free |
| `pi-5` | bus1 | xhci | 2 | 480M | — | 2 free |
| `pi-5` | bus2 | xhci | 1 | 5G | — | 1 free |
| `pi-5` | bus3 | xhci | 2 | 480M | — | 2 free |
| `pi-5` | bus4 | xhci | 1 | 5G | — | 1 free |
| `pi-zero` | bus1 | dwc_otg | 1 | 480M | — | 1 free (the only port; OTG) |
| `rock5c` | bus1 | xhci | 1 | 480M | — | 1 free |
| `rock5c` | bus2 | xhci | 1 | 5G | — | 1 free |
| `rock5c` | bus3 | xhci | 1 | 480M | — | 1 free |
| `rock5c` | bus4 | ehci | 1 | 480M | — | internal hub (AIC8800 on 3); 3 free |
| `rock5c` | bus5, bus7 | ohci | 1 | 12M | — | 2 free (low-speed) |
| `rock5c` | bus6 | ehci | 1 | 480M | — | 1 free |
| `rock5c` | bus8 | xhci | 1 | 5G | — | 1 free |
| `bm1684x` | bus1 | xhci | 1 | 480M | — | internal hub (audio, BT); 2 free |
| `bm1684x` | bus2 | xhci | 4 | 5G | — | camera on 1; 3 free |
| `tgl-tb4` | bus1 | xhci | 1 | 480M | — | 1 free |
| `tgl-tb4` | bus2 | xhci | 4 | 10G | C·PD·TB | 4 free |
| `tgl-tb4` | bus3 | xhci | 12 | 480M | — | hub + storage + webcam + BT; 8 free |
| `tgl-tb4` | bus4 | xhci | 4 | 10G | C·PD·TB | internal hub w/ 2 IDS cams; 3 root free |
| `cezanne` | bus1 | xhci | 4 | 480M | — | HID on 3; 3 free |
| `cezanne` | bus2 | xhci | 2 | 10G | C·PD | 2 free |
| `cezanne` | bus3 | xhci | 4 | 480M | — | BT on 4; 3 free |
| `cezanne` | bus4 | xhci | 2 | 10G | — | 2 free |
| `tgl-x360` | bus1 | xhci | 1 | 480M | — | 1 free |
| `tgl-x360` | bus2 | xhci | 4 | 10G | C·PD | 4 free (TB4 USB controller, no TB domain) |
| `tgl-x360` | bus3 | xhci | 12 | 480M | — | webcam, Elan touch, BT internal; 9 free |
| `tgl-x360` | bus4 | xhci | 4 | 10G | — | 4 free |

Both USB-C ports on `tgl-tb4` currently show a connected partner, so neither
is free right now; `cezanne`'s single USB-C port (PD-capable, no TB) is
free, and so is `tgl-x360`'s (PD-capable, no partner, no TB domain).
Only `tgl-tb4` exposes a Thunderbolt fabric. `usbfs_memory_mb` is
raised to 1024 on `tgl-tb4`; every other host sits at the 16 MB default, so
raise it (see [the usbfs buffer note](#the-usbfs-buffer-limit-the-one-that-bites))
before running a high-rate USB3 Vision camera on them.

Where the [to-acquire](#to-acquire) devices go:

- **USB 3.2 Gen 2 SSD enclosure (10 Gbps)** -> `tgl-tb4` (bus2/bus4) or
  `cezanne` (bus2/bus4), the only 10 Gbps hosts. Raise `cezanne`'s
  `usbfs_memory_mb` first.
- **USB 3.2 Gen 2x2 enclosure (20 Gbps)** -> no host on the fleet exposes
  a 20 Gbps bus, so this one still needs new hardware, not just a host.
- **USB4 / Thunderbolt dock** -> `tgl-tb4` only; it is the sole Thunderbolt
  host (`cezanne` has none).
- **E-marked USB-C cables (the future cable diagnostics)** -> `cezanne`'s
  or `tgl-x360`'s free USB-C/PD port today, or free one of `tgl-tb4`'s two
  USB-C ports. This
  is the hardware that unblocks
  [Cable and port diagnostics](ROADMAP.md#cable-and-port-diagnostics).
- **USB2-only flash drive** -> any 480M port (e.g. `cezanne` bus1/bus3,
  `pi-5` bus1/bus3).
- **Powered USB2 OTG hub + micro-B adapter** -> `pi-zero`; its single OTG
  port is the whole reason that item exists.
- **SD card for the reader** -> put the card reader on any free 5 Gbps
  port (e.g. `pi-4` bus2) and the card in it.

### On hand

Verified working in captures on the reference laptop.

| Device | ID | Speed | Exercises |
| --- | --- | --- | --- |
| Genesys USB2 hubs, 3 units | 05e3:0610 | 480 | chains, dual-personality USB2 side |
| Genesys USB3 hub | 05e3:0620 | 5000 | USB3 side |
| Genesys USB3.2 hub tier | 05e3:0625, 0626 | 10000 uplink | SuperSpeedPlus link display |
| VIA dual-personality hub | 2109:2822, 0822 | 480 + 5000 | connector pairing across two buses |
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
   and a USB3 leaf attached. Pass: the hub's two halves list under one
   connector heading spanning both buses (`bus 03 + 04 · hub`), each leaf
   under its own connector heading on its matching side; a hub whose
   upstream port the kernel links no `peer` for lists as two single-bus
   connectors instead.
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

### Capturing hardware fixtures

The same ladder run also feeds the fixture-capture & golden-replay harness:
committed, hermetic regression fixtures under `tests/fixtures/hosts/`, each
a real host+topology replayed against a golden report in the default test
suite. Capture a fixture for a stage instead of, or alongside, that stage's
throwaway `stageN.json`.

1. Build with the feature:
   ```bash
   cargo build --release --features capture-fixture
   ```
2. Attach the stage's devices, start the stage's traffic generator, then run
   as root:
   ```bash
   sudo ./target/release/usbtop-ng --capture-fixture tests/fixtures/hosts/<board>-<date>/stage<N> --window 20
   ```
   Use `--bus <n>` to scope the capture to one bus; the default captures the
   aggregate across every bus. At the bare-board stage (stage 1) omit
   `--baseline` -- a fresh internal-device baseline is captured and reused
   by every later stage. From stage 2 on, pass that baseline:
   `--baseline tests/fixtures/hosts/<board>-<date>/stage1/internal-devices.toml`.
3. The capturer sanitizes at the source: SEC-1, no captured USB payload in
   either trace file, and SEC-2, no symlink under the fixture's `sysfs/`
   escapes the bundle. Both are asserted by the capturer and re-asserted by
   the corpus tests, so a violation fails the PR that adds the bundle. The
   sysfs snapshot copies device attributes only and never `serial` (a
   bundle is published, and no replay reads it), plus each hub's port
   objects (`<hub>-port<N>` with `connect_type` and `location`) and their
   `peer` links, rewritten as relative in-bundle symlinks, so a bundle
   carries the connector pairings the TUI groups by.
4. `bm1684x` contributes no fixtures. Its 5.4 vendor kernel carries no usbmon
   module, so the stage-0 gate fails before any capture is possible (see
   [Test hosts](#test-hosts)). That usbmon-absent state is a documented
   coverage boundary, not a missing bundle.
5. After committing a bundle, run `cargo test fixture_corpus` and confirm it
   passes -- the same replay the default suite runs on every push.
6. Pull a bundle off a host with tar over ssh
   (`ssh <host> 'tar -C ~/fixtures -cf - <bundle>' | tar -C tests/fixtures/hosts -xf -`),
   never `scp -r` -- scp dereferences the bundle's relative `usbN` symlink
   into a duplicate directory, which SEC-2 then rejects.
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
   the `devhost` Chicony ran at 10 fps and the `tgl-tb4` internal webcam at
   8 fps, so 100 or 200 frames need 10 to 26 s and the window must end
   after the stream does. Run the whole stream inside the window or the
   totals describe only part of it.
9. The `devhost` stage2 bundle is the corpus's accuracy anchor: its
   `[generator]` note records the `v4l2-ctl` command, the exact frame
   bytes, and the concurrent mmap and eBPF totals. Repeat that procedure
   on any kernel that changes the xHCI isochronous path. The anchor's own
   figures: mmap, eBPF, and the capturer byte-identical (61,854,792 bytes
   for 61,440,000 raw frame bytes, 0.68% of UVC header overhead), the
   text estimate within 1% of them, and `binary_kernel_dropped = 0`.
10. A host without root can still contribute a static bundle: `--support
    --no-capture` there writes a `fixture/` with the sysfs snapshot and no
    traces; copied into the corpus it declares `sources = []` and is read by
    the connector tests only (the `tgl-x360` bundle is one).
11. Only the `peer` links inside the corpus
    (`tests/fixtures/hosts/**/peer`) are excluded from cargo's package
    walk, because the reciprocal links form a cycle cargo's
    symlink-following walker would otherwise warn about on every build. The
    bundles themselves stay packaged, so `cargo test` from a packaged crate
    still finds them.

Fleet build notes, learned capturing the Pi bundles (2026-08-31):

- One aarch64 binary covers every 64-bit Pi when built on the oldest-glibc
  host (`pi-5`, Debian 12 / glibc 2.36 -- runs unmodified on the Debian 13
  Pis). `pi-5` carries the build box: rustup stable plus this repo.
- `pi-zero` (armv6l) needs the **static musl** cross-build, made on `pi-5`:
  `rustup target add arm-unknown-linux-musleabihf`, Debian's
  `gcc-arm-linux-gnueabihf` as the linker, and
  `-C target-feature=+crt-static`. The glibc cross-target
  (`arm-unknown-linux-gnueabihf`) links Debian's ARMv7-flavored runtime and
  SIGSEGVs on the ARMv6 Zero; the musl target ships its own ARMv6 CRT and
  runs. (This build is also what forced `IoctlRequest` in
  `usbmon/mmap_ring.rs`: musl declares `ioctl`'s request as `c_int` where
  glibc says `c_ulong`.)

`--support` embeds the same bundle layout under `fixture/` (with a live
capture when run as root, without traces otherwise), so a bug reporter's
bundle can be promoted to a corpus fixture by copying its `fixture/`
directory into `tests/fixtures/hosts/<board>-<date>/stage<N>/` and
re-running `cargo test fixture_corpus`.

## Per-platform notes

- Reference x86 laptop: run the full ladder. This is the baseline every
  board compares against.
- `tgl-tb4` (x86, Tiger Lake-LP): the Thunderbolt / USB4 and IDS-camera
  platform, and one of the two eBPF-ready hosts (BTF present). It carries
  a Thunderbolt 4 controller and a 10 Gbps USB bus, and two IDS
  `1409:3270` USB3 cameras already sit on a 10 Gbps hub bound to `usbfs`.
  Use it for the Thunderbolt/USB4 matrix in the roadmap's testing
  follow-ups, for high-rate USB3 bulk near the 10 Gbps ceiling, and to run
  the `ebpf` backend; the IDS cameras are the isochronous/bulk vision
  source.
- `cezanne` (x86, AMD Cezanne): the other eBPF-ready host (BTF present), a
  16-core Ryzen 9 5900HX with AMD Renoir/Cezanne USB 3.1 (two 10 Gbps
  buses) but no Thunderbolt or USB4. Use it to exercise the `ebpf` backend
  on an AMD host and as a second high-rate USB3 bulk target.
- `pi-zero` (Pi Zero W): one `dwc_otg` OTG port, so stages 3 through 8 all
  run behind the powered OTG hub. It is `armv6l`, so it needs the 32-bit
  `arm-unknown-linux-gnueabihf` build, and it is the single slowest core
  in the fleet. Also test the TUI over SSH and over the serial console,
  and watch `dropped`.
- `pi-4` (Pi 4) and `pi-400` (Pi 400): two host stacks. Run stage 3
  once on a blue USB3 port and once on the USB2 path, and confirm both
  controllers group correctly. `pi-400` already carries an RTL9210 NVMe
  over UAS on USB3 -- a built-in saturation source for stage 7.
- `pi-5` (Pi 5): repeat stage 6 on both front ports.
- `rock5c` (ROCK 5C): include the OTG-capable port in stage 3 and note
  its role. Built-in usbmon is confirmed here (`CONFIG_USB_MON=y`,
  `/dev/usbmon0..8` present), so this host exercises the debugfs-free
  binary path directly -- the binary-only vendor-kernel shape the startup
  code must handle.
- `bm1684x` (SOPHGO BM1684x board): the stage-0 gate already fails. The 5.4
  vendor kernel carries no usbmon module (`modprobe: FATAL: Module usbmon
  not found`) and no BTF, so neither the usbmon nor the eBPF backend can
  capture on it as shipped -- it needs a kernel built with
  `CONFIG_USB_MON` (and, for eBPF, `CONFIG_DEBUG_INFO_BTF`) before it can
  join the ladder. Until then it is a build-and-kernel task, not a test
  run. The Imaging Source 37UX273-ML camera on it is the vision source
  once capture works.
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
