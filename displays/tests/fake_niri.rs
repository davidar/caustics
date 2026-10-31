//! The binary against a socket standing in for niri's: the first read
//! publishes the chip, a scale row's click is one `Output` request and its
//! outcome one witness, the only output on refuses to turn off with no
//! request at all, and stdin's EOF is the exit.

mod common;

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::net::UnixListener;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use caustics_bar_abi::buffer::{ACTION_ACTIVATE, ACTION_MENU_CLOSE, ACTION_MENU_OPEN};
use caustics_bar_abi::wire::{self, ActionBody, Frame};
use caustics_displays::rows;

/// eDP-1's slot in the fixture (DP-1 sorts first).
const EDP: u32 = 2 * rows::SLOT_STRIDE;

#[test]
fn a_click_is_one_request_and_one_witness() {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("displays-fake-niri");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("niri.sock");
    let listener = UnixListener::bind(&socket).unwrap();

    // The fake: every line it is sent is kept; `"Outputs"` is the fixture,
    // anything else is `Applied`.
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let kept = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut line = String::new();
            if BufReader::new(&stream).read_line(&mut line).is_err() {
                continue;
            }
            let line = line.trim().to_owned();
            let reply = if line == r#""Outputs""# {
                common::REPLY.replace('\n', "")
            } else {
                r#"{"Ok":{"OutputConfigChanged":"Applied"}}"#.to_owned()
            };
            kept.lock().unwrap().push(line);
            let _ = writeln!(stream, "{reply}");
        }
    });

    let mut child = Command::new(env!("CARGO_BIN_EXE_caustics-displays"))
        .env("NIRI_SOCKET", &socket)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawning caustics-displays");
    let mut stdin = child.stdin.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let lines = Arc::new(Mutex::new(String::new()));
    let into = lines.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n) = stderr.read(&mut buf) {
            if n == 0 {
                break;
            }
            into.lock()
                .unwrap()
                .push_str(&String::from_utf8_lossy(&buf[..n]));
        }
    });
    let wait_for = |what: &str| {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !lines.lock().unwrap().contains(what) {
            assert!(
                Instant::now() < deadline,
                "no {what:?} in {}",
                lines.lock().unwrap()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    let act = |stdin: &mut std::process::ChildStdin, row, kind| {
        let body = ActionBody {
            row,
            kind,
            ..ActionBody::default()
        };
        wire::write(stdin, &Frame::Action(body)).unwrap();
        stdin.flush().unwrap();
    };

    wait_for("published rev=1 outputs=DP-1 off; eDP-1 on 2880x1800@120.001 scale=1.75 Normal");
    act(&mut stdin, rows::ROW_CHIP, ACTION_MENU_OPEN);
    act(&mut stdin, EDP + rows::OFF_SCALES + 2, ACTION_ACTIVATE);
    wait_for("eDP-1 scale 1.5: ok");
    act(&mut stdin, EDP + rows::OFF_ON, ACTION_ACTIVATE);
    wait_for("eDP-1 off: refused");
    act(&mut stdin, rows::ROW_CHIP, ACTION_MENU_CLOSE);
    drop(stdin);

    let mut out = Vec::new();
    child.stdout.take().unwrap().read_to_end(&mut out).unwrap();
    let status = child.wait().unwrap();
    assert!(status.success(), "{status:?}");
    wait_for("stdin closed; exiting");

    let seen = seen.lock().unwrap().clone();
    let changes: Vec<&String> = seen.iter().filter(|l| *l != r#""Outputs""#).collect();
    assert_eq!(
        changes,
        [r#"{"Output":{"action":{"Scale":{"scale":{"Specific":1.5}}},"output":"eDP-1"}}"#],
        "every request: {seen:?}"
    );
    // The first read, the menu's opening read, and the read after the change
    // (some of which may be one read).
    assert!(!seen.is_empty() && seen[0] == r#""Outputs""#);

    // Hello, then exactly one rows frame: the fake's outputs never changed.
    let mut stream = out.as_slice();
    let mut frames = Vec::new();
    while let Some(frame) = wire::read(&mut stream).unwrap() {
        frames.push(frame);
    }
    assert!(matches!(frames[0], Frame::Hello(_)), "{:?}", frames[0]);
    let rows_frames = frames
        .iter()
        .filter(|f| matches!(f, Frame::Rows(_)))
        .count();
    assert_eq!(rows_frames, 1);
}

#[test]
fn without_niri_there_is_no_chip() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_caustics-displays"))
        .env_remove("NIRI_SOCKET")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawning caustics-displays");
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no $NIRI_SOCKET; nothing to show"),
        "{stderr}"
    );
    let mut stream = output.stdout.as_slice();
    let mut frames = Vec::new();
    while let Some(frame) = wire::read(&mut stream).unwrap() {
        frames.push(frame);
    }
    assert_eq!(frames.len(), 1, "Hello and nothing else: {frames:?}");
}
