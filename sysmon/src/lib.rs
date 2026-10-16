//! `caustics-sysmon` — the bar's sampling sidecar: the charts' numbers.
//!
//! It samples `/proc`, hwmon and drm sysfs once a tick and writes **one
//! `WIRE_SAMPLES` frame per tick to stdout** — a 12-byte wire header and then
//! the [`Samples`] body, byte for byte what the bar uploads to its `samples`
//! buffer and the caustic reads (docs/abi.md, *The wire*). There is no text on stdout, ever:
//! stderr is the witness channel, and it inherits the bar's journal.
//!
//! Both directions go through `caustics_bar_abi::wire`: [`wire::write`] emits
//! header and body in one `write_all` so the bar never sees half a frame, and
//! [`wire::read`] on stdin returns `Ok(None)` at EOF, which is this process's
//! notice that the bar is gone.
//!
//! Why a separate process at all is the bar's membership test (docs/architecture.md,
//! *Sidecars*): this reads kernel files and dlopens NVML, and NVML opening
//! `/dev/nvidia*` can pin a runtime-suspended dGPU awake. None of that belongs
//! in the address space of a process that draws.
//!
//! The split into a library and a two-line `main.rs` is so `sysmon/tests/` can
//! drive the parsers and the NVML lease against recorded snapshots and
//! fakes; nothing else depends on this crate.

pub mod amd;
pub mod cpu;
pub mod disk;
pub mod memory;
pub mod net;
pub mod nvidia;
pub mod temp;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use caustics_bar_abi::wire::{self, Frame, Samples};
use caustics_bar_abi::{
    CHART_CPU, CHART_DISK, CHART_GPU, CHART_MEM, CHART_NET, CHART_OTHER, CHART_SAMPLES, CHART_TEMP,
    MAX_CHARTS,
};

/// A sample the sidecar could not take: a device asleep, a rate without a
/// delta yet, a sensor that is not there. Negative is the ABI's gap.
pub const GAP: f32 = -1.0;

/// What the sidecar samples when nothing says otherwise: six charts, in the
/// order the bar shows them. The bar runs it with no arguments,
/// so this list is what the bar shows.
pub const DEFAULT_CHARTS: [&str; 6] = ["cpu", "cpu-temp", "mem", "net", "disk", "gpu"];

/// The sidecar's own knob, read only when there are no chart arguments. The
/// bar never sets it and never sees it: the bar has no configuration but its caustic.
pub const CHARTS_ENV: &str = "CAUSTICS_SYSMON_CHARTS";

/// Default tick: one second.
pub const DEFAULT_REFRESH_MS: u64 = 1000;
/// The fastest tick accepted, so a typo cannot spin the CPU.
pub const MIN_REFRESH_MS: u64 = 100;
/// The slowest tick accepted.
pub const MAX_REFRESH_MS: u64 = 600_000;

/// What the command line asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Args {
    pub refresh_ms: u64,
    pub charts: Vec<String>,
}

/// How `parse_args` ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Parsed {
    Run(Args),
    /// `--help`: print the usage and exit 0.
    Help,
}

/// The usage text, on stderr — stdout carries frames and nothing else.
pub const USAGE: &str = "\
usage: caustics-sysmon [--refresh <ms>] [<chart>…]

  <chart>  cpu | cpu-temp | mem | net | net:<iface> | disk | disk:<name>
           | gpu | gpu:amd | gpu:nvidia
           With no chart words, $CAUSTICS_SYSMON_CHARTS (whitespace-separated)
           is used, and failing that the compiled-in default:
             cpu cpu-temp mem net disk gpu

One WIRE_SAMPLES frame per tick on stdout (see the caustics-bar-abi crate's `wire`
module); witnesses on stderr; exits 0 when stdin reaches EOF.";

