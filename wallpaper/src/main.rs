//! Two lines: everything is in the library, so the tests reach the parser,
//! the fitting and the frames.

fn main() {
    // The first frame on the wire is the contract this binary was built
    // against (docs/abi.md, *Hello*).
    caustics_bar_abi::wire::greet(env!("CARGO_BIN_NAME"));
    std::process::exit(caustics_wallpaper::run());
}
