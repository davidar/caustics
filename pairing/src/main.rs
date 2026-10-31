//! Two lines: everything is in the library, so the tests reach the
//! conductor, the question table and the socket.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // `--ask` is a client of a running sink, for a person or the gate:
    // stdout is its reply, so no `Hello` goes there.
    if args.first().is_some_and(|a| a == "--ask") {
        std::process::exit(caustics_pairing::ask_sink(&args[1..]));
    }
    // The first frame on the wire is the contract this binary was built
    // against (docs/abi.md, *Hello*).
    caustics_bar_abi::wire::greet(env!("CARGO_BIN_NAME"));
    std::process::exit(caustics_pairing::run(&args));
}