/// Parse the command line, falling back to the environment and then the
/// compiled-in default for the chart list.
pub fn parse_args<I: IntoIterator<Item = String>>(
    argv: I,
    env_charts: Option<String>,
) -> Result<Parsed, String> {
    let mut refresh_ms = DEFAULT_REFRESH_MS;
    let mut charts: Vec<String> = Vec::new();
    let mut args = argv.into_iter();
    let mut only_words = false;

    while let Some(arg) = args.next() {
        if only_words {
            charts.push(arg);
            continue;
        }
        match arg.as_str() {
            "--" => only_words = true,
            "-h" | "--help" => return Ok(Parsed::Help),
            "--refresh" => {
                let value = args
                    .next()
                    .ok_or("--refresh wants a number of milliseconds")?;
                refresh_ms = parse_refresh(&value)?;
            }
            other if other.starts_with("--refresh=") => {
                refresh_ms = parse_refresh(&other["--refresh=".len()..])?;
            }
            other if other.starts_with('-') => return Err(format!("unknown option `{other}`")),
            other => charts.push(other.to_owned()),
        }
    }

    if charts.is_empty() {
        charts = match env_charts {
            Some(words) if !words.split_whitespace().collect::<Vec<_>>().is_empty() => {
                words.split_whitespace().map(str::to_owned).collect()
            }
            _ => DEFAULT_CHARTS.iter().map(|w| (*w).to_owned()).collect(),
        };
    }

    Ok(Parsed::Run(Args { refresh_ms, charts }))
}

fn parse_refresh(value: &str) -> Result<u64, String> {
    let ms: u64 = value
        .parse()
        .map_err(|_| format!("`{value}` is not a number of milliseconds"))?;
    if !(MIN_REFRESH_MS..=MAX_REFRESH_MS).contains(&ms) {
        return Err(format!(
            "--refresh {ms} is outside {MIN_REFRESH_MS}..={MAX_REFRESH_MS} ms"
        ));
    }
    Ok(ms)
}

// ---------------------------------------------------------------------------
// Charts
// ---------------------------------------------------------------------------

/// Which vendors a `gpu` chart covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vendors {
    /// `gpu`: whatever is there, AMD first.
    Any,
    /// `gpu:amd`.
    Amd,
    /// `gpu:nvidia`.
    Nvidia,
}

/// A chart word, parsed. An unrecognised word is [`Word::Unknown`] rather than
/// an error: it still occupies its index in the frame, all gaps, so a
/// third-party invocation never shifts the rows the caustic already drew.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Word {
    Cpu,
    CpuTemp,
    Mem,
    Net(Option<String>),
    Disk(Option<String>),
    Gpu(Vendors),
    Unknown(String),
}

impl Word {
    pub fn parse(word: &str) -> Word {
        match word {
            "cpu" => Word::Cpu,
            "cpu-temp" => Word::CpuTemp,
            "mem" => Word::Mem,
            "net" => Word::Net(None),
            "disk" => Word::Disk(None),
            "gpu" => Word::Gpu(Vendors::Any),
            "gpu:amd" => Word::Gpu(Vendors::Amd),
            "gpu:nvidia" => Word::Gpu(Vendors::Nvidia),
            other => match other.split_once(':') {
                Some(("net", iface)) if !iface.is_empty() => Word::Net(Some(iface.to_owned())),
                Some(("disk", name)) if !name.is_empty() => Word::Disk(Some(name.to_owned())),
                _ => Word::Unknown(other.to_owned()),
            },
        }
    }

    /// The `ChartRow::kind` this word writes — a fact about what the row
    /// measures, never a presentation choice.
    pub fn kind(&self) -> u32 {
        match self {
            Word::Cpu => CHART_CPU,
            Word::CpuTemp => CHART_TEMP,
            Word::Mem => CHART_MEM,
            Word::Net(_) => CHART_NET,
            Word::Disk(_) => CHART_DISK,
            Word::Gpu(_) => CHART_GPU,
            Word::Unknown(_) => CHART_OTHER,
        }
    }

