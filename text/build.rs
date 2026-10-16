//! Bakes `caustics/text_demo.slang` — and with it `caustics/text.slang`, which
//! it imports — into SPIR-V for the harness.
//!
//! The driver is `caustics_core::slang`, the same one `bar/build.rs` and caustics-reload use. One entry
//! point, one module, no format variants: nothing here writes a swapchain image,
//! so `DstFormat` does not come into it.
//!
//! Baking it at build time rather than in the example means a Slang mistake is a
//! build failure, which is the cheaper place to find one.

use std::path::{Path, PathBuf};

use caustics_core::slang;

const ENTRY: &str = "text_demo";

fn main() {
    let caustics = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../caustics");
    let source = caustics.join("text_demo.slang");
    let module = caustics.join("text.slang");

    // The two files, not the directory: the bar's caustics changing must not rebuild this crate.
    println!("cargo:rerun-if-changed={}", source.display());
    println!("cargo:rerun-if-changed={}", module.display());
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=SLANGC");

    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    let slangc = slang::slangc_program();
    let includes: [&Path; 1] = [&caustics];

    let compiled = slang::compile_capturing(&slangc, &source, ENTRY, &[], &[], &includes)
        .unwrap_or_else(|err| panic!("{err}"));
    // Nothing subscribes to `tracing` in a build script, so slangc's
    // success-path warnings are surfaced by hand. The gate is zero of them.
    for line in compiled.warnings.lines() {
        println!("cargo:warning=slangc: {line}");
    }

    let output = out_dir.join(format!("{ENTRY}.spv"));
    std::fs::write(&output, &compiled.spirv)
        .unwrap_or_else(|err| panic!("could not write {}: {err}", output.display()));
}
