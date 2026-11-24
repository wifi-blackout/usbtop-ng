# The text reader waits in read(), not in a sleep

## The defect

The capture interface benchmark found it on `tgl-tb4` (see
`docs/BENCHMARK.md`, "What the second host's runs show"): on the debugfs text
interface, every run lost nine events when the camera started.

The cause is the reader's wait. `UsbmonReader::read_packets` opens the `Nu`
file non-blocking and, when a read finds nothing queued, sleeps
`POLL_INTERVAL` (50 ms) before it reads again. The kernel holds at most
`EVENT_MAX` events for a text reader, four pages of event records, which is
93 on a 64-bit kernel with 4 KiB pages (`drivers/usb/mon/mon_text.c`), and
drops what arrives when that queue is full. So whatever arrives beyond 93
events inside one sleep is lost: a burst from an idle bus, as on that camera,
or any bus steadily busier than about 1,860 events a second.

The two ring interfaces sleep the same way and do not have the defect at
these rates, because the ring holds far more than 93 events.

## Goal

The text reader loses no event to its own waiting. It is woken when an event
arrives, and it still stops within one `POLL_INTERVAL` of being asked to, on a
silent bus too, which is the property the non-blocking open was put there for
(`open_nonblocking` in `src/usbmon/mod.rs`).