    /// The `ChartRow::ceiling`: the row's unit, or 0 for a rate the caustic
    /// scales to its own window.
    pub fn ceiling(&self) -> f32 {
        match self {
            Word::Cpu | Word::Mem => 1.0,
            Word::CpuTemp => 100.0,
            Word::Gpu(_) => 100.0,
            Word::Net(_) | Word::Disk(_) | Word::Unknown(_) => 0.0,
        }
    }
}

/// One GPU series, in the order the row carries them.
enum GpuSeries {
    Amd(amd::Amd),
    Nvidia(nvidia::Lease),
}

/// The per-chart sampling state.
enum Source {
    Cpu(cpu::Cpu),
    Temp(PathBuf),
    Mem,
    Net(net::Net),
    Disk(disk::Disk),
    Gpu(Vec<GpuSeries>),
    /// A chart this sidecar cannot provide: all gaps, `series = 1`, its index
    /// held so nothing after it shifts.
    Absent,
}

/// One row of the frame: what it measures, how to scale it, and how to sample
/// it.
pub struct Row {
    pub kind: u32,
    pub ceiling: f32,
    pub series: u32,
    source: Source,
}

impl Row {
    /// One tick. `dt` is the seconds since the previous tick; `log` takes one
    /// witness line per decision change, never one per tick.
    pub fn tick(&mut self, dt: f64, log: &mut dyn FnMut(&str)) -> [f32; 2] {
        match &mut self.source {
            Source::Cpu(cpu) => [cpu.sample(), GAP],
            Source::Temp(path) => [temp::read(path), GAP],
            Source::Mem => [memory::sample(), GAP],
            Source::Net(net) => net.sample(dt),
            Source::Disk(disk) => disk.sample(dt),
            Source::Gpu(series) => {
                let mut out = [GAP, GAP];
                for (i, one) in series.iter_mut().take(2).enumerate() {
                    out[i] = match one {
                        GpuSeries::Amd(amd) => amd.sample(),
                        GpuSeries::Nvidia(lease) => {
                            lease.sample(Duration::from_secs_f64(dt.max(0.0)), log)
                        }
                    };
                }
                out
            }
            Source::Absent => [GAP, GAP],
        }
    }
}

/// Where discovery looks. Overridable so the tests never touch the real
/// machine.
pub struct Roots {
    pub hwmon: PathBuf,
    pub drm: PathBuf,
}

impl Default for Roots {
    fn default() -> Self {
        Self {
            hwmon: PathBuf::from(temp::ROOT),
            drm: PathBuf::from(amd::ROOT),
        }
    }
}

/// Turn the chart words into rows, discovering what each needs and saying so
/// on stderr — one line per discovery, one per chart it cannot provide.
pub fn build(words: &[String], roots: &Roots, log: &mut dyn FnMut(&str)) -> Vec<Row> {
    let mut rows = Vec::new();
    for word in words {
        if rows.len() == MAX_CHARTS {
            log(&format!(
                "more than {MAX_CHARTS} charts asked for; `{word}` and the rest dropped"
            ));
            break;
        }
        let parsed = Word::parse(word);
        let (series, source) = build_one(&parsed, roots, log);
        rows.push(Row {
            kind: parsed.kind(),
            ceiling: parsed.ceiling(),
            series,
            source,
        });
    }
    rows
}

fn build_one(word: &Word, roots: &Roots, log: &mut dyn FnMut(&str)) -> (u32, Source) {
    match word {
        Word::Cpu => (1, Source::Cpu(cpu::Cpu::default())),
        Word::Mem => (1, Source::Mem),
        Word::CpuTemp => match temp::discover(&roots.hwmon) {
            Some(source) => {
                log(&format!("cpu-temp from {}", source.witness));
                (1, Source::Temp(source.input))
            }
            None => {
                log("cpu-temp: no k10temp/coretemp/zenpower/acpitz sensor; chart is gaps");
                (1, Source::Absent)
            }
        },
        Word::Net(iface) => (2, Source::Net(net::Net::new(iface.clone()))),
        Word::Disk(name) => (2, Source::Disk(disk::Disk::new(name.clone()))),
        Word::Gpu(vendors) => build_gpu(*vendors, roots, log),
        Word::Unknown(other) => {
            log(&format!("unknown chart `{other}`; chart is gaps"));
            (1, Source::Absent)
        }
    }
}

