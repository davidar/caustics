//! The binary itself: three ticks of the real thing, read the way the bar's
//! pipe feeder reads it, and the exits the contract promises.

use std::io::{Read, Write};
use std::process::{Command, Stdio};

use caustics_bar_abi::wire::{self, Frame, SAMPLES_SIZE, WIRE_MAGIC, WireHeader};
use caustics_bar_abi::{CHART_CPU, CHART_MEM, CHART_OTHER, CHART_SAMPLES};
use caustics_sysmon::{DEFAULT_CHARTS, Frames, GAP, Parsed, Roots, Word, build, parse_args};

const BIN: &str = env!("CARGO_BIN_EXE_caustics-sysmon");

fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_owned()).collect()
}

/// A greeting, then three ticks at 250 ms: a `WIRE_HELLO` naming this build's
/// contract (docs/abi.md, *Hello*), then three `WIRE_SAMPLES` frames of
/// exactly 12 + 8336 bytes, the magic, two charts, `sample_count` 1, 2, 3 —
/// and a clean exit when stdin closes. Read the way the bar's pipe feeder
/// reads it, through `wire::read`.
#[test]
fn three_ticks_are_three_frames_and_stdin_ends_it() {
    let mut child = Command::new(BIN)
        .args(["--refresh", "250", "cpu", "mem"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawning caustics-sysmon");

    let mut stdout = child.stdout.take().unwrap();
    let mut previous_t = 0.0f32;

    // The first frame, before any picture: a bar reads nothing from a child
    // that has not said which contract it was built against.
    let Ok(Some(Frame::Hello(hello))) = wire::read(&mut stdout) else {
        panic!("the first frame was not a Hello");
    };
    assert_eq!(
        hello.abi_hash,
        caustics_bar_abi::ABI_HASH,
        "and it is this tree's"
    );
    assert_eq!(hello.name, "caustics-sysmon");

    for tick in 1..=3u32 {
        let read = wire::read(&mut stdout).unwrap_or_else(|e| panic!("frame {tick}: {e}"));
        let Some(Frame::Samples(frame)) = read else {
            panic!("frame {tick}: {read:?} is not a samples frame");
        };
        assert!(frame.is_valid(), "frame {tick} is not well-formed");
        assert_eq!(frame.chart_count, 2, "two chart words, two rows");
        assert_eq!(frame.sample_count, tick, "one new column per tick");
        assert_eq!(frame.charts[0].kind, CHART_CPU);
        assert_eq!(frame.charts[1].kind, CHART_MEM);
        assert_eq!(frame.charts[0].ceiling, 1.0);
        assert!(frame.t > previous_t, "t is monotonic");
        previous_t = frame.t;

        // Memory is sampled from the first tick; the CPU is a rate and so its
        // first column is a gap.
        let mem = frame.sample(1, 0, CHART_SAMPLES - 1);
        assert!((0.0..=1.0).contains(&mem), "memory used fraction: {mem}");
        let cpu = frame.sample(0, 0, CHART_SAMPLES - 1);
        if tick == 1 {
            assert_eq!(cpu, GAP, "the first CPU column has no delta yet");
        } else {
            assert!((0.0..=1.0).contains(&cpu), "cpu busy: {cpu}");
        }

        // Everything past `chart_count` is untouched: gaps all the way.
        assert_eq!(frame.sample(2, 0, CHART_SAMPLES - 1), GAP);
    }

    // Closing the write end of stdin is what the bar's `Drop` does.
    drop(child.stdin.take());
    let status = child.wait().expect("waiting for caustics-sysmon");
    assert_eq!(status.code(), Some(0), "stdin EOF is a clean exit");
}

/// A bad argument is exit 2, and no *picture* on stdout.
///
/// The greeting is written before anything else the sidecar does, arguments
/// included, and that is deliberate: a bar that reads a well-formed `Hello`
/// and then a clean EOF knows the child was its own and left, which is a
/// better diagnosis than a silent exit. What must never appear is a frame.
#[test]
fn a_bad_argument_is_exit_two_and_no_frames() {
    let mut greeting = Vec::new();
    wire::hello(&mut greeting, "caustics-sysmon").unwrap();
    for bad in [
        vec!["--nonsense"],
        vec!["--refresh", "soon"],
        vec!["--refresh", "3"],
    ] {
        let out = Command::new(BIN)
            .args(&bad)
            .output()
            .expect("running caustics-sysmon");
        assert_eq!(out.status.code(), Some(2), "{bad:?}");
        assert_eq!(out.stdout, greeting, "{bad:?} wrote more than its greeting");
        assert!(!out.stderr.is_empty(), "{bad:?} said nothing about it");
    }
}

/// No chart words means the compiled-in six, and `$CAUSTICS_SYSMON_CHARTS` stands
/// in for arguments when there are none — the sidecar's own knob, which the
/// bar neither sets nor sees.
#[test]
fn the_chart_list_falls_back_to_the_environment_then_the_default() {
    let Ok(Parsed::Run(bare)) = parse_args(args(&[]), None) else {
        panic!()
    };
    assert_eq!(bare.charts, DEFAULT_CHARTS.map(str::to_owned).to_vec());
    assert_eq!(bare.refresh_ms, 1000);

    let Ok(Parsed::Run(env)) = parse_args(args(&[]), Some("  cpu   gpu:nvidia \n".into())) else {
        panic!()
    };
    assert_eq!(env.charts, vec!["cpu".to_owned(), "gpu:nvidia".to_owned()]);

    // Arguments win over the environment.
    let Ok(Parsed::Run(both)) = parse_args(args(&["mem"]), Some("cpu".into())) else {
        panic!()
    };
    assert_eq!(both.charts, vec!["mem".to_owned()]);

    // Both spellings of --refresh, and `--` for a chart word that looks like a
    // flag.
    let Ok(Parsed::Run(a)) = parse_args(args(&["--refresh", "500", "cpu"]), None) else {
        panic!()
    };
    let Ok(Parsed::Run(b)) = parse_args(args(&["--refresh=500", "cpu"]), None) else {
        panic!()
    };
    assert_eq!(a, b);

    assert_eq!(parse_args(args(&["--help"]), None), Ok(Parsed::Help));
}

/// Every word maps to the kind and ceiling the ABI documents, and an unknown
/// word is `CHART_OTHER` rather than an error — so a third-party invocation
/// never shifts the rows that follow it.
#[test]
fn every_word_carries_its_kind_and_an_unknown_word_is_a_row_of_gaps() {
    assert_eq!(Word::parse("cpu").kind(), CHART_CPU);
    assert_eq!(Word::parse("net:wlo1"), Word::Net(Some("wlo1".into())));
    assert_eq!(
        Word::parse("disk:nvme0n1"),
        Word::Disk(Some("nvme0n1".into()))
    );
    assert_eq!(Word::parse("frobnicate").kind(), CHART_OTHER);
    assert_eq!(Word::parse("net:").kind(), CHART_OTHER);

    // Discovery against empty roots: nothing is there, so every row that needs
    // a device is a row of gaps — and it still holds its index.
    let roots = Roots {
        hwmon: "/nonexistent/hwmon".into(),
        drm: "/nonexistent/drm".into(),
    };
    let lines = std::cell::RefCell::new(Vec::new());
    let mut log = |line: &str| lines.borrow_mut().push(line.to_owned());
    let words = args(&["cpu", "cpu-temp", "frobnicate", "gpu"]);
    let mut rows = build(&words, &roots, &mut log);
    assert_eq!(
        rows.len(),
        4,
        "an unprovidable chart still occupies its index"
    );
    assert_eq!(rows[1].kind, caustics_bar_abi::CHART_TEMP);
    assert_eq!(
        rows[1].series, 1,
        "a row it cannot fill is one series of gaps"
    );
    assert_eq!(rows[2].kind, CHART_OTHER);
    assert_eq!(rows[3].series, 1);
    assert_eq!(
        lines.borrow().len(),
        3,
        "one line per chart it cannot provide: {lines:?}"
    );

    // And the frame it produces is gaps in those rows, with the kinds intact.
    let mut frames = Frames::new(&rows);
    let columns: Vec<[f32; 2]> = rows.iter_mut().map(|r| r.tick(1.0, &mut log)).collect();
    frames.advance(1.0, &columns);
    let frame = frames.body();
    assert_eq!(frame.chart_count, 4);
    assert_eq!(frame.charts[1].kind, caustics_bar_abi::CHART_TEMP);
    assert_eq!(frame.sample(1, 0, CHART_SAMPLES - 1), GAP);
    assert_eq!(frame.sample(2, 0, CHART_SAMPLES - 1), GAP);
    assert_eq!(frame.sample(3, 0, CHART_SAMPLES - 1), GAP);
}

/// The history is oldest-first and right-aligned: the newest column is always
/// the last one, and `sample_count` counts back from there.
#[test]
fn the_history_is_right_aligned_and_stops_growing_at_the_window() {
    let roots = Roots {
        hwmon: "/nonexistent".into(),
        drm: "/nonexistent".into(),
    };
    let lines = std::cell::RefCell::new(Vec::new());
    let mut log = |line: &str| lines.borrow_mut().push(line.to_owned());
    let rows = build(&args(&["cpu"]), &roots, &mut log);
    let mut frames = Frames::new(&rows);

    for tick in 1..=(CHART_SAMPLES + 5) {
        frames.advance(tick as f32, &[[tick as f32, GAP]]);
        let frame = frames.body();
        assert_eq!(
            frame.sample_count as usize,
            tick.min(CHART_SAMPLES),
            "sample_count saturates at the window"
        );
        assert_eq!(
            frame.sample(0, 0, CHART_SAMPLES - 1),
            tick as f32,
            "the newest column is the last one"
        );
    }
    // The oldest valid column is `sample_count` back from the end.
    let frame = frames.body();
    assert_eq!(
        frame.sample(0, 0, 0),
        (CHART_SAMPLES + 5 - CHART_SAMPLES + 1) as f32
    );
}

/// stdout never carries anything but frames: a whole run's bytes divide into
/// four headers and four bodies, and the header says so on every one.
#[test]
fn stdout_is_frames_and_nothing_else() {
    let mut child = Command::new(BIN)
        .args(["--refresh", "100", "cpu"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawning caustics-sysmon");
    let mut stdout = child.stdout.take().unwrap();
    let mut greeting = vec![
        0u8;
        {
            let mut out = Vec::new();
            wire::hello(&mut out, "caustics-sysmon").unwrap();
            out.len()
        }
    ];
    stdout.read_exact(&mut greeting).expect("the greeting");
    let mut bytes = vec![0u8; (WireHeader::SIZE + SAMPLES_SIZE) * 4];
    stdout.read_exact(&mut bytes).expect("four frames");
    drop(child.stdin.take());
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(0));

    for chunk in bytes.chunks_exact(WireHeader::SIZE + SAMPLES_SIZE) {
        assert_eq!(
            &chunk[..4],
            &WIRE_MAGIC.to_le_bytes(),
            "every chunk starts a frame"
        );
        let mut cursor = chunk;
        let Some(Frame::Samples(frame)) = wire::read(&mut cursor).unwrap() else {
            panic!("a chunk that is not a samples frame");
        };
        assert!(frame.is_valid(), "every chunk is a well-formed frame");
        assert!(
            cursor.is_empty(),
            "a frame is exactly its header plus its body"
        );
    }
}

/// The witnesses go to stderr, one per discovery, and never to stdout.
#[test]
fn the_discovery_witnesses_are_on_stderr() {
    let mut child = Command::new(BIN)
        .args(["--refresh", "100", "cpu", "frobnicate"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawning caustics-sysmon");
    let mut stdout = child.stdout.take().unwrap();
    wire::read(&mut stdout).expect("the greeting");
    wire::read(&mut stdout).expect("a frame");
    // Ending it the way the bar does, then reading what it said.
    let mut stdin = child.stdin.take().unwrap();
    let _ = stdin.write_all(&[]);
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("charts: cpu frobnicate"), "{stderr}");
    assert!(stderr.contains("unknown chart `frobnicate`"), "{stderr}");
}
