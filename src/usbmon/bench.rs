//! A benchmark of the three usbmon capture interfaces (the mmap ring, read()
//! on `/dev/usbmonN`, and the debugfs text file) against a webcam, which is a
//! constant source of isochronous traffic. Each run streams a fixed number of
//! video frames while one interface reads the camera's bus, and reports how
//! long the capture took and what the CPU did during it. See
//! `docs/BENCHMARK.md` for how to run it and how to read the tables.
//!
//! Test-only code. Everything in this file but the `live` submodule is pure
//! and unit-tested hermetically, so the default `cargo test` covers it. The
//! `live` submodule needs the `integration` feature. It holds the benchmark
//! itself, an ignored test that no gate runs (it needs root, usbmon and a USB
//! camera, and it takes minutes), and six small tests of the system calls
//! and `/proc` files the benchmark reads, which need none of that and run
//! with the feature's other tests.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::monitor::SourceKind;

const ENV_CAMERA: &str = "USBTOP_NG_BENCH_CAMERA";
const ENV_FRAMES: &str = "USBTOP_NG_BENCH_FRAMES";
const ENV_ROUNDS: &str = "USBTOP_NG_BENCH_ROUNDS";
const ENV_FORMAT: &str = "USBTOP_NG_BENCH_FORMAT";
const ENV_WIDTH: &str = "USBTOP_NG_BENCH_WIDTH";
const ENV_HEIGHT: &str = "USBTOP_NG_BENCH_HEIGHT";
const ENV_FPS: &str = "USBTOP_NG_BENCH_FPS";
const ENV_INTERFACES: &str = "USBTOP_NG_BENCH_INTERFACES";

/// What reads the camera's bus during a run. `None` is the control: the
/// camera streams and nothing reads usbmon, which is what the whole-machine
/// CPU figure of the other three is compared against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Interface {
    None,
    Mmap,
    Binary,
    Text,
}

impl Interface {
    const ALL: [Interface; 4] = [
        Interface::None,
        Interface::Mmap,
        Interface::Binary,
        Interface::Text,
    ];

    /// The names the startup log line prints for the three interfaces, taken
    /// from the code that prints them, so a table reads like the log.
    fn label(self) -> &'static str {
        match self {
            Interface::None => "none",
            Interface::Mmap => SourceKind::Mmap.label(),
            Interface::Binary => SourceKind::Binary.label(),
            Interface::Text => SourceKind::Text.label(),
        }
    }

    fn from_label(label: &str) -> Option<Self> {
        Interface::ALL
            .into_iter()
            .find(|interface| interface.label() == label)
    }
}

/// The order of the runs: every chosen interface once per round, each round
/// starting one place further along, so no interface always runs first. As
/// many rounds as there are interfaces put each in every position once; the
/// default three rounds of four interfaces do not.
fn schedule(rounds: u32, interfaces: &[Interface]) -> Vec<(u32, Interface)> {
    let per_round = interfaces.len();
    (0..rounds)
        .flat_map(|round| {
            let shift = usize::try_from(round).map_or(0, |round| round % per_round.max(1));
            (0..per_round).map(move |slot| (round + 1, interfaces[(slot + shift) % per_round]))
        })
        .collect()
}

/// What the camera is asked to stream, and how many times.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Workload {
    camera: PathBuf,
    frames: u32,
    rounds: u32,
    /// The interfaces to run, in the order a round starts with.
    interfaces: Vec<Interface>,
    format: String,
    width: u32,
    height: u32,
    fps: u32,
}

impl Workload {
    /// Reads the settings through `lookup`, which the live benchmark backs
    /// with the environment and the tests with a table. An unset variable
    /// takes its default; a set one that does not make sense is an error that
    /// names the variable and quotes the value.
    fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let count = |name: &str, default: u32| match lookup(name) {
            None => Ok(default),
            Some(value) => match value.trim().parse::<u32>() {
                Ok(n) if n > 0 => Ok(n),
                _ => Err(format!(
                    "{name} must be a whole number above zero, got {value:?}"
                )),
            },
        };

        let camera = lookup(ENV_CAMERA).unwrap_or_else(|| "/dev/video0".to_string());
        if camera.is_empty() {
            return Err(format!(
                "{ENV_CAMERA} must name a video device node such as /dev/video0, got \"\""
            ));
        }

        // The format goes into a comma-separated v4l2-ctl option, so it is
        // held to what a V4L2 format code is: four characters, letters and
        // digits, padded with spaces when the code is shorter (`Y16 `).
        let format = lookup(ENV_FORMAT).unwrap_or_else(|| "YUYV".to_string());
        let well_formed = format.len() == 4
            && format.as_bytes()[0].is_ascii_alphanumeric()
            && format
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b' ');
        if !well_formed {
            return Err(format!(
                "{ENV_FORMAT} must be a four-character V4L2 format code such as YUYV, got {format:?}"
            ));
        }

        // All four by default; a list of the tables' labels picks some, for
        // a run that must hold one interface alone (a kernel trace, say).
        let interfaces = match lookup(ENV_INTERFACES) {
            None => Interface::ALL.to_vec(),
            Some(list) => {
                let mut chosen = Vec::new();
                for label in list.split(',').map(str::trim) {
                    let interface = Interface::from_label(label).ok_or_else(|| {
                        format!(
                            "{ENV_INTERFACES} must be a comma-separated list of none, \
                             mmap-ring, binary and text, got {list:?}"
                        )
                    })?;
                    if !chosen.contains(&interface) {
                        chosen.push(interface);
                    }
                }
                chosen
            }
        };

        Ok(Self {
            camera: PathBuf::from(camera),
            frames: count(ENV_FRAMES, 3000)?,
            rounds: count(ENV_ROUNDS, 3)?,
            interfaces,
            format,
            width: count(ENV_WIDTH, 640)?,
            height: count(ENV_HEIGHT, 480)?,
            fps: count(ENV_FPS, 30)?,
        })
    }
}

/// Where the camera sits on USB, read from sysfs. The readers open this bus's
/// own usbmon node, and the device number picks the camera's events out of
/// whatever else the bus carries.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Camera {
    bus: u8,
    device: u8,
    speed: String,
    product: String,
}

impl Camera {
    /// `class_dir` is `/sys/class/video4linux` and `node` the camera's device
    /// node with every symbolic link already resolved (`/dev/video0`, not a
    /// `/dev/v4l/by-id` name), because sysfs knows the node by its last
    /// component. `<class_dir>/<name>/device` links to the USB interface the
    /// video driver bound to, and that interface's parent directory is the
    /// USB device, which holds `busnum`, `devnum`, `speed` and `product`.
    fn from_sysfs(class_dir: &Path, node: &Path) -> Result<Self, String> {
        let name = node
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| format!("{} does not name a video device node", node.display()))?;
        let link = class_dir.join(name).join("device");
        let interface = std::fs::canonicalize(&link)
            .map_err(|e| format!("could not resolve {}: {e}", link.display()))?;
        let device = interface
            .parent()
            .ok_or_else(|| format!("{} has no parent directory", interface.display()))?;

        // Bus and device numbers start at 1; usbmon's bus 0 is every bus at
        // once, which is not what a per-bus measurement may open.
        let number = |attribute: &str| -> Result<u8, String> {
            let path = device.join(attribute);
            let text = std::fs::read_to_string(&path).map_err(|e| {
                format!(
                    "could not read {}: {e} (the camera must be a USB device)",
                    path.display()
                )
            })?;
            match text.trim().parse::<u8>() {
                Ok(number) if number > 0 => Ok(number),
                _ => Err(format!(
                    "{} holds {:?}, which is not a USB bus or device number",
                    path.display(),
                    text.trim()
                )),
            }
        };
        let text = |attribute: &str| {
            std::fs::read_to_string(device.join(attribute))
                .ok()
                .map(|text| text.trim().to_string())
                .filter(|text| !text.is_empty())
                .unwrap_or_else(|| "unknown".to_string())
        };

        Ok(Self {
            bus: number("busnum")?,
            device: number("devnum")?,
            speed: text("speed"),
            product: text("product"),
        })
    }
}

/// The busy ticks of the whole machine so far, from the aggregate `cpu` line
/// of `/proc/stat`. proc_stat(5) gives the fields as user, nice, system,
/// idle, iowait, irq, softirq, steal, guest, guest_nice, in units of
/// `sysconf(_SC_CLK_TCK)`. Busy is everything a CPU spent running something:
/// user, nice, system, irq, softirq and steal. Idle and iowait are not busy.
/// Guest time is left out because the kernel has already added it to user
/// (`account_guest_time` in `kernel/sched/cputime.c` charges both), so adding
/// it again would count it twice. A kernel too old to have the later fields
/// simply omits them.
fn busy_ticks(stat: &str) -> Option<u64> {
    let line = stat.lines().find(|line| line.starts_with("cpu "))?;
    let fields = line
        .split_whitespace()
        .skip(1)
        .map(|field| field.parse::<u64>().ok())
        .collect::<Option<Vec<u64>>>()?;
    if fields.len() < 4 {
        return None;
    }
    let field = |index: usize| fields.get(index).copied().unwrap_or(0);
    Some(field(0) + field(1) + field(2) + field(5) + field(6) + field(7))
}

/// The time a thread has spent on a CPU so far, from its `schedstat` file.
/// `Documentation/scheduler/sched-stats.rst` gives the file's three fields
/// as the time spent on the CPU in nanoseconds, the time spent waiting on a
/// run queue in nanoseconds, and the number of timeslices run. The first is
/// the scheduler's own `sum_exec_runtime`, user and kernel time together,
/// which is also what `getrusage` scales its user and system times to add up
/// to (`cputime_adjust` in `kernel/sched/cputime.c`).
fn on_cpu_time(schedstat: &str) -> Option<Duration> {
    let nanoseconds = schedstat.split_whitespace().next()?.parse::<u64>().ok()?;
    Some(Duration::from_nanos(nanoseconds))
}

/// The events usbmon's text interface has lost on one bus since the module
/// was loaded, from the bus's statistics file in debugfs
/// (`/sys/kernel/debug/usb/usbmon/<bus>s`), which `mon_stat_open` in
/// `drivers/usb/mon/mon_stat.c` prints as
/// `nreaders 1 events 1145218 text_lost 0`. `mon_text_event` adds to it when
/// a text reader's queue is full or an event record cannot be allocated. The
/// count belongs to the bus: every text reader of that bus adds to the same
/// one.
fn text_lost(statistics: &str) -> Option<u64> {
    let mut words = statistics.split_whitespace();
    while let Some(word) = words.next() {
        if word == "text_lost" {
            return words.next()?.parse::<u64>().ok();
        }
    }
    None
}