fn build_gpu(vendors: Vendors, roots: &Roots, log: &mut dyn FnMut(&str)) -> (u32, Source) {
    let mut series = Vec::new();

    if matches!(vendors, Vendors::Any | Vendors::Amd) {
        match amd::discover(&roots.drm) {
            Some(found) => {
                log(&format!("gpu {}", found.witness));
                series.push(GpuSeries::Amd(found));
            }
            None if vendors == Vendors::Amd => log("gpu:amd: no AMD device; chart is gaps"),
            None => {}
        }
    }

    if matches!(vendors, Vendors::Any | Vendors::Nvidia) {
        match nvidia::discover(&roots.drm) {
            Some(device) => {
                let autosuspend = nvidia::autosuspend(&device.dev);
                let lease = nvidia::Lease::new(
                    Box::new(nvidia::NvmlPort::new(&device.slot)),
                    Box::new(nvidia::SysfsGate::new(&device.dev)),
                    autosuspend,
                );
                log(&format!(
                    "gpu {}, autosuspend {}, a rest lasts up to {} s",
                    device.witness,
                    match autosuspend {
                        Some(delay) => format!("{} ms", delay.as_millis()),
                        None => format!(
                            "unreadable (assuming {} ms)",
                            nvidia::DEFAULT_AUTOSUSPEND.as_millis()
                        ),
                    },
                    lease.rest_limit().as_secs(),
                ));
                series.push(GpuSeries::Nvidia(lease));
            }
            None if vendors == Vendors::Nvidia => {
                log("gpu:nvidia: no NVIDIA device; chart is gaps")
            }
            None => {}
        }
    }

    if series.is_empty() {
        if vendors == Vendors::Any {
            log("gpu: no AMD or NVIDIA device; chart is gaps");
        }
        return (1, Source::Absent);
    }
    (series.len() as u32, Source::Gpu(series))
}

// ---------------------------------------------------------------------------
// The frame
// ---------------------------------------------------------------------------

/// The body and its history: one `Samples`, shifted left by a column each
/// tick, and nothing else. This is the sidecar's entire memory.
pub struct Frames {
    frame: Box<Samples>,
    ticks: u32,
}

impl Frames {
    /// Describe the rows once; only the columns change afterwards.
    pub fn new(rows: &[Row]) -> Self {
        let mut frame = Box::new(Samples::ZERO);
        frame.chart_count = rows.len() as u32;
        for (i, row) in rows.iter().enumerate() {
            frame.charts[i].kind = row.kind;
            frame.charts[i].ceiling = row.ceiling;
            frame.charts[i].series = row.series.max(1);
        }
        Self { frame, ticks: 0 }
    }

    /// Shift every series left by one and write `columns` into the new last
    /// column: oldest first, right-aligned, exactly as the ABI says.
    pub fn advance(&mut self, t: f32, columns: &[[f32; 2]]) {
        for s in 0..MAX_CHARTS * 2 {
            let base = s * CHART_SAMPLES;
            self.frame
                .samples
                .copy_within(base + 1..base + CHART_SAMPLES, base);
            self.frame.samples[base + CHART_SAMPLES - 1] = GAP;
        }
        for (c, column) in columns.iter().take(MAX_CHARTS).enumerate() {
            for (k, value) in column.iter().enumerate() {
                self.frame.samples[(2 * c + k) * CHART_SAMPLES + CHART_SAMPLES - 1] = *value;
            }
        }
        self.ticks = self.ticks.saturating_add(1);
        self.frame.sample_count = self.ticks.min(CHART_SAMPLES as u32);
        self.frame.t = t;
    }

