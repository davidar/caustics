//! Bakes `caustics/bar.slang` into SPIR-V, once per swapchain format.
//!
//! The slangc driver is `punchcutter_core::slang`, the same one `caustics-reload` calls
//! at run time, so a baked caustic and a hot-reloaded one are compiled alike.
//! Its `compile_capturing` takes `(program, source, entry, defines, extra_args, includes)`;
//! the search path is the source's own directory — which is exactly
//! `caustics/`, where `bar_abi.slang` lives.
//!
//! Every entry point × each [`DstFormat`] variant is one module in `OUT_DIR`,
//! named `<entry>.<suffix>.spv`; `bar/src/gpu.rs` picks the set matching the
//! surface format it negotiated. A storage image's format is fixed in the
//! SPIR-V that writes it, which is the whole reason for one module per format.

use std::path::{Path, PathBuf};

// The entry points and the swapchain formats, from the one place each is
// named: the compiler sidecar reads the same two from the same crate.
use caustics_bar_abi::{DstFormat, ENTRIES};
use punchcutter_core::slang;

fn main() {
    let caustics = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../caustics");
    let source = caustics.join("bar.slang");
    // The named files, not the directory: the bar is one caustic and the
    // modules it imports, and another file in `caustics/` changing must not
    // rebuild this crate. The tunables are baked in like anything else — a user's override of `bar_tunables.slang` (caustics/README.md) is read only by `caustics-reload` at run time, never by this build. `bar_abi` and
    // `bar_wire` are generated from `bar/abi/` and checked in (docs/abi.md
    // *The two contracts*); `bar_model` is the caustic's own helpers.
    println!("cargo:rerun-if-changed={}", source.display());
    // Everything `bar.slang` imports, transitively. A module missing from this
    // list is a silent bug: an edit to it leaves the baked SPIR-V alone, so
    // `cargo build` rebuilds the binary around the *old* shader and the bar
    // runs code nobody wrote.
    for module in [
        "bar_abi.slang",
        "bar_wire.slang",
        "bar_model.slang",
        "bar_tunables.slang",
    ] {
        println!("cargo:rerun-if-changed={}", caustics.join(module).display());
    }
    // And the engine's, which the baked SPIR-V contains as much as the bar's
    // own. An engine change normally arrives as a dependency bump, which
    // rebuilds this crate anyway; naming the files covers a local checkout.
    let engine = punchcutter_core::slang_dir();
    for module in ["text.slang", "shape.slang", "shape_abi.slang", "ot.slang"] {
        println!("cargo:rerun-if-changed={}", engine.join(module).display());
    }
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=SLANGC");

    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    let slangc = slang::slangc_program();
    let includes: [&Path; 2] = [&caustics, engine];
    let mut reflected = false;

    for format in DstFormat::ALL {
        let (define, extra_args) = format.args();
        for entry in ENTRIES {
            let compiled =
                slang::compile_capturing(&slangc, &source, entry, &[define], extra_args, &includes)
                    .unwrap_or_else(|err| panic!("{err}"));
            // Nothing subscribes to `tracing` in a build script, so slangc's
            // success-path warnings are surfaced by hand.
            for line in compiled.warnings.lines() {
                println!("cargo:warning=slangc: {line}");
            }
            let output = out_dir.join(format!("{entry}.{}.spv", format.suffix()));
            write(&output, &compiled.spirv);
            // One reflection serves every entry point and both formats: the
            // parameters (bindings, sets, access) and the `getStringHash`
            // literals are the program's, not an entry point's. Baked once so
            // the bar knows both at startup without slangc.
            if !reflected {
                write(
                    &out_dir.join("bar.reflection.json"),
                    compiled.reflection.as_bytes(),
                );
                reflected = true;
            }
        }
    }
}

fn write(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes)
        .unwrap_or_else(|err| panic!("could not write {}: {err}", path.display()));
}
