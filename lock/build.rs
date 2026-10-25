//! `libpam.so` for the linker.
//!
//! `pam-sys` links `-lpam` and `-lpam_misc`, which the linker resolves
//! through the development names `libpam.so` and `libpam_misc.so`. Those are
//! the PAM development package's; a system without it still has the
//! libraries under their runtime names, `libpam.so.0` and `libpam_misc.so.0`,
//! which is all a program linked against them needs. So where the compiler
//! finds a runtime name and no development name, this links the one to the
//! other in `OUT_DIR` and adds that directory to the search path. Where the
//! development package is installed it does nothing. No header is read and
//! nothing is compiled.

use std::path::PathBuf;
use std::process::Command;

/// Where the C compiler's own search finds `name`: `cc
/// -print-file-name=NAME` prints an absolute path when it found the file and
/// the bare name when it did not.
fn found(cc: &str, name: &str) -> Option<PathBuf> {
    let out = Command::new(cc)
        .arg(format!("-print-file-name={name}"))
        .output()
        .ok()?;
    let path = PathBuf::from(String::from_utf8(out.stdout).ok()?.trim());
    path.is_absolute().then_some(path)
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=CC");
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_owned());
    let dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR")).join("link");
    std::fs::create_dir_all(&dir).expect("OUT_DIR/link");
    for lib in ["pam", "pam_misc"] {
        let link = dir.join(format!("lib{lib}.so"));
        let _ = std::fs::remove_file(&link);
        if found(&cc, &format!("lib{lib}.so")).is_some() {
            continue;
        }
        match found(&cc, &format!("lib{lib}.so.0")) {
            Some(runtime) => std::os::unix::fs::symlink(&runtime, &link).unwrap_or_else(|e| {
                panic!("linking {} to {}: {e}", link.display(), runtime.display())
            }),
            None => println!(
                "cargo:warning=neither lib{lib}.so nor lib{lib}.so.0 is on the compiler's search path; \
                 the link will fail"
            ),
        }
    }
    println!("cargo:rustc-link-search=native={}", dir.display());
}