/// What `v4l2-ctl --verbose --stream-mmap` reported, one line per frame:
/// `cap dqbuf: 0 seq:      4 bytesused: 614400 ts: 253715.873383 …`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct StreamLog {
    /// Frames handed to the application.
    frames: u32,
    /// Frames the driver numbered and never delivered: the gaps in `seq`.
    skipped: u32,
    /// The sum of `bytesused`.
    bytes: u64,
}

impl StreamLog {
    /// A frame line without a readable sequence number or size is an error
    /// and not a frame: a `v4l2-ctl` that prints these lines some other way
    /// must stop the benchmark, not feed it frames it cannot account for.
    fn parse(log: &str) -> Result<Self, String> {
        let mut stream = Self::default();
        let mut last_sequence: Option<u32> = None;
        for line in log.lines().filter(|line| line.starts_with("cap dqbuf:")) {
            let mut words = line.split_whitespace();
            let mut sequence = None;
            let mut bytes_used = None;
            while let Some(word) = words.next() {
                match word {
                    "seq:" => sequence = words.next().and_then(|w| w.parse::<u32>().ok()),
                    "bytesused:" => bytes_used = words.next().and_then(|w| w.parse::<u64>().ok()),
                    _ => {}
                }
            }
            let (Some(sequence), Some(bytes_used)) = (sequence, bytes_used) else {
                return Err(format!(
                    "could not read a sequence number and a size from the frame line {line:?}"
                ));
            };
            stream.frames += 1;
            stream.bytes += bytes_used;
            if let Some(last) = last_sequence {
                // A sequence that goes backwards is a counter restart, not a
                // gap; it adds nothing.
                let gap = sequence.saturating_sub(last).saturating_sub(1);
                stream.skipped = stream.skipped.saturating_add(gap);
            }
            last_sequence = Some(sequence);
        }
        Ok(stream)
    }
}

/// CPU time of the reader thread over its whole life, from `getrusage`:
/// setting the interface up, the capture, and tearing it down. The total is
/// exact; the split between user and system is the kernel's estimate from
/// what it found the thread doing at its timer ticks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReaderCpu {
    user: Duration,
    system: Duration,
}

impl ReaderCpu {
    fn total(self) -> Duration {
        self.user + self.system
    }
}

/// How a reader's read loop ended.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReaderExit {
    /// Whether the benchmark had already asked the reader to stop when its
    /// loop returned.
    asked_to_stop: bool,
    /// The error the loop returned, if it returned one.
    error: Option<String>,
}

impl ReaderExit {
    /// A reader is only good for a measurement if it read until it was told
    /// to stop. The read() and text readers log a read error, leave their
    /// loop and return `Ok`, so a clean return is not enough: a reader that
    /// stopped by itself part of the way through would look cheap, having
    /// done part of the work.
    fn verdict(&self) -> Result<(), String> {
        if let Some(error) = &self.error {
            return Err(format!("the reader failed: {error}"));
        }
        if !self.asked_to_stop {
            return Err(
                "the reader stopped by itself before the capture ended; its own log line says why"
                    .to_string(),
            );
        }
        Ok(())
    }
}

/// What a reader saw and what it cost. Absent from a no-reader run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReaderFigures {
    /// Events the reader parsed during the capture.
    bus_events: u64,
    /// Those from the camera's device number.
    camera_events: u64,
    /// Events the kernel dropped before the reader got to them, over the
    /// reader's whole life: the ring's own count (`MON_IOCG_STATS`) on the
    /// mmap ring and read(), and on text the growth of the bus's `text_lost`.
    kernel_dropped: u64,
    /// The reader thread's time on a CPU during the capture.
    cpu: Duration,
    /// Its CPU time over its whole life.
    lifetime: ReaderCpu,
}

impl ReaderFigures {
    /// A run in which the reader saw nothing from the camera measured some
    /// other device or nothing: the camera was re-enumerated under a new
    /// device number, or the reader was on the wrong bus.
    fn saw_the_camera(&self) -> Result<(), String> {
        if self.camera_events == 0 {
            return Err(format!(
                "the reader saw {} events and none from the camera's device number; \
                 the camera may have been re-enumerated",
                self.bus_events
            ));
        }
        Ok(())
    }

    /// The share of the reader's whole-life CPU time spent in the kernel, as
    /// a percentage. `None` when the thread used no measurable time.
    fn system_share(&self) -> Option<f64> {
        let total = self.lifetime.total();
        if total.is_zero() {
            return None;
        }
        Some(self.lifetime.system.as_secs_f64() / total.as_secs_f64() * 100.0)
    }

    /// CPU the reader thread used outside the capture: opening the
    /// interface, allocating and mapping the ring, and letting go of it.
    fn outside_the_capture(&self) -> Duration {
        self.lifetime.total().saturating_sub(self.cpu)
    }
}

/// One capture: one interface, one stream of frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Run {
    interface: Interface,
    round: u32,
    /// Wall-clock time of the `v4l2-ctl` run, camera start-up included.
    elapsed: Duration,
    stream: StreamLog,
    reader: Option<ReaderFigures>,
    /// Busy CPU time of the whole machine over the same window, summed over
    /// its cores.
    machine_cpu: Duration,
}

impl Run {
    fn frames_per_second(&self) -> f64 {
        f64::from(self.stream.frames) / self.elapsed.as_secs_f64()
    }

    fn megabytes_per_second(&self) -> f64 {
        self.stream.bytes as f64 / 1e6 / self.elapsed.as_secs_f64()
    }

    /// `cpu` as a percentage of one core over the run: 100 is one core kept
    /// fully busy for the whole capture.
    fn share_of_a_core(&self, cpu: Duration) -> f64 {
        cpu.as_secs_f64() / self.elapsed.as_secs_f64() * 100.0
    }

    fn camera_events_per_second(&self) -> Option<f64> {
        self.reader
            .map(|reader| reader.camera_events as f64 / self.elapsed.as_secs_f64())
    }

    /// Reader CPU per thousand events parsed, in milliseconds. `None` when
    /// there was no reader or it saw nothing.
    fn reader_ms_per_thousand_events(&self) -> Option<f64> {
        let reader = self.reader.filter(|reader| reader.bus_events > 0)?;
        Some(reader.cpu.as_secs_f64() * 1e6 / reader.bus_events as f64)
    }
}

const ABSENT: &str = "-";

fn cell(value: Option<f64>, decimals: usize) -> String {
    value.map_or_else(|| ABSENT.to_string(), |v| format!("{v:.decimals$}"))
}

/// The top of the report: where and on what the figures were taken. Nothing
/// here identifies the host.
fn render_header(
    kernel: &str,
    cores: usize,
    debug_build: bool,
    workload: &Workload,
    camera: &Camera,
) -> String {
    let build = if debug_build {
        "DEBUG build: the CPU figures do not describe the shipped binary"
    } else {
        "release build"
    };
    format!(
        "usbmon capture interface benchmark\n\
         kernel {kernel}, {cores} cores, {build}\n\
         camera: {} on bus {} as device {}, {} Mb/s ({})\n\
         workload: {} frames of {} {}x{} at a nominal {} fps, {} {}",
        camera.product,
        camera.bus,
        camera.device,
        camera.speed,
        workload.camera.display(),
        workload.frames,
        workload.format,
        workload.width,
        workload.height,
        workload.fps,
        workload.rounds,
        if workload.rounds == 1 {
            "round"
        } else {
            "rounds"
        },
    ) + &interfaces_line(workload)
}

/// A fifth header line when only some of the interfaces are being run.
fn interfaces_line(workload: &Workload) -> String {
    if workload.interfaces == Interface::ALL {
        return String::new();
    }
    let labels: Vec<&str> = workload.interfaces.iter().map(|i| i.label()).collect();
    format!("\ninterfaces: {} only", labels.join(", "))
}

/// One finished run as a sentence, printed while the benchmark is still
/// going.
fn render_progress(run: &Run) -> String {
    let capture = format!(
        "round {} {}: {} frames in {:.2} s ({:.2} fps, {} skipped, {:.2} MB/s)",
        run.round,
        run.interface.label(),
        run.stream.frames,
        run.elapsed.as_secs_f64(),
        run.frames_per_second(),
        run.stream.skipped,
        run.megabytes_per_second(),
    );
    let machine = format!("machine CPU {:.2} s", run.machine_cpu.as_secs_f64());
    match run.reader {
        None => format!("{capture}; no reader; {machine}"),
        Some(reader) => format!(
            "{capture}; {} bus events, {} from the camera, kernel drops {}; \
             reader CPU {:.3} s, {:.2}% of a core ({}% system, {:.3} s more outside the capture); \
             {machine}",
            reader.bus_events,
            reader.camera_events,
            reader.kernel_dropped,
            reader.cpu.as_secs_f64(),
            run.share_of_a_core(reader.cpu),
            cell(reader.system_share(), 0),
            reader.outside_the_capture().as_secs_f64(),
        ),
    }
}

/// Lays `rows` out under `headers`: the first column left-aligned, the rest
/// right-aligned, two spaces between columns, no trailing spaces.
fn table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let mut widths: Vec<usize> = headers.iter().map(|header| header.len()).collect();
    for row in rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.len());
        }
    }
    let line = |cells: &mut dyn Iterator<Item = &str>| {
        let mut out = String::new();
        for (index, (cell, width)) in cells.zip(&widths).enumerate() {
            if index == 0 {
                out.push_str(&format!("{cell:<width$}"));
            } else {
                out.push_str(&format!("  {cell:>width$}"));
            }
        }
        out.trim_end().to_string()
    };
    let mut lines = vec![line(&mut headers.iter().copied())];
    lines.extend(
        rows.iter()
            .map(|row| line(&mut row.iter().map(String::as_str))),
    );
    lines.join("\n")
}

const RUN_HEADERS: [&str; 15] = [
    "interface",
    "round",
    "elapsed s",
    "frames",
    "skipped",
    "fps",
    "MB/s",
    "bus events",
    "cam events",
    "kdropped",
    "reader CPU s",
    "% of a core",
    "system %",
    "outside s",
    "machine CPU s",
];

