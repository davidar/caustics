//! The binary in its dry run with **no system bus at all**: no logind, no
//! signals, no hint, and the socket's `lock` as the only trigger. This test
//! plays the canvas host on the sidecar's pipes and a client on its socket,
//! through a wrong answer and the right one, and reads the witness for both
//! secrets.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use caustics_bar_abi::wire::{
    self, Answer, CANVAS_EVENT_LOCKED, CANVAS_EVENT_UNLOCKED, CanvasEventBody, Frame, LOCK_LOCK,
    LOCK_UNLOCK, PROMPT_DONE, PROMPT_ERROR, PROMPT_INFO, PROMPT_INFO_TITLE, PROMPT_SECRET, Prompt,
    Secret,
};

const RIGHT: &str = "dry-run-right-7c1f";
const WRONG: &str = "dry-run-wrong-30aa";

struct Sidecar {
    child: Child,
    to: Option<ChildStdin>,
    from: ChildStdout,
    runtime: PathBuf,
}

impl Sidecar {
    fn start(name: &str) -> Sidecar {
        // A socket path must fit `sun_path`, which the target directory's
        // may not: the system's temporary directory, a socket file in it.
        let runtime =
            std::env::temp_dir().join(format!("caustics-lock-it-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&runtime);
        std::fs::create_dir_all(&runtime).unwrap();
        let stderr = std::fs::File::create(runtime.join("stderr")).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_caustics-lock"))
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("XDG_RUNTIME_DIR", &runtime)
            .env(
                "DBUS_SYSTEM_BUS_ADDRESS",
                format!("unix:path={}", runtime.join("no-system-bus").display()),
            )
            .env("CAUSTICS_LOCK_DRY_RUN_SECRET", RIGHT)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr)
            .spawn()
            .expect("the sidecar starts");
        let to = child.stdin.take();
        let from = child.stdout.take().unwrap();
        Sidecar {
            child,
            to,
            from,
            runtime,
        }
    }

    fn socket(&self) -> PathBuf {
        self.runtime.join("caustics-lock.sock")
    }

    fn frame(&mut self) -> Frame {
        wire::read(&mut self.from)
            .expect("a frame")
            .expect("not the end")
    }

    fn write(&mut self, frame: &Frame) {
        wire::write(self.to.as_mut().unwrap(), frame).expect("the sidecar's stdin");
    }

    fn event(&mut self, event: u32) {
        self.write(&Frame::CanvasEvent(CanvasEventBody {
            event,
            ..CanvasEventBody::default()
        }));
    }

    fn prompt(&mut self) -> Prompt {
        match self.frame() {
            Frame::Prompt(prompt) => prompt,
            other => panic!("not a prompt: {other:?}"),
        }
    }

    fn lock(&mut self) -> u32 {
        match self.frame() {
            Frame::Lock(lock) => lock.command,
            other => panic!("not a lock: {other:?}"),
        }
    }

    fn stderr(&self) -> String {
        std::fs::read_to_string(self.runtime.join("stderr")).unwrap_or_default()
    }
}

impl Drop for Sidecar {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.runtime);
    }
}

/// One line to the socket and its reply, retried while the socket is not
/// there yet.
fn ask(socket: &Path, line: &str) -> UnixStream {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut stream = loop {
        match UnixStream::connect(socket) {
            Ok(stream) => break stream,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => panic!("no socket at {}: {e}", socket.display()),
        }
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream.write_all(format!("{line}\n").as_bytes()).unwrap();
    stream
}

fn reply(stream: UnixStream) -> String {
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    line.trim_end().to_owned()
}

fn text(id: u32, bytes: &str) -> Frame {
    Frame::Answer(Answer::Text {
        id,
        bytes: Secret::new(bytes.as_bytes().to_vec()),
    })
}

#[test]
fn without_a_system_bus_the_socket_locks_and_only_the_right_answer_unlocks() {
    let mut lock = Sidecar::start("whole");
    match lock.frame() {
        Frame::Hello(hello) => assert_eq!(hello.name, "caustics-lock"),
        other => panic!("not a hello: {other:?}"),
    }
    let waiting = ask(&lock.socket(), "lock wait");
    assert_eq!(lock.lock(), LOCK_LOCK);
    lock.event(CANVAS_EVENT_LOCKED);
    assert_eq!(reply(waiting), "locked");

    let title = lock.prompt();
    assert_eq!((title.kind, title.arg), (PROMPT_INFO, PROMPT_INFO_TITLE));
    let ask_one = lock.prompt();
    assert_eq!(
        (ask_one.id, ask_one.kind, ask_one.message.as_str()),
        (title.id, PROMPT_SECRET, "Password: ")
    );
    lock.write(&text(ask_one.id, WRONG));
    let error = lock.prompt();
    assert_eq!(
        (error.kind, error.message.as_str()),
        (PROMPT_ERROR, "Authentication failure")
    );
    let done = lock.prompt();
    assert_eq!((done.id, done.kind, done.arg), (ask_one.id, PROMPT_DONE, 0));
    let title = lock.prompt();
    assert_eq!(title.kind, PROMPT_INFO);
    let ask_two = lock.prompt();
    assert_eq!(ask_two.kind, PROMPT_SECRET);
    assert_ne!(ask_two.id, ask_one.id);
    assert_eq!(
        reply(ask(&lock.socket(), "status")),
        "ok locked=yes attempts=1"
    );

    lock.write(&text(ask_two.id, RIGHT));
    let done = lock.prompt();
    assert_eq!((done.id, done.kind, done.arg), (ask_two.id, PROMPT_DONE, 1));
    assert_eq!(lock.lock(), LOCK_UNLOCK);
    lock.event(CANVAS_EVENT_UNLOCKED);
    // The unlocked event is read on the sidecar's own time: wait for it.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let status = reply(ask(&lock.socket(), "status"));
        if status == "ok locked=no attempts=1" {
            break;
        }
        assert!(Instant::now() < deadline, "still {status:?}");
        std::thread::sleep(Duration::from_millis(20));
    }

    // The host goes; the sidecar leaves with it, and takes its socket.
    drop(lock.to.take());
    let mut rest = Vec::new();
    lock.from.read_to_end(&mut rest).unwrap();
    assert!(
        rest.is_empty(),
        "frames after the end: {} bytes",
        rest.len()
    );
    let status = lock.child.wait().unwrap();
    assert_eq!(status.code(), Some(0));
    assert!(!lock.socket().exists(), "the socket outlived the sidecar");

    let witness = lock.stderr();
    assert!(witness.contains("no logind"), "{witness}");
    assert!(!witness.contains("LockedHint of session"), "{witness}");
    assert!(
        !witness.contains(RIGHT),
        "the right secret is in the witness"
    );
    assert!(
        !witness.contains(WRONG),
        "the wrong secret is in the witness"
    );
}
