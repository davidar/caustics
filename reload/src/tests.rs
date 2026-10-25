use std::cell::Cell;

use super::*;

/// A directory of this test's own, named after the test: the suite runs its
/// cases on threads of one process, so a shared temp name is a shared file.
fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("caustics-reload-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the temp dir is writable");
    dir
}

/// A [`Toolchain`] whose compiler must never be called and a cache directory
/// of its own: everything about a key and an entry can be tested without
/// compiling.
fn offline(cache: &Path) -> Toolchain {
    Toolchain {
        compiler: never,
        version: "test".to_owned(),
        cache: Some(cache.to_path_buf()),
    }
}

/// A compiler that fails the test if it is called at all.
fn never(source: &Path, _: &[&str], _: &Options) -> Result<punchcutter_slang::Program, SlangError> {
    panic!("compiled {} where nothing should be", source.display())
}

thread_local! {
    /// How many times [`recording`] has run on this test's thread. Per
    /// thread because the suite runs its cases on threads of one process, and
    /// [`recording`] compiles on the caller's thread, so each test counts
    /// only its own.
    static COMPILES: Cell<usize> = const { Cell::new(0) };
}

/// The real compiler, counted: the only honest way to assert "compiled
/// nothing". A witness line says what the process believes, and this says
/// what it did.
fn recording(
    source: &Path,
    entries: &[&str],
    options: &Options,
) -> Result<punchcutter_slang::Program, SlangError> {
    COMPILES.with(|count| count.set(count.get() + 1));
    punchcutter_slang::compile_program(source, entries, options)
}

/// How many times [`recording`] has run on this thread.
fn compiles() -> usize {
    COMPILES.with(Cell::get)
}

/// A tree with every file [`watched`] lists in the shipped `caustics/`, so a
/// key has something to read.
fn watched_tree(dir: &Path, body: &str) {
    for name in watched(&caustics_dir(), &BAR) {
        std::fs::write(dir.join(&name), format!("// {name}\n{body}")).expect("writable");
    }
}

/// Modules that are not SPIR-V and do not have to be: what a cache entry is
/// asked is whether the bytes come back exactly as they went in.
fn fake_modules(entries: &[&str], fill: u8) -> Vec<Module> {
    entries
        .iter()
        .enumerate()
        .map(|(index, entry)| Module {
            generation: 1,
            index: index as u32,
            count: entries.len() as u32,
            format: format_code(DstFormat::Bgra8.define()),
            name: (*entry).to_owned(),
            spirv: vec![fill; 64 + index],
            reflection: format!("{{\"bindings\":[],\"entry\":\"{entry}\"}}"),
        })
        .collect()
}

/// The tree is compiled where it stands: the compiler is handed the directory's own
/// `bar.slang` with the directory on the include path, so the files its
/// diagnostics name are the files a person edits.
#[test]
fn the_shipped_module_is_compiled_in_place() {
    let dir = caustics_dir();
    let source = source(&dir, &BAR);
    assert_eq!(source.entry_file, dir.join("bar.slang"));
    assert_eq!(
        source.includes,
        vec![dir.clone(), caustics_bar_abi::rendered_dir().to_path_buf(),]
    );
    let watched = watch(&dir, &BAR);
    assert!(
        watched.paths().contains(&dir.join(TUNABLES)),
        "the tunables are watched in the tree like every other module"
    );
}