fn run_row(run: &Run) -> Vec<String> {
    let mut row = vec![
        run.interface.label().to_string(),
        run.round.to_string(),
        format!("{:.2}", run.elapsed.as_secs_f64()),
        run.stream.frames.to_string(),
        run.stream.skipped.to_string(),
        format!("{:.2}", run.frames_per_second()),
        format!("{:.2}", run.megabytes_per_second()),
    ];
    match run.reader {
        None => row.extend(std::iter::repeat_n(ABSENT.to_string(), 7)),
        Some(reader) => row.extend([
            reader.bus_events.to_string(),
            reader.camera_events.to_string(),
            reader.kernel_dropped.to_string(),
            format!("{:.3}", reader.cpu.as_secs_f64()),
            format!("{:.2}", run.share_of_a_core(reader.cpu)),
            cell(reader.system_share(), 0),
            format!("{:.3}", reader.outside_the_capture().as_secs_f64()),
        ]),
    }
    row.push(format!("{:.2}", run.machine_cpu.as_secs_f64()));
    row
}

/// Every run, one row each, in the order they ran.
fn render_runs(runs: &[Run]) -> String {
    let rows: Vec<Vec<String>> = runs.iter().map(run_row).collect();
    table(&RUN_HEADERS, &rows)
}

const SUMMARY_HEADERS: [&str; 19] = [
    "interface",
    "runs",
    "elapsed s",
    "fps",
    "skipped",
    "cam events/s",
    "kdropped",
    "reader CPU s",
    "min",
    "max",
    "% of a core",
    "ms/1000 events",
    "system %",
    "outside s",
    "machine CPU s",
    "min",
    "max",
    "% of a core",
    "above none",
];

fn mean(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    Some(values.iter().sum::<f64>() / values.len() as f64)
}

fn lowest(values: &[f64]) -> Option<f64> {
    values.iter().copied().reduce(f64::min)
}

fn highest(values: &[f64]) -> Option<f64> {
    values.iter().copied().reduce(f64::max)
}

/// The whole machine's busy CPU over a run, as a percentage of one core,
/// averaged over `runs`.
fn machine_share(runs: &[&Run]) -> Option<f64> {
    let shares: Vec<f64> = runs
        .iter()
        .map(|run| run.share_of_a_core(run.machine_cpu))
        .collect();
    mean(&shares)
}

/// One row per interface that ran, in [`Interface::ALL`] order: the mean of
/// each figure over that interface's runs, the lowest and highest of the two
/// CPU-seconds figures, and the skipped frames and kernel drops summed.
///
/// "above none" is the machine's share of a core during this interface's runs
/// less its share during the no-reader runs, in percentage points: what
/// reading usbmon this way added to the machine, kernel side included, as far
/// as the machine's other activity lets it show.
fn summary_rows(runs: &[Run]) -> Vec<Vec<String>> {
    let of = |interface: Interface| -> Vec<&Run> {
        runs.iter()
            .filter(|run| run.interface == interface)
            .collect()
    };
    let baseline = machine_share(&of(Interface::None));

    Interface::ALL
        .into_iter()
        .filter_map(|interface| {
            let runs = of(interface);
            if runs.is_empty() {
                return None;
            }
            let over = |figure: &dyn Fn(&Run) -> Option<f64>| -> Vec<f64> {
                runs.iter().filter_map(|run| figure(run)).collect()
            };
            let elapsed = over(&|run| Some(run.elapsed.as_secs_f64()));
            let fps = over(&|run| Some(run.frames_per_second()));
            let camera_rate = over(&|run| run.camera_events_per_second());
            let reader_cpu = over(&|run| run.reader.map(|r| r.cpu.as_secs_f64()));
            let reader_share = over(&|run| run.reader.map(|r| run.share_of_a_core(r.cpu)));
            let per_thousand = over(&|run| run.reader_ms_per_thousand_events());
            let system_share = over(&|run| run.reader.and_then(|r| r.system_share()));
            let outside = over(&|run| run.reader.map(|r| r.outside_the_capture().as_secs_f64()));
            let machine_cpu = over(&|run| Some(run.machine_cpu.as_secs_f64()));
            let skipped: u64 = runs.iter().map(|run| u64::from(run.stream.skipped)).sum();

            let readers: Vec<ReaderFigures> = runs.iter().filter_map(|run| run.reader).collect();
            let kernel_dropped = if readers.is_empty() {
                ABSENT.to_string()
            } else {
                readers
                    .iter()
                    .map(|reader| reader.kernel_dropped)
                    .sum::<u64>()
                    .to_string()
            };

            let share = machine_share(&runs);
            let above_none = match (interface, share, baseline) {
                (Interface::None, _, _) => ABSENT.to_string(),
                (_, Some(share), Some(baseline)) => format!("{:+.2}", share - baseline),
                _ => ABSENT.to_string(),
            };

            Some(vec![
                interface.label().to_string(),
                runs.len().to_string(),
                cell(mean(&elapsed), 2),
                cell(mean(&fps), 2),
                skipped.to_string(),
                cell(mean(&camera_rate), 1),
                kernel_dropped,
                cell(mean(&reader_cpu), 3),
                cell(lowest(&reader_cpu), 3),
                cell(highest(&reader_cpu), 3),
                cell(mean(&reader_share), 2),
                cell(mean(&per_thousand), 3),
                cell(mean(&system_share), 0),
                cell(mean(&outside), 3),
                cell(mean(&machine_cpu), 2),
                cell(lowest(&machine_cpu), 2),
                cell(highest(&machine_cpu), 2),
                cell(share, 2),
                above_none,
            ])
        })
        .collect()
}

