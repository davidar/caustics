/// The rendered modules are written once into a directory named by the hash,
/// and what is there is the rendering, byte for byte: a compile at run time
/// puts this directory on the compiler's search path and reads nothing else for
/// `import bar_abi;`.
#[test]
fn the_rendered_dir_holds_the_rendering() {
    let dir = crate::rendered_dir();
    assert!(dir.is_absolute(), "{} is not absolute", dir.display());
    assert_eq!(
        dir.file_name().and_then(|name| name.to_str()),
        Some(format!("{:016x}", crate::ABI_HASH).as_str()),
        "{} is not named by the ABI hash",
        dir.display()
    );
    let rendered = crate::rendered();
    assert_eq!(
        rendered.each_ref().map(|(name, _)| *name),
        crate::RENDERED,
        "rendered() names its modules as RENDERED does"
    );
    for (name, text) in rendered {
        let path = dir.join(name);
        let on_disk = std::fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("could not read {}: {err}", path.display()));
        assert!(
            on_disk == text,
            "{} is not what the declarations render",
            path.display()
        );
    }
    assert_eq!(
        crate::rendered_dir(),
        dir,
        "the directory is resolved once per process"
    );
}

/// `ABI_HASH` is a hash of something.
///
/// There is no second copy to compare it against — that is the point of it —
/// so what can be asserted is that `build.rs` read its five inputs and folded
/// them in: a value still at FNV's offset basis means it hashed nothing, and
/// zero means it was never written at all. The property that matters, "the
/// number moves when the contract does", is `build.rs`'s `rerun-if-changed`
/// lines and is checked by the nested gate (bar/bar-test.sh), which plays back a greeting carrying another hash and watches the bar refuse the child.
#[test]
fn the_contract_has_one_hash() {
    assert_ne!(crate::ABI_HASH, 0, "ABI_HASH was never written");
    assert_ne!(
        crate::ABI_HASH,
        0xcbf2_9ce4_8422_2325,
        "ABI_HASH hashed nothing"
    );
}

/// Every constant and struct is declared once, so a name cannot appear in both
/// modules — `bar_abi` re-exports `bar_wire`, and a duplicate would be a
/// compile error in every caustic at once.
#[test]
fn the_two_modules_declare_disjoint_names() {
    let mut names: Vec<&str> = Vec::new();
    for group in crate::buffer::CONST_GROUPS
        .iter()
        .chain(crate::wire::CONST_GROUPS)
    {
        names.extend(group.consts.iter().map(|decl| decl.name));
    }
    for entry in crate::buffer::STRUCTS.iter().chain(crate::wire::STRUCTS) {
        names.push(entry.decl.name);
    }
    let mut sorted = names.clone();
    sorted.sort_unstable();
    let before = sorted.len();
    sorted.dedup();
    assert_eq!(before, sorted.len(), "a name is declared in both modules");
}

/// Binding numbers are never declared as constants: the bar builds its bind
/// groups from the reflection by name (docs/abi.md, *Bindings and the oracle*),
/// and a constant would be a second copy that could drift.
#[test]
fn no_binding_number_is_declared_as_a_constant() {
    let [(_, abi), (_, wire), (_, canvas)] = crate::rendered();
    for gone in [
        // The bindings are read from the reflection by name (docs/abi.md, *Bindings and the oracle*).
        "SAMPLES_BINDING",
        "LITERALS_BINDING",
        "FACTS_BINDING",
    ] {
        let needle = format!(" {gone} ");
        for (name, source) in [
            ("bar_abi", &abi),
            ("bar_wire", &wire),
            ("canvas_abi", &canvas),
        ] {
            let found = source
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
                .any(|line| line.contains(&needle));
            assert!(!found, "`{gone}` is declared again in {name}");
        }
    }
}

/// The canvas's module stands alone: it imports nothing (the bar's modules
/// declare names the canvas renders again), it declares no name twice, and
/// the wire's canvas words reach it and not `bar_wire` (no bar caustic reads
/// a `Canvas` body).
#[test]
fn the_canvas_module_imports_nothing_of_the_bar_s() {
    let [_, (_, wire), (_, canvas)] = crate::rendered();
    let code = |source: &str| -> Vec<String> {
        source
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .map(str::to_owned)
            .collect()
    };
    assert!(
        !code(&canvas).iter().any(|line| line.contains("import ")),
        "canvas_abi imports something"
    );
    assert!(
        code(&canvas)
            .iter()
            .any(|line| line.contains(" CANVAS_SHOW "))
    );
    assert!(
        !code(&wire)
            .iter()
            .any(|line| line.contains(" CANVAS_SHOW "))
    );
    let mut names: Vec<&str> = Vec::new();
    for group in crate::canvas::CONST_GROUPS {
        names.extend(group.consts.iter().map(|decl| decl.name));
    }
    names.extend(crate::canvas::STRUCTS.iter().map(|entry| entry.decl.name));
    let before = names.len();
    names.sort_unstable();
    names.dedup();
    assert_eq!(
        before,
        names.len(),
        "a name is declared twice in canvas_abi"
    );
}