Non-goals: a larger kernel queue (not ours to change); the ring readers; a
reader fast enough for every bus (one `read()` per event still bounds what
text can carry, and `text_lost` is still the kernel's to count).

## Approaches

1. **A shorter sleep.** Poll every 5 or 10 ms. The smallest change, and it
   only moves the limit: 93 events per sleep is 9,300 a second at 10 ms, a
   burst can still exceed it, and an idle reader wakes a hundred times a
   second for nothing. Rejected as the fix, kept as the fallback below.
2. **A blocking read that the stopper interrupts.** Block in `read()`; the
   kernel wakes the reader on each event (`wake_up(&rp->wait)` in
   `mon_text_event`). To stop, signal the thread. No loss to waiting, but
   every caller that sets the shutdown flag must also send the signal, and
   one that forgets leaves a thread blocked forever on a silent bus. That
   changes the stop contract of `MonitorHandle`, the benchmark and every
   test.
3. **A blocking read with its own tick.** Block in `read()`, and have a
   per-thread interval timer deliver a signal to the reader thread every
   `POLL_INTERVAL`. A blocked `read()` on the text file returns `EINTR` on a
   signal (`mon_text_read_wait` returns `-EINTR` when `signal_pending`), the
   loop looks at the shutdown flag and reads again. The reader is woken per
   event, checks for shutdown twenty times a second exactly as it does now,
   and the whole mechanism lives inside the reader: no caller changes.

**Chosen: 3**, with a short pause between batches that follows the event
rate (below). It removes the loss, keeps the shutdown contract, and fails
safe: if the timer cannot be set up, the reader stays non-blocking and polls,
as approach 1.

## Design

### `src/usbmon/tick.rs` (new)

`Ticker::start(interval) -> io::Result<Ticker>` does three things for the
calling thread, in this order:

1. Once per process, installs a handler that does nothing for one real-time
   signal, `SIGRTMIN + 0` as glibc or musl reports it, with `sa_flags = 0`:
   no `SA_RESTART`, so a blocked call fails with `EINTR` rather than being
   restarted (signal(7); a FIFO in the tests would otherwise restart). The
   signal's disposition is read first, and written only when it is the
   default one. If the signal is handled or ignored, by something else in
   the process or by the program that launched it, it is left untouched and
   the ticker refuses. The handler, once installed, is never removed.
2. Unblocks that signal in the calling thread's mask (`pthread_sigmask`): a
   thread inherits its creator's mask, and a blocked tick would never
   interrupt anything. Whether it was blocked is kept, and it is blocked
   again when the ticker is dropped or fails to start.
3. Creates a `CLOCK_MONOTONIC` timer with `SIGEV_THREAD_ID` naming the
   calling thread (`gettid`) and arms it with `it_value = it_interval =
   interval` (timer_create(2), timer_settime(2)).

`Drop` deletes the timer. `Ticker` is `!Send` (a `PhantomData` of a raw
pointer says so, whatever `timer_t` is): it cannot leave the thread it ticks.

The signal is directed at one thread. It interrupts nothing in any other
thread. In the reader thread it can interrupt a blocked system call other
than the read; the ones the thread makes are handled: `std::thread::sleep`
resumes after `EINTR`, and writes through `std::io::Write::write_all` retry.
The timer is periodic and runs for as long as the reader does, not only
around the blocking read: a timer armed and disarmed around each read would
spare a busy reader its twenty signals a second at the cost of two more
system calls per wait, and a one-shot one would lose the wake-up when it
fired before the read began.

### `src/usbmon/reader.rs`

- Open non-blocking as now (a FIFO in the tests would otherwise hang in
  `open` until a writer appears).
- When following a live interface, start a `Ticker` and read the file's
  status flags. If either fails, log one warning naming the cause and never
  block: the loop then sleeps `PAUSE_LIMIT` (10 ms) whenever it finds
  nothing queued. A file read once to its end (a fixture) gets no tick.
- Read with `File::read`, not `BufRead::read_line`: `read_line` retries
  `EINTR` inside the standard library and would never return to look at the
  shutdown flag. The reader keeps its own byte buffer and splits lines at
  `\n`. A line that is not UTF-8 or does not parse is skipped, as before. A
  run of more than 64 KiB with no newline is dropped, up to and including
  its newline.
- The loop, each pass:
  1. Take everything queued, with the file non-blocking, counting the events.
  2. If that was nothing, wait for the next event: clear `O_NONBLOCK` with
     `fcntl(F_SETFL)`, which the text interface consults on every read
     (`file->f_flags & O_NONBLOCK` in `mon_text_read_wait`), and read. A
     tick returns the read with `EINTR`; the loop looks at the shutdown flag
     and reads again, still blocking. An event returns it with the event.
     Then set `O_NONBLOCK` again.
  3. Pause for what `pause_for` allows, given the events of this batch, the
     time since the batch before it, and the pause before this one. Go
     round.
- End of file: as now (sleep `POLL_INTERVAL` and retry when following, stop
  when not; a last line without a newline is parsed when not following).

### The pause

The first build of this had none: it sat in a blocking `read` and was woken
for every event. It lost nothing, and at a webcam's 500 events a second it
used several times the CPU of the sleeping reader it replaced (1.5% of a core
against 0.40% on the development host, 3.1% against 0.59% on `tgl-tb4`). The
cost of a reader at that rate is its wake-ups, about one per event, each
finding a cold cache, and not the work per event.

The pause lets a few events collect. `pause_for(events, cycle, previous)` is
the time in which `BATCH_TARGET` (16) more events arrive at the rate
`events / cycle`, and it is bounded four ways so that it does not bring the
defect back:

- **never more than `PAUSE_LIMIT`** (10 ms);
- **none on a slow bus**: below 200 events a second not even two would
  collect in the longest pause. This is also what keeps a pause away from
  the start of a burst on a quiet bus: the first event after a quiet spell
  is one event over a long cycle. The cycle runs from the batch before, and
  a tick does not restart it;
- **none on a fast bus**: under `PAUSE_FLOOR` (1 ms, which is above 16,000
  events a second) the reader does not stop. A sleep that short overshoots
  by a good part of its own length while the queue fills in a few
  milliseconds;
- **gradual**: never more than twice the pause before plus `PAUSE_STEP`
  (1 ms), so the first pauses after none are at most 1, 3 and 7 ms. A rate
  read off the first two events of a burst is weak evidence.

What is left: once the reader is pausing, a rate that jumps inside one pause
by more than the queue can absorb in that pause loses the excess of that one
pause: more than about 9,300 events a second inside a settled 10 ms pause,
and more inside a shorter one. The next pause is sized by the new rate. A
pause is also only a request: the kernel returns the thread when it can, and
on a loaded machine that is later.

### What changes for a user

`text` no longer drops a burst the reader slept through, nor everything
above about 1,860 events a second. Shutdown latency is unchanged. A reader
on a silent bus is still woken twenty times a second, by the tick now and by
its own sleep before. While events flow at between 200 and 16,000 a second it
is woken up to a hundred times a second, and outside that range once per
event or not at all (it does not stop); the benchmark is rerun to say what
that costs at a webcam's rate.

## Testing

- A reader on a FIFO with a writer attached delivers a line written after an
  idle spell without waiting for a poll: the median delay over fifteen lines
  is under 20 ms. This fails on the sleeping reader, whose median is about
  27 ms.
- A silent interface is waited on in `read` and not polled: over 400 ms the
  loop reads about once per tick, which a 10 ms poll (a hundred a second) and
  a blocking read that did not block (without end) both exceed.
- The first event after a quiet spell is followed by no pause, at twenty
  points in the tick's cycle. This fails if a tick restarts the cycle.
- A reader blocked on an idle FIFO with a writer attached stops when asked
  to (the tests give it seconds; it takes a tick); so does one whose
  descriptor was left blocking, with and without the tick-bounded wait.
- `Ticker`: a blocked read returns `Interrupted`; a dropped ticker interrupts
  nothing; a sleep under a ticker lasts its length and another thread's read
  is left alone; a thread that had the signal blocked gets it back blocked.
- Line assembly: several lines in one read, a line split across reads, a last
  line with no newline, a line that is not UTF-8, an over-long run and a line
  of exactly the limit.
- The polling fallback, driven directly, and its read count.
- `pause_for`: exact values at a webcam's rate, at the limit, at 200 and 16,000
  events a second, after a quiet spell, for no events, and the
  first four pauses after none.
- Four hundred events half a millisecond apart arrive once each and in
  order, waiting in `read` and polled, and the loop pauses while they do.
- Live, on `tgl-tb4`: the benchmark's text runs lose nothing and count the
  same camera events as the ring readers. Live, on `pi-400`: a USB SSD read
  in 4 KiB commands, tens of thousands of events a second, before and after.

## Limits

- One `read()` per event is how the kernel's text interface works. A bus
  that outruns a reader that never sleeps still overflows the queue: on
  `pi-400` that begins at about 59,000 events a second, where a run in four
  drops a few tenths of a percent at most.
- A sandbox that refuses `timer_create` or `sigaction`, or a process in
  which something else handles `SIGRTMIN`, leaves the reader polling every
  10 ms, with a warning. That is five times the headroom of the old 50 ms
  and not a cure.
- A burst that starts inside a pause, on a bus that was already busy, can
  still exceed the queue (see above).
- A regression in the handling of `EINTR` would hang a test in its wait
  rather than fail it, except where the test bounds the wait itself.

## What the review changed

Three engines reviewed the first complete build adversarially (a Claude
subagent, Codex, Antigravity).

Taken:

- **A tick restarted the measure of how long the bus had been quiet** (Codex
  and Antigravity). An event that came within 10 ms of a tick, after any
  length of silence, was followed by a full pause: the defect again, one
  burst in five. The cycle now runs from the last batch, and the tick is
  handled inside the wait.
- **A blocking read that did not block would have spun** (the subagent), and
  no test would have noticed: `WouldBlock` and `EINTR` were one case. They
  are two, the first sleeps, and the loop's read count is tested.
- **A descriptor left blocking could take two ticks to stop** (Codex,
  Antigravity). An interrupted read now always goes back to the flag first.
- **The handler was installed and then checked** (all three). It is read
  first and written only if it is the default one.
- **The thread's signal mask was changed for good** (Codex). It is put back.
- **Sub-millisecond pauses overshoot** (Antigravity): `PAUSE_FLOOR`, first
  at 250 µs and then at 1 ms, after ninety runs on `pi-400` at 59,000
  events a second: events dropped in 13 runs of 35 with 250 µs and in 13 of
  55 with 1 ms. That leans towards 1 ms without settling it; either way the
  reader is at the edge of keeping up there (see `docs/BENCHMARK.md`).
- **`tv_nsec` took `c_long`, which is wrong on x32** (Codex, Antigravity):
  the conversion takes its type from the field.
- **The tests**: the latency test's limit left no room for a loaded single
  core and could not tell waiting from a 10 ms poll; the ticker test's lower
  bound could fail on a working ticker; the left-blocking test did not use
  the production wait; nothing tested a thread that starts with the signal
  blocked. All four changed or added.
- **The documents said a silent bus cost nothing.** It is woken twenty times
  a second, as before. Corrected here, in the code and in
  `docs/ARCHITECTURE.md`; the queue's size is given with the page size it
  holds for; stale lines elsewhere fixed.
- An over-long run's tail was parsed as a line; a fixture read started a
  tick it never needed; `Ticker` was `!Send` by an accident of `timer_t`.

A scoped re-review (Codex) confirmed those and added four. Taken: "ignore" is
not the tick's to replace either, since a launcher can have chosen it, so
only the default disposition is taken; the idle read-count test lost a lower
bound that a descheduled reader could miss; the two mask tests set their own
starting mask and no longer assume the runner's. Declined: delivering a
pending line at the end of a followed file. The usbmon interface has no end;
on a file that grows, the end can fall inside a line, and the line is kept
until its newline arrives.

Declined:

- **"`timer_create` rejects `SIGEV_THREAD_ID` on musl, so the fix is dead
  code on the ARMv6 build"** (Antigravity, as critical). The static musl
  builds for ARMv6 and aarch64 ran the suite on `pi-zero` and `bm1684x`,
  including the tests that unwrap `Ticker::start` and read under it. It
  works there.
- **A stray `mod bench` with no file** (Antigravity): the file exists; the
  reviewer was shown part of the tree.
- **Arm the timer only around the blocking read** (Antigravity): see the
  note under `tick.rs` above.
- **Scan for a free real-time signal** (Antigravity): untestable in one
  process, and the fallback already covers a taken signal.
- **Check the shutdown flag before every sleep** (Antigravity): each is
  bounded by `POLL_INTERVAL` already, as the sleeping reader's were.