fn render_summary(runs: &[Run]) -> String {
    table(&SUMMARY_HEADERS, &summary_rows(runs))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup<'a>(settings: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            settings
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_string())
        }
    }

    #[test]
    fn the_labels_are_the_ones_the_startup_log_line_prints() {
        let labels: Vec<&str> = Interface::ALL.iter().map(|i| i.label()).collect();
        assert_eq!(labels, ["none", "mmap-ring", "binary", "text"]);
    }

    #[test]
    fn an_empty_environment_gives_the_documented_defaults() {
        assert_eq!(
            Workload::from_lookup(lookup(&[])),
            Ok(Workload {
                camera: PathBuf::from("/dev/video0"),
                frames: 3000,
                rounds: 3,
                interfaces: Interface::ALL.to_vec(),
                format: "YUYV".to_string(),
                width: 640,
                height: 480,
                fps: 30,
            })
        );
    }

    #[test]
    fn every_variable_overrides_its_default() {
        let settings = [
            (ENV_CAMERA, "/dev/v4l/by-id/usb-camera-video-index0"),
            (ENV_FRAMES, "600"),
            (ENV_ROUNDS, "1"),
            (ENV_FORMAT, "MJPG"),
            (ENV_WIDTH, "1920"),
            (ENV_HEIGHT, " 1080 "),
            (ENV_FPS, "60"),
        ];
        assert_eq!(
            Workload::from_lookup(lookup(&settings)),
            Ok(Workload {
                camera: PathBuf::from("/dev/v4l/by-id/usb-camera-video-index0"),
                frames: 600,
                rounds: 1,
                interfaces: Interface::ALL.to_vec(),
                format: "MJPG".to_string(),
                width: 1920,
                height: 1080,
                fps: 60,
            })
        );
    }

    #[test]
    fn the_interfaces_to_run_can_be_picked_by_their_labels() {
        use Interface::{Binary, Mmap, None, Text};
        let chosen = |list: &str| {
            Workload::from_lookup(lookup(&[(ENV_INTERFACES, list)])).map(|w| w.interfaces)
        };
        assert_eq!(chosen("mmap-ring"), Ok(vec![Mmap]));
        // In the order given, spaces ignored, a repeat dropped.
        assert_eq!(chosen("text, mmap-ring ,text"), Ok(vec![Text, Mmap]));
        assert_eq!(
            chosen("none,mmap-ring,binary,text"),
            Ok(vec![None, Mmap, Binary, Text])
        );
        for list in ["", "ebpf", "mmap", "text,", "mmap-ring;text"] {
            assert_eq!(
                chosen(list),
                Err(format!(
                    "USBTOP_NG_BENCH_INTERFACES must be a comma-separated list of none, \
                     mmap-ring, binary and text, got {list:?}"
                ))
            );
        }
    }

    #[test]
    fn a_short_format_code_keeps_its_padding() {
        let workload = Workload::from_lookup(lookup(&[(ENV_FORMAT, "Y16 ")])).unwrap();
        assert_eq!(workload.format, "Y16 ");
    }

    #[test]
    fn a_setting_that_makes_no_sense_names_the_variable_and_the_value() {
        let error =
            |settings: &[(&str, &str)]| Workload::from_lookup(lookup(settings)).unwrap_err();
        assert_eq!(
            error(&[(ENV_FRAMES, "many")]),
            "USBTOP_NG_BENCH_FRAMES must be a whole number above zero, got \"many\""
        );
        assert_eq!(
            error(&[(ENV_ROUNDS, "0")]),
            "USBTOP_NG_BENCH_ROUNDS must be a whole number above zero, got \"0\""
        );
        assert_eq!(
            error(&[(ENV_WIDTH, "-640")]),
            "USBTOP_NG_BENCH_WIDTH must be a whole number above zero, got \"-640\""
        );
        assert_eq!(
            error(&[(ENV_HEIGHT, "")]),
            "USBTOP_NG_BENCH_HEIGHT must be a whole number above zero, got \"\""
        );
        assert_eq!(
            error(&[(ENV_FPS, "29.97")]),
            "USBTOP_NG_BENCH_FPS must be a whole number above zero, got \"29.97\""
        );
        // A comma would smuggle a second key into the v4l2-ctl option.
        for format in ["YUYV,width=1", "YU,V", "YUY", "    ", " UYV", "YU=V"] {
            assert_eq!(
                error(&[(ENV_FORMAT, format)]),
                format!(
                    "USBTOP_NG_BENCH_FORMAT must be a four-character V4L2 format code such as \
                     YUYV, got {format:?}"
                )
            );
        }
        assert_eq!(
            error(&[(ENV_CAMERA, "")]),
            "USBTOP_NG_BENCH_CAMERA must name a video device node such as /dev/video0, got \"\""
        );
    }

    #[test]
    fn each_round_runs_every_interface_once_and_starts_one_place_further_along() {
        use Interface::{Binary, Mmap, None, Text};
        assert_eq!(
            schedule(3, &Interface::ALL),
            [
                (1, None),
                (1, Mmap),
                (1, Binary),
                (1, Text),
                (2, Mmap),
                (2, Binary),
                (2, Text),
                (2, None),
                (3, Binary),
                (3, Text),
                (3, None),
                (3, Mmap),
            ]
        );
        assert_eq!(schedule(0, &Interface::ALL), []);
        // The fifth round is back where the first began.
        let five = schedule(5, &Interface::ALL);
        let fifth: Vec<Interface> = five[16..].iter().map(|(_, i)| *i).collect();
        assert_eq!(fifth, [None, Mmap, Binary, Text]);
        assert_eq!(five[16].0, 5);
        // A shorter list rotates within itself, and an empty one runs nothing.
        assert_eq!(
            schedule(3, &[Mmap, Text]),
            [
                (1, Mmap),
                (1, Text),
                (2, Text),
                (2, Mmap),
                (3, Mmap),
                (3, Text)
            ]
        );
        assert_eq!(schedule(2, &[]), []);
    }

    #[test]
    fn four_rounds_put_every_interface_in_every_position() {
        let runs = schedule(4, &Interface::ALL);
        for interface in Interface::ALL {
            let mut positions: Vec<usize> = runs
                .iter()
                .enumerate()
                .filter(|(_, (_, ran))| *ran == interface)
                .map(|(index, _)| index % 4)
                .collect();
            positions.sort_unstable();
            assert_eq!(positions, [0, 1, 2, 3], "{}", interface.label());
        }
    }

    /// A sysfs tree shaped like the real one: the video node's `device` link
    /// points at a USB interface directory, whose parent is the USB device.
    fn fake_sysfs(attributes: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let device = root.path().join("devices/usb1/1-4");
        let interface = device.join("1-4:1.0");
        std::fs::create_dir_all(&interface).unwrap();
        for (name, value) in attributes {
            std::fs::write(device.join(name), value).unwrap();
        }
        let class_dir = root.path().join("class/video4linux");
        std::fs::create_dir_all(class_dir.join("video0")).unwrap();
        std::os::unix::fs::symlink(&interface, class_dir.join("video0/device")).unwrap();
        (root, class_dir)
    }

    fn video0() -> &'static Path {
        Path::new("/dev/video0")
    }

    #[test]
    fn a_video_node_resolves_to_the_usb_device_behind_it() {
        let (_root, class_dir) = fake_sysfs(&[
            ("busnum", "1\n"),
            ("devnum", "4\n"),
            ("speed", "480\n"),
            ("product", "HD Webcam\n"),
        ]);
        assert_eq!(
            Camera::from_sysfs(&class_dir, video0()),
            Ok(Camera {
                bus: 1,
                device: 4,
                speed: "480".to_string(),
                product: "HD Webcam".to_string(),
            })
        );
    }

    #[test]
    fn a_camera_with_no_product_string_or_an_empty_one_is_still_resolved() {
        let (_root, class_dir) = fake_sysfs(&[("busnum", "3\n"), ("devnum", "10\n")]);
        let unnamed = Camera {
            bus: 3,
            device: 10,
            speed: "unknown".to_string(),
            product: "unknown".to_string(),
        };
        assert_eq!(
            Camera::from_sysfs(&class_dir, video0()),
            Ok(unnamed.clone())
        );

        let (_root, class_dir) = fake_sysfs(&[
            ("busnum", "3\n"),
            ("devnum", "10\n"),
            ("speed", "\n"),
            ("product", "  \n"),
        ]);
        assert_eq!(Camera::from_sysfs(&class_dir, video0()), Ok(unnamed));
    }

    #[test]
    fn a_camera_that_cannot_be_resolved_says_which_path_failed() {
        let (_root, class_dir) = fake_sysfs(&[("busnum", "1\n"), ("devnum", "4\n")]);
        let error = Camera::from_sysfs(&class_dir, Path::new("/dev/video9")).unwrap_err();
        assert!(
            error.starts_with(&format!(
                "could not resolve {}: ",
                class_dir.join("video9/device").display()
            )),
            "{error}"
        );
        assert_eq!(
            Camera::from_sysfs(&class_dir, Path::new("/")),
            Err("/ does not name a video device node".to_string())
        );

        // Not a USB camera: the directory above the interface has no busnum.
        let (root, class_dir) = fake_sysfs(&[("devnum", "4\n")]);
        let error = Camera::from_sysfs(&class_dir, video0()).unwrap_err();
        let busnum = root
            .path()
            .canonicalize()
            .unwrap()
            .join("devices/usb1/1-4/busnum");
        assert!(
            error.starts_with(&format!("could not read {}: ", busnum.display())),
            "{error}"
        );
        assert!(
            error.ends_with("(the camera must be a USB device)"),
            "{error}"
        );

        // Neither a word nor bus 0, usbmon's every-bus node, is a bus number.
        for value in ["one", "0"] {
            let (root, class_dir) = fake_sysfs(&[("busnum", value), ("devnum", "4\n")]);
            let busnum = root
                .path()
                .canonicalize()
                .unwrap()
                .join("devices/usb1/1-4/busnum");
            assert_eq!(
                Camera::from_sysfs(&class_dir, video0()),
                Err(format!(
                    "{} holds {value:?}, which is not a USB bus or device number",
                    busnum.display()
                ))
            );
        }
    }

    #[test]
    fn busy_ticks_leave_out_idle_and_iowait_and_do_not_count_guest_twice() {
        // user 100, nice 5, system 40, idle 800, iowait 20, irq 3, softirq 2,
        // steal 1, guest 50, guest_nice 7.
        let stat = "cpu  100 5 40 800 20 3 2 1 50 7\n\
                    cpu0 60 5 20 400 10 3 1 1 50 7\n\
                    intr 12345 0 0\n";
        assert_eq!(busy_ticks(stat), Some(100 + 5 + 40 + 3 + 2 + 1));
    }

    #[test]
    fn busy_ticks_are_read_from_the_aggregate_line_wherever_it_is() {
        let stat = "cpu0 60 5 20 400 10 3 1 1 0 0\ncpu  7 0 3 90 0 0 0 0 0 0\n";
        assert_eq!(busy_ticks(stat), Some(10));
    }

    #[test]
    fn an_old_kernels_short_cpu_line_is_still_read() {
        // Linux before 2.5.41 printed user, nice, system and idle alone.
        assert_eq!(busy_ticks("cpu  7 1 3 90\n"), Some(11));
    }

    #[test]
    fn a_stat_file_without_a_usable_cpu_line_gives_nothing() {
        assert_eq!(busy_ticks(""), None);
        assert_eq!(busy_ticks("cpu0 1 2 3 4\n"), None);
        assert_eq!(busy_ticks("cpu  1 2 3\n"), None);
        assert_eq!(busy_ticks("cpu  1 2 three 4\n"), None);
    }

    #[test]
    fn on_cpu_time_is_the_first_schedstat_field_in_nanoseconds() {
        assert_eq!(
            on_cpu_time("3970123456 81234 1999\n"),
            Some(Duration::new(3, 970_123_456))
        );
        assert_eq!(on_cpu_time("0 0 0\n"), Some(Duration::ZERO));
        assert_eq!(on_cpu_time(""), None);
        assert_eq!(on_cpu_time("soon 0 0\n"), None);
    }

    #[test]
    fn text_lost_is_read_from_the_bus_statistics_line() {
        assert_eq!(
            text_lost("nreaders 0 events 1145218 text_lost 0\n"),
            Some(0)
        );
        assert_eq!(text_lost("nreaders 1 events 77 text_lost 12\n"), Some(12));
        // The number that follows the word, not the last number on the line.
        assert_eq!(text_lost("text_lost 5 nreaders 1 events 77\n"), Some(5));
        assert_eq!(text_lost("nreaders 0 events 1145218\n"), None);
        assert_eq!(text_lost("nreaders 0 events 1 text_lost\n"), None);
        assert_eq!(text_lost("nreaders 0 events 1 text_lost many\n"), None);
        assert_eq!(text_lost(""), None);
    }

    /// Lines as v4l2-ctl 1.26.1 prints them, the ioctl chatter around them
    /// included. Sequence 3 is missing, and so are 6 and 7.
    const STREAM_LOG: &str = "\
\t\tVIDIOC_QBUF returned 0 (Success)
\t\tVIDIOC_STREAMON returned 0 (Success)
cap dqbuf: 0 seq:      0 bytesused: 614400 ts: 253715.617196 field: None (ts-monotonic, ts-src-soe)
cap dqbuf: 1 seq:      1 bytesused: 614400 ts: 253715.737219 delta: 120.023 ms field: None (ts-monotonic, ts-src-soe)
cap dqbuf: 2 seq:      2 bytesused: 614400 ts: 253715.773197 delta: 35.978 ms field: None (ts-monotonic, ts-src-soe)
cap dqbuf: 3 seq:      4 bytesused: 614400 ts: 253715.873383 delta: 100.186 ms fps: 15.61 field: None (ts-monotonic, ts-src-soe)
cap dqbuf: 0 seq:      5 bytesused: 600000 ts: 253715.921211 delta: 47.828 ms fps: 16.45 field: None (ts-monotonic, ts-src-soe)
cap dqbuf: 1 seq:      8 bytesused: 614400 ts: 253716.073383 delta: 152.172 ms fps: 15.35 field: None (ts-monotonic, ts-src-soe)
\t\tVIDIOC_STREAMOFF returned 0 (Success)
";

    #[test]
    fn the_stream_log_gives_frames_skipped_frames_and_bytes() {
        assert_eq!(
            StreamLog::parse(STREAM_LOG),
            Ok(StreamLog {
                frames: 6,
                skipped: 3,
                bytes: 5 * 614_400 + 600_000,
            })
        );
    }

    #[test]
    fn a_log_with_no_frame_lines_is_an_empty_stream() {
        assert_eq!(
            StreamLog::parse("VIDIOC_QUERYCAP: ok\n<<<<< 30.00 fps\n"),
            Ok(StreamLog {
                frames: 0,
                skipped: 0,
                bytes: 0,
            })
        );
    }

    #[test]
    fn a_sequence_counter_that_restarts_is_not_a_gap() {
        let log = "cap dqbuf: 0 seq: 7 bytesused: 10\ncap dqbuf: 1 seq: 0 bytesused: 10\n\
                   cap dqbuf: 2 seq: 1 bytesused: 10\n";
        assert_eq!(
            StreamLog::parse(log),
            Ok(StreamLog {
                frames: 3,
                skipped: 0,
                bytes: 30,
            })
        );
    }

    #[test]
    fn a_gap_wider_than_the_counter_does_not_overflow_it() {
        let log = "cap dqbuf: 0 seq: 0 bytesused: 1\ncap dqbuf: 1 seq: 4294967295 bytesused: 1\n\
                   cap dqbuf: 2 seq: 0 bytesused: 1\ncap dqbuf: 3 seq: 4294967295 bytesused: 1\n";
        assert_eq!(
            StreamLog::parse(log),
            Ok(StreamLog {
                frames: 4,
                skipped: u32::MAX,
                bytes: 4,
            })
        );
    }

    #[test]
    fn a_frame_line_that_cannot_be_read_stops_the_parse() {
        // No sequence number, between two good lines.
        let missing = "cap dqbuf: 0 seq: 0 bytesused: 10\ncap dqbuf: 1 bytesused: 10 ts: 1.5\n\
                       cap dqbuf: 2 seq: 2 bytesused: 10\n";
        assert_eq!(
            StreamLog::parse(missing),
            Err(
                "could not read a sequence number and a size from the frame line \
                 \"cap dqbuf: 1 bytesused: 10 ts: 1.5\""
                    .to_string()
            )
        );
        // A size that is not a number.
        let invalid = "cap dqbuf: 0 seq: 0 bytesused: lots\n";
        assert_eq!(
            StreamLog::parse(invalid),
            Err(
                "could not read a sequence number and a size from the frame line \
                 \"cap dqbuf: 0 seq: 0 bytesused: lots\""
                    .to_string()
            )
        );
        // A sequence number with nothing after it.
        assert!(StreamLog::parse("cap dqbuf: 0 bytesused: 10 seq:\n").is_err());
    }

    #[test]
    fn a_reader_counts_only_if_it_read_until_it_was_told_to_stop() {
        let exit = |asked_to_stop: bool, error: Option<&str>| ReaderExit {
            asked_to_stop,
            error: error.map(str::to_string),
        };
        assert_eq!(exit(true, None).verdict(), Ok(()));
        // A read error makes the read() and text readers leave their loop
        // and return Ok; nobody had asked them to stop.
        assert_eq!(
            exit(false, None).verdict(),
            Err(
                "the reader stopped by itself before the capture ended; its own log line says why"
                    .to_string()
            )
        );
        assert_eq!(
            exit(false, Some("mmap /dev/usbmon1: Operation not permitted")).verdict(),
            Err("the reader failed: mmap /dev/usbmon1: Operation not permitted".to_string())
        );
        // An error reported after the stop request is still an error.
        assert_eq!(
            exit(true, Some("MON_IOCX_MFETCH: Bad address")).verdict(),
            Err("the reader failed: MON_IOCX_MFETCH: Bad address".to_string())
        );
    }

    fn milliseconds(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    /// A run of `seconds` at 30 YUYV 640x480 frames a second: 18.432 MB/s.
    fn run_lasting(
        seconds: u32,
        interface: Interface,
        round: u32,
        reader: Option<ReaderFigures>,
        machine_ms: u64,
    ) -> Run {
        Run {
            interface,
            round,
            elapsed: Duration::from_secs(u64::from(seconds)),
            stream: StreamLog {
                frames: 30 * seconds,
                skipped: 0,
                bytes: 30 * u64::from(seconds) * 614_400,
            },
            reader,
            machine_cpu: milliseconds(machine_ms),
        }
    }

    /// Fifty seconds, so that a share of a core is never the same number as
    /// the CPU seconds it was computed from.
    fn run(
        interface: Interface,
        round: u32,
        reader: Option<ReaderFigures>,
        machine_ms: u64,
    ) -> Run {
        run_lasting(50, interface, round, reader, machine_ms)
    }

    /// `[bus events, camera events]`, kernel drops, CPU during the capture,
    /// and `[user, system]` CPU over the thread's life, in milliseconds.
    fn reader(
        events: [u64; 2],
        kernel_dropped: u64,
        cpu_ms: u64,
        lifetime_ms: [u64; 2],
    ) -> Option<ReaderFigures> {
        Some(ReaderFigures {
            bus_events: events[0],
            camera_events: events[1],
            kernel_dropped,
            cpu: milliseconds(cpu_ms),
            lifetime: ReaderCpu {
                user: milliseconds(lifetime_ms[0]),
                system: milliseconds(lifetime_ms[1]),
            },
        })
    }

    /// Two mmap runs, one no-reader run and one text run.
    fn sample_runs() -> Vec<Run> {
        vec![
            run(Interface::None, 1, None, 5_000),
            run(
                Interface::Mmap,
                1,
                reader([25_000, 24_500], 0, 500, [150, 450]),
                6_250,
            ),
            run(
                Interface::Text,
                1,
                reader([20_000, 19_500], 3, 200, [55, 165]),
                5_500,
            ),
            run(
                Interface::Mmap,
                2,
                reader([25_000, 24_500], 2, 1_000, [300, 800]),
                6_750,
            ),
        ]
    }

    fn rounded(value: Option<f64>) -> String {
        cell(value, 6)
    }

    #[test]
    fn a_runs_derived_figures() {
        let runs = sample_runs();
        let mmap = &runs[1];
        assert_eq!(mmap.frames_per_second(), 30.0);
        // 1500 frames of 614400 bytes is 921.6 MB in 50 s; the division is
        // not exact in binary floating point, so compare what is printed.
        assert_eq!(rounded(Some(mmap.megabytes_per_second())), "18.432000");
        // Half a second of CPU over a 50 s capture is 1% of one core.
        assert_eq!(
            rounded(Some(mmap.share_of_a_core(milliseconds(500)))),
            "1.000000"
        );
        assert_eq!(mmap.camera_events_per_second(), Some(490.0));
        // 500 ms over 25 thousand events.
        assert_eq!(rounded(mmap.reader_ms_per_thousand_events()), "20.000000");

        let figures = mmap.reader.unwrap();
        // 450 ms of 600 ms in the kernel; 600 ms in all, 500 ms of it
        // during the capture.
        assert_eq!(rounded(figures.system_share()), "75.000000");
        assert_eq!(figures.outside_the_capture(), milliseconds(100));

        let none = &runs[0];
        assert_eq!(none.camera_events_per_second(), None);
        assert_eq!(none.reader_ms_per_thousand_events(), None);

        let silent = run(Interface::Binary, 1, reader([0, 0], 0, 1, [0, 0]), 1);
        assert_eq!(silent.reader_ms_per_thousand_events(), None);
        let figures = silent.reader.unwrap();
        assert_eq!(figures.system_share(), None);
        // The two clocks are read at different moments; a lifetime that
        // comes out shorter than the capture leaves nothing outside it.
        assert_eq!(figures.outside_the_capture(), Duration::ZERO);
    }

    #[test]
    fn a_reader_that_saw_nothing_from_the_camera_measured_something_else() {
        let seen = reader([25_000, 1], 0, 500, [150, 450]).unwrap();
        assert_eq!(seen.saw_the_camera(), Ok(()));
        let unseen = reader([144, 0], 0, 500, [150, 450]).unwrap();
        assert_eq!(
            unseen.saw_the_camera(),
            Err(
                "the reader saw 144 events and none from the camera's device number; \
                 the camera may have been re-enumerated"
                    .to_string()
            )
        );
    }

    #[test]
    fn the_header_names_the_conditions_and_flags_a_debug_build() {
        let workload = Workload::from_lookup(lookup(&[])).unwrap();
        let camera = Camera {
            bus: 1,
            device: 4,
            speed: "480".to_string(),
            product: "HD Webcam".to_string(),
        };
        assert_eq!(
            render_header("7.0.0-43-generic", 16, false, &workload, &camera),
            "usbmon capture interface benchmark\n\
             kernel 7.0.0-43-generic, 16 cores, release build\n\
             camera: HD Webcam on bus 1 as device 4, 480 Mb/s (/dev/video0)\n\
             workload: 3000 frames of YUYV 640x480 at a nominal 30 fps, 3 rounds"
        );
        let once = Workload::from_lookup(lookup(&[(ENV_ROUNDS, "1")])).unwrap();
        assert_eq!(
            render_header("7.0.0-43-generic", 16, false, &once, &camera)
                .lines()
                .nth(3),
            Some("workload: 3000 frames of YUYV 640x480 at a nominal 30 fps, 1 round")
        );
        let some = Workload::from_lookup(lookup(&[(ENV_INTERFACES, "mmap-ring,text")])).unwrap();
        assert_eq!(
            render_header("7.0.0-43-generic", 16, false, &some, &camera)
                .lines()
                .nth(4),
            Some("interfaces: mmap-ring, text only")
        );
        assert_eq!(
            render_header("7.0.0-43-generic", 16, false, &workload, &camera)
                .lines()
                .count(),
            4
        );
        let debug = render_header("7.0.0-43-generic", 16, true, &workload, &camera);
        assert_eq!(
            debug.lines().nth(1),
            Some(
                "kernel 7.0.0-43-generic, 16 cores, DEBUG build: the CPU figures do not \
                 describe the shipped binary"
            )
        );
    }

    #[test]
    fn the_progress_line_of_a_run_with_and_without_a_reader() {
        let runs = sample_runs();
        assert_eq!(
            render_progress(&runs[0]),
            "round 1 none: 1500 frames in 50.00 s (30.00 fps, 0 skipped, 18.43 MB/s); \
             no reader; machine CPU 5.00 s"
        );
        assert_eq!(
            render_progress(&runs[1]),
            "round 1 mmap-ring: 1500 frames in 50.00 s (30.00 fps, 0 skipped, 18.43 MB/s); \
             25000 bus events, 24500 from the camera, kernel drops 0; \
             reader CPU 0.500 s, 1.00% of a core (75% system, 0.100 s more outside the capture); \
             machine CPU 6.25 s"
        );
        assert_eq!(
            render_progress(&runs[2]),
            "round 1 text: 1500 frames in 50.00 s (30.00 fps, 0 skipped, 18.43 MB/s); \
             20000 bus events, 19500 from the camera, kernel drops 3; \
             reader CPU 0.200 s, 0.40% of a core (75% system, 0.020 s more outside the capture); \
             machine CPU 5.50 s"
        );
    }

    #[test]
    fn a_table_aligns_the_first_column_left_and_the_rest_right() {
        let rows = vec![
            vec![
                "mmap-ring".to_string(),
                "1".to_string(),
                "100.00".to_string(),
            ],
            vec!["text".to_string(), "12".to_string(), "9.50".to_string()],
        ];
        assert_eq!(
            table(&["interface", "n", "seconds"], &rows),
            "interface   n  seconds\n\
             mmap-ring   1   100.00\n\
             text       12     9.50"
        );
        // A cell wider than its header widens the column, and a short last
        // column leaves no trailing spaces.
        let rows = vec![vec!["a-long-first-cell".to_string(), "-".to_string()]];
        assert_eq!(
            table(&["x", "wide header"], &rows),
            "x                  wide header\n\
             a-long-first-cell            -"
        );
    }

    #[test]
    fn a_run_row_has_a_cell_for_every_header() {
        let runs = sample_runs();
        assert_eq!(
            run_row(&runs[1]),
            [
                "mmap-ring",
                "1",
                "50.00",
                "1500",
                "0",
                "30.00",
                "18.43",
                "25000",
                "24500",
                "0",
                "0.500",
                "1.00",
                "75",
                "0.100",
                "6.25",
            ]
        );
        assert_eq!(
            run_row(&runs[0]),
            [
                "none", "1", "50.00", "1500", "0", "30.00", "18.43", "-", "-", "-", "-", "-", "-",
                "-", "5.00",
            ]
        );
        assert_eq!(
            run_row(&runs[2])[7..10],
            ["20000".to_string(), "19500".to_string(), "3".to_string()]
        );
        for run in &runs {
            assert_eq!(run_row(run).len(), RUN_HEADERS.len());
        }
        let rendered = render_runs(&runs);
        assert_eq!(rendered.lines().count(), 1 + runs.len());
        assert!(
            rendered.starts_with("interface  round  elapsed s"),
            "{rendered}"
        );
    }

    #[test]
    fn the_summary_averages_each_interface_and_compares_the_machine_to_no_reader() {
        let rows = summary_rows(&sample_runs());
        assert_eq!(
            rows,
            [
                // No reader: only the capture and the machine columns. 5 s
                // of machine CPU over 50 s is 10% of a core.
                vec![
                    "none", "1", "50.00", "30.00", "0", "-", "-", "-", "-", "-", "-", "-", "-",
                    "-", "5.00", "5.00", "5.00", "10.00", "-",
                ],
                // Two runs: 0.5 s and 1.0 s of reader CPU, which is 1% and
                // 2% of a core and 20 and 40 ms per thousand events; 75% and
                // 72.7% system; 6.25 s and 6.75 s of machine CPU, 13% of a
                // core against the no-reader 10%; kernel drops 0 + 2.
                vec![
                    "mmap-ring",
                    "2",
                    "50.00",
                    "30.00",
                    "0",
                    "490.0",
                    "2",
                    "0.750",
                    "0.500",
                    "1.000",
                    "1.50",
                    "30.000",
                    "74",
                    "0.100",
                    "6.50",
                    "6.25",
                    "6.75",
                    "13.00",
                    "+3.00",
                ],
                // Text's count is the growth of the bus's text_lost.
                vec![
                    "text", "1", "50.00", "30.00", "0", "390.0", "3", "0.200", "0.200", "0.200",
                    "0.40", "10.000", "75", "0.020", "5.50", "5.50", "5.50", "11.00", "+1.00",
                ],
            ]
        );
        for row in &rows {
            assert_eq!(row.len(), SUMMARY_HEADERS.len());
        }
        let rendered = render_summary(&sample_runs());
        assert_eq!(rendered.lines().count(), 4);
        assert!(
            rendered.starts_with("interface  runs  elapsed s"),
            "{rendered}"
        );
    }

    #[test]
    fn the_summary_adds_up_skipped_frames() {
        let mut runs = sample_runs();
        runs[1].stream.skipped = 2;
        runs[3].stream.skipped = 5;
        let rows = summary_rows(&runs);
        assert_eq!(rows[1][0], "mmap-ring");
        assert_eq!(rows[1][4], "7");
        assert_eq!(rows[0][4], "0");
    }

    #[test]
    fn the_machine_is_compared_as_a_share_of_a_core_and_not_in_seconds() {
        // The no-reader run lasted twice as long: 10 s of machine CPU over
        // 100 s is 10% of a core, and 6 s over 50 s is 12%. In seconds the
        // reader's run would look 4 s cheaper than no reader at all.
        let runs = [
            run_lasting(100, Interface::None, 1, None, 10_000),
            run_lasting(
                50,
                Interface::Binary,
                1,
                reader([1, 1], 0, 1, [1, 1]),
                6_000,
            ),
        ];
        let rows = summary_rows(&runs);
        assert_eq!(rows[1][17], "12.00");
        assert_eq!(rows[1][18], "+2.00");
    }

    #[test]
    fn a_reader_that_cost_less_than_no_reader_shows_a_negative_difference() {
        let runs = [
            run(Interface::None, 1, None, 10_000),
            run(Interface::Binary, 1, reader([1, 1], 0, 1, [1, 1]), 9_250),
        ];
        // 18.5% of a core against 20%.
        assert_eq!(summary_rows(&runs)[1][18], "-1.50");
    }

    #[test]
    fn without_a_no_reader_run_there_is_nothing_to_compare_the_machine_to() {
        let runs = [run(
            Interface::Binary,
            1,
            reader([1, 1], 0, 1, [1, 1]),
            9_250,
        )];
        let rows = summary_rows(&runs);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0][0], "binary");
        assert_eq!(rows[0][18], "-");
    }
}

