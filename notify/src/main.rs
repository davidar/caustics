//! Two lines: everything is in the library so `notify/tests/` can run the
//! daemon against a private bus.

fn main() {
    // The first frame on the wire is the contract this binary was built against
    // (docs/abi.md, *Hello*). Which clicks want
    // an activation token is said per row (`RFLAG_ACTIVATION`), not here.
    caustics_bar_abi::wire::greet(env!("CARGO_BIN_NAME"));
    std::process::exit(caustics_notify::run(std::env::args().skip(1)));
}
