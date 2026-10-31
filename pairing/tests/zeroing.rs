//! A typed PIN leaves no copy of itself behind in this sink: the `Secret`
//! the wire hands up, and the reply line the conductor builds from it, are
//! each zeroed before their memory goes back (docs/abi.md, *Answer*).
//!
//! The allocator watch is the secret agent's (`secrets/tests/zeroing.rs`):
//! this binary's global allocator looks into each block as it is freed and
//! remembers whether the marker was in it. The watch covers the conductor and
//! the reply line up to the socket's write; the kernel's socket buffer, and
//! the Bluetooth sidecar's copy (`bluetooth/tests/zeroing.rs`), are outside
//! it. One test in its own binary, so no other test's allocations are being
//! watched.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, Ordering};

use std::sync::mpsc::channel;
use std::time::Duration;

use caustics_bar_abi::wire::{Answer, Frame, MAX_ANSWER_LEN, PROMPT_DONE, PROMPT_ECHO, Secret};
use caustics_pairing::conversation::{Command, Conductor, Event, Question, Sink};

/// What was typed, as the test's own copy spells it: a PIN of the longest
/// length BlueZ takes.
const MARKER: &[u8] = b"pin-marker-5d2aq";

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
    let (events, inbox) = channel();
    let (frames_tx, frames) = channel();
    let sink: Sink = Box::new(move |frame| {
        let _ = frames_tx.send(frame.clone());
        Ok(())
    });
    let conductor = std::thread::spawn(move || Conductor::new(sink).run(inbox));
    let wait = Duration::from_secs(5);

    let (reply, line) = channel();
    events
        .send(Event::Command(
            Command::Ask {
                question: Question::Pin,
                title: "Example Keyboard".to_owned(),
            },
            reply,
        ))
        .unwrap();
    let id = loop {
        match frames.recv_timeout(wait) {
            Ok(Frame::Prompt(p)) if p.kind == PROMPT_ECHO => break p.id,
            Ok(_) => continue,
            Err(_) => panic!("no ask"),
        }
    };

    let mut typed = Vec::with_capacity(MAX_ANSWER_LEN);
    typed.extend_from_slice(MARKER);
    ARMED.store(true, Ordering::SeqCst);
    events
        .send(Event::Answer(Answer::Text {
            id,
            bytes: Secret::new(typed),
        }))
        .unwrap();
    let got = line.recv_timeout(wait).expect("the reply line");
    assert_eq!(&got[..5], b"text ");
    assert_eq!(&got[5..got.len() - 1], MARKER);
    drop(got);
    match frames.recv_timeout(wait) {
        Ok(Frame::Prompt(p)) => assert_eq!((p.kind, p.arg), (PROMPT_DONE, 1)),
        other => panic!("no done: {other:?}"),
    }
    ARMED.store(false, Ordering::SeqCst);
    let _ = events.send(Event::Closed);
    let _ = conductor.join();
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