/// The live benchmark. See `docs/BENCHMARK.md`.
#[cfg(feature = "integration")]
mod live {
    use std::fs::File;
    use std::io;
    use std::os::unix::fs::FileExt;
    use std::os::unix::io::AsRawFd;
    use std::process::{Command, Output};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{mpsc, Arc};
    use std::thread::JoinHandle;
    use std::time::Instant;

    use super::*;
    use crate::usbmon::binary::BinaryReader;
    use crate::usbmon::mmap_ring::MmapReader;
    use crate::usbmon::parser::UsbPacket;
    use crate::usbmon::reader::UsbmonReader;
    use crate::usbmon::{open_nonblocking, ring, POLL_INTERVAL};

    /// How long the reader is given before the capture to set its interface
    /// up, and after it to read what the kernel still holds for it: a ring
    /// reader that finds nothing to read sleeps one poll interval, and the
    /// text reader is back at the interface sooner than that.
    const SETTLE: Duration = POLL_INTERVAL.saturating_mul(4);
    const DRAIN: Duration = POLL_INTERVAL.saturating_mul(2);

    fn timeval_duration(time: libc::timeval) -> Duration {
        Duration::from_secs(u64::try_from(time.tv_sec).unwrap_or(0))
            + Duration::from_micros(u64::try_from(time.tv_usec).unwrap_or(0))
    }