/// The startup rule, against a compiler that fails the test if it is ever
/// called: the format arriving over a tree the cache has never seen compiles
/// nothing and sends nothing, and the same format over a tree whose bytes the
/// cache holds sends that generation without compiling.
#[test]
fn a_start_sends_the_cached_program_and_never_compiles() {
    let dir = temp_dir("start");
    let tree = dir.join("caustics");
    std::fs::create_dir_all(&tree).expect("writable");
    watched_tree(&tree, "const int WIDTH = 3;\n");
    let cache = dir.join("cache");
    let entries = &BAR_ENTRIES[..2];
    let define = DstFormat::Bgra8.define();

    // `never` panics on a call, so every step below that returns at all
    // compiled nothing.
    let tools = Toolchain::with(never, Some(cache.clone()));

    // A miss: no generation, and no compile either.
    assert!(warm_start(&tools, &tree, &BAR, define, 1, entries).is_none());
    assert!(!cache.is_dir(), "a start that misses writes nothing");

    // The generation a save would have compiled, stored as `run` stores it.
    let key = cache_key(&tools, &tree, &BAR, define, entries).expect("the tree reads");
    let file = tools.entry_file(key).expect("a caching toolchain");
    write_entry(&file, key, &fake_modules(entries, 0x5a)).expect("writable");

    let program = warm_start(&tools, &tree, &BAR, define, 7, entries).expect("a hit");
    assert!(program.warm);
    assert_eq!(program.modules.len(), entries.len());
    for (index, module) in program.modules.iter().enumerate() {
        assert_eq!(module.generation, 7);
        assert_eq!(module.index, index as u32);
        assert_eq!(module.format, format_code(define));
        assert_eq!(module.name, entries[index]);
    }

    // An edit the cache has not seen is a miss again: the start leaves the
    // bar on what it was built from, and the save compiles it.
    std::fs::write(tree.join(TUNABLES), "const int WIDTH = 4;\n").expect("writable");
    assert!(warm_start(&tools, &tree, &BAR, define, 8, entries).is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

/// The sidecar's whole job in one pass: the shipped caustic compiled for the
/// format the bar asked for, turned into a frame, and read back as the bar's
/// own reader reads it. It is what would catch a `format` word that does not
/// match the define, a body over the wire's cap, and a compile whose
/// reflection never reached the frame.
///
/// **Two entry points, not all of them.** That every entry point compiles —
/// for both swapchain formats — is `bar/src/abi.rs`'s test and
/// `bar/build.rs`'s every build. What this one buys beyond those is the
/// framing, and two entries buy the ordering with it. All of them would be
/// the slowest thing in the workspace's suite and would run beside the bar's
/// own, whose pipe tests time child processes: that made them flake, which
/// is the whole reason `compile_entries` takes a list.
///
/// It is also the **cache**'s end-to-end test, for the same budget: the cold
/// pass above is a miss that writes an entry, and the second pass through the
/// same two entry points is the warm start the sidecar exists to make fast.
/// The compiler both passes are given is a counting wrapper around the real
/// one, so "compiled nothing" is not a witness line taken on trust but a
/// count of the times the compiler was called at all.
#[test]
fn a_generation_is_its_entry_points_in_order_for_the_asked_format() {
    let dir = caustics_dir();
    if !dir.join("bar.slang").is_file() {
        eprintln!("skipping: no caustics directory at {}", dir.display());
        return;
    }
    let define = DstFormat::Bgra8.define();
    let entries = &BAR_ENTRIES[..2];
    let scratch = temp_dir("generation");
    let cache = scratch.join("cache");

    let before = compiles();
    let cold = Toolchain::with(recording, Some(cache.clone()));
    let program = compile_entries(&cold, &dir, &BAR, define, 3, entries)
        .expect("the shipped caustic compiles");
    assert_eq!(program.modules.len(), entries.len());
    assert!(
        !program.warm,
        "the first compile of a tree cannot be a cache hit"
    );

    // The frames as `send` writes them — a module is one frame where it fits
    // under the cap and several where it does not (docs/abi.md, *Module*) — and the
    // reassembly the bar does, here against the compiler's own output.
    let mut wire_bytes = Vec::new();
    let mut frames = 0usize;
    for module in &program.modules {
        for chunk in module.chunks(wire::WIRE_MAX_BODY) {
            wire::write(&mut wire_bytes, &Frame::Module(chunk)).expect("frames");
            frames += 1;
        }
    }
    assert!(
        frames >= entries.len(),
        "at least one frame per entry point"
    );
    let mut cursor = io::Cursor::new(wire_bytes);
    let mut assembly = wire::ModuleAssembly::new();
    for (index, &entry) in entries.iter().enumerate() {
        let back = loop {
            let Some(Frame::Module(chunk)) = wire::read(&mut cursor).expect("reads") else {
                panic!("a frame before entry point {index} is not a module");
            };
            if let Some(whole) = assembly.push(chunk) {
                break whole;
            }
        };
        assert_eq!(back.generation, 3);
        assert_eq!(back.index, index as u32);
        assert_eq!(back.count, entries.len() as u32);
        assert_eq!(back.format, format_code(define));
        assert_eq!(back.name, entry);
        assert!(
            back.spirv.len() > 4 && back.spirv.len() % 4 == 0,
            "{}",
            back.spirv.len()
        );
        // The reflection is what carries the caustic's literals and its
        // bindings to the bar, and it is the whole program's: every entry
        // point of a generation carries the same one.
        let reflection = punchcutter_slang::Reflection::from_json(&back.reflection)
            .unwrap_or_else(|err| panic!("{entry}'s reflection does not read back: {err}"));
        assert!(!reflection.bindings.is_empty(), "{entry} has no bindings");
        assert_eq!(back.reflection, program.modules[0].reflection);
    }
    assert!(
        wire::read(&mut cursor).expect("reads").is_none(),
        "nothing past the last entry point"
    );

    // And a real generation is `BAR_ENTRIES`, which is the bar's list and is what
    // the bar checks a `count` against (`host/src/modules.rs`).
    assert!(
        BAR_ENTRIES.len() <= MAX_MODULES,
        "a generation has to fit in one"
    );

    // --- and now the cache ---------------------------------------------
    //
    // Nothing is written until the frames are out, which is the order `run`
    // uses: the bar waits for a compile and never for a megabyte of disk.
    assert!(!cache.is_dir(), "a program is not cached before it is sent");
    cold.store(&program);
    assert_eq!(
        compiles() - before,
        1,
        "one compile for the generation: every entry point in one link"
    );

    // A second process over the same tree: a new `Toolchain`, and nothing
    // compiles at all.
    let warm_tools = Toolchain::with(recording, Some(cache.clone()));
    let warm =
        compile_entries(&warm_tools, &dir, &BAR, define, 4, entries).expect("the cache answers");
    assert!(warm.warm, "the same bytes must not be compiled twice");
    assert_eq!(
        compiles() - before,
        1,
        "a warm start calls the compiler not at all"
    );

    // Byte for byte what the cold compile would have sent, with this
    // generation's number on it.
    assert_eq!(warm.modules.len(), program.modules.len());
    for (fresh, cached) in program.modules.iter().zip(&warm.modules) {
        assert_eq!(cached.generation, 4);
        assert_eq!(cached.index, fresh.index);
        assert_eq!(cached.count, fresh.count);
        assert_eq!(cached.format, fresh.format);
        assert_eq!(cached.name, fresh.name);
        assert_eq!(
            cached.spirv, fresh.spirv,
            "{} came back changed",
            fresh.name
        );
        assert_eq!(cached.reflection, fresh.reflection);
    }
    assert_eq!(warm.bytes, program.bytes);
    // The witness a warm start is read by in a harness run, and the
    // milliseconds the cache exists to move.
    eprintln!("cold {} ms, warm {} ms", program.millis, warm.millis);

    let _ = std::fs::remove_dir_all(&scratch);
}

/// The key is the inputs' **bytes**, and nothing else about them: an editor
/// that writes the same file back is a hit, and a checkout that restores older
/// bytes with an older mtime is a miss. Neither question mtimes can answer,
/// which is why [`cache_key`] does not ask them.
#[test]
fn the_key_is_content_and_not_mtime() {
    let dir = temp_dir("key");
    let tools = offline(&dir.join("cache"));
    watched_tree(&dir, "const int WIDTH = 3;\n");
    let entries = &BAR_ENTRIES[..2];
    let define = DstFormat::Bgra8.define();
    let key = cache_key(&tools, &dir, &BAR, define, entries).expect("the tree reads");

    // Rewritten with the same bytes and a newer mtime: the same program.
    std::thread::sleep(std::time::Duration::from_millis(10));
    watched_tree(&dir, "const int WIDTH = 3;\n");
    let touched = std::fs::metadata(dir.join(TUNABLES))
        .expect("there")
        .modified()
        .expect("mtime");
    assert_eq!(cache_key(&tools, &dir, &BAR, define, entries), Some(key));

    // One byte different: another program, however the clock reads. The mtime
    // is put *back* to prove the point.
    std::fs::write(
        dir.join(TUNABLES),
        "// bar_tunables.slang\nconst int WIDTH = 4;\n",
    )
    .expect("writable");
    let handle = std::fs::File::options()
        .write(true)
        .open(dir.join(TUNABLES))
        .expect("there");
    let older = touched - std::time::Duration::from_secs(60);
    handle
        .set_times(std::fs::FileTimes::new().set_modified(older))
        .expect("times");
    drop(handle);
    assert_ne!(cache_key(&tools, &dir, &BAR, define, entries), Some(key));

    // And the things that are not files: the format, the toolchain, the entry
    // list. Each of them changes the SPIR-V from bytes that did not move.
    watched_tree(&dir, "const int WIDTH = 3;\n");
    assert_eq!(cache_key(&tools, &dir, &BAR, define, entries), Some(key));
    assert_ne!(
        cache_key(&tools, &dir, &BAR, DstFormat::Rgba8.define(), entries),
        Some(key),
        "a program for the other swapchain format is another program"
    );
    let newer = Toolchain {
        version: "2027.1.0".to_owned(),
        ..offline(&dir.join("cache"))
    };
    assert_ne!(cache_key(&newer, &dir, &BAR, define, entries), Some(key));
    assert_ne!(
        cache_key(&tools, &dir, &BAR, define, &BAR_ENTRIES),
        Some(key)
    );

    // A tree a compile would fail on has no key, so it writes no entry and
    // finds none: an unreadable input is never cached over.
    // An engine module is read where the compiler will find it: a copy beside the
    // caustic comes first on the search path, so it is what the key hashes.
    std::fs::write(dir.join("text.slang"), "// a copy\n").expect("writable");
    assert_ne!(cache_key(&tools, &dir, &BAR, define, entries), Some(key));
    std::fs::remove_file(dir.join("text.slang")).expect("there");
    assert_eq!(cache_key(&tools, &dir, &BAR, define, entries), Some(key));

    std::fs::remove_file(dir.join(ENTRY)).expect("there");
    assert_eq!(cache_key(&tools, &dir, &BAR, define, entries), None);
    let _ = std::fs::remove_dir_all(&dir);
}

/// An entry is a round trip or it is a miss. Every way of being wrong —
/// truncated, damaged, keyed elsewhere, built for other entry points — reads
/// as "compile it again" and says so, because a cache that hands the bar
/// half a module is worse than no cache at all.
#[test]
fn a_short_or_damaged_entry_is_a_miss() {
    let dir = temp_dir("entry");
    let entries = &BAR_ENTRIES[..3];
    let modules = fake_modules(entries, 0xa5);
    let file = dir.join("00000000deadbeef.modules");
    write_entry(&file, 0xdead_beef, &modules).expect("writable");

    let back = read_entry(&file, 0xdead_beef, entries).expect("its own key reads it");
    assert_eq!(back.len(), modules.len());
    for (cached, module) in back.iter().zip(&modules) {
        assert_eq!(cached.name, module.name);
        assert_eq!(cached.spirv, module.spirv);
        assert_eq!(cached.reflection, module.reflection);
    }

    assert!(
        read_entry(&file, 0x0bad_0bad, entries).is_none(),
        "another key is another program"
    );
    assert!(
        read_entry(&file, 0xdead_beef, &BAR_ENTRIES[..2]).is_none(),
        "another entry list"
    );
    assert!(
        read_entry(
            &file,
            0xdead_beef,
            &["bar_shape", "bar_compose", "bar_apply"]
        )
        .is_none()
    );
    assert!(read_entry(&dir.join("nothing.modules"), 0, entries).is_none());

    // Truncated — a sidecar killed mid-write could not leave this, because the
    // write is a rename, but a full disk or a bad block can.
    let whole = std::fs::read(&file).expect("there");
    std::fs::write(&file, &whole[..whole.len() - 9]).expect("writable");
    assert!(
        read_entry(&file, 0xdead_beef, entries).is_none(),
        "short in its bodies"
    );
    std::fs::write(&file, &whole[..20]).expect("writable");
    assert!(
        read_entry(&file, 0xdead_beef, entries).is_none(),
        "short in its head"
    );

    // One flipped byte in the middle of the SPIR-V, which no length catches.
    let mut damaged = whole.clone();
    let middle = damaged.len() / 2;
    damaged[middle] ^= 0xff;
    std::fs::write(&file, &damaged).expect("writable");
    assert!(
        read_entry(&file, 0xdead_beef, entries).is_none(),
        "the payload hash catches it"
    );

    // And an entry from some other program entirely.
    std::fs::write(&file, b"caustics, but not this").expect("writable");
    assert!(read_entry(&file, 0xdead_beef, entries).is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

/// The bound: [`CACHE_KEEP`] entries survive, the oldest go, and nothing that
/// is not an entry is touched.
#[test]
fn the_bound_evicts_the_oldest() {
    let dir = temp_dir("bound");
    let entries = &BAR_ENTRIES[..1];
    let extra = 3;
    let mut oldest = Vec::new();
    for step in 0..CACHE_KEEP + extra {
        let file = dir.join(format!("{step:016x}.modules"));
        write_entry(&file, step as u64, &fake_modules(entries, step as u8)).expect("writable");
        // Backwards in time, one minute per step, so the last written is the
        // oldest and a mtime sort is the only thing that can pick it out.
        let handle = std::fs::File::options()
            .write(true)
            .open(&file)
            .expect("there");
        let age = std::time::Duration::from_secs(60 * step as u64);
        handle
            .set_times(std::fs::FileTimes::new().set_modified(SystemTime::now() - age))
            .expect("times");
        if step >= CACHE_KEEP {
            oldest.push(file);
        }
    }
    // Something that is not an entry, which eviction must not see.
    std::fs::write(dir.join("README"), "not a module").expect("writable");

    prune(&dir);
    let left: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("readable")
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|suffix| suffix == "modules"))
        .collect();
    assert_eq!(left.len(), CACHE_KEEP, "{left:?}");
    for gone in &oldest {
        assert!(!gone.exists(), "{} is older than the bound", gone.display());
    }
    assert!(dir.join("README").exists(), "eviction only removes entries");

    // A second pass with nothing to do is not an error.
    prune(&dir);
    assert_eq!(
        std::fs::read_dir(&dir).expect("readable").count(),
        CACHE_KEEP + 1
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Every module the caustic `import`s is one this sidecar watches, one of the
/// bar's rendered contract or one of the engine's; and every watched one is a
/// file `bar/bar-test.sh` copies to a remote target.
///
/// A watched module that is not copied leaves a remote target running its
/// baked SPIR-V, and every reload there fails with `cannot open file`; this
/// test keeps the lists in step. The rendered and the engine's modules are not
/// copied: the target's own binaries write them from their compiled-in copies.
///
/// The list is one fact — *which files the caustic is made of* — and its
/// answer is "every `*.slang` in `caustics/`", spelled three times: by
/// [`watched`], by `bar/build.rs`'s `rerun-if-changed` and by the harness's
/// copy, which is a shell glob. The Rust ones read the directory; the shell
/// one is checked by reading the script.
#[test]
fn every_imported_module_is_watched_and_copied() {
    // Anchored on the manifest, not on the working directory: `cargo test -p`
    // runs from the package, so a relative `caustics` finds nothing and a
    // relative `bar/bar-test.sh` is silently absent. `bar_check.slang` is
    // appended under an `#ifdef` by the oracle rather than imported, and is
    // listed like every other file of the directory.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let dir = root.join("caustics");
    assert!(dir.is_dir(), "no caustics directory at {}", dir.display());
    let harness = root.join("bar/bar-test.sh");
    let script = std::fs::read_to_string(&harness)
        .unwrap_or_else(|err| panic!("{} is not readable: {err}", harness.display()));
    assert!(
        script.contains("\"$REPO\"/caustics/*.slang"),
        "`bar/bar-test.sh` does not copy every `caustics/*.slang` into the guest"
    );

    let watched = watched(&dir, &BAR);
    assert!(
        watched.iter().any(|name| name == TUNABLES),
        "the tunables are a watched module"
    );
    let files = watched
        .iter()
        .map(|name| (name.clone(), dir.join(name)))
        .chain(RENDERED.iter().map(|name| {
            (
                (*name).to_owned(),
                caustics_bar_abi::rendered_dir().join(name),
            )
        }))
        .chain(ENGINE.iter().map(|name| {
            (
                (*name).to_owned(),
                punchcutter_slang::slang_dir().join(name),
            )
        }));
    for (name, path) in files {
        assert!(path.is_file(), "{name} is not at {}", path.display());
        let text = std::fs::read_to_string(&path).expect("a readable module");
        for line in text.lines() {
            let line = line.trim();
            // Both spellings: `__exported import` is how a module re-exports
            // the names it imports, and `shape_abi` arrives that way.
            let Some(rest) = line
                .strip_prefix("import ")
                .or_else(|| line.strip_prefix("__exported import "))
            else {
                continue;
            };
            let module = rest.trim_end_matches(';').trim();
            let file = format!("{module}.slang");
            assert!(
                watched.contains(&file)
                    || RENDERED.contains(&file.as_str())
                    || ENGINE.contains(&file.as_str()),
                "{name} imports `{module}` and `{file}` is in none of `caustics/`, RENDERED and ENGINE"
            );
        }
    }
}

/// A sidecar with no directory to watch waits for the bar to close the pipe
/// rather than exiting into the bar's restart backoff: the wait is the read
/// to end of input, and it returns when that end comes.
#[test]
fn idling_ends_with_the_input() {
    let mut input = std::io::Cursor::new(b"not a frame, never read as one".to_vec());
    idle_until_eof(&mut input);
    assert_eq!(input.position(), input.get_ref().len() as u64);
}

/// `REQ_PROGRAM`'s program word names the caustic: zero is the bar's, which
/// is every request the bar has ever sent, one is the canvas's, and a word
/// past them is no caustic at all.
#[test]
fn the_program_word_names_the_caustic() {
    assert_eq!(Caustic::of(PROGRAM_BAR), Some(&BAR));
    assert_eq!(
        PROGRAM_BAR, 0,
        "the bar's word is the one its frames always carried"
    );
    assert_eq!(Caustic::of(PROGRAM_CANVAS), Some(&CANVAS));
    assert_eq!(Caustic::of(caustics_bar_abi::wire::PROGRAM_LAST + 1), None);
    assert_eq!(BAR.entries, &BAR_ENTRIES[..]);
    assert_eq!(CANVAS.entries, &CANVAS_ENTRIES[..]);
    assert_eq!(ENTRY, "bar.slang");
    assert_eq!(CANVAS.entry, "canvas.slang");
    assert_eq!(CANVAS.tunables, "canvas_tunables.slang");
}

/// One directory, two caustics: each watches its own modules, entry file
/// first, and an edit to one is not an input of the other, so a saved
/// `canvas_tunables.slang` never recompiles the bar and a saved
/// `bar_tunables.slang` never recompiles the canvas.
#[test]
fn each_caustic_watches_its_own_modules_and_no_other() {
    let dir = temp_dir("two-caustics");
    for name in [
        "bar.slang",
        "bar_tunables.slang",
        "dock.slang",
        "canvas.slang",
        "canvas_state.slang",
        "canvas_tunables.slang",
    ] {
        std::fs::write(dir.join(name), format!("// {name}\n")).expect("writable");
    }
    assert_eq!(
        watched(&dir, &BAR),
        ["bar.slang", "bar_tunables.slang", "dock.slang"]
    );
    assert_eq!(
        watched(&dir, &CANVAS),
        [
            "canvas.slang",
            "canvas_state.slang",
            "canvas_tunables.slang"
        ]
    );

    let tools = offline(&dir.join("cache"));
    let define = DstFormat::Rgba8.define();
    let bar = cache_key(&tools, &dir, &BAR, define, &BAR_ENTRIES).expect("reads");
    let canvas = cache_key(&tools, &dir, &CANVAS, define, &CANVAS_ENTRIES).expect("reads");
    std::fs::write(dir.join("canvas_tunables.slang"), "// edited\n").expect("writable");
    assert_eq!(
        cache_key(&tools, &dir, &BAR, define, &BAR_ENTRIES),
        Some(bar),
        "a canvas edit moved the bar's key"
    );
    assert_ne!(
        cache_key(&tools, &dir, &CANVAS, define, &CANVAS_ENTRIES),
        Some(canvas)
    );
    let canvas = cache_key(&tools, &dir, &CANVAS, define, &CANVAS_ENTRIES);
    std::fs::write(dir.join("bar_tunables.slang"), "// edited\n").expect("writable");
    assert_eq!(
        cache_key(&tools, &dir, &CANVAS, define, &CANVAS_ENTRIES),
        canvas,
        "a bar edit moved the canvas's key"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
/// The shipped tree has both caustics in it, and the canvas's entry file is
/// one of its own modules' list like the bar's is of its.
#[test]
fn the_shipped_tree_has_both_caustics() {
    let dir = caustics_dir();
    let canvas = watched(&dir, &CANVAS);
    assert_eq!(canvas.first().map(String::as_str), Some("canvas.slang"));
    assert!(canvas.iter().any(|name| name == CANVAS.tunables));
    assert!(dir.join(CANVAS.entry).is_file());
    assert!(
        !watched(&dir, &BAR)
            .iter()
            .any(|name| name.starts_with("canvas"))
    );
}

/// The canvas's caustic compiles where it stands, both entry points at once,
/// for both formats, and its literals ride the reflection as the bar's do.
#[test]
fn the_shipped_canvas_compiles_for_both_formats() {
    let dir = caustics_dir();
    let scratch = temp_dir("canvas");
    for format in DstFormat::ALL {
        let tools = Toolchain::with(recording, Some(scratch.join("cache")));
        let program = compile(&tools, &dir, &CANVAS, format.define(), 1)
            .expect("the shipped canvas compiles");
        let names: Vec<&str> = program.modules.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, CANVAS_ENTRIES);
        let reflection = punchcutter_slang::Reflection::from_json(&program.modules[0].reflection)
            .expect("the reflection reads");
        assert!(
            reflection
                .strings
                .values()
                .any(|text| text.contains("midpoint")),
            "the fade's witnesses are in the reflection's literals"
        );
    }
    let _ = std::fs::remove_dir_all(&scratch);
}
