//! An `Answer`'s typed bytes are zeroed by every copy the wire makes of them
//! (docs/abi.md, *Answer*): the writer's frame, the reader's body, and the
//! `Secret` the reader hands up, once it drops.
//!
//! What is checked is the allocator's view, which is the only one that sees
//! every copy: this binary's global allocator looks into each block as it is
//! freed and remembers whether the marker was in it. A copy that was not
//! zeroed before its memory went back is caught however it was made (a
//! growing buffer's old block, a forgotten `to_vec`), with nothing read
//! after it was freed. One test in its own binary, so no other test's
//! allocations are being watched.

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::Cursor;
use std::sync::atomic::{AtomicBool, Ordering};

use caustics_bar_abi::wire::{self, Answer, Frame, MAX_ANSWER_LEN, Secret, WireHeader};

/// What was typed, as the test's own copy spells it.
const MARKER: &[u8] = b"zeroing-marker-7c1e";

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

/// Zero a buffer the test made itself and then let it go: the test's own
/// copies are not the wire's to clean.
fn wipe(mut bytes: Vec<u8>) {
    zeroize::Zeroize::zeroize(&mut bytes);
}

#[test]
fn no_copy_of_an_answer_outlives_its_frame() {
    // Room for everything up front, so no test buffer grows while armed.
    let mut wire_bytes: Vec<u8> = Vec::with_capacity(4096);
    let mut typed = Vec::with_capacity(MAX_ANSWER_LEN);
    typed.extend_from_slice(MARKER);
    ARMED.store(true, Ordering::SeqCst);

    // The writer: the frame it built is zeroed after the write.
    let frame = Frame::Answer(Answer::Text {
        id: 3,
        bytes: Secret::new(typed),
    });
    wire::write(&mut wire_bytes, &frame).expect("writes");
    assert!(wire_bytes.windows(MARKER.len()).any(|w| w == MARKER));
    drop(frame);

    // A malformed answer with the same bytes: skipped, and its body zeroed.
    let mut bad = wire_bytes.clone();
    bad[WireHeader::SIZE + 4..WireHeader::SIZE + 8].copy_from_slice(&0u32.to_le_bytes());

    // The reader: the body is zeroed, and the secret it hands up zeroes
    // itself when it drops.
    let mut input = Cursor::new(wire_bytes);
    match wire::read(&mut input).expect("reads") {
        Some(Frame::Answer(Answer::Text { id: 3, bytes })) => {
            assert_eq!(bytes.as_bytes(), MARKER);
            drop(bytes);
        }
        other => panic!("not the answer: {other:?}"),
    }
    let mut input_bad = Cursor::new(bad);
    assert!(matches!(
        wire::read(&mut input_bad).expect("reads"),
        Some(Frame::Skipped(_))
    ));

    wipe(input.into_inner());
    wipe(input_bad.into_inner());
    ARMED.store(false, Ordering::SeqCst);
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
