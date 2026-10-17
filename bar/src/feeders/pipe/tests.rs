use super::*;
use crate::abi::wire::{Request, RequestBody, RowsBuilder, Samples, WireHeader};
use smithay_client_toolkit::reexports::calloop::channel::channel;
use std::io::Write;

/// The frame every sidecar owes first (docs/abi.md, *Hello*). Every recorded
/// stream below starts with it, because a bar reads nothing from a child that
/// has not said which contract it was built against.
fn greeting() -> Vec<u8> {
    let mut out = Vec::new();
    wire::hello(&mut out, "a test sidecar").unwrap();
    out
}

/// A recorded stream on disk: the greeting, then `body`.
fn stream(name: &str, body: &[u8]) -> std::path::PathBuf {
    let mut bytes = greeting();
    bytes.extend_from_slice(body);
    scratch(name, &bytes)
}

/// A samples frame the sidecar would write: `charts` rows, `t` seconds.
fn samples(charts: u32, t: f32) -> Vec<u8> {
    let mut body = Box::new(Samples::ZERO);
    body.chart_count = charts;
    body.sample_count = 1;
    body.t = t;
    let mut out = Vec::new();
    wire::write(&mut out, &Frame::Samples(body)).unwrap();
    out
}

/// Write `bytes` to a file under the scratch dir and return its path.
fn scratch(name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("caustics-pipe-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    let mut file = std::fs::File::create(&path).unwrap();
    file.write_all(bytes).unwrap();
    path
}

/// A command line that plays `bytes` back. Sidecars take no arguments, so
/// a shell script on disk is how a test fakes one.
fn player(name: &str, script: &str) -> String {
    let path = scratch(name, format!("#!/bin/sh\n{script}\n").as_bytes());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path.to_string_lossy().into_owned()
}

/// Collect updates until `want` of them arrive or the deadline passes.
fn collect(
    rx: &smithay_client_toolkit::reexports::calloop::channel::Channel<Update>,
    want: usize,
    secs: u64,
) -> Vec<Update> {
    let deadline = std::time::Instant::now() + Duration::from_secs(secs);
    let mut seen = Vec::new();
    while seen.len() < want && std::time::Instant::now() < deadline {
        while let Ok(update) = rx.try_recv() {
            seen.push(update);
        }
        if seen.len() < want {
            thread::sleep(Duration::from_millis(10));
        }
    }
    seen
}

/// The happy path: two frames in, two `Samples` out, and the body survives
/// the trip byte for byte.
#[test]
fn two_recorded_frames_become_two_updates() {
    let mut bytes = samples(2, 1.0);
    bytes.extend_from_slice(&samples(2, 2.0));
    let path = stream("two-frames.bin", &bytes);

    let (tx, rx) = channel::<Update>();
    let pipe = Pipe::start(&player("two.sh", &format!("cat {}", path.display())), tx);
    let seen = collect(&rx, 2, 5);
    drop(pipe);

    assert_eq!(seen.len(), 2, "two frames in, two updates out");
    let mut t = Vec::new();
    for update in seen {
        match update {
            Update::Samples { body, .. } => {
                assert!(body.is_valid());
                assert_eq!(body.chart_count, 2);
                t.push(body.t);
            }
            other => panic!("unexpected update {other:?}"),
        }
    }
    assert_eq!(t, vec![1.0, 2.0]);
}

/// A child that writes less than a frame and dies is a restart, not a
/// panic and not a partial update.
#[test]
fn a_short_write_is_no_update() {
    let path = stream("short.bin", &samples(1, 1.0)[..100]);
    let (tx, rx) = channel::<Update>();
    let pipe = Pipe::start(
        &player("short.sh", &format!("cat {}; exit 1", path.display())),
        tx,
    );
    let seen = collect(&rx, 1, 1);
    drop(pipe);
    assert!(
        seen.is_empty(),
        "a partial frame is never sent on: {seen:?}"
    );
}

/// A frame with the wrong magic is not a wire stream at all: the child is
/// restarted rather than the byte skipped (docs/abi.md, *Framing*, rule 2), and nothing is
/// uploaded.
#[test]
fn a_bad_magic_is_no_update() {
    let mut bytes = samples(1, 1.0);
    bytes[0] ^= 0xff;
    let path = stream("bad-magic.bin", &bytes);
    let (tx, rx) = channel::<Update>();
    let pipe = Pipe::start(&player("magic.sh", &format!("cat {}", path.display())), tx);
    let seen = collect(&rx, 1, 1);
    drop(pipe);
    assert!(
        seen.is_empty(),
        "a frame with a bad magic is never sent on: {seen:?}"
    );
}

/// A rows frame is read, counted, witnessed and forwarded as `Update::Rows`.
/// A frame kind the bar does not know
/// is skipped and costs the stream nothing: the samples frame after it
/// still arrives.
#[test]
fn rows_arrive_and_an_unknown_kind_is_skipped_without_losing_the_next_frame() {
    let mut rows = RowsBuilder::new(crate::abi::APPLET_AUDIO, 7);
    rows.item(1, Some("42%"), None, 0.42, wire::RFLAG_HAS_MENU);
    let mut bytes = Vec::new();
    wire::write(&mut bytes, &rows.frame()).unwrap();
    // A kind from the future, length-prefixed so it can be stepped over.
    bytes.extend_from_slice(crate::abi::as_bytes(&WireHeader::new(99, 4)));
    bytes.extend_from_slice(&[1, 2, 3, 4]);
    bytes.extend_from_slice(&samples(3, 9.0));
    let path = stream("rows-then-unknown.bin", &bytes);

    let (tx, rx) = channel::<Update>();
    let pipe = Pipe::start(&player("rows.sh", &format!("cat {}", path.display())), tx);
    let seen = collect(&rx, 2, 5);
    let skips = pipe.stats().skips();
    drop(pipe);

    assert!(
        seen.len() >= 2,
        "a rows frame and a samples frame: {seen:?}"
    );
    match &seen[0] {
        Update::Rows { sidecar, rows } => {
            assert_eq!(sidecar, "rows");
            assert_eq!(rows.applet, crate::abi::APPLET_AUDIO);
            assert_eq!(rows.revision, 7);
            assert_eq!(rows.rows.len(), 1);
        }
        other => panic!("expected rows, got {other:?}"),
    }
    match &seen[1] {
        Update::Samples { body, .. } => assert_eq!(body.chart_count, 3),
        other => panic!("expected samples, got {other:?}"),
    }
    assert!(skips >= 1, "the unknown kind was counted as a skip");
}

/// A child that never exits on its own is ended by `Drop` closing its
/// stdin — the contract every sidecar is held to.
#[test]
fn drop_ends_a_sleeping_child() {
    let (tx, _rx) = channel::<Update>();
    // `read` on stdin returns when the write end closes; without that this
    // shell would sit for an hour.
    let pipe = Pipe::start(&player("sleeper.sh", "read line; exit 0"), tx);
    // Poll for the child rather than sleeping a fixed 200 ms: under a loaded
    // host — `cargo test --workspace` compiling Slang in the next crate — the
    // spawn can take longer than that, and this is a test of `Drop`, not of
    // how fast a process starts.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut pid = None;
    while pid.is_none() && std::time::Instant::now() < deadline {
        pid = pipe.child.lock().unwrap().as_ref().map(|c| c.id());
        if pid.is_none() {
            thread::sleep(Duration::from_millis(20));
        }
    }
    assert!(pid.is_some(), "the child should be running");
    let started = std::time::Instant::now();
    drop(pipe);
    assert!(
        started.elapsed() < SHUTDOWN_GRACE + Duration::from_millis(500),
        "Drop must not hang on a child that exits on EOF"
    );
    // The pid must be gone: `kill -0` fails for a reaped process.
    let alive = std::process::Command::new("kill")
        .args(["-0", &pid.unwrap().to_string()])
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    assert!(!alive, "the child outlived the feeder");
}

/// A binary that is not there is one `Absent` under the sidecar's own
/// name — plus the `SidecarGone` that takes the state with it — and no
/// retry storm.
#[test]
fn a_missing_binary_is_absent_once() {
    let (tx, rx) = channel::<Update>();
    let pipe = Pipe::start("caustics-sysmon-that-is-not-installed", tx);
    let seen = collect(&rx, 2, 5);
    thread::sleep(Duration::from_millis(200));
    drop(pipe);
    let name = "caustics-sysmon-that-is-not-installed".to_owned();
    assert_eq!(
        seen,
        vec![Update::Absent(name.clone()), Update::SidecarGone(name)],
        "the witness, then the one absence with state behind it"
    );
}

/// `$CAUSTICS_SIDECARS` replaces the compiled-in set; nothing else does, and
/// no command ever carries arguments (a sidecar's settings are the
/// sidecar's).
#[test]
fn the_set_comes_from_the_environment_or_the_compiled_in_list() {
    // The variable is process-wide, so this test reads rather than sets
    // it: whichever way the harness runs, the names that come out are the
    // ones the environment or the constant asked for.
    let commands = commands();
    match std::env::var_os(SIDECARS_ENV) {
        Some(value) if !value.to_string_lossy().split_whitespace().count() == 0 => {
            assert_eq!(
                commands.len(),
                value.to_string_lossy().split_whitespace().count()
            );
        }
        _ => {
            assert_eq!(commands.len(), SIDECARS.len());
            for (command, name) in commands.iter().zip(SIDECARS) {
                assert!(command.ends_with(name), "{command} is not a {name}");
            }
        }
    }
    assert!(
        commands.iter().all(|c| !c.contains(' ')),
        "a command carries no arguments"
    );
}

/// The order: `$CAUSTICS_SIDECARS` replaces the set, then each bare name
/// resolves to the sidecar beside the bar, then to the bare name for
/// `PATH`. The middle one is what makes an installed pair work under a
/// user manager, whose `PATH` has no `~/.local/bin`.
#[test]
fn the_sidecar_beside_the_bar_beats_the_bare_name() {
    let dir = std::env::temp_dir().join(format!("caustics-resolve-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let bar = dir.join("caustics-bar");
    std::fs::write(&bar, b"not really a bar").unwrap();
    let sidecar = dir.join("caustics-sysmon");

    // Nothing beside it yet: the bare names, for `PATH` to find.
    let bare: Vec<String> = SIDECARS.iter().map(|n| (*n).to_owned()).collect();
    assert_eq!(resolve(None, Some(&bar)), bare);
    assert_eq!(resolve(Some("".into()), Some(&bar)), bare);
    assert_eq!(resolve(None, None), bare);

    // Installed beside the bar: that one, by its whole path — and the two
    // that are *not* installed beside it stay bare names, because a
    // partial install is the ordinary case (a sidecar absent is a feature
    // absent, not a bar that will not start).
    std::fs::write(&sidecar, b"not really a sidecar").unwrap();
    let mut want = bare.clone();
    want[0] = sidecar.to_string_lossy().into_owned();
    assert_eq!(
        resolve(None, Some(&bar)),
        want,
        "the sidecar beside the bar is what a user manager can actually find"
    );

    // ...and the variable replaces the whole set, in its own order, with
    // a path taken as given.
    assert_eq!(
        resolve(
            Some("  /opt/other-sysmon   caustics-sysmon ".into()),
            Some(&bar)
        ),
        vec![
            "/opt/other-sysmon".to_owned(),
            sidecar.to_string_lossy().into_owned()
        ],
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// A sidecar's name is the file stem of what was spawned, so an installed
/// binary and one run out of `target/debug` are the same sidecar.
#[test]
fn a_sidecar_is_named_by_its_file_stem() {
    assert_eq!(name_of("caustics-sysmon"), "caustics-sysmon");
    assert_eq!(
        name_of("/home/x/.local/bin/caustics-sysmon"),
        "caustics-sysmon"
    );
    assert_eq!(name_of("target/debug/examples/rows_demo"), "rows_demo");
}

/// The writer half (docs/abi.md, *Action*): an action written from the
/// render thread reaches the child's stdin as one whole frame.
///
/// The scripted sidecar speaks first — one samples frame, so the pipe has
/// a live child and the reader stays parked — and then copies its stdin
/// into a file. The second `cat` is *not* `exec`ed: redirecting its stdout
/// into the file would close the write end of the wire, and the bar would
/// read EOF and reap the child out from under the test. `cat`
/// block-buffers, so the file is read after `Drop` closes stdin and the
/// child flushes and leaves.
#[test]
fn an_action_reaches_the_childs_stdin_whole() {
    let frames = stream("writer-in.bin", &samples(1, 1.0));
    let echoed = scratch("writer-out.bin", &[]);
    let script = format!("cat {}; cat > {}", frames.display(), echoed.display());

    let (tx, rx) = channel::<Update>();
    let pipe = Pipe::start(&player("writer.sh", &script), tx);
    assert_eq!(collect(&rx, 1, 5).len(), 1, "the child spoke first");

    let body = ActionBody {
        row: 7,
        kind: crate::abi::ACTION_SET_VALUE,
        button: crate::abi::BTN_LEFT,
        pos: [12.0, 34.0],
        value: 42.5,
        ..ActionBody::default()
    };
    assert!(pipe.send(&body), "the frame went to the child's stdin");
    // Closing stdin is what makes `cat` flush and exit, so the child is
    // reaped before the file is read.
    drop(pipe);

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut bytes = Vec::new();
    while std::time::Instant::now() < deadline {
        bytes = std::fs::read(&echoed).unwrap_or_default();
        if bytes.len() >= WireHeader::SIZE + size_of::<ActionBody>() {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    let mut input = std::io::Cursor::new(bytes);
    match wire::read(&mut input) {
        Ok(Some(Frame::Action(got))) => assert_eq!(got, body, "byte for byte"),
        other => panic!("the child's stdin held {other:?}, not one action frame"),
    }
    assert!(
        matches!(wire::read(&mut input), Ok(None)),
        "and nothing else: the bar writes one frame per decision"
    );
}

/// A sidecar that is not there takes no action — and says so once rather
/// than blocking the render thread that offered it.
#[test]
fn an_action_to_a_sidecar_that_is_not_there_is_dropped() {
    let (tx, _rx) = channel::<Update>();
    let pipe = Pipe::start("", tx);
    assert!(!pipe.send(&ActionBody {
        row: 1,
        ..ActionBody::default()
    }));
    assert!(pipe.stats().absent());
}

/// A child that never reads its stdin fills the pipe and is then refused,
/// which is the one thing that must never park the render thread. 64 KiB
/// of pipe buffer is ~1300 frames, so this writes until it is told no.
#[test]
fn a_child_that_never_reads_its_stdin_costs_a_witness_and_not_a_stall() {
    let frames = stream("deaf-in.bin", &samples(1, 1.0));
    // Speaks once, then sleeps: stdout stays open, stdin is never read.
    let script = format!("cat {}; exec sleep 30", frames.display());
    let (tx, rx) = channel::<Update>();
    let pipe = Pipe::start(&player("deaf.sh", &script), tx);
    assert_eq!(collect(&rx, 1, 5).len(), 1, "the child spoke first");

    let body = ActionBody {
        row: 1,
        ..ActionBody::default()
    };
    let started = std::time::Instant::now();
    let mut refused = false;
    for _ in 0..4000 {
        if !pipe.send(&body) {
            refused = true;
            break;
        }
    }
    let spent = started.elapsed();
    drop(pipe);
    assert!(refused, "a full pipe has to be refused, not waited on");
    assert!(
        spent < Duration::from_secs(5),
        "and refused promptly ({spent:?})"
    );
}

/// The pid, the ticks and the restarts the health line reports.
#[test]
fn the_stats_follow_the_child() {
    let bytes = samples(1, 1.0);
    let path = stream("stats.bin", &bytes);
    let (tx, rx) = channel::<Update>();
    // A child that delivers one frame and leaves, twice over: the restart
    // is the point.
    let pipe = Pipe::start(&player("stats.sh", &format!("cat {}", path.display())), tx);
    let seen = collect(&rx, 2, 6);
    assert!(seen.len() >= 2, "two children, two frames: {seen:?}");
    assert!(pipe.stats().ticks() >= 2, "ticks: {}", pipe.stats().ticks());
    assert!(
        pipe.stats().restarts() >= 1,
        "restarts: {}",
        pipe.stats().restarts()
    );
    assert!(!pipe.stats().absent());
    drop(pipe);
}

/// An absent sidecar says so through the stats too, which is what the
/// health line reports instead of a pid.
#[test]
fn a_missing_binary_reads_as_absent_in_the_stats() {
    let (tx, rx) = channel::<Update>();
    let pipe = Pipe::start("caustics-sysmon-that-is-not-installed", tx);
    collect(&rx, 1, 5);
    assert!(pipe.stats().absent());
    assert_eq!(pipe.stats().pid(), None);
    assert_eq!(pipe.stats().ticks(), 0);
    drop(pipe);
}

/// A request asked before the child has a stdin is not dropped: it is
/// written to the child when it starts — the bar's first build runs a
/// millisecond before its sidecars do.
#[test]
fn a_request_asked_before_the_child_exists_reaches_it_when_it_starts() {
    let echoed = scratch("early-out.bin", &[]);
    let script = format!("sleep 0.5; cat > {}", echoed.display());
    let (tx, _rx) = channel::<Update>();
    let pipe = Pipe::start(&player("early.sh", &script), tx);
    let asked = Frame::Request(Request {
        kind: wire::REQ_APP,
        arg: 0,
        name: "org.example.App".into(),
    });
    assert!(pipe.request(&asked, "the early request"));
    // Asking twice writes once.
    assert!(pipe.request(&asked, "the early request again"));
    // Wait for the child to have copied the request out before ending it: a
    // fixed sleep here loses to a Slang compile in the next crate's tests,
    // the drop killing the child before its `cat` began.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut bytes = Vec::new();
    while std::time::Instant::now() < deadline {
        bytes = std::fs::read(&echoed).unwrap_or_default();
        if bytes.len() >= WireHeader::SIZE + size_of::<RequestBody>() {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    drop(pipe);
    let mut input = std::io::Cursor::new(bytes);
    match wire::read(&mut input) {
        Ok(Some(Frame::Request(got))) => {
            assert_eq!(got.kind, wire::REQ_APP);
            assert_eq!(got.name, "org.example.App");
        }
        other => panic!("the child's stdin held {other:?}, not the request"),
    }
    assert!(
        matches!(wire::read(&mut input), Ok(None)),
        "asked twice, written once"
    );
}

/// A child that restarts is asked everything its predecessor was: its
/// answers named glyph ids and slots the new child knows nothing about.
#[test]
fn a_restarted_child_is_asked_everything_again() {
    let counter = scratch("restart-count", b"0");
    let echoed = scratch("restart-out.bin", &[]);
    // First life: read one frame's worth and exit, so the pipe restarts it;
    // second life: append what arrives.
    let script = format!(
        "n=$(cat {c}); echo $((n+1)) > {c}; if [ \"$n\" = 0 ]; then head -c 48 > /dev/null; exit 0; fi; cat >> {o}",
        c = counter.display(),
        o = echoed.display()
    );
    let (tx, _rx) = channel::<Update>();
    let pipe = Pipe::start(&player("restart.sh", &script), tx);
    thread::sleep(Duration::from_millis(300));
    let asked = Frame::Request(Request {
        kind: wire::REQ_GLYPH,
        arg: 0,
        name: "audio-volume-high-symbolic".into(),
    });
    assert!(pipe.request(&asked, "the request"));
    // Wait for the second child to have copied the replay out before ending
    // it, as the test above does: a fixed sleep here loses to a Slang
    // compile in another crate's tests, the drop killing the restarted child
    // before its `cat` began. The restart itself waits out BACKOFF_START.
    let deadline = std::time::Instant::now() + BACKOFF_START + Duration::from_secs(10);
    let mut bytes = Vec::new();
    while std::time::Instant::now() < deadline {
        bytes = std::fs::read(&echoed).unwrap_or_default();
        if bytes.len() >= WireHeader::SIZE + size_of::<RequestBody>() {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    drop(pipe);
    let mut input = std::io::Cursor::new(bytes);
    match wire::read(&mut input) {
        Ok(Some(Frame::Request(got))) => assert_eq!(got.name, "audio-volume-high-symbolic"),
        other => panic!("the second child's stdin held {other:?}, not the replayed request"),
    }
}

/// *Hello*: a sidecar built against another contract is one witness, a dead
/// child and **no restart**. The proof that a mixed install stops at the first
/// frame rather than at the first wrong byte in a GPU buffer (docs/architecture.md,
/// *The ABI*).
#[test]
fn a_sidecar_from_another_tree_is_refused_and_never_restarted() {
    let mut greeting = Vec::new();
    wire::write(
        &mut greeting,
        &Frame::Hello(wire::Hello {
            abi_hash: !crate::abi::ABI_HASH,
            name: "caustics-sysmon".to_owned(),
            flags: 0,
        }),
    )
    .unwrap();
    // It would have gone on to write a perfectly good samples frame.
    greeting.extend_from_slice(&samples(2, 1.0));
    let path = scratch("other-tree.bin", &greeting);

    let (tx, rx) = channel::<Update>();
    let pipe = Pipe::start(
        &player("other-tree.sh", &format!("cat {}", path.display())),
        tx,
    );
    let seen = collect(&rx, 2, 5);
    // Long enough that a backoff restart would have happened twice over.
    thread::sleep(BACKOFF_START + Duration::from_millis(500));
    let restarts = pipe.stats().restarts();
    let refused = pipe.stats().refused();
    let ticks = pipe.stats().ticks();
    drop(pipe);

    assert!(refused, "the stats say the child was refused");
    assert_eq!(restarts, 0, "a refused child is not a crashed one");
    assert_eq!(ticks, 0, "and nothing it wrote was read");
    assert!(
        seen.iter()
            .any(|update| matches!(update, Update::Absent(name) if name == "other-tree")),
        "its rows leave the bar: {seen:?}"
    );
    assert!(
        !seen
            .iter()
            .any(|update| matches!(update, Update::Samples { .. })),
        "the frame behind the greeting was never forwarded: {seen:?}"
    );
}

/// And a child that writes a frame *before* it says what it is gets the same
/// answer: a process that will not say it is ours has not said it is ours.
#[test]
fn a_sidecar_that_never_greets_is_refused_too() {
    let path = scratch("ungreeted.bin", &samples(1, 1.0));
    let (tx, rx) = channel::<Update>();
    let pipe = Pipe::start(
        &player("ungreeted.sh", &format!("cat {}", path.display())),
        tx,
    );
    let seen = collect(&rx, 2, 5);
    thread::sleep(BACKOFF_START + Duration::from_millis(500));
    let refused = pipe.stats().refused();
    let restarts = pipe.stats().restarts();
    drop(pipe);

    assert!(refused, "an ungreeted child is refused");
    assert_eq!(restarts, 0, "and not restarted");
    assert!(
        !seen
            .iter()
            .any(|update| matches!(update, Update::Samples { .. })),
        "its frame was never forwarded: {seen:?}"
    );
}

/// `Hello.flags` is reserved, and the embassy reads none of it — a child
/// whose greeting sets a bit is
/// greeted and heard like any other.
#[test]
fn a_greetings_reserved_word_is_not_read() {
    let mut greeting = Vec::new();
    wire::write(
        &mut greeting,
        &wire::Frame::Hello(wire::Hello {
            abi_hash: crate::abi::ABI_HASH,
            name: "a test sidecar".into(),
            flags: 1,
        }),
    )
    .unwrap();
    greeting.extend_from_slice(&samples(1, 1.0));
    let path = scratch("reserved-word.bin", &greeting);
    let script = format!("cat {}; exec sleep 30", path.display());
    let (tx, rx) = channel::<Update>();
    let pipe = Pipe::start(&player("reserved-word.sh", &script), tx);
    assert_eq!(collect(&rx, 1, 5).len(), 1, "the child greeted and spoke");
    assert!(
        !pipe.stats().refused(),
        "a bit in the reserved word refuses nothing"
    );
}