    /// The CPU time the calling thread has used since it started.
    /// getrusage(2): `RUSAGE_THREAD` (Linux 2.6.26 and later) returns the
    /// usage of the calling thread alone, with `ru_utime` and `ru_stime` the
    /// time spent in user and in kernel mode.
    fn thread_cpu() -> io::Result<ReaderCpu> {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
        // SAFETY: `usage` is writable storage of the size and alignment of a
        // `struct rusage`, and getrusage writes nowhere else.
        let ret = unsafe { libc::getrusage(libc::RUSAGE_THREAD, usage.as_mut_ptr()) };
        if ret != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the storage was zeroed, which is a valid `rusage` (it is
        // integers all the way down), and getrusage returned 0, so it has
        // filled it in.
        let usage = unsafe { usage.assume_init() };
        Ok(ReaderCpu {
            user: timeval_duration(usage.ru_utime),
            system: timeval_duration(usage.ru_stime),
        })
    }

    /// The unit of the `/proc/stat` counters: sysconf(3) `_SC_CLK_TCK`, clock
    /// ticks per second.
    fn clock_ticks_per_second() -> Result<u64, String> {
        // SAFETY: sysconf takes an integer and returns one; it reads and
        // writes no memory of ours.
        let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        u64::try_from(ticks)
            .ok()
            .filter(|ticks| *ticks > 0)
            .ok_or_else(|| format!("sysconf(_SC_CLK_TCK) returned {ticks}"))
    }

    fn machine_busy_ticks() -> Result<u64, String> {
        let stat = std::fs::read_to_string("/proc/stat")
            .map_err(|e| format!("could not read /proc/stat: {e}"))?;
        busy_ticks(&stat).ok_or_else(|| "/proc/stat has no usable cpu line".to_string())
    }

    /// The bus's count of events the text interface has lost (see
    /// [`text_lost`]), from the statistics file beside the bus's text file.
    fn bus_text_lost(bus: u8) -> Result<u64, String> {
        let path = UsbmonReader::new(bus)
            .path
            .with_file_name(format!("{bus}s"));
        let statistics = std::fs::read_to_string(&path)
            .map_err(|e| format!("could not read {}: {e}", path.display()))?;
        text_lost(&statistics).ok_or_else(|| {
            format!(
                "{} has no text_lost count: {:?}",
                path.display(),
                statistics.trim()
            )
        })
    }

    /// Everything the runs need, checked before the first one so a missing
    /// prerequisite fails in a second and not ten minutes in. Returns one
    /// line per problem.
    fn preflight(bus: u8) -> Vec<String> {
        let mut problems = Vec::new();
        let node = MmapReader::new(bus).path;
        match open_nonblocking(&node) {
            Err(e) => problems.push(format!(
                "could not open {}: {e} (load usbmon with 'sudo modprobe usbmon' and run the \
                 benchmark as root)",
                node.display()
            )),
            Ok(file) => {
                if !MmapReader::probe(&node) {
                    problems.push(format!(
                        "could not map the ring of {}: this kernel or sandbox does not allow it",
                        node.display()
                    ));
                }
                // The readers log a failing MON_IOCG_STATS and carry on, and
                // a drop count that was never read looks like zero drops.
                if let Err(e) = ring::stats(file.as_raw_fd()) {
                    problems.push(format!(
                        "could not read the drop count of {} (MON_IOCG_STATS): {e}",
                        node.display()
                    ));
                }
            }
        }
        let text = UsbmonReader::new(bus).path;
        if let Err(e) = open_nonblocking(&text) {
            problems.push(format!(
                "could not open {}: {e} (debugfs must be mounted, and the benchmark run as root)",
                text.display()
            ));
        }
        if let Err(e) = bus_text_lost(bus) {
            problems.push(e);
        }
        // Reader CPU comes from a thread's schedstat file. The process's
        // first thread has run and is waiting, so its own shows whether the
        // kernel has the file and is filling it in.
        match std::fs::read_to_string("/proc/self/schedstat") {
            Err(e) => problems.push(format!(
                "could not read /proc/self/schedstat: {e} (the kernel needs CONFIG_SCHED_INFO)"
            )),
            Ok(own) => {
                if !matches!(on_cpu_time(&own), Some(time) if !time.is_zero()) {
                    problems.push(format!(
                        "/proc/self/schedstat reads {:?}: this kernel is not keeping scheduler \
                         statistics",
                        own.trim()
                    ));
                }
            }
        }
        if let Err(e) = machine_busy_ticks() {
            problems.push(e);
        }
        if let Err(e) = Command::new("v4l2-ctl").arg("--version").output() {
            problems.push(format!(
                "could not run v4l2-ctl: {e} (it comes with the v4l-utils package)"
            ));
        }
        problems
    }

    /// The time on a CPU of the thread an open `schedstat` file belongs to,
    /// read from any thread: the file was opened through `/proc/thread-self`,
    /// which the kernel resolved to one thread at that moment, and it goes on
    /// reporting that thread whoever reads it. A reading of zero means the
    /// kernel is not keeping scheduler statistics (it then prints `0 0 0`),
    /// which no thread that has run can otherwise show.
    fn read_on_cpu(schedstat: &File) -> Result<Duration, String> {
        let mut buffer = [0u8; 128];
        let length = schedstat
            .read_at(&mut buffer, 0)
            .map_err(|e| format!("could not read a thread's schedstat: {e}"))?;
        let text = String::from_utf8_lossy(&buffer[..length]);
        on_cpu_time(&text)
            .filter(|cpu| !cpu.is_zero())
            .ok_or_else(|| {
                format!(
                    "a thread's schedstat reads {:?}: this kernel is not keeping scheduler \
                     statistics",
                    text.trim()
                )
            })
    }

    /// Runs `work` on a new thread and returns that thread together with its
    /// own `schedstat` file, which the new thread opens for itself before it
    /// does anything else. `/proc/thread-self` names whichever thread opens
    /// it, so the open has to happen on the thread to be measured; once open,
    /// the file can be read from any thread (see [`read_on_cpu`]).
    fn spawn_measured<T, F>(work: F) -> Result<(JoinHandle<Option<T>>, File), String>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        let (sender, receiver) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let schedstat = File::open("/proc/thread-self/schedstat");
            let opened = schedstat.is_ok();
            // The caller is waiting in `recv`; if it has gone there is nobody
            // left to do the work for.
            let delivered = sender.send(schedstat).is_ok();
            (opened && delivered).then(work)
        });
        match receiver.recv() {
            Ok(Ok(schedstat)) => Ok((thread, schedstat)),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(format!(
                    "could not open /proc/thread-self/schedstat: {e} (the kernel needs \
                     CONFIG_SCHED_INFO)"
                ))
            }
            Err(_) => {
                let _ = thread.join();
                Err("the measured thread ended before it had started".to_string())
            }
        }
    }

    /// What a reader thread hands back when it is joined.
    struct ReaderOutcome {
        lifetime: Result<ReaderCpu, String>,
        exit: ReaderExit,
    }

    /// The reader's counters at one moment, read from the benchmark's own
    /// thread.
    struct ReaderSample {
        cpu: Duration,
        bus_events: u64,
        camera_events: u64,
    }

    /// A production reader running on a thread of its own.
    struct Reader {
        interface: Interface,
        bus: u8,
        thread: JoinHandle<Option<ReaderOutcome>>,
        /// The reader thread's own schedstat file (see [`spawn_measured`]).
        schedstat: File,
        shutdown: Arc<AtomicBool>,
        /// The ring's drop count, which the mmap and read() readers add to.
        ring_dropped: Arc<AtomicU64>,
        /// The bus's `text_lost` before a text reader started.
        text_lost_before: u64,
        bus_events: Arc<AtomicU64>,
        camera_events: Arc<AtomicU64>,
    }

    impl Reader {
        /// Starts the production reader for `interface` with a callback that
        /// only counts. Nothing is started for [`Interface::None`].
        ///
        /// The callback passes each packet through `black_box`: the
        /// production callback sends the whole packet down a channel, so
        /// every field has to exist, and a counter that looked at one field
        /// would let the optimiser drop the work of producing the others.
        fn start(interface: Interface, camera: &Camera) -> Result<Option<Self>, String> {
            if interface == Interface::None {
                return Ok(None);
            }
            let bus = camera.bus;
            let camera_device = camera.device;
            let shutdown = Arc::new(AtomicBool::new(false));
            let ring_dropped = Arc::new(AtomicU64::new(0));
            let bus_events = Arc::new(AtomicU64::new(0));
            let camera_events = Arc::new(AtomicU64::new(0));
            let text_lost_before = bus_text_lost(bus)?;

            let work = {
                let shutdown = Arc::clone(&shutdown);
                let ring_dropped = Arc::clone(&ring_dropped);
                let bus_events = Arc::clone(&bus_events);
                let camera_events = Arc::clone(&camera_events);
                move || {
                    let started = thread_cpu();
                    let count = |packet: UsbPacket| {
                        let packet = std::hint::black_box(packet);
                        bus_events.fetch_add(1, Ordering::Relaxed);
                        if packet.device_id == camera_device {
                            camera_events.fetch_add(1, Ordering::Relaxed);
                        }
                        Ok(())
                    };
                    let result = match interface {
                        Interface::Mmap => {
                            MmapReader::new(bus).read_packets(&shutdown, &ring_dropped, count)
                        }
                        Interface::Binary => {
                            BinaryReader::new(bus).read_packets(&shutdown, &ring_dropped, count)
                        }
                        Interface::Text => UsbmonReader::new(bus).read_packets(&shutdown, count),
                        Interface::None => {
                            unreachable!("no reader thread is started for a no-reader run")
                        }
                    };
                    // Read at once: this is what tells a reader that was
                    // stopped from one that stopped.
                    let asked_to_stop = shutdown.load(Ordering::Relaxed);
                    let finished = thread_cpu();
                    let lifetime = match (started, finished) {
                        (Ok(started), Ok(finished)) => Ok(ReaderCpu {
                            user: finished.user.saturating_sub(started.user),
                            system: finished.system.saturating_sub(started.system),
                        }),
                        (Err(e), _) | (_, Err(e)) => Err(format!("getrusage(RUSAGE_THREAD): {e}")),
                    };
                    ReaderOutcome {
                        lifetime,
                        exit: ReaderExit {
                            asked_to_stop,
                            error: result.err().map(|e| format!("{e:#}")),
                        },
                    }
                }
            };

            let (thread, schedstat) = spawn_measured(work)?;
            Ok(Some(Self {
                interface,
                bus,
                thread,
                schedstat,
                shutdown,
                ring_dropped,
                text_lost_before,
                bus_events,
                camera_events,
            }))
        }

        /// The reader's CPU time and event counts right now. The CPU time is
        /// the scheduler's stored figure, which a thread that is on a CPU at
        /// this instant has not been charged up to. The reader sleeps most
        /// of the time, and a running thread is normally charged at every
        /// scheduler tick, so the error is normally under a tick; a CPU the
        /// kernel runs without ticks (`nohz_full`) gives no such bound.
        fn sample(&self) -> Result<ReaderSample, String> {
            Ok(ReaderSample {
                cpu: read_on_cpu(&self.schedstat)?,
                bus_events: self.bus_events.load(Ordering::Relaxed),
                camera_events: self.camera_events.load(Ordering::Relaxed),
            })
        }

        /// Asks the reader to stop and waits for it. Returns how it ended
        /// and what the kernel dropped while it ran: the ring's count, which
        /// the mmap and read() readers publish for the last time as their
        /// loop exits, or for text the growth of the bus's `text_lost`.
        fn stop(self) -> Result<(ReaderOutcome, u64), String> {
            self.shutdown.store(true, Ordering::Relaxed);
            let outcome = self
                .thread
                .join()
                .map_err(|_| "the reader thread panicked".to_string())?
                .ok_or_else(|| "the reader thread did not run".to_string())?;
            let kernel_dropped = if self.interface == Interface::Text {
                bus_text_lost(self.bus)?.saturating_sub(self.text_lost_before)
            } else {
                self.ring_dropped.load(Ordering::Relaxed)
            };
            Ok((outcome, kernel_dropped))
        }
    }

    /// Streams the workload's frames with `v4l2-ctl` and returns how long
    /// the child ran, by the wall clock, and what it printed. The time
    /// includes opening the camera and starting the stream.
    fn run_camera(workload: &Workload) -> Result<(Duration, Output), String> {
        let mut command = Command::new("v4l2-ctl");
        command
            .arg("-d")
            .arg(&workload.camera)
            .arg(format!(
                "--set-fmt-video=width={},height={},pixelformat={}",
                workload.width, workload.height, workload.format
            ))
            .arg(format!("--set-parm={}", workload.fps))
            .arg("--verbose")
            .arg("--stream-mmap")
            .arg(format!("--stream-count={}", workload.frames));
        let started = Instant::now();
        let output = command.output().map_err(|e| {
            format!("could not run v4l2-ctl: {e} (it comes with the v4l-utils package)")
        })?;
        Ok((started.elapsed(), output))
    }

    /// What `v4l2-ctl` delivered, or why the run does not count: the child
    /// failed, a frame line could not be read, or the frames are not the
    /// number asked for.
    fn delivered(workload: &Workload, output: &Output) -> Result<StreamLog, String> {
        // The per-frame lines go to stderr.
        let log = String::from_utf8_lossy(&output.stderr);
        let stream = StreamLog::parse(&log)?;
        if output.status.success() && stream.frames == workload.frames {
            return Ok(stream);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut tail: Vec<&str> = log
            .lines()
            .chain(stdout.lines())
            .filter(|line| !line.starts_with("cap dqbuf:") && !line.trim().is_empty())
            .rev()
            .take(6)
            .collect();
        tail.reverse();
        Err(format!(
            "v4l2-ctl on {} delivered {} of {} frames ({}); its last lines:\n{}",
            workload.camera.display(),
            stream.frames,
            workload.frames,
            output.status,
            tail.join("\n")
        ))
    }

    /// The raw measurements of one capture.
    struct Measured {
        elapsed: Duration,
        output: Output,
        machine_ticks: u64,
        /// The reader's counters before the capture and after it.
        reader: Option<(ReaderSample, ReaderSample)>,
    }

    /// Runs one capture and samples around it. The machine's CPU and the
    /// wall clock bracket the `v4l2-ctl` child exactly. The reader's counters
    /// are read just before the child starts and [`DRAIN`] after it ends, so
    /// that the events still queued in the kernel when the camera stopped,
    /// and the CPU spent reading them, are counted; the reader's setup
    /// before the window and its teardown after it are not.
    fn measure(reader: Option<&Reader>, workload: &Workload) -> Result<Measured, String> {
        if let Some(reader) = reader {
            std::thread::sleep(SETTLE);
            // A reader that has returned by now failed its setup: a running
            // one only returns when it is told to stop.
            if reader.thread.is_finished() {
                return Err("the reader returned before the capture began".to_string());
            }
        }
        let opened = reader.map(Reader::sample).transpose()?;
        let ticks_before = machine_busy_ticks()?;
        let (elapsed, output) = run_camera(workload)?;
        let ticks_after = machine_busy_ticks()?;
        let closed = match reader {
            Some(reader) => {
                std::thread::sleep(DRAIN);
                Some(reader.sample()?)
            }
            None => None,
        };
        Ok(Measured {
            elapsed,
            output,
            machine_ticks: ticks_after.saturating_sub(ticks_before),
            reader: opened.zip(closed),
        })
    }

    /// One capture with one interface.
    fn one_run(
        interface: Interface,
        round: u32,
        workload: &Workload,
        camera: &Camera,
        ticks_per_second: u64,
    ) -> Result<Run, String> {
        let reader = Reader::start(interface, camera)?;
        let measured = measure(reader.as_ref(), workload);
        // Stop the reader before looking at any result, so that no error
        // path leaves its thread running; and hear the reader out first,
        // because its failure explains a failed measurement better than the
        // measurement does.
        let stopped = reader.map(Reader::stop).transpose()?;
        if let Some((outcome, _)) = &stopped {
            outcome.exit.verdict()?;
        }
        let measured = measured?;
        let stream = delivered(workload, &measured.output)?;

        let figures = match (stopped, measured.reader) {
            (Some((outcome, kernel_dropped)), Some((opened, closed))) => {
                let figures = ReaderFigures {
                    bus_events: closed.bus_events.saturating_sub(opened.bus_events),
                    camera_events: closed.camera_events.saturating_sub(opened.camera_events),
                    kernel_dropped,
                    cpu: closed.cpu.saturating_sub(opened.cpu),
                    lifetime: outcome.lifetime?,
                };
                figures.saw_the_camera()?;
                Some(figures)
            }
            _ => None,
        };

        Ok(Run {
            interface,
            round,
            elapsed: measured.elapsed,
            stream,
            reader: figures,
            machine_cpu: Duration::from_secs_f64(
                measured.machine_ticks as f64 / ticks_per_second as f64,
            ),
        })
    }

    /// The benchmark. Built as the user and run as root, by name:
    ///
    /// ```text
    /// cargo test --release --features integration --no-run
    /// sudo target/release/deps/usbtop_ng-<hash> capture_interface_benchmark \
    ///     --ignored --nocapture
    /// ```
    ///
    /// The workload comes from the `USBTOP_NG_BENCH_*` variables (see
    /// [`Workload::from_lookup`]). A missing prerequisite fails the run and
    /// says what is missing; nothing here skips quietly, because a benchmark
    /// that passes without measuring anything is worse than one that fails.
    #[test]
    #[ignore = "a live benchmark that needs root, usbmon and a USB camera and takes minutes; \
                run it by name, see docs/BENCHMARK.md"]
    fn capture_interface_benchmark() {
        let fail = |message: String| -> ! { panic!("the benchmark cannot run: {message}") };

        let workload =
            Workload::from_lookup(|name| std::env::var(name).ok()).unwrap_or_else(|e| fail(e));
        // sysfs knows the node by its real name, not by a /dev/v4l/by-id link.
        let node = std::fs::canonicalize(&workload.camera).unwrap_or_else(|e| {
            fail(format!(
                "could not resolve {}: {e}",
                workload.camera.display()
            ))
        });
        let camera = Camera::from_sysfs(Path::new("/sys/class/video4linux"), &node)
            .unwrap_or_else(|e| fail(e));
        let problems = preflight(camera.bus);
        if !problems.is_empty() {
            fail(problems.join("\n"));
        }
        let ticks_per_second = clock_ticks_per_second().unwrap_or_else(|e| fail(e));

        let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .map(|release| release.trim().to_string())
            .unwrap_or_else(|_| "unknown".to_string());
        let cores = std::thread::available_parallelism().map_or(1, usize::from);
        let debug_build = cfg!(debug_assertions);
        println!(
            "{}\n",
            render_header(&kernel, cores, debug_build, &workload, &camera)
        );

        let mut runs = Vec::new();
        for (round, interface) in schedule(workload.rounds, &workload.interfaces) {
            let run = one_run(interface, round, &workload, &camera, ticks_per_second)
                .unwrap_or_else(|e| panic!("round {round}, {}: {e}", interface.label()));
            println!("{}", render_progress(&run));
            runs.push(run);
        }

        println!("\n{}\n", render_runs(&runs));
        if debug_build {
            println!("DEBUG build: the CPU figures below do not describe the shipped binary\n");
        }
        println!("{}", render_summary(&runs));
    }

    /// Keeps the calling thread on a CPU until its own usage, by
    /// [`thread_cpu`], has grown by `amount`.
    fn burn(amount: Duration) {
        let target = thread_cpu().expect("getrusage").total() + amount;
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut value = 0u64;
        while thread_cpu().expect("getrusage").total() < target {
            for step in 0..10_000u64 {
                value = std::hint::black_box(value.wrapping_mul(31).wrapping_add(step));
            }
            assert!(
                Instant::now() < deadline,
                "the calling thread's CPU time did not grow by {amount:?} in 30 s of work"
            );
        }
    }

    // The tests below need no root, no usbmon and no camera, so the ordinary
    // `--features integration` run executes them: they are what runs the
    // benchmark's system calls and /proc readers outside a benchmark.

    #[test]
    fn a_timeval_becomes_a_duration_and_a_negative_one_becomes_zero() {
        let time = libc::timeval {
            tv_sec: 3,
            tv_usec: 250_000,
        };
        assert_eq!(timeval_duration(time), Duration::from_millis(3_250));
        let negative = libc::timeval {
            tv_sec: -1,
            tv_usec: -1,
        };
        assert_eq!(timeval_duration(negative), Duration::ZERO);
    }

    #[test]
    fn thread_cpu_grows_with_the_calling_threads_own_work() {
        let before = thread_cpu().expect("getrusage").total();
        burn(Duration::from_millis(20));
        let after = thread_cpu().expect("getrusage").total();
        assert!(
            after >= before + Duration::from_millis(20),
            "{before:?} to {after:?}"
        );
    }

    /// The defect this guards is `RUSAGE_SELF`, which would charge every
    /// reader with the whole process's time.
    #[test]
    fn thread_cpu_does_not_grow_with_another_threads_work() {
        let before = thread_cpu().expect("getrusage").total();
        std::thread::spawn(|| burn(Duration::from_millis(100)))
            .join()
            .expect("the sibling thread must not panic");
        let after = thread_cpu().expect("getrusage").total();
        assert!(
            after.saturating_sub(before) < Duration::from_millis(50),
            "the calling thread only waited, yet its usage went from {before:?} to {after:?}"
        );
    }

    #[test]
    fn the_clock_tick_rate_is_a_positive_number() {
        let ticks = clock_ticks_per_second().expect("sysconf(_SC_CLK_TCK)");
        assert!(ticks > 0);
    }

    #[test]
    fn the_machines_busy_ticks_grow_while_a_thread_works() {
        let before = machine_busy_ticks().expect("/proc/stat");
        // A fifth of a second of one core: twenty ticks at the usual 100 a
        // second, far more than the counter's granularity.
        burn(Duration::from_millis(200));
        let after = machine_busy_ticks().expect("/proc/stat");
        assert!(after > before, "{before} to {after}");
    }

    /// The benchmark reads the reader thread's CPU time from another thread,
    /// through the `schedstat` file [`spawn_measured`] has that thread open
    /// for itself. This runs that same start and proves the file follows the
    /// measured thread and no other: were the open moved to the caller, the
    /// caller's own work below would show up in it.
    #[test]
    fn a_measured_threads_schedstat_follows_that_thread_and_no_other() {
        // Skipped only where the kernel provably has nothing to read: no
        // schedstat at all, or scheduler statistics switched off, which
        // prints `0 0 0` for every thread. Anything else must work.
        match std::fs::read_to_string("/proc/self/schedstat") {
            Err(e) => {
                eprintln!("no /proc/self/schedstat ({e}); schedstat check skipped");
                return;
            }
            Ok(own) if on_cpu_time(&own) == Some(Duration::ZERO) => {
                eprintln!("this kernel keeps no scheduler statistics; schedstat check skipped");
                return;
            }
            Ok(_) => {}
        }

        let (go_sender, go_receiver) = mpsc::channel::<()>();
        let (done_sender, done_receiver) = mpsc::channel::<()>();
        let (worker, schedstat) = spawn_measured(move || {
            if go_receiver.recv().is_ok() {
                burn(Duration::from_millis(100));
                let _ = done_sender.send(());
                // Parked here until the test has taken its last reading.
                let _ = go_receiver.recv();
            }
        })
        .expect("schedstat exists, so the measured thread's own must open");

        // Let the worker reach its `recv`, so that it is off the CPU and the
        // scheduler has charged it for what it has done so far.
        std::thread::sleep(POLL_INTERVAL);
        let parked = read_on_cpu(&schedstat).expect("the worker has run and is alive");

        // This thread's own work must not show in the worker's file.
        burn(Duration::from_millis(100));
        let still_parked = read_on_cpu(&schedstat).expect("the worker is alive");
        assert!(
            still_parked.saturating_sub(parked) < Duration::from_millis(50),
            "the worker was parked, yet its time went from {parked:?} to {still_parked:?}"
        );

        // The worker's own work must.
        go_sender.send(()).expect("the worker is waiting");
        done_receiver.recv().expect("the worker reports back");
        std::thread::sleep(POLL_INTERVAL);
        let worked = read_on_cpu(&schedstat).expect("the worker is alive");
        assert!(
            worked.saturating_sub(still_parked) >= Duration::from_millis(80),
            "the worker used 100 ms of CPU, yet its time went from {still_parked:?} to {worked:?}"
        );

        drop(go_sender);
        assert!(worker
            .join()
            .expect("the worker thread must not panic")
            .is_some());
    }
}