    /// One tick as a wire frame — what stdout carries, in one `write_all`.
    pub fn frame(&self) -> Frame {
        Frame::Samples(self.frame.clone())
    }

    /// The body itself, for the tests.
    pub fn body(&self) -> &Samples {
        &self.frame
    }
}

// ---------------------------------------------------------------------------
// The run
// ---------------------------------------------------------------------------

/// `CLOCK_MONOTONIC` seconds — the frame's `t`, which the caustic compares to tell a new tick from a repeat.
pub fn monotonic() -> f64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `clock_gettime` writes exactly this struct and returns 0 or -1;
    // a failure leaves the zeroed value, which is a monotonic time of 0.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as f64 + ts.tv_nsec as f64 / 1e9
}

/// This sidecar's witness line: `caustics-sysmon: …` on stderr, in **one write**
/// (docs/abi.md, *Writing a sidecar*; the one copy is `caustics_sidecar::Witness`).
pub const WITNESS: caustics_sidecar::Witness = caustics_sidecar::Witness::new("caustics-sysmon");

/// One witness line; see [`WITNESS`].
pub fn witness(line: &str) {
    WITNESS.line(line)
}

/// Exit 0 as soon as stdin reaches EOF: the bar holds the write end, so its
/// death is this process's. Frames that arrive on it are read through
/// `wire::read` and discarded: this sidecar publishes no rows, so nothing is ever addressed to it (docs/abi.md, *Action*).
fn watch_stdin() {
    std::thread::Builder::new()
        .name("stdin-eof".into())
        .spawn(|| {
            let stdin = std::io::stdin();
            let mut input = stdin.lock();
            loop {
                match wire::read(&mut input) {
                    Ok(None) => break,
                    Ok(Some(_)) => {}
                    Err(err) => {
                        witness(&format!("stdin: {err}"));
                        break;
                    }
                }
            }
            witness("stdin closed; exiting");
            // The tick loop may be asleep for most of a second; there is
            // nothing to flush (every frame left in one `write_all`) so
            // exiting from here is the prompt, honest answer.
            std::process::exit(0);
        })
        .expect("spawning the stdin watcher");
}

/// The whole program. Returns the process's exit status.
pub fn run<I: IntoIterator<Item = String>>(argv: I) -> i32 {
    let args = match parse_args(argv, std::env::var(CHARTS_ENV).ok()) {
        Ok(Parsed::Run(args)) => args,
        Ok(Parsed::Help) => {
            eprintln!("{USAGE}");
            return 0;
        }
        Err(why) => {
            eprintln!("caustics-sysmon: {why}\n\n{USAGE}");
            return 2;
        }
    };

    witness(&format!(
        "charts: {}; refresh {} ms",
        args.charts.join(" "),
        args.refresh_ms
    ));
    let mut rows = build(&args.charts, &Roots::default(), &mut witness);
    let mut frames = Frames::new(&rows);

    watch_stdin();

    let refresh = Duration::from_millis(args.refresh_ms);
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut deadline = Instant::now();
    let mut last = monotonic();

    loop {
        let now = monotonic();
        let dt = (now - last).max(0.0);
        last = now;

        let columns: Vec<[f32; 2]> = rows
            .iter_mut()
            .map(|row| row.tick(dt, &mut witness))
            .collect();
        frames.advance(now as f32, &columns);

        // One `write_all` per tick, and never anything else on this fd.
        if let Err(err) = wire::write(&mut out, &frames.frame()) {
            if err.kind() == std::io::ErrorKind::BrokenPipe {
                witness("stdout closed; exiting");
                return 0;
            }
            witness(&format!("stdout write failed: {err}"));
            return 1;
        }

        deadline += refresh;
        let now = Instant::now();
        if deadline <= now {
            // Fell behind (a suspend, a stalled sysfs read): resynchronise
            // rather than sprinting through the backlog.
            deadline = now + refresh;
        }
        std::thread::sleep(deadline.saturating_duration_since(now));
    }
}
