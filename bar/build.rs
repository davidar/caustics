//! Bakes `caustics/bar.slang` into SPIR-V, once per swapchain format.
//!
//! The compiler is `punchcutter_slang`, Slang in process, the same one
//! `caustics-reload` calls at run time, so a baked caustic and a hot-reloaded
//! one are compiled alike: one `compile_program` per format, linking every
//! entry point at once. The search path is the source's own directory,
//! `caustics/`, then one directory under `OUT_DIR` holding the bar's two
//! modules rendered from `bar/abi/`; the text engine's modules come last, on
//! every compile's path by themselves.
//!
//! Every entry point × each [`DstFormat`] variant is one module in `OUT_DIR`,
//! named `<entry>.<suffix>.spv`; `bar/src/gpu.rs` picks the set matching the
//! surface format it negotiated. A storage image's format is fixed in the
//! SPIR-V that writes it, which is the whole reason for one module per format.

use std::path::{Path, PathBuf};

// The entry points and the swapchain formats, from the one place each is
// named: the compiler sidecar reads the same two from the same crate.
use caustics_bar_abi::{DstFormat, ENTRIES};
use punchcutter_slang::Options;

fn main() {
    let caustics = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../caustics");
    let source = caustics.join("bar.slang");
    // Every `*.slang` in `caustics/`, not the directory: the bar is one
    // caustic and the modules of its own it imports, which are the directory's
    // `.slang` files (caustics/README.md, *The files*), and another file there
    // changing (the README) must not rebuild this crate. The tunables are
    // baked in like anything else. A module missing from this list is a
    // silent bug: an edit to it leaves the baked SPIR-V alone, so `cargo
    // build` rebuilds the binary around the *old* shader and the bar runs code
    // nobody wrote. A module added later is found when this script next runs,
    // which an edit to the module that imports it causes. `bar_abi` and
    // `bar_wire` are not files here: they are rendered from `bar/abi/`
    // (docs/abi.md, *The two contracts*) below, and a change to that crate
    // reruns this script.
    let mut modules: Vec<PathBuf> = std::fs::read_dir(&caustics)
        .unwrap_or_else(|err| panic!("could not list {}: {err}", caustics.display()))
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "slang"))
        .collect();
    modules.sort();
    for module in &modules {
        println!("cargo:rerun-if-changed={}", module.display());
    }
    println!("cargo:rerun-if-changed=build.rs");

    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    // The modules the caustic imports that are not in `caustics/`, which the
    // baked SPIR-V contains as much as the bar's own: the bar's two rendered
    // contracts, from the build-dependency `caustics-bar-abi`. A change to it
    // is a change to a build-dependency, which reruns this script; so is a
    // change to the engine's modules, which travel inside `punchcutter-slang`.
    let rendered = out_dir.join("slang");
    caustics_bar_abi::write_rendered(&rendered).unwrap_or_else(|err| {
        panic!(
            "could not write the bar's rendered modules to {}: {err}",
            rendered.display()
        )
    });
    let includes: [&Path; 2] = [&caustics, &rendered];
    let mut reflected = false;

    for format in DstFormat::ALL {
        let (define, image_format_unknown) = format.args();
        let options = Options {
            defines: &[define],
            includes: &includes,
            image_format_unknown,
        };
        let program = punchcutter_slang::compile_program(&source, &ENTRIES, &options)
            .unwrap_or_else(|err| panic!("{err}"));
        // Nothing subscribes to `tracing` in a build script, so Slang's
        // success-path warnings are surfaced by hand.
        for line in program.warnings.lines() {
            println!("cargo:warning=slang: {line}");
        }
        for entry in &program.entries {
            let output = out_dir.join(format!("{}.{}.spv", entry.name, format.suffix()));
            write(&output, &entry.spirv);
        }
        // One reflection serves every entry point and both formats: the
        // parameters (bindings, sets, access), the struct layouts and the
        // `getStringHash` literals are the program's, and the program is the
        // same source either way. Baked once so the bar knows all three at
        // startup without compiling.
        if !reflected {
            write(
                &out_dir.join("bar.reflection.json"),
                program.reflection.to_json().as_bytes(),
            );
            reflected = true;
        }
    }
}

fn write(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes)
        .unwrap_or_else(|err| panic!("could not write {}: {err}", path.display()));
}
