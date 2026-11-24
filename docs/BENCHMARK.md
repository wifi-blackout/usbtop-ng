# The capture interface benchmark

usbtop-ng reads usbmon through one of three interfaces (see "Reading usbmon"
in the [README](../README.md#reading-usbmon)): the mmap ring, read() on
`/dev/usbmonN`, and the debugfs text file. This benchmark runs each of them
against the same webcam and reports how long the capture took and what the
CPU did during it.

It is a tool to run by hand, not a gate. It needs root, usbmon and a USB
camera, and a full run takes about twenty minutes.

## What a run is

A webcam is the traffic source because it is a constant one: while it
streams, it sends isochronous data at a fixed rate whether or not anything
changes in front of it. One run streams a fixed number of video frames (3000
by default) with `v4l2-ctl` while one interface reads the camera's bus.

A reader only watches the traffic. It cannot slow the camera, so **the time
to deliver the frames is the same for every interface**: the camera's frame
rate sets it. The elapsed time is reported because it is the control that
every run saw the same workload. The interfaces differ in the CPU they use,
and could differ in the events they lose.

Each round runs four captures: one with no reader at all (`none`), then
`mmap-ring`, `binary` and `text`, the names the startup log line uses. Three
rounds by default, each starting one place further along the list, so no
interface always runs first. Four rounds put every interface in every
position once.

The reader in each run is the production reader code with a callback that
only counts events. Three things about it differ from a normal session:

- It reads the camera's own bus (`/dev/usbmon3`, say). usbtop-ng itself
  normally reads the all-buses node, `usbmon0`. The other buses' traffic
  would only add noise here.
- The callback counts and does nothing else. The work downstream of the
  reader, the channel to the UI thread and the accounting, is the same for
  all three interfaces and is not measured.
- It is the test build of the code. In a test build each event carries a few
  fields the shipped binary leaves out, and the mmap ring and read() readers
  format one of them (the URB tag, a short string) for every event, which
  the shipped binary does not do. The text reader parses the same fields in
  every build. So the mmap ring's and read()'s figures are slightly higher
  here than the shipped binary's would be, and text's is not: a few percent
  of the mmap ring's small figure, and nothing visible in read()'s.

## What is measured

| Figure | Where it comes from |
| --- | --- |
| elapsed time | the wall clock around the `v4l2-ctl` run, camera start-up included |
| frames, skipped frames, frame rate, MB/s | the line `v4l2-ctl --verbose` prints per frame: their count, the gaps in the driver's sequence numbers, the count over the elapsed time, and the sum of the frame sizes |
| bus events, camera events | the reader's callback, counted over the capture: every event it parsed, and those from the camera's device number |
| reader CPU | the reader thread's time on a CPU over the capture, from its `schedstat` file (the scheduler's own nanosecond count, user and kernel time together) |
| system % | the share of the reader thread's CPU time that was spent in the kernel, from `getrusage(RUSAGE_THREAD)` over the thread's whole life, setup and teardown included (for the mmap ring, whose capture costs so little, that is a tenth to a fifth of the total); the kernel estimates this split from its timer ticks, so it is approximate |
| outside s | reader CPU spent outside the capture: opening the interface, allocating and mapping the ring, and releasing it |
| kernel drops (`kdropped`) | events the kernel dropped before the reader got to them, over the reader's whole life: on the mmap ring and on read(), the ring's own count (`MON_IOCG_STATS`); on text, the growth of the bus's `text_lost` counter in the bus's debugfs statistics file (`…/usbmon/Ns`) |
| machine CPU | the busy time of every core over the capture, from the first line of `/proc/stat` |

The reader's CPU and its event counts are read at the same two moments: just
before `v4l2-ctl` starts, and two poll intervals (100 ms) after it exits, so
that the events still queued in the kernel when the camera stopped are read
and counted. The machine's CPU and the elapsed time bracket `v4l2-ctl`
exactly.

Both waits are fixed, because the production readers give no signal that
they are ready or that they have caught up: 200 ms before the capture for
the reader to set its interface up, and the 100 ms after it. A reader slower
than that would have setup counted inside the capture or its last events
left out. Two columns show whether that happened: "outside s" stays at the
ring's setup cost when setup stayed outside, and the camera event count is
the same from run to run when nothing was left out.

"% of a core" is CPU time over elapsed time: 100 is one core kept fully busy
for the whole capture. "ms/1000 events" is reader CPU divided by the events
parsed. It is not a cost per event: a reader also wakes twenty times a second
whether or not anything arrived, and that is in the figure too.

### What the reader's CPU leaves out, and for which interfaces

Reader CPU is the time the reader thread itself spent, in user mode and in
the kernel on its behalf (its system calls). It leaves out the work usbmon
does when a transfer is submitted or completes, which runs in the kernel
outside the reader's thread. **That work is not the same size for the three
interfaces:**

- For the mmap ring and for read(), which share the ring, usbmon copies the
  event header and the captured payload into the ring at that moment
  (`mon_bin_event` in `drivers/usb/mon/mon_bin.c`). The reader thread is
  charged none of it.
- For text, usbmon keeps a small record with at most 32 bytes of payload at
  that moment (`mon_text_event` in `drivers/usb/mon/mon_text.c`), and formats
  the line later, inside the reader's own `read()`, where the reader thread
  is charged for it.

So reader CPU compares the mmap ring with read() like for like: the kernel
side is the same code for both, and the difference between them is all in
the reader. It does **not** compare text with the other two like for like:
it counts most of what text costs and leaves out the larger part of what the
ring costs.

Machine CPU takes in everything the machine was doing: the camera driver,
`v4l2-ctl`, a desktop, a browser. "above none" is the machine's share of a
core during an interface's runs less its share during the no-reader runs. On
a machine doing anything else the difference between two interfaces is
smaller than the noise, and the `min` and `max` columns show how much noise
there was.

It does not reliably take in the kernel's side. usbmon's work on a completed
transfer runs in interrupt context (on xHCI, the bottom-half work queue that
hands the transfer back, `usb_hcd_giveback_urb` in `drivers/usb/core/hcd.c`).
A kernel built without `CONFIG_IRQ_TIME_ACCOUNTING`, which both hosts below
run, does not time that work. It charges a whole timer tick to whatever the
CPU was doing when the tick fired, and counts a CPU that is idle with its
tick stopped as idle for the whole stretch (`kernel/sched/cputime.c`). A few
microseconds of work on an otherwise idle CPU, several hundred times a
second, is mostly counted as idle.

The kernel's side can be timed directly with a tracer; the results below
include one such measurement, taken outside the benchmark.

### Loss

Three things are looked at:

- frames the driver numbered and did not deliver (the `skipped` column);
- the kernel's own count of events it dropped before the reader got to them
  (`kdropped`). On the mmap ring and on read() that is the ring's count. The
  text interface has no count per reader, but usbmon keeps one per bus,
  `text_lost`, and the benchmark reads how much it grew while the text reader
  ran. Another text reader on the same bus would add to the same count;
- the camera events each reader counted, which should be the same number for
  every interface and every run of one camera.

The text interface has much the smallest queue of the three: at most
`EVENT_MAX` events wait for a reader, four pages' worth of its 176-byte
records, which is 93 on x86-64 (`drivers/usb/mon/mon_text.c`). An event that
arrives when the queue is full is dropped and counted in `text_lost`.

A frame the driver delivered but flagged as damaged still counts as
delivered.

## Running it

1. Load usbmon (`sudo modprobe usbmon`) and close anything that is using the
   camera.
2. If the camera lowers its frame rate in dim light, pin it, so every run
   sees the same traffic. On a UVC camera that is usually
   `v4l2-ctl -d /dev/video0 --set-ctrl=exposure_dynamic_framerate=0`; note
   the old value (`--get-ctrl`) and put it back afterwards.
3. Build the test binary as yourself, in release mode, and note the path
   cargo prints:

   ```bash
   cargo test --release --features integration --no-run
   ```

4. Run the one test as root, by name:

   ```bash
   sudo target/release/deps/usbtop_ng-<hash> capture_interface_benchmark \
       --ignored --nocapture
   ```

Building as yourself and running the binary as root keeps root-owned files
out of `target/`. Leave the mouse and keyboard alone while it runs if they
share a bus with the camera: their events are counted as bus events and cost
the reader CPU.

A debug build runs and says `DEBUG build` in its header: its CPU figures do
not describe the shipped binary.

The test is marked `#[ignore]`, so no ordinary `cargo test` runs it, with or
without the `integration` feature. A run fails, naming the cause, when a
prerequisite is missing, when `v4l2-ctl` delivers a different number of
frames, when a reader stops before it is told to, and when a reader sees no
event from the camera. A camera that stalls leaves `v4l2-ctl` waiting; stop
the run with Ctrl-C.

### Settings

Each is an environment variable, passed through `sudo` on its command line
(`sudo USBTOP_NG_BENCH_FRAMES=600 target/release/deps/…`).

| Variable | Default | Meaning |
| --- | --- | --- |
| `USBTOP_NG_BENCH_CAMERA` | `/dev/video0` | the V4L2 capture node, or a link to it such as a `/dev/v4l/by-id` name; it must be a USB camera |
| `USBTOP_NG_BENCH_FRAMES` | `3000` | frames per run |
| `USBTOP_NG_BENCH_ROUNDS` | `3` | times each interface is run |
| `USBTOP_NG_BENCH_FORMAT` | `YUYV` | the V4L2 pixel format code (`v4l2-ctl --list-formats-ext` lists the camera's) |
| `USBTOP_NG_BENCH_WIDTH` | `640` | frame width |
| `USBTOP_NG_BENCH_HEIGHT` | `480` | frame height |
| `USBTOP_NG_BENCH_FPS` | `30` | the frame rate asked of the camera |
| `USBTOP_NG_BENCH_INTERFACES` | all four | a comma-separated list of `none`, `mmap-ring`, `binary` and `text`, to run only those, in that order |

The camera decides what it actually delivers: a driver that does not offer
the format or size picks the nearest it has. Check the `fps` and `MB/s`
columns against what was asked. Uncompressed YUYV is the steadiest workload,
since every frame is the same size; an MJPEG frame's size follows the scene.

## What the figures do not say

- The benchmark's figures are from two x86-64 hosts on one kernel version,
  and from webcams, which is one kind of traffic at one event rate. The ranking can differ
  elsewhere, most of all on slow hardware, at higher event rates, and for
  traffic that is not isochronous.
- Reader CPU omits the kernel's work when a transfer is submitted or
  completes, and omits more of it for the ring than for text. Machine CPU
  includes everything else the machine was doing, and on these kernels
  mostly misses that same kernel work.
- Reader CPU is the scheduler's stored figure. A thread that is on a CPU at
  the instant it is read has not been charged up to that instant. A running
  thread is normally charged at every scheduler tick, so the error is
  normally under one tick at each end; on a CPU the kernel runs without
  ticks (`nohz_full`) there is no such bound.
- The eBPF backend is not in the benchmark.
- Nothing here measures memory.

## Results

Both fallback readers were changed on 2026-11-23, because of what the
sections dated 2026-11-22 found, and their figures in those sections are the
readers' as they were then. [The text reader after the fix](#2026-11-23-the-text-reader-after-the-fix)
and [the read() reader after the fix](#2026-11-23-the-read-reader-after-the-fix)
have the figures as they are now. Nothing in the mmap ring's figures
changed.

### 2026-11-22, the development host

`devhost` in [TESTING.md](TESTING.md): AMD Ryzen 9 5900HX, 16 logical cores,
kernel 7.0.0-43-generic, usbtop-ng 1.8.0 plus the unreleased changes of that
day, a release-mode test build. The machine was in ordinary desktop use
throughout, which is why the machine figures are large and loose. Four runs
of each interface per camera; each figure is the mean of the four, with the
lowest and highest in brackets where the spread matters.

**Built-in UVC webcam, USB 2 high speed.** 3000 frames of uncompressed YUYV
640x480 at 30 fps (18.4 MB/s), with `exposure_dynamic_framerate` pinned to 0.
The bus's other devices were silent throughout: every event counted was the
camera's.

| Interface | Elapsed s | Frames/s | Frames skipped | Camera events per run | Kernel drops | Reader CPU s | Reader, % of a core | Reader, system % | Machine CPU s | Machine, above none |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| none | 99.98 | 30.01 | 0 | - | - | - | - | - | 75.87 (73.40 to 82.38) | - |
| `mmap-ring` | 99.96 | 30.01 | 0 | 50018 | 0 | 0.104 (0.101 to 0.106) | 0.10 | 68 | 75.17 (74.18 to 76.91) | -0.68 |
| `binary` | 99.96 | 30.01 | 0 | 50018 | 0 | 3.938 (3.929 to 3.950) | 3.94 | 67 | 79.10 (77.38 to 82.18) | +3.25 |
| `text` | 99.96 | 30.01 | 0 | 50018 | 0 | 0.399 (0.395 to 0.401) | 0.40 | 74 | 77.29 (75.94 to 78.63) | +1.44 |

**External UVC camera, USB 2 high speed, behind two hubs.** 3000 frames of
MJPEG 1920x1080 at 60 fps (1.23 MB/s). Between 142 and 144 of each run's
events, about 25,330 in all, came from other devices on the bus.

| Interface | Elapsed s | Frames/s | Frames skipped | Camera events per run | Kernel drops | Reader CPU s | Reader, % of a core | Reader, system % | Machine CPU s | Machine, above none |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| none | 50.34 | 59.59 | 0 | - | - | - | - | - | 36.90 (36.08 to 38.10) | - |
| `mmap-ring` | 50.33 | 59.61 | 0 | 25184 | 0 | 0.048 (0.047 to 0.049) | 0.09 | 70 | 37.79 (35.71 to 40.42) | +1.80 |
| `binary` | 50.34 | 59.60 | 0 | 25184 | 0 | 1.951 (1.946 to 1.959) | 3.88 | 66 | 40.11 (37.57 to 41.65) | +6.40 |
| `text` | 50.33 | 59.61 | 0 | 25184 | 0 | 0.196 (0.195 to 0.199) | 0.39 | 77 | 37.09 (34.96 to 40.64) | +0.41 |

"Camera events per run" was the same number in all four runs of an interface.
"Kernel drops" is the total over the four runs. "Machine, above none" is in
percentage points of one core. The two ring readers also spent 0.012 to
0.014 s of CPU outside each capture, setting the ring up and releasing it;
text spent none that showed.

### The development host: the kernel's side, timed separately

Reader CPU leaves out what usbmon does when a transfer is submitted or
completes (see [above](#what-the-readers-cpu-leaves-out-and-for-which-interfaces)).
That was timed on the same day with `bpftrace` 0.20.2. One round of 1500
frames was run for each interface on its own, wrapped in this script, which
adds up the time spent in the two functions that do the work:

```
kprobe:mon_bin_event  { @bs[cpu] = nsecs; @bt[cpu] = arg2; }
kretprobe:mon_bin_event /@bs[cpu]/ {
  @ring_ns[@bt[cpu]] = sum(nsecs - @bs[cpu]); @ring_events[@bt[cpu]] = count();
  delete(@bs[cpu]); delete(@bt[cpu]);
}
kprobe:mon_text_event { @ts[cpu] = nsecs; @tt[cpu] = arg2; }
kretprobe:mon_text_event /@ts[cpu]/ {
  @text_ns[@tt[cpu]] = sum(nsecs - @ts[cpu]); @text_events[@tt[cpu]] = count();
  delete(@ts[cpu]); delete(@tt[cpu]);
}
```

```bash
sudo USBTOP_NG_BENCH_ROUNDS=1 USBTOP_NG_BENCH_FRAMES=1500 \
    USBTOP_NG_BENCH_INTERFACES=mmap-ring bpftrace kernel-side.bt \
    -c "$PWD/target/release/deps/usbtop_ng-<hash> capture_interface_benchmark --ignored --nocapture"
```

The key of each total is the event type: 83 is `S`, a submission, and 67 is
`C`, a completion. `mon_bin_event` serves the mmap ring and read() alike but
takes a different path through the ring for each, which is why each was
traced in a run of its own.

| Camera | Interface | Per submission | Per completion | In the run | % of a core |
| --- | --- | ---: | ---: | ---: | ---: |
| built-in, YUYV | `mmap-ring` | 2.53 µs | 15.00 µs | 220.0 ms of 50.17 s | 0.44 |
| built-in, YUYV | `binary` | 2.49 µs | 14.65 µs | 215.1 ms of 50.17 s | 0.43 |
| built-in, YUYV | `text` | 2.56 µs | 5.27 µs | 98.3 ms of 50.16 s | 0.20 |
| external, MJPEG | `mmap-ring` | 3.04 µs | 13.99 µs | 108.7 ms of 25.38 s | 0.43 |
| external, MJPEG | `binary` | 2.88 µs | 14.37 µs | 110.2 ms of 25.39 s | 0.43 |
| external, MJPEG | `text` | 3.06 µs | 4.50 µs | 48.3 ms of 25.40 s | 0.19 |

These times include the tracer's own cost of entering and leaving each
probe, which is the same for every event and is not known here, so each is an
upper bound. One run per interface and camera, and not the runs of the
tables above. The readers' own figures in the traced runs matched the
untraced ones (0.11%, 3.94% and 0.40% of a core on the first camera).

Adding the two, as a share of one core:

| Camera | Interface | Reader thread | Kernel side | Together |
| --- | --- | ---: | ---: | ---: |
| built-in, YUYV | `mmap-ring` | 0.10 | 0.44 | 0.54 |
| built-in, YUYV | `binary` | 3.94 | 0.43 | 4.37 |
| built-in, YUYV | `text` | 0.40 | 0.20 | 0.60 |
| external, MJPEG | `mmap-ring` | 0.09 | 0.43 | 0.52 |
| external, MJPEG | `binary` | 3.88 | 0.43 | 4.31 |
| external, MJPEG | `text` | 0.39 | 0.19 | 0.58 |

### What the development host's runs show

- **The capture took the same time whatever read usbmon.** Every run of the
  first camera took between 99.95 and 100.07 s and every run of the second
  between 50.32 and 50.39 s, the no-reader runs included.
- **No loss was observed.** No frame was skipped. The kernel reported no
  dropped event on any interface: not the ring, for the mmap ring and
  read(), and not the bus's `text_lost`, for text. Every reader run of a
  camera counted the same number of camera events: 50018 in all twelve runs
  of the first camera and 25184 in all twelve of the second.
- **read() costs the most by a wide margin.** Against the mmap ring, whose
  kernel side measured the same to within a few percent, its reader used
  about 38 times the CPU on the first camera and about 41 times on the
  second: just under 4% of a core against 0.10%. About two thirds of that
  was system time.
- **The mmap ring and text are too close to rank.** The text reader used
  about four times the mmap reader's CPU (0.40% against 0.10% of a core), but
  the kernel does more of the ring's work outside the reader, and by about
  as much: together they came to 0.54% against 0.60% on the first camera and
  0.52% against 0.58% on the second. The kernel figures are upper bounds
  from separate runs, so a difference that small between those sums is not a
  result. What separates the two interfaces is what they deliver, not what
  they cost: text estimates isochronous rates.
- **Taken together, read() cost about 4.3% of a core, the mmap ring about
  0.5% and text about 0.6%**: read() roughly seven to eight times the other
  two.
- **All three are small on this machine at webcam rates.**
- **The cost did not follow the byte rate.** Both cameras produced 500
  events a second, and each interface's reader cost nearly the same share
  of a core on both, although the second camera sent a fifteenth of the
  bytes per second. The event rate was the same on both, so nothing here
  shows how the cost grows with it; and at this rate part of every reader's
  figure is its twenty wake-ups a second, whether or not anything arrived.
- **The kernel copies about as much for the small stream as for the large
  one.** A completion cost the ring's kernel side 14 to 15 µs on both
  cameras. For an isochronous transfer coming in, usbmon's binary interface
  captures the transfer buffer from its start to the end of the last packet
  that carried anything (`mon_bin_collate_isodesc` in
  `drivers/usb/mon/mon_bin.c`), gaps between packets included, so a camera
  sending small packets can leave nearly as much to capture as one filling
  them. The read() reader then has to read all of it out of the kernel to
  reach the next header, 512 bytes at a time (`DRAIN_CHUNK` in
  `src/usbmon/binary.rs`), which the mmap reader never does. That is the
  code's account of read()'s figure, and it fits the timings; no profile of
  the reader was taken to confirm it.
- **The machine figure points the same way for read() and resolves nothing
  else.** With no reader at all the machine's busy time ranged over 9.0 s
  between runs of the first camera and 2.0 s between runs of the second.
  read() had the highest mean on both cameras, 3.25 and 6.40 points of a core
  above the no-reader runs, around the 4.3 that its reader and kernel figures
  add up to; but its range overlaps text's on the first camera and both of
  the others' on the second. The mmap ring came out below no reader at all on
  the first camera, which only noise can do.

### 2026-11-22, a second host

`tgl-tb4` in [TESTING.md](TESTING.md): a laptop with an Intel Core i5-1135G7,
8 logical cores, kernel 7.0.0-43-generic, the same test binary as above,
copied over. It was on mains power with the `powersave` frequency governor,
and idle apart from the benchmark, which is why its machine figures are
small and tight where the development host's are not. Four runs of each
interface; each figure is the mean of the four.

**Built-in UVC webcam, USB 2 high speed.** 3000 frames of uncompressed YUYV
640x480 at 30 fps (18.3 MB/s), with `exposure_dynamic_framerate` pinned to 0.
The camera shares its bus with hubs, storage, a Bluetooth adapter and other
devices: of the roughly 51,230 events in each run, 790 to 798 were theirs.

| Interface | Elapsed s | Frames/s | Frames skipped | Camera events per run | Kernel drops | Reader CPU s | Reader, % of a core | Reader, system % | Machine CPU s | Machine, above none |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| none | 100.76 | 29.77 | 0 | - | - | - | - | - | 2.66 (2.50 to 2.83) | - |
| `mmap-ring` | 100.74 | 29.78 | 0 | 50442 | 0 | 0.178 (0.173 to 0.185) | 0.18 | 68 | 2.79 (2.48 to 3.11) | +0.13 |
| `binary` | 100.75 | 29.78 | 0 | 50442 | 0 | 4.414 (4.361 to 4.491) | 4.38 | 82 | 7.06 (6.96 to 7.25) | +4.36 |
| `text` | 100.74 | 29.78 | 0 | 50433 | 36 | 0.590 (0.580 to 0.598) | 0.59 | 66 | 3.17 (2.98 to 3.40) | +0.50 |

The columns are the ones of the first host's tables. Text's 36 kernel drops
were 9 in each of its four runs. The two ring readers spent 0.009 to 0.012 s
of CPU outside each capture; text spent 0.001 s or less.

The kernel's side, timed the same way as on the first host (`bpftrace`
0.20.2, one round of 1500 frames for each interface on its own):

| Interface | Per submission | Per completion | In the run | % of a core |
| --- | ---: | ---: | ---: | ---: |
| `mmap-ring` | 4.61 µs | 22.97 µs | 354.7 ms of 50.56 s | 0.70 |
| `binary` | 4.33 µs | 23.08 µs | 352.2 ms of 50.47 s | 0.70 |
| `text` | 4.13 µs | 8.56 µs | 163.1 ms of 50.47 s | 0.32 |

The same caution applies: each time includes the tracer's own cost and is an
upper bound. The readers' own figures in the traced runs matched the
untraced ones (0.19%, 4.45% and 0.59% of a core), and text lost its 9 events
in the traced run too.

Adding the two, as a share of one core, beside the first host's figures for
the same workload:

| Interface | Reader thread | Kernel side | Together | Together, the development host |
| --- | ---: | ---: | ---: | ---: |
| `mmap-ring` | 0.18 | 0.70 | 0.88 | 0.54 |
| `binary` | 4.38 | 0.70 | 5.08 | 4.37 |
| `text` | 0.59 | 0.32 | 0.91 | 0.60 |

### What the second host's runs show

- **The same picture as the first host, at a higher cost.** The capture took
  the same time whatever read usbmon: every run was between 100.71 and
  100.85 s. read() cost the most by a wide margin, about 5.1% of a core
  against about 0.9% for each of the other two, between five and six times
  as much. The mmap ring and text were again too close to rank: 0.88 against
  0.91. Every figure is higher than the first host's, the readers' by a
  factor of 1.1 (read()) to 1.7 (the mmap ring) and the kernel's by about
  1.6, on a slower processor running a power-saving governor.
- **The read() reader used about 25 times the mmap reader's CPU**, and the
  text reader about 3.3 times, against about 38 and 4 on the first host.
- **Text lost events, and the other two did not.** In every text run, the
  four of the table and the traced one, the bus's `text_lost` grew by 9 and
  the reader counted 9 camera events fewer than the ring readers did: 50433
  against 50442. The mmap ring and read() lost nothing in any run. No frame
  was skipped on any interface, since the reader only watches.
- **The loss is a burst at camera start meeting the text interface's small
  queue.** The bus was traced once more outside the benchmark, with a reader
  that blocks in read() and so takes each event as it arrives (`cat` on the
  bus's text file, which lost nothing). In the 50 ms in which the stream
  started, 96 events arrived, against 24 to 30 in every 50 ms after it; the
  densest 50 ms held 106. Half of them were on the camera's interrupt
  endpoint, the rest control transfers and the first isochronous transfers.
  usbtop-ng's text reader sleeps 50 ms whenever it finds nothing queued, and
  the kernel holds at most 93 events for it (see [Loss](#loss)): a burst of
  about 102 between two polls loses 9. The first host's camera lost nothing
  in any run, so its start-up, which was not traced, stayed inside the
  queue.
- **That makes text's limit a rate, not only a burst.** By the same
  arithmetic, 93 events every 50 ms is about 1,860 events a second: a bus
  steadily busier than that would lose events on the text interface between
  every two polls. No run here tested that; both cameras produced about 500
  a second. The ring holds far more, and neither ring reader lost anything.
- **The machine figure is clean here and still does not show the kernel's
  side.** With nothing else running, the no-reader runs ranged over 0.33 s
  of busy time in 100 s. Above them, the machine's figure came to +0.13
  points of a core for the mmap ring, +4.36 for read() and +0.50 for text,
  which is each reader thread's own figure (0.18, 4.38 and 0.59) or a little
  under it, with no sign of the 0.3 to 0.7 points the tracer put on the
  kernel's side. That fits a counter that does not time interrupt-context
  work (see [above](#what-the-readers-cpu-leaves-out-and-for-which-interfaces)),
  and it fits a tracer whose own cost is much of what it reports; these runs
  cannot tell the two apart. So the "together" figures stay upper bounds, and
  the machine's own counter supports read()'s reader cost and nothing finer.

### 2026-11-23, the text reader after the fix

The text reader used to sleep 50 ms whenever it found the kernel's queue
empty, and lost whatever a burst added beyond the queue's 93 events
meanwhile. It now waits for the next event inside `read()`, where the kernel
wakes it, and pauses between batches only while events flow steadily, for at
most 10 ms and by the rate it just saw
(`docs/superpowers/specs/2026-11-23-text-reader-blocking-read-design.md`).

**The two webcams again.** The same workload on the same two hosts, four
runs of the text interface on each, with the mmap ring's figure from the
earlier runs beside it.

| Host | Interface | Camera events per run | Kernel drops | Reader CPU s | Reader, % of a core | Kernel side | Together |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| development host | `text`, as it was | 50018 | 0 | 0.399 (0.395 to 0.401) | 0.40 | 0.20 | 0.60 |
| development host | `text`, now | 50018 | 0 | 0.781 (0.775 to 0.787) | 0.78 | 0.20 | 0.98 |
| development host | `mmap-ring` | 50018 | 0 | 0.104 (0.101 to 0.106) | 0.10 | 0.44 | 0.54 |
| `tgl-tb4` | `text`, as it was | 50433 | 36 | 0.590 (0.580 to 0.598) | 0.59 | 0.32 | 0.91 |
| `tgl-tb4` | `text`, now | 50442 | 0 | 1.359 (1.340 to 1.386) | 1.35 | 0.32 | 1.67 |
| `tgl-tb4` | `mmap-ring` | 50442 | 0 | 0.178 (0.173 to 0.185) | 0.18 | 0.70 | 0.88 |

"Kernel side" is the traced figure from the earlier sections. It was taken
with the reader as it was; what usbmon does when a transfer is submitted or
completes does not depend on how the reader waits. On the development host
the first run after the camera's control was pinned counted 50022 camera
events, four more than the rest, on the mmap ring as well as on text: the
camera's doing, not a reader's.

- **Text loses nothing here now.** On `tgl-tb4` it counts the 50442 camera events
  the ring readers count, in all four runs, and the bus's `text_lost` does
  not move. On the machine's own CPU counter the text runs came to +1.11
  points of a core above the no-reader runs, against +0.50 before.
- **It costs about twice what it did.** At a webcam's rate the reader's
  pauses are 10 ms, so it is woken up to a hundred times a second, against
  twenty before, and wake-ups are what a reader costs at that rate. A first build that woke for every event,
  about five hundred times a second, cost 1.5% of a core on the development
  host and 3.1% on `tgl-tb4`, and was not kept.
- **That settles the ranking the earlier runs left open.** With the kernel's
  side added, the mmap ring comes to about 0.5% and 0.9% of a core on the
  two hosts, text to about 1.0% and 1.7%, and read(), as it was that
  morning, to about 4.3% and 5.1%. The next section changes read()'s figure.

**A USB SSD read in small commands.** `pi-400` in [TESTING.md](TESTING.md):
a Raspberry Pi 400, four Cortex-A72 cores, kernel 6.18.60, with an NVMe
drive behind a USB 3 adapter on UAS. The benchmark itself needs a camera, so
this is the shipped program run for one eight-second window
(`usbtop-ng --once --json --window 8`) while `dd` read 164 MB from the drive
in 40,000 direct 4 KiB reads, which took 4.1 s. That is about 240,100
events, or 59,000 a second while it lasted: thirty times the 1,860 a second
at which the old reader's sleep began to cost events. To put the program on
the text interface its device nodes were hidden from it (a mount namespace
with something unopenable bound over each `/dev/usbmonN`).

| Reader | Runs | Bytes reported for the drive | Events the kernel dropped (`text_lost`) | CPU, the whole program |
| --- | ---: | ---: | ---: | ---: |
| `mmap-ring` | 1 | 164,480,256 | - | 1.5 s |
| `text`, as it was | 2 | 8,495,408 and 8,437,744 | 227,796 and 227,920 | 1.3 s |
| `text`, now | 55 | 164,455,392 to 164,480,224 (five runs) | none in 42 runs; 38 to 780 in the other 13 | 3.7 to 3.8 s |

The bytes `dd` read were 163,840,000; the rest is the drive's status replies
and the root file system, which lives on that drive. "CPU" is the user and
system time of the whole program over the window, start-up and device
discovery included, not the reader thread alone. Bytes and CPU were recorded
for five of the 55 runs and the dropped events for all of them.

- **The old reader saw a twentieth of this traffic** and dropped the other
  95% of the events, which is the arithmetic of a 93-event queue emptied
  every 50 ms.
- **The reader as it is now dropped nothing in three runs out of four, and
  at most 0.3% in the rest**: 780 events of 240,100 in the worst run, 38 to
  407 in twelve others. When it dropped nothing it reported the mmap ring's
  byte count to within 100 bytes.
- **This rate is about where text stops keeping up on this board.** At
  59,000 events a second the reader does not pause at all (it pauses only
  below 16,000), so what is lost is lost to the reader thread being away
  from the interface for the millisecond and a half the queue takes to
  fill, or to the kernel's wake-up of it arriving late. A build that did
  take its quarter-millisecond pauses at this rate used less CPU (3.0 s) and
  dropped events in 13 runs of 35, against 13 of 55 without them, which
  leans towards not pausing and does not settle it.
- **It is the dearest way to read a bus this busy**: two and a half times
  the mmap ring's CPU for nearly the same figures, a `read()` and a line of
  text to parse for every event. The mmap ring reported no drops here.

### 2026-11-23, the read() reader after the fix

The read() reader used to take an event's 48-byte header in one `read()` and
then read the captured payload behind it 512 bytes at a time, to reach the
next header. The first host's write-up above gave that as the code's account
of read()'s cost and said no profile had confirmed it. The kernel gives one
`read()` one whole event when the buffer has room for it (`mon_bin_read` in
`drivers/usb/mon/mon_bin.c`), so the reader now asks for 128 KiB at every
read and takes an event, header and payload, in one call
(`docs/superpowers/specs/2026-11-23-binary-reader-one-read-per-event-design.md`).

**The system calls, counted.** One run of 300 frames of the first camera on
the development host, for each reader, under `strace -c -f`. A traced
program runs many times slower, so only the counts mean anything:

| Reader | `read()` calls in the process | Events delivered |
| --- | ---: | ---: |
| read(), as it was | 492,345 | 5,128 |
| read(), now | 7,654 | 5,124 |

The counts take in the benchmark's own reads of `v4l2-ctl`'s output and the
reader's empty-handed reads, twenty a second. The old reader made 96 calls
for every event, which is about 190 for every completed transfer, since a
submission carries no payload.

**The two webcams again.** The same workload on the same two hosts, four
runs of read() on each, beside the other two interfaces' figures from the
sections above.

| Host | Interface | Camera events per run | Kernel drops | Reader CPU s | Reader, % of a core | Kernel side | Together |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| development host | `binary`, as it was | 50018 | 0 | 3.938 (3.929 to 3.950) | 3.94 | 0.43 | 4.37 |
| development host | `binary`, now | 50018 | 0 | 0.467 (0.457 to 0.478) | 0.47 | 0.43 | 0.90 |
| development host | `mmap-ring` | 50018 | 0 | 0.104 (0.101 to 0.106) | 0.10 | 0.44 | 0.54 |
| development host | `text`, now | 50018 | 0 | 0.781 (0.775 to 0.787) | 0.78 | 0.20 | 0.98 |
| `tgl-tb4` | `binary`, as it was | 50442 | 0 | 4.414 (4.361 to 4.491) | 4.38 | 0.70 | 5.08 |
| `tgl-tb4` | `binary`, now | 50442 | 0 | 0.611 (0.597 to 0.632) | 0.61 | 0.70 | 1.31 |
| `tgl-tb4` | `mmap-ring` | 50442 | 0 | 0.178 (0.173 to 0.185) | 0.18 | 0.70 | 0.88 |
| `tgl-tb4` | `text`, now | 50442 | 0 | 1.359 (1.340 to 1.386) | 1.35 | 0.32 | 1.67 |

"Kernel side" is again the traced figure from the earlier sections: what
usbmon does when a transfer is submitted or completes does not depend on how
the reader reads. On the development host the first run after the camera's
control was pinned counted 50022 camera events, as it did for the other
interfaces.

- **The reader costs about an eighth of what it did**: 0.47% of a core
  against 3.94% on the development host, 0.61% against 4.38% on `tgl-tb4`.
  It counts the same camera events and the ring dropped nothing, before or
  after. On `tgl-tb4`, whose own CPU counter is clean, the read() runs came
  to +0.79 points of a core above the no-reader runs of the day before,
  against +4.36.
- **About two thirds of what is left is the copy.** About nine tenths of
  the reader's time is now system time, and it is still four and a half
  times the mmap reader's on the development host and three and a half
  times on `tgl-tb4`, for the same events from the same ring: every captured
  byte is copied out to the reader, which throws it away, and the mmap
  reader copies 48 bytes an event. One run of 900 frames on the development
  host was profiled with `perf record -g` at about 10,000 samples a second.
  Of the reader thread's 1,672 samples, over its whole life, 64% were under
  the kernel's copy of the payload to the reader (`copy_from_buf`, called
  from `mon_bin_read`), 4% elsewhere in `mon_bin_read`, 9% in the ring's
  ioctls, most of it allocating the enlarged ring before the capture and
  freeing it after, and 4% in going to sleep and waking.
- **A larger buffer does not change it.** 64 KiB, 128 KiB and 1 MiB came to
  0.49%, 0.47% and 0.47% of a core on the development host, over four runs
  of 600 frames each.
- **With the kernel's side added, the mmap ring comes to about 0.5% and 0.9%
  of a core on the two hosts, read() to about 0.9% and 1.3%, and text to
  about 1.0% and 1.7%.** On the development host read() and text are within
  a tenth of a point of each other, which these sums cannot rank; on
  `tgl-tb4` read() is the lower.

**A USB SSD.** `pi-400` again, and again the shipped program for one
eight-second window (`usbtop-ng --once --json --window 8`) while `dd` read
the drive. To hold the program to read(), a preloaded library made its
`mmap` of a character device fail, a test harness that is not in the tree;
the program then warned that the ring could not be used and reported
`"source": "binary"`. Three runs of each read() reader and two of the mmap
ring for each workload.

| Workload | Reader | Bytes reported for the drive | Kernel drops | CPU, the whole program |
| --- | --- | ---: | ---: | ---: |
| 300 direct reads of 1 MiB, about 305 MB/s | read(), as it was | 314,582,480 to 314,582,848 | 0 | 1.17 to 1.36 s |
| | read(), now | 314,582,480 to 314,582,528 | 0 | 0.47 to 0.49 s |
| | `mmap-ring` | 314,582,480 and 314,582,496 | 0 | 0.28 and 0.29 s |
| 40,000 direct reads of 4 KiB, about 59,000 events a second | read(), as it was | 164,480,080 to 164,480,192 | 0 | 2.32 to 2.46 s |
| | read(), now | 164,480,080 to 164,480,096 | 0 | 1.99 to 2.03 s |
| | `mmap-ring` | 164,480,080 and 164,480,144 | 0 | 1.46 and 1.47 s |

The bytes `dd` read were 314,572,800 and 163,840,000; the rest is the
drive's status replies and the root file system, which lives on that drive.
"CPU" is the user and system time of the whole program over the window.

- **Every reader reported the same traffic, and none dropped an event.**
  The ring holds megabytes; neither the old loop nor the new one fell behind
  it here.
- **Transfers longer than the buffer are read a buffer at a time.** The
  drive's 1 MiB reads arrive as transfers of 524,288 bytes, each captured
  whole. Traced once with `strace`, each took the new reader four reads of
  131,072 bytes and a fifth of 48; the old loop's 512-byte chunks come to
  1,025 reads for the same transfer.
- **The gain was larger where more bytes were captured.** On the large
  transfers the program's CPU fell to about four tenths of what it was. On
  the 4 KiB commands, where an event carries little, it fell by a sixth, to
  within 0.6 s of what the mmap ring's runs took for the same traffic. No
  profile was taken on this board, so these runs do not say where the rest
  goes.
