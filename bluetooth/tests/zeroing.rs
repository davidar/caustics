//! A typed PIN leaves no copy of itself behind in this sidecar: the sink's
//! reply line, read into the socket client's buffer, the `Secret` it hands
//! up, and the reply the conductor builds for BlueZ, are each zeroed before
//! their memory goes back.
//!
//! The allocator watch is the secret agent's (`secrets/tests/zeroing.rs`):
//! this binary's global allocator looks into each block as it is freed and
//! remembers whether the marker was in it. The method return zbus serialises
//! the reply into is zbus's, and is not watched here (`src/reply.rs` says
//! so). One test in its own binary, so no other test's allocations are being
//! watched.

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use caustics_bluetooth::agent::{Event, Outcome, Request};
use caustics_bluetooth::pairing::{Effect, Pairing};
use caustics_bluetooth::sink;
use tokio::sync::oneshot;

/// What was typed, as the test's own copy spells it: a PIN of the longest
/// length BlueZ takes.
const MARKER: &[u8] = b"pin-marker-5d2aq";
/// The sink's reply carrying it: a static, which is never freed.
const REPLY: &[u8] = b"text pin-marker-5d2aq\n";

static ARMED: AtomicBool = AtomicBool::new(false);
static LEAKED: AtomicBool = AtomicBool::new(false);

struct Watching;

unsafe impl GlobalAlloc for Watching {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if ARMED.load(Ordering::SeqCst) {
            // SAFETY: the block is still ours until `System.dealloc` below,
            // and it is `layout.size()` bytes long.
            let block = unsafe { std::slice::from_raw_parts(ptr, layout.size()) };
            if block.windows(MARKER.len()).any(|w| w == MARKER) {
                LEAKED.store(true, Ordering::SeqCst);
            }
        }
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Watching = Watching;

#[test]
fn no_copy_of_a_pin_outlives_its_reply() {
    let dir = std::env::temp_dir().join(format!("caustics-bt-zero-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(sink::SOCKET);
    let listener = UnixListener::bind(&path).unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap();
        stream.write_all(REPLY).unwrap();
    });

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    runtime.block_on(async {
        let mut pairing = Pairing::new();
        let (reply, outcome) = oneshot::channel();
        let effects = pairing.event(
            Event::Ask {
                device: "/org/bluez/hci0/dev_02_5E_00_00_0B_30".to_owned(),
                title: "Example Keyboard".to_owned(),
                request: Request::Pin,
                reply,
            },
            Instant::now(),
        );
        let Some(Effect::Ask { id, typed, title }) = effects.into_iter().next() else {
            panic!("no ask");
        };
        ARMED.store(true, Ordering::SeqCst);
        let answer = sink::ask(&path, typed, &title).await;
        pairing.typed(id, answer);
        match outcome.await {
            Ok(Outcome::Pin(pin)) => {
                assert_eq!(pin.value().as_bytes(), MARKER);
                drop(pin);
            }
            other => panic!("not the reply: {other:?}"),
        }
        ARMED.store(false, Ordering::SeqCst);
    });
    server.join().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        !LEAKED.load(Ordering::SeqCst),
        "a block holding the typed bytes was freed without being zeroed"
    );

    // And the watch can fail: a plain copy freed with the bytes in it is
    // caught.
    let mut plain = Vec::with_capacity(MARKER.len());
    plain.extend_from_slice(MARKER);
    ARMED.store(true, Ordering::SeqCst);
    drop(plain);
    ARMED.store(false, Ordering::SeqCst);
    assert!(
        LEAKED.load(Ordering::SeqCst),
        "the watch missed a plain copy"
    );
}
