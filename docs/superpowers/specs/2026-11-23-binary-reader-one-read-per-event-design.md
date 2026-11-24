# The read() reader takes an event in one read

## The defect

The capture interface benchmark put the read() fallback at about 4% of one
core at a webcam's rate, against 0.1% for the mmap reader on the same ring
(`docs/BENCHMARK.md`). The benchmark's write-up named the likely cause and
said no profile had confirmed it; the roadmap had carried the same suspicion
since the audit of 2026-09-07, with the instruction to measure before
changing anything.

The cause is how the reader gets past an event's captured payload.
`BinaryReader` read the 48-byte header in one `read()`, then read the
`len_cap` bytes that follow it 512 bytes at a time (`DRAIN_CHUNK`) into a
scratch buffer, because the next header starts right after them. A
high-speed camera's isochronous transfer is captured whole, 70 to 96 KiB of
it, so one completion cost about 190 system calls.

Measured on the development host, one ten-second run of the camera under
`strace -c`: 492,345 `read()` calls for 5,128 events, 96 for each. That is
the profile the benchmark's write-up lacked.

## What the kernel does with a read

From `mon_bin_read` in `drivers/usb/mon/mon_bin.c` (read at v6.12):

- One `read()` returns bytes of one event and never of two. It copies what
  is left of the 48 header bytes, then as much of the event's `len_cap`
  payload bytes as the caller's buffer has room for, and returns.
- The event is released from the ring, and the next read starts at the next
  event, only when the last payload byte has been read
  (`rp->b_read >= hdrbytes + ep->len_cap`). A short read leaves the event
  where it is and the next read carries on inside it.
- On an empty ring a non-blocking read returns `EWOULDBLOCK`. It cannot
  return that in the middle of an event: an event being read is still in the
  ring.

So a read at an event boundary with a buffer of at least `48 + len_cap`
bytes returns the whole event, and `len_cap` is in the bytes it returned.

`mon_bin_event` bounds `len_cap` at a fifth of the ring plus 2,048 bytes of
isochronous packet descriptors. On the enlarged ring that bound is
megabytes; what a device actually sends in one transfer is what matters.

## Goal

An event costs the read() reader one system call, wherever its header and
payload fit the buffer, and one more for each further buffer's worth where
they do not. Nothing else about the reader changes: the same events in the
same order, the same handling of an unknown event type, of a truncated
capture, of an empty ring and of a stop request, and the same kernel drop
count.

Non-goals: the 50 ms sleep on an empty ring (the ring holds megabytes, and
no run has lost an event to it); the mmap reader; making read() as cheap as
the mmap ring, which it cannot be while the payload is copied out.

## Approaches

1. **A larger drain chunk.** Keep the header read and the drain, raise
   `DRAIN_CHUNK`. The smallest change. Two system calls for every event with
   a payload where one will do, and it keeps a read loop written as if a
   read could not span the header and the payload.
2. **One read for the header and the payload.** Ask for a buffer's worth at
   every read and cut what comes back into events. One system call per
   event. The framing has to cope with reads that end anywhere, because the
   same reader replays fixture files, where a read returns many events, and
   the tests drive it over a FIFO, where it returns part of one.
3. **`MON_IOCX_GET` with a zero-length data buffer.** The ioctl copies the
   header, copies `min(len_cap, alloc)` payload bytes, and releases the
   event, so with `alloc = 0` nothing but the header leaves the kernel. One
   system call per event and no payload copy at all. But the session is on
   this fallback because the ring's ioctl or its mmap was refused
   (`MmapReader::probe`), and where ioctls are refused this one is too: it
   would need read() beneath it anyway, as a second path to keep right, for
   the one case of an mmap refused and ioctls allowed.

**Chosen: 2.** It removes the system calls, which is the measured cost, and
keeps one path. What it leaves is the copy of the payload out of the kernel;
the results below say what that is worth.

## Design

All in `src/usbmon/binary.rs`.

- `READ_LEN`, 128 KiB: the size of the one buffer, allocated once per
  `read_packets` call, and the most asked of a read. It holds a high-speed
  camera's largest isochronous transfer (32 packets of 3,072 bytes) with its
  descriptors and header. 64 KiB, 128 KiB and 1 MiB were each timed on the
  camera and came out the same within the noise; 128 KiB is the smallest of
  them that takes that transfer in one read.
- `Frames` owns the buffer and the framing. `space()` is where the next read
  goes: the whole buffer, after the bytes of an unfinished header have been
  moved to the front. `filled(n, each)` takes in the `n` bytes just read and
  hands `each` every event they complete. Its state is the unread bytes
  (`buf[start..end]`) and the event whose payload is still arriving
  (`open`: the parsed packet, or none for a type that is skipped, and the
  payload bytes not yet seen).
- An event is delivered when the last byte of its payload has gone by, as
  before, so an event that the end of a file cuts short is still not
  delivered.
- `read_from` is the loop, generic over `Read` so that the tests can count
  reads: the stop flag, the statistics once per `POLL_INTERVAL`, one read
  into `space()`, then `filled`. End of file, `WouldBlock`, `Interrupted`
  and a real error are handled as they were.
