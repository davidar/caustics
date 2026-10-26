//! Bakes `caustics/canvas.slang` into SPIR-V, once per swapchain format,
//! exactly as `bar/build.rs` bakes the bar's: one `compile_program` per format
//! linking both entry points, the search path `caustics/` and then the
//! rendered contracts under `OUT_DIR` (the text engine's modules follow on
//! every compile by themselves), and the program's reflection beside the
//! modules so the host knows its bindings and literals without compiling.
//!
//! Every entry point × each [`DstFormat`] variant is `<entry>.<suffix>.spv` in
//! `OUT_DIR`; `src/gpu.rs` picks the pair matching the surface format it
//! negotiated.

use std::path::{Path, PathBuf};

use caustics_bar_abi::{CANVAS_ENTRIES, DstFormat, SHARED_MODULES};
use punchcutter_slang::Options;

fn main() {
    let caustics = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../caustics");
    let source = caustics.join("canvas.slang");
    // The canvas's own modules, the `canvas*.slang` files and the modules it
    // shares with the bar (`SHARED_MODULES`): the same set `caustics-reload`
    // watches for this caustic. A module missing from this list would leave
    // an edit to it out of the baked SPIR-V.
    let mut modules: Vec<PathBuf> = std::fs::read_dir(&caustics)
        .unwrap_or_else(|err| panic!("could not list {}: {err}", caustics.display()))
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().is_some_and(|ext| ext == "slang")
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.starts_with("canvas") || SHARED_MODULES.contains(&name)
                    })
        })
        .collect();
    modules.sort();
    for module in &modules {
        println!("cargo:rerun-if-changed={}", module.display());
    }
    println!("cargo:rerun-if-changed=build.rs");

    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    let rendered = out_dir.join("slang");
    caustics_bar_abi::write_rendered(&rendered).unwrap_or_else(|err| {
        panic!(
            "could not write the rendered modules to {}: {err}",
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
        let program = punchcutter_slang::compile_program(&source, &CANVAS_ENTRIES, &options)
            .unwrap_or_else(|err| panic!("{err}"));
        for line in program.warnings.lines() {
            println!("cargo:warning=slang: {line}");
        }
        for entry in &program.entries {
            let output = out_dir.join(format!("{}.{}.spv", entry.name, format.suffix()));
            write(&output, &entry.spirv);
        }
        if !reflected {
            write(
                &out_dir.join("canvas.reflection.json"),
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
