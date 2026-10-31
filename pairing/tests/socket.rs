//! The socket's language, its bind and takeover, and a line in reaching the
//! conductor and its reply coming back.

use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

use caustics_pairing::conversation::{Command, Event, Question, line};
use caustics_pairing::socket::{SOCKET, bind, parse, serve};

#[test]
fn the_four_commands_parse_and_nothing_else_does() {
    assert_eq!(
        parse("pin Example Keyboard\n"),
        Ok(Command::Ask {
            question: Question::Pin,
            title: "Example Keyboard".to_owned()
        })
    );
    assert_eq!(
        parse("passkey  02:5E:00:00:0B:99 \n"),
        Ok(Command::Ask {
            question: Question::Passkey,
            title: "02:5E:00:00:0B:99".to_owned()
        })
    );
    assert_eq!(parse("cancel\n"), Ok(Command::Cancel));
    assert_eq!(parse("status"), Ok(Command::Status));
    for bad in [
        "",
        "\n",
        "pin",
        "pin \n",
        "passkey\n",
        "pin a\0b\n",
        "pin a\rb\n",
        "cancel now",
        "status please",
        "PIN x",
        "confirm x",
    ] {
        assert!(parse(bad).is_err(), "{bad:?} parsed");
    }
}

fn scratch(name: &str) -> PathBuf {
    // A socket path must fit `sun_path` (108 bytes), which the target
    // directory's does not: the system's temporary directory, as the lock's
    // and the wallpaper's tests use it, with a socket file and nothing else.
    let dir = std::env::temp_dir().join(format!("caustics-pairing-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(SOCKET)
}

#[test]
fn a_stale_socket_is_replaced_and_a_live_one_is_not() {
    let path = scratch("bind");
    let window = Duration::from_millis(200);
    drop(UnixListener::bind(&path).unwrap());
    assert!(path.exists(), "a stale socket file to start from");
    let live = bind(&path, window).expect("the stale socket replaced");
    let err = bind(&path, window).expect_err("a live socket taken");
    assert_eq!(err.kind(), io::ErrorKind::AddrInUse);
    drop(live);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn a_listener_that_closes_during_the_window_is_taken() {
    let path = scratch("closing");
    let first = bind(&path, Duration::from_millis(200)).expect("binds");
    let closer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        drop(first);
    });
    let started = Instant::now();
    let second = bind(&path, Duration::from_secs(5)).expect("taken once the first closed");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the window was waited out instead of the listener's closing"
    );
    closer.join().unwrap();
    drop(second);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

/// A client's line reaches the conductor as a command, and the reply the
/// conductor sends is the client's line back; a bad line is answered with
/// an error without reaching it.
#[test]
fn a_line_in_is_a_command_and_its_reply_comes_back() {
    let path = scratch("serve");
    let (events, inbox) = channel();
    serve(bind(&path, Duration::ZERO).unwrap(), events).unwrap();
    let client = std::thread::spawn({
        let path = path.clone();
        move || {
            let mut stream = UnixStream::connect(&path).unwrap();
            let mut lines = BufReader::new(stream.try_clone().unwrap());
            let mut got = Vec::new();
            for line in ["confirm Example\n", "pin Example Keyboard\n"] {
                stream.write_all(line.as_bytes()).unwrap();
                let mut reply = String::new();
                lines.read_line(&mut reply).unwrap();
                got.push(reply);
            }
            got
        }
    });
    match inbox.recv_timeout(Duration::from_secs(5)) {
        Ok(Event::Command(Command::Ask { question, title }, reply)) => {
            assert_eq!(
                (question, title.as_str()),
                (Question::Pin, "Example Keyboard")
            );
            reply.send(line("text 0000")).unwrap();
        }
        _ => panic!("not the pin"),
    }
    let got = client.join().unwrap();
    assert!(got[0].starts_with("error "), "{got:?}");
    assert_eq!(got[1], "text 0000\n");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}
