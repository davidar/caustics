//! Two lines: everything is in the library, so the tests reach the
//! conductor, the question table and the agent object.

fn main() {
    // The first frame on the wire is the contract this binary was built
    // against (docs/abi.md, *Hello*).
    caustics_bar_abi::wire::greet(env!("CARGO_BIN_NAME"));
    std::process::exit(caustics_secrets::run());
}
