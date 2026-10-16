//! Two lines: everything is in the library so `glyphs/src/tests.rs` can reach
//! the font load, the chunking and the icon append.

fn main() {
    // The first frame on the wire is the contract this binary was built against
    // (docs/abi.md, *Hello*). A bar whose own `ABI_HASH`
    // differs kills this child and never starts it again, which is what makes a
    // bar and its sidecars one install set rather than binaries that happen to be
    // beside each other.
    caustics_bar_abi::wire::greet(env!("CARGO_BIN_NAME"));
    std::process::exit(caustics_glyphs::run());
}
