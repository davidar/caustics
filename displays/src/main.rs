//! Two lines: everything is in the library so `displays/tests/` can drive the
//! translation with fakes.

fn main() {
    // The first frame on the wire is the contract this binary was built against
    // (docs/abi.md, *Hello*). A bar whose own `ABI_HASH` differs kills this
    // child and never starts it again.
    caustics_bar_abi::wire::greet(env!("CARGO_BIN_NAME"));
    std::process::exit(caustics_displays::run(std::env::args().skip(1)));
}
