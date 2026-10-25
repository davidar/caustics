use tokio::sync::mpsc::unbounded_channel;

use super::*;

#[test]
fn the_three_commands_parse_and_nothing_else_does() {
    assert_eq!(parse("lock\n"), Ok(Command::Lock { wait: false }));
    assert_eq!(parse("  lock   wait \n"), Ok(Command::Lock { wait: true }));
    assert_eq!(parse("status"), Ok(Command::Status));
    for bad in [
        "",
        "\n",
        "unlock",
        "lock now",
        "lock wait wait",
        "status please",
        "LOCK",
    ] {
        assert!(parse(bad).is_err(), "{bad:?} parsed");
    }
}

fn scratch(name: &str) -> PathBuf {
    // A socket path must fit `sun_path` (108 bytes), which the target
    // directory's does not: the system's temporary directory, as the
    // wallpaper's tests use it, with a socket file and nothing else in it.
    let dir = std::env::temp_dir().join(format!("caustics-lock-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(SOCKET)
}

/// A stale socket file (nothing listening) is taken over; a live one is
/// another lock's and is not.
#[test]
fn a_stale_socket_is_replaced_and_a_live_one_is_not() {
    let path = scratch("bind");
    drop(UnixListener::bind(&path).unwrap());
    assert!(path.exists(), "a stale socket file to start from");
    let live = bind(&path).expect("the stale socket replaced");
    let err = bind(&path).expect_err("a live socket taken");
    assert_eq!(err.kind(), io::ErrorKind::AddrInUse);
    drop(live);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

/// A client's line reaches the conductor as a command, and the reply the
/// conductor sends is the client's line back; a bad line is answered with
/// an error without reaching it.
#[tokio::test]
async fn a_line_in_is_a_command_and_its_reply_comes_back() {
    let path = scratch("serve");
    let (events, mut inbox) = unbounded_channel();
    serve(bind(&path).unwrap(), events).unwrap();
    let client = std::thread::spawn({
        let path = path.clone();
        move || {
            let mut stream = UnixStream::connect(&path).unwrap();
            let mut lines = BufReader::new(stream.try_clone().unwrap());
            let mut got = Vec::new();
            for line in ["bogus\n", "lock wait\n"] {
                stream.write_all(line.as_bytes()).unwrap();
                let mut reply = String::new();
                lines.read_line(&mut reply).unwrap();
                got.push(reply);
            }
            got
        }
    });
    match inbox.recv().await {
        Some(Event::Command(Command::Lock { wait: true }, reply)) => {
            reply.send(LOCKED.to_owned()).unwrap()
        }
        _ => panic!("not the lock wait"),
    }
    let got = tokio::task::spawn_blocking(move || client.join().unwrap())
        .await
        .unwrap();
    assert!(got[0].starts_with("error "), "{got:?}");
    assert_eq!(got[1], "locked\n");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}