- The stop flag is looked at before every read. The old loop looked at it
  before every 512-byte chunk for the same reason: a source that always has
  more never lets the reader reach the sleep where it would otherwise look.
  It is also looked at before each event is handed to the callback, because
  one read of a file can hold thousands of events (see "What the review
  changed").
- The statistics are read at most once per `POLL_INTERVAL` while the loop
  runs and once more when it ends, both inside `read_from`, where a test can
  count them.
- Nothing is allocated for an event. A header that claims a 4 GiB payload
  costs reads, not memory.

No caller changes. `fill`, `drain`, `Fill` and `DRAIN_CHUNK` go.

## Testing

The failing test came first: a stand-in for the device that, like
`mon_bin_read`, gives one read the bytes of one event, and counts its reads.
Three events, one with a 3,000-byte payload, took 11 reads on the old loop
and must take 4 (one each, and one that finds the end).

Then, on the same stand-in or on one that cuts a stream into reads of a
fixed size:

- an event that exactly fills the buffer takes one read;
- a payload of two and a half buffers takes three, the event after it is
  found where it starts, and that holds for a skipped event type as for a
  delivered one;
- a stream of six events is cut into reads of 1, 7, 47, 48, 49, 96, 297 and
  4,096 bytes, its whole length and a buffer's length, and gives the same
  five deliveries every time;
- an event whose payload is one byte short at the end of the file is not
  delivered;
- a source that never runs dry is stopped at the read after the flag is
  set;
- the statistics are read by the clock and not by the event.

And, after the review:

- a stop asked for while the first of three events of one read is being
  handled is seen before the second is handed on;
- a header that the end of a full buffer cuts 17 bytes in is put together
  from two reads;
- a payload of two and a half buffers that is one byte short delivers
  nothing;
- an interrupted read, a read that would block and an end of file in follow
  mode, in the middle of a header and of a payload, lose nothing; a read
  that fails for another reason ends the loop with the event under way
  undelivered and no further read;
- a reader that waits three poll intervals and then finds eight events
  reads the statistics after each wait and once after the last read, and no
  more often than the intervals that went by.

The existing tests stay as they are, the FIFO one included, which covers a
`WouldBlock` in the middle of a header and of a payload.

Each of seven deliberate breakages fails at least one of the new tests: no
move of a partial header, no stop check before a read, none before a
delivery, statistics read on every pass, never read periodically, not read
at the end, and a long payload counted as done after its first buffer.

## Results

The tables are in `docs/BENCHMARK.md`, "The read() reader after the fix".

- The same ten-second camera run under `strace -c`: 7,654 `read()` calls in
  the process against 492,345.
- The benchmark, four runs on each webcam host: the reader thread at 0.47%
  of a core on the development host and 0.61% on `tgl-tb4`, against 3.94%
  and 4.38%. The same camera events counted, nothing dropped.
- `pi-400`, the shipped program held to read() while its USB drive was
  read: the same bytes reported by the old loop, the new one and the mmap
  ring, no drops, and the program's CPU down to about four tenths on 1 MiB
  reads and by a sixth on 4 KiB ones. The drive's 512 KiB transfers are the
  live case of a payload longer than the buffer: four reads of 131,072
  bytes and one of 48 for each.
- The test suite, built with `integration`, on `pi-5` (aarch64, 16 KiB
  pages) and on `pi-zero` (ARMv6, 32-bit, static musl).

## Limits

- The payload is still copied out of the kernel, a buffer at a time. On the
  webcam that copy is about two thirds of what remains of the reader's cost
  (64% of the reader thread's samples in one profiled run).
- A payload longer than 128 KiB takes more than one read. A USB 3 drive's
  large transfers do that.
- The reader still sleeps 50 ms when the ring is empty, and reads until it
  is empty again when it wakes.

## What the review changed

Codex and Antigravity reviewed the change adversarially, and the session's
Claude reconciled their findings as the final arbiter.

Taken:

- **The stop flag between the events of one read** (both reviewers). The
  old loop looked at the flag before every event. The first version of this
  one looked before every read, which on the device is the same thing and
  on a file is not: a read of a fixture returns up to 2,730 events, and all
  of them were handed on before the flag was looked at again. The flag is
  now looked at before each delivery.
- **"What is left is the copy" was not measured** (Codex). The benchmark
  established the gain, and a buffer size that made no difference; neither
  says where the rest goes. A profile was taken and the docs give its
  figure.
- **Tests** (both): a header cut by the end of a full buffer; a long
  payload cut short; reads that bring nothing, and a read that fails, in
  the middle of an event; and the statistics test, which had only an upper
  bound and so passed with no periodic read at all. The final read of the
  statistics moved from `read_packets` into `read_from` so that a test can
  count it.
- The comment on `Frames`' unread bytes said they are shorter than a header
  whenever `filled` returns; that holds only when it returns to go on.

Declined:

- **A callback error in the middle of a batch is untested** (Antigravity).
  `callback_error_stops_reading`, which the change did not touch and the
  reviewer was not shown, writes two events to a file, fails the first
  callback and asserts one delivery; both events now arrive in one read.

The scoped re-review by both engines confirmed each fix and the declined
item, and added two things, both taken:

- **The new statistics test counted reads of the statistics and allowed
  four or five** (both). Two stalls of a whole interval would have failed
  it for no fault, a final read moved to the start of the loop would have
  passed it, and so would a clock that is never reset. It now records where
  in the script each read of the statistics fell, requires one after each
  wait and one after the last read, and bounds the total by the intervals
  that went by, with events after the waits so that sampling on every pass
  shows.
- **The SSD write-up said where the program's remaining CPU goes on 4 KiB
  commands** (Codex), which those runs did not measure. It now gives the
  figures and says no profile was taken on that board.
