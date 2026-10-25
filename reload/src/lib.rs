//! `caustics-reload` — the compiler sidecar.
//!
//! The bar's hot reload: its caustic, and its configuration too, because they
//! are the same kind of file (docs/architecture.md, *Configuration is the
//! caustic*). A few times a second this process asks whether `bar.slang` or any
//! module it imports, `bar_tunables.slang` among them, has moved in the
//! `caustics/` directory the bar was built from ([`caustics_dir`]); on a change
//! it compiles every entry point, with Slang in this process, and writes the
//! SPIR-V and the reflection to the bar as `Module` frames (docs/abi.md, *Module*). The bar collects a
//! generation and swaps it in with `Gpu::reload_all`, which is all-or-nothing
//! and never touches the state buffer: the bar keeps its popup, its menu level
//! and its animations across the swap.
//!
//! The reload loop lives outside the bar's address space on purpose: the poll,
//! the compiler and its memory, and the invariant that a failed compile sends
//! *nothing* are all here, and the bar links no compiler, reads no `.slang`
//! file and watches no mtime. A caustic is computation that arrives as bytes,
//! the same pattern as the font: a sidecar writes frames, the bar copies bytes
//! into a buffer, the caustic reads them.
//!
//! # Diagnostics go to stderr, verbatim
//!
//! A sidecar's stderr is inherited (docs/abi.md, *Lifecycle*), so Slang's text
//! lands in the journal under the bar's own unit, naming the files in
//! `caustics/` it found wrong. That text is the whole feedback loop for
//! whoever is editing the caustic, and it is never reformatted.
//!
//! # Nothing is compiled before the bar says which program
//!
//! SPIR-V bakes the storage-image format in, so a module is only meaningful
//! for the swapchain format the session actually landed on. The bar sends that
//! format once, as a `REQ_PROGRAM` request naming the format's define (docs/abi.md,
//! *Request*); the pipe remembers it and replays it to every child, so a
//! restarted compiler learns it again with no bar code for it. Before it
//! arrives this process watches and compiles nothing.
//!
//! And when it arrives this process **compiles nothing either**. The bar's
//! baked SPIR-V is this directory as `build.rs` saw it, and a compile of
//! every entry point is seconds of CPU beside a bar that has just started. What it does instead is ask the cache: a generation already
//! compiled from the directory's present bytes is sent at once, which is what
//! makes a restart after a live edit show the edited look within a second. A
//! miss is witnessed and left alone; the next save compiles.
//!
//! # The cache: the same bytes are never compiled twice
//!
//! Every generation is written to `$XDG_CACHE_HOME/caustics/reload/`, keyed on
//! the **content** of what reaches the compiler (the `.slang` files [`watched`],
//! [`RENDERED`] and [`ENGINE`] name), the format define and the compiler's
//! version ([`punchcutter_slang::version`]); a lookup whose
//! inputs hash to an entry already there reads it and compiles nothing.
//! Content, not mtimes: an editor that rewrites a file with the same bytes has
//! to hit, and a `git checkout` that restores older bytes with an older mtime
//! has to miss. The text engine's font cache has the same shape: one file per
//! key, written to a temp name and renamed, and every way of being wrong is a
//! miss and a witness, never a frame.
//!
//! # The compile runs on a thread of its own
//!
//! Slang's global session is made per thread and lives as long as the
//! thread: a hundred-odd milliseconds to create, and the core module it loads
//! stays resident with it. A compile here is rare (a save) and the process
//! is otherwise idle for hours, so each generation is compiled on a thread
//! that ends with it ([`compile_apart`]), and the session and everything it
//! allocated go with the thread, back to the system. A save pays the
//! session's creation again; an idle bar does not pay for a compiler it is
//! not running.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::SystemTime;

use caustics_bar_abi::DstFormat;
use caustics_bar_abi::ENTRIES;
use caustics_bar_abi::wire::{
    self, Frame, MAX_MODULES, Module, REQ_PROGRAM, WIRE_MAX_BODY, format_code,
};
use punchcutter_slang::{Options, SlangError};

use crate::watch::{MtimeWatch, POLL_INTERVAL};

pub mod watch;

/// The caustic itself, the file every compile is handed.
pub const ENTRY: &str = "bar.slang";

/// The bar caustic and every module of its own: each `*.slang` file in `dir`,
/// [`ENTRY`] first and the rest by name. A copy of a [`RENDERED`] or an
/// [`ENGINE`] module that sits beside them is not one of them: it is found
/// by [`unwatched_file`] and is never a live edit.
///
/// A change to any of them has to reach the running bar the same way a change
/// to the tunables does. The directory is **listed when this is called**,
/// which for the watch is once, at start: a module added to `caustics/` later
/// is compiled (an edit to an existing module has to import it, and the
/// compile reads the directory as it finds it) but is not itself watched
/// until the sidecar restarts. [`ENTRY`] is always listed, present or not, so
/// a tree without it has no cache key.
pub fn watched(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| {
            name.ends_with(".slang")
                && name != ENTRY
                && !RENDERED.contains(&name.as_str())
                && !ENGINE.contains(&name.as_str())
        })
        .collect();
    names.sort();
    names.insert(0, ENTRY.to_owned());
    names
}

/// The bar's own contract as the caustic imports it, `bar_abi` and
/// `bar_wire`: rendered from `caustics-bar-abi`, the crate this sidecar links,
/// and written out by [`caustics_bar_abi::rendered_dir`]
/// (docs/abi.md, *The two contracts*).
///
/// The first of two kinds of input that are **not watched**: these are not
/// files a person edits, and they change only with a rebuild, which is a new
/// install set with a new sidecar. Inputs to every compile and to its cache
/// key all the same.
pub use caustics_bar_abi::RENDERED;

/// The text engine's modules the caustic imports, directly (`text`, `shape`)
/// or through them (`ot`, `shape_abi`): punchcutter's, compiled into it and
/// served from memory on every compile's search path, last; the copies
/// [`punchcutter_slang::slang_dir`] writes out are what the cache key hashes.
///
/// The second kind of input that is **not watched**, beside [`RENDERED`]:
/// inputs to every compile and to its cache key, but an engine change is a
/// dependency bump and a rebuild, never a live edit.
pub const ENGINE: &[&str] = &["text.slang", "shape.slang", "ot.slang", "shape_abi.slang"];

/// Where the compiler finds an unwatched module ([`RENDERED`] or [`ENGINE`]) for a
/// compile of `dir`: the directory itself when a copy sits there (the search
/// order puts the source's own directory first, so a copy beside the caustic
/// wins), else the directory its compiled-in copy is written to —
/// [`caustics_bar_abi::rendered_dir`] for the bar's, [`punchcutter_slang::slang_dir`]
/// for the engine's (which the compiler itself serves from memory; the file
/// is for the cache key and the tests).
pub fn unwatched_file(dir: &Path, name: &str) -> PathBuf {
    let beside = dir.join(name);
    if beside.is_file() {
        beside
    } else if RENDERED.contains(&name) {
        caustics_bar_abi::rendered_dir().join(name)
    } else {
        punchcutter_slang::slang_dir().join(name)
    }
}

/// The tunables module's file name: the bar's configuration, watched like
/// every other module the caustic imports.
pub const TUNABLES: &str = "bar_tunables.slang";

/// This sidecar's witness line: `caustics-reload: …` on stderr, in **one write**
/// (docs/abi.md, *Writing a sidecar*; the one copy is `caustics_sidecar::Witness`).
pub const WITNESS: caustics_sidecar::Witness = caustics_sidecar::Witness::new("caustics-reload");

/// One witness line; see [`WITNESS`].
pub fn witness(line: &str) {
    WITNESS.line(line)
}

/// Slang's own text, unframed and unprefixed: it is often several lines with
/// carets under them, and a prefix per line would ruin it.
fn diagnostics(text: &str) {
    let _ = io::stderr().write_all(text.as_bytes());
    if !text.ends_with('\n') {
        let _ = io::stderr().write_all(b"\n");
    }
}

// ---------------------------------------------------------------------------
// Where the files are
// ---------------------------------------------------------------------------

/// Locate `caustics/`: next to the current directory in the `cargo run` case,
/// then `$CAUSTICS_DIR` (which is what the test harness's unit sets, since a user
/// unit's working directory is the home dir), then the source checkout this
/// binary was built from. Inherited from the bar, which is spawned with the
/// unit's environment and passes it on to every child.
///
/// A deployed bar whose caustics are not on disk simply never hot-reloads:
/// this process says so and exits 0; the bar restarts it on its ordinary backoff, so the line recurs.
pub fn caustics_dir() -> PathBuf {
    let cwd_relative = PathBuf::from("caustics");
    if cwd_relative.is_dir() {
        return cwd_relative;
    }
    if let Some(dir) = std::env::var_os("CAUSTICS_DIR") {
        let dir = PathBuf::from(dir);
        if dir.is_dir() {
            return dir;
        }
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../caustics")
}

// ---------------------------------------------------------------------------
// The watch, and what one compile is built from
// ---------------------------------------------------------------------------

/// The poller over every file [`watched`] lists in `dir` now, primed with
/// the state on disk so that startup is not a change.
pub fn watch(dir: &Path) -> MtimeWatch {
    MtimeWatch::new(watched(dir).iter().map(|name| dir.join(name)).collect())
}

/// The `bar.slang` to hand the compiler and the module search path: the
/// directory itself, compiled in place, so Slang's diagnostics name the files
/// a person edits, then the bar's rendered contract ([`RENDERED`]). The text
/// engine's modules ([`ENGINE`]) follow these on every compile by themselves.
pub struct Source {
    pub entry_file: PathBuf,
    pub includes: Vec<PathBuf>,
}

/// Where a compile of `dir` comes from.
pub fn source(dir: &Path) -> Source {
    Source {
        entry_file: dir.join(ENTRY),
        includes: vec![
            dir.to_path_buf(),
            caustics_bar_abi::rendered_dir().to_path_buf(),
        ],
    }
}

// ---------------------------------------------------------------------------
// The cache
// ---------------------------------------------------------------------------

/// The first eight bytes of a cache entry: the magic and this layout's version
/// in one, so a sidecar that writes a different file never reads an older one.
///
/// `2`: the reflection an entry carries is `punchcutter_slang`'s JSON
/// ([`punchcutter_slang::Reflection::to_json`]), the whole program's, where
/// `1` held a reflection in another format, one per entry point.
const CACHE_MAGIC: [u8; 8] = *b"cstcrld2";

/// Entries kept under [`cache_dir`]; the newest survive an eviction.
///
/// Eight, because an entry is one whole generation (a module and the program's reflection per entry point, a few MiB) and the useful history is small: the two
/// swapchain formats a session can land on, times the last few shapes of a
/// tunables file somebody is editing. Twelve megabytes is the bound that buys,
/// and it holds however long the editing goes on.
pub const CACHE_KEEP: usize = 8;

/// The suffix every entry carries, and the only thing [`prune`] will remove.
const CACHE_SUFFIX: &str = "modules";

/// `$XDG_CACHE_HOME/caustics/reload`, or `~/.cache/caustics/reload`.
pub fn cache_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("caustics").join("reload")
}

/// FNV-1a, 64-bit, over length-prefixed fields.
///
/// Not a cryptographic digest and not meant to be: everything it names is
/// derived data this process wrote, and the only question asked of a key is
/// whether these are the same inputs as last time. The length prefix is what
/// keeps two different field splits from hashing alike.
struct Fnv(u64);

impl Fnv {
    fn new() -> Fnv {
        Fnv(0xcbf2_9ce4_8422_2325)
    }

    fn eat(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    /// One field, so that `["ab", "c"]` and `["a", "bc"]` are different keys.
    fn field(&mut self, bytes: &[u8]) {
        self.eat(&(bytes.len() as u64).to_le_bytes());
        self.eat(bytes);
    }
}

/// How a [`Toolchain`] compiles: [`punchcutter_slang::compile_program`]'s
/// shape, so the tests can put a recording compiler where the real one goes.
pub type Compiler = fn(&Path, &[&str], &Options) -> Result<punchcutter_slang::Program, SlangError>;

/// The compiler this process drives, and where it keeps what it has compiled.
///
/// Built once, at startup: a compiler swapped under a running sidecar is not a
/// case worth serving, and the supervisor restarts this process for far less.
pub struct Toolchain {
    /// The compile itself: [`compile_apart`], or in the tests a wrapper that
    /// records every call.
    pub compiler: Compiler,
    /// What compiles: [`punchcutter_slang::version`], which names the Slang
    /// release and the engine's modules. Part of every key: a toolchain
    /// upgrade changes the SPIR-V it emits from bytes that did not move.
    pub version: String,
    /// Where entries live, or `None` for a process that must not cache (the
    /// tests, which get a directory of their own instead).
    pub cache: Option<PathBuf>,
}

impl Toolchain {
    /// The real one: Slang in process, a thread per compile, caching under
    /// [`cache_dir`].
    pub fn new() -> Toolchain {
        Toolchain::with(compile_apart, Some(cache_dir()))
    }

    /// A given compiler and a given cache directory — what the tests use to
    /// prove a warm start compiles nothing.
    pub fn with(compiler: Compiler, cache: Option<PathBuf>) -> Toolchain {
        Toolchain {
            compiler,
            version: punchcutter_slang::version(),
            cache,
        }
    }

    /// Where one program's entry lives, given its key.
    fn entry_file(&self, key: u64) -> Option<PathBuf> {
        self.cache
            .as_ref()
            .map(|dir| dir.join(format!("{key:016x}.{CACHE_SUFFIX}")))
    }

    /// Write a freshly compiled generation, then bring the directory back
    /// under [`CACHE_KEEP`]. Called **after** the frames are on the wire: the
    /// bar waits for a compile, never for the disk write.
    ///
    /// A warm program is not written back — it is already the file it came
    /// from, and rewriting it would only cost a rename.
    pub fn store(&self, program: &Program) {
        if program.warm {
            return;
        }
        let (Some(dir), Some(file), Some(key)) = (&self.cache, &program.file, program.key) else {
            return;
        };
        match write_entry(file, key, &program.modules) {
            Ok(()) => witness(&format!(
                "cache: wrote {} ({} KiB)",
                file.display(),
                program.bytes / 1024
            )),
            Err(err) => witness(&format!("cache: could not write {}: {err}", file.display())),
        }
        prune(dir);
    }
}

/// [`punchcutter_slang::compile_program`] on a thread that ends with it.
///
/// Slang's global session belongs to the thread that made it, so the thread
/// ending is what frees it, and with it the compiler's whole working set: a
/// sidecar that compiled once an hour ago holds next to none of it now (the
/// module docs, *The compile runs on a thread of its own*). The stack is a
/// main thread's usual eight mebibytes rather than a spawned thread's two:
/// every other compile of the caustic (the build script, the tests) runs
/// with at least that much, and this one should not be the first to find out
/// how deep Slang goes.
pub fn compile_apart(
    source: &Path,
    entries: &[&str],
    options: &Options,
) -> Result<punchcutter_slang::Program, SlangError> {
    let result = std::thread::scope(|scope| {
        let compile = std::thread::Builder::new()
            .name("slang".to_owned())
            .stack_size(8 << 20)
            .spawn_scoped(scope, || {
                punchcutter_slang::compile_program(source, entries, options)
            })
            .map_err(|err| SlangError::Toolchain {
                detail: format!("no thread to compile on: {err}"),
            })?;
        compile
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    });
    give_back();
    result
}

/// Hand the compile's freed memory back to the system.
///
/// The thread's end frees what Slang allocated, but the allocator keeps
/// freed pages for reuse, and a thread's allocations sit in an arena of its
/// own that nothing else will reuse: measured on the bar's caustic, the
/// sidecar stays at 300 to 370 MiB resident after a compile without this and
/// at 50 to 65 MiB with it (most of that is the compiler's library, mapped),
/// against a peak of about 390 MiB either way.
fn give_back() {
    // SAFETY: `malloc_trim` takes no pointers and only returns free memory
    // to the system; any thread may call it at any time.
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    unsafe {
        libc::malloc_trim(0);
    }
}

/// The files whose bytes reach the compiler, in a fixed order: [`watched`], in
/// `dir`, then [`RENDERED`] and [`ENGINE`], wherever [`unwatched_file`] finds
/// each.
fn inputs(dir: &Path) -> Vec<PathBuf> {
    watched(dir)
        .iter()
        .map(|name| dir.join(name))
        .chain(
            RENDERED
                .iter()
                .chain(ENGINE)
                .map(|name| unwatched_file(dir, name)),
        )
        .collect()
}

/// The key for one program: everything that changes the SPIR-V and nothing
/// that does not.
///
/// `None` when an input cannot be read, which is a compile about to fail
/// anyway: no key means no hit and no entry written, so a broken tree never
/// leaves a cache behind for a working one to find.
pub fn cache_key(tools: &Toolchain, dir: &Path, define: &str, entries: &[&str]) -> Option<u64> {
    let mut key = Fnv::new();
    key.field(&CACHE_MAGIC);
    key.field(tools.version.as_bytes());
    key.field(define.as_bytes());
    key.eat(&(entries.len() as u64).to_le_bytes());
    for entry in entries {
        key.field(entry.as_bytes());
    }
    for path in inputs(dir) {
        // The *name* and the bytes, never the directory: the same files
        // compiled from a checkout and from an install produce the same
        // program.
        let name = path.file_name().unwrap_or(path.as_os_str());
        key.field(name.as_encoded_bytes());
        key.field(&std::fs::read(&path).ok()?);
    }
    Some(key.0)
}

/// One entry point as the cache holds it, which is what a [`Module`] carries
/// beyond the generation it belongs to.
///
/// `reflection` is the whole program's ([`punchcutter_slang::Reflection`] as
/// JSON), the same in every entry point of a generation: one link compiles
/// them all and reflects the program once. Each module carries its copy
/// anyway, so the wire and this layout stay one record per entry point.
pub struct Cached {
    pub name: String,
    pub spirv: Vec<u8>,
    pub reflection: String,
}

/// The fixed head of an entry: the magic, the key the name claims, how many
/// modules follow and a hash of everything after this head.
const CACHE_HEAD: usize = 8 + 8 + 4 + 8;

/// Read an entry, or `None` for every way it can be wrong.
///
/// Wrong is: absent, short, another layout's, another key's (a file renamed
/// under us), the wrong number of modules, the wrong entry points or in the
/// wrong order, lengths that do not add up to the file, or a payload that does
/// not hash to what the head says. Every one of those is a miss and, where the
/// file exists at all, a witness — a cache is a courtesy and never a frame.
pub fn read_entry(file: &Path, key: u64, entries: &[&str]) -> Option<Vec<Cached>> {
    let bytes = std::fs::read(file).ok()?;
    let bad = |why: &str| -> Option<Vec<Cached>> {
        witness(&format!("cache: {} is {why}; compiling", file.display()));
        None
    };
    if bytes.len() < CACHE_HEAD || bytes[..8] != CACHE_MAGIC {
        return bad("not a cache this build reads");
    }
    if u64::from_le_bytes(bytes[8..16].try_into().ok()?) != key {
        return bad("keyed for another program");
    }
    let count = u32::from_le_bytes(bytes[16..20].try_into().ok()?) as usize;
    let payload_hash = u64::from_le_bytes(bytes[20..28].try_into().ok()?);
    if count != entries.len() {
        return bad("a different entry list");
    }
    let table = CACHE_HEAD + count * 12;
    if bytes.len() < table {
        return bad("short in its table");
    }
    let mut hash = Fnv::new();
    hash.eat(&bytes[CACHE_HEAD..]);
    if hash.0 != payload_hash {
        return bad("damaged");
    }
    let mut at = table;
    let mut modules = Vec::with_capacity(count);
    for (index, &entry) in entries.iter().enumerate() {
        let head = CACHE_HEAD + index * 12;
        let word = |offset: usize| -> usize {
            u32::from_le_bytes(bytes[head + offset..head + offset + 4].try_into().unwrap()) as usize
        };
        let (name_len, spirv_len, reflection_len) = (word(0), word(4), word(8));
        let end = at
            .checked_add(name_len)?
            .checked_add(spirv_len)?
            .checked_add(reflection_len)?;
        if end > bytes.len() {
            return bad("short in its bodies");
        }
        let Ok(name) = String::from_utf8(bytes[at..at + name_len].to_vec()) else {
            return bad("not text where it names an entry point");
        };
        if name != entry {
            return bad("built for other entry points");
        }
        let spirv = bytes[at + name_len..at + name_len + spirv_len].to_vec();
        let Ok(reflection) = String::from_utf8(bytes[at + name_len + spirv_len..end].to_vec())
        else {
            return bad("not text where it holds a reflection");
        };
        modules.push(Cached {
            name,
            spirv,
            reflection,
        });
        at = end;
    }
    if at != bytes.len() {
        return bad("longer than its table");
    }
    Some(modules)
}

/// Write one generation through a temporary file in the same directory and
/// rename it into place, so a sidecar killed mid-write leaves no half entry —
/// `caustics-glyphs` writes its curves the same way, for the same reason.
pub fn write_entry(file: &Path, key: u64, modules: &[Module]) -> io::Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut table = Vec::with_capacity(modules.len() * 12);
    let mut bodies = Vec::new();
    for module in modules {
        table.extend_from_slice(&(module.name.len() as u32).to_le_bytes());
        table.extend_from_slice(&(module.spirv.len() as u32).to_le_bytes());
        table.extend_from_slice(&(module.reflection.len() as u32).to_le_bytes());
        bodies.extend_from_slice(module.name.as_bytes());
        bodies.extend_from_slice(&module.spirv);
        bodies.extend_from_slice(module.reflection.as_bytes());
    }
    let mut hash = Fnv::new();
    hash.eat(&table);
    hash.eat(&bodies);

    let mut bytes = Vec::with_capacity(CACHE_HEAD + table.len() + bodies.len());
    bytes.extend_from_slice(&CACHE_MAGIC);
    bytes.extend_from_slice(&key.to_le_bytes());
    bytes.extend_from_slice(&(modules.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&hash.0.to_le_bytes());
    bytes.extend_from_slice(&table);
    bytes.extend_from_slice(&bodies);

    let temp = file.with_extension(format!("{CACHE_SUFFIX}.new"));
    std::fs::write(&temp, &bytes)?;
    std::fs::rename(&temp, file)
}

/// An entry read is an entry used: stamp it so eviction sees the history and
/// not the order things were first compiled in. Failure is silent — a cache
/// that cannot be stamped still answered.
fn touch(file: &Path) {
    if let Ok(handle) = std::fs::File::options().write(true).open(file) {
        let _ = handle.set_times(std::fs::FileTimes::new().set_modified(SystemTime::now()));
    }
}

/// Keep the [`CACHE_KEEP`] newest entries and remove the rest.
///
/// Only files this process's own suffix names are ever considered, and a
/// directory that cannot be read is not an error: the cache is a courtesy.
pub fn prune(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut found: Vec<(SystemTime, PathBuf)> = entries
        .flatten()
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|suffix| suffix == CACHE_SUFFIX)
        })
        .map(|entry| {
            let stamp = entry
                .metadata()
                .and_then(|meta| meta.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            (stamp, entry.path())
        })
        .collect();
    if found.len() <= CACHE_KEEP {
        return;
    }
    // Newest first, so everything past the bound is the oldest.
    found.sort_by(|left, right| right.0.cmp(&left.0));
    for (_, path) in &found[CACHE_KEEP..] {
        match std::fs::remove_file(path) {
            Ok(()) => witness(&format!("cache: evicted {}", path.display())),
            Err(err) => witness(&format!("cache: could not evict {}: {err}", path.display())),
        }
    }
}

// ---------------------------------------------------------------------------
// The compile, and the frames it becomes
// ---------------------------------------------------------------------------

/// One compile's modules, in [`ENTRIES`] order, ready to be framed.
pub struct Program {
    pub modules: Vec<Module>,
    pub bytes: usize,
    pub millis: u128,
    /// Whether the modules came out of the cache rather than out of the compiler.
    pub warm: bool,
    /// This program's cache key and the file it belongs in, or `None` for a
    /// process that is not caching or a tree that could not be read.
    pub key: Option<u64>,
    pub file: Option<PathBuf>,
}

/// What the cache says about the directory as it is now.
enum Lookup {
    /// A generation already compiled from these bytes, ready to send.
    Hit(Program),
    /// Nothing usable: the key and the file a compile of these bytes belongs
    /// in, either of which is `None` when there is no cache or no key.
    Miss {
        key: Option<u64>,
        file: Option<PathBuf>,
    },
}

/// Ask the cache for `dir`'s program, compiling nothing.
fn lookup(
    tools: &Toolchain,
    dir: &Path,
    define: &str,
    generation: u32,
    entries: &[&str],
) -> Lookup {
    let started = std::time::Instant::now();
    let key = cache_key(tools, dir, define, entries);
    let file = key.and_then(|key| tools.entry_file(key));
    if let (Some(file), Some(key)) = (&file, key)
        && let Some(cached) = read_entry(file, key, entries)
        && let Some((modules, bytes)) = assemble(cached, define, generation, entries)
    {
        touch(file);
        let millis = started.elapsed().as_millis();
        witness(&format!(
            "cache: hit ({} modules, {} KiB, {millis} ms) {}",
            modules.len(),
            bytes / 1024,
            file.display()
        ));
        let file = Some(file.clone());
        return Lookup::Hit(Program {
            modules,
            bytes,
            millis,
            warm: true,
            key: Some(key),
            file,
        });
    }
    Lookup::Miss { key, file }
}

/// The startup rule: the cached generation for `dir` as it is now, or `None`
/// with nothing compiled.
///
/// The bar starts on SPIR-V its own `build.rs` baked from this directory, so a
/// compile here would be seconds of compiling to arrive, most of the time, where
/// the bar already is — and a slow first second on a small machine. A hit
/// costs milliseconds and is what puts an edit made since the build back on
/// screen after a restart.
pub fn warm_start(
    tools: &Toolchain,
    dir: &Path,
    define: &str,
    generation: u32,
    entries: &[&str],
) -> Option<Program> {
    match lookup(tools, dir, define, generation, entries) {
        Lookup::Hit(program) => Some(program),
        Lookup::Miss { .. } => None,
    }
}

/// Compile every entry point of the caustic in `dir` for `define`.
///
/// `None` carries nothing back: the diagnostics are already on stderr,
/// verbatim, and the contract with the bar is **silence** (docs/abi.md,
/// *Request*) — a failed compile sends no frame and the running caustic stays.
/// That invariant is also why this returns a whole [`Program`] rather than
/// yielding modules one at a time: half a generation on the wire is a
/// generation the bar would hold for ever.
pub fn compile(tools: &Toolchain, dir: &Path, define: &str, generation: u32) -> Option<Program> {
    compile_entries(tools, dir, define, generation, &ENTRIES)
}

/// [`compile`] over a given entry list, which is [`ENTRIES`] everywhere but
/// the tests: a run over every entry point is the slowest thing in this
/// workspace's test suite, and running it beside the bar's own — whose pipe
/// tests time children — makes them flake. One entry point proves this path;
/// that all six compile, for both swapchain formats, is `bar/src/abi.rs`'s
/// test and is checked on every build by `bar/build.rs` besides.
pub fn compile_entries(
    tools: &Toolchain,
    dir: &Path,
    define: &str,
    generation: u32,
    entries: &[&str],
) -> Option<Program> {
    let started = std::time::Instant::now();
    let (key, file) = match lookup(tools, dir, define, generation, entries) {
        Lookup::Hit(program) => return Some(program),
        Lookup::Miss { key, file } => (key, file),
    };
    if file.is_some() {
        witness("cache: miss; compiling");
    }
    let image_format_unknown = DstFormat::ALL
        .into_iter()
        .find(|format| format.define() == define)
        .is_some_and(DstFormat::image_format_unknown);
    let source = source(dir);
    let includes: Vec<&Path> = source.includes.iter().map(PathBuf::as_path).collect();
    let options = Options {
        defines: &[define],
        includes: &includes,
        image_format_unknown,
    };

    // One link of every entry point: the reflection carries the caustic's
    // `getStringHash` literals to the bar, and the warnings go to the journal
    // as Slang wrote them.
    let program = match (tools.compiler)(&source.entry_file, entries, &options) {
        Ok(program) => program,
        Err(err) => {
            witness("failed; keeping the old bar");
            match err.diagnostics() {
                Some(text) => diagnostics(text),
                None => witness(&format!("{err}")),
            }
            return None;
        }
    };
    if !program.warnings.is_empty() {
        witness("slang warnings:");
        diagnostics(&program.warnings);
    }
    let reflection = program.reflection.to_json();
    let compiled_entries: Vec<Cached> = program
        .entries
        .into_iter()
        .map(|entry| Cached {
            name: entry.name,
            spirv: entry.spirv,
            reflection: reflection.clone(),
        })
        .collect();

    let (modules, bytes) = assemble(compiled_entries, define, generation, entries)?;
    let millis = started.elapsed().as_millis();
    Some(Program {
        modules,
        bytes,
        millis,
        warm: false,
        key,
        file,
    })
}

/// Turn one compile's entry points — freshly built or read back — into the
/// modules of one generation.
///
/// Both paths go through here so that a cache entry becomes a generation
/// exactly as a compile does, whichever side of the cache it came from.
///
/// There is no cap to refuse against. `Module` carries an `offset` and a
/// `total` (docs/abi.md, *Module*), so a module of any size — `bar_shape` is
/// about 1.7 MiB, past `WIRE_MAX_BODY` — goes as [`Module::chunks`] frames,
/// and the frame cap stays at the mebibyte docs/abi.md, *Framing*, asks for.
fn assemble(
    cached: Vec<Cached>,
    define: &str,
    generation: u32,
    entries: &[&str],
) -> Option<(Vec<Module>, usize)> {
    let mut modules = Vec::with_capacity(cached.len());
    let mut bytes = 0usize;
    for (index, module) in cached.into_iter().enumerate() {
        bytes += size_of::<wire::ModuleHeader>()
            + module.name.len()
            + module.spirv.len()
            + module.reflection.len();
        modules.push(Module {
            generation,
            index: index as u32,
            count: entries.len() as u32,
            format: format_code(define),
            name: module.name,
            spirv: module.spirv,
            reflection: module.reflection,
        });
    }
    Some((modules, bytes))
}

// ---------------------------------------------------------------------------
// The run
// ---------------------------------------------------------------------------

/// What the stdin thread hands the main loop. Everything else on that stream
/// is witnessed there and never reaches here.
enum Input {
    /// The bar's swapchain format, as the format's define (`REQ_PROGRAM`).
    Program(String),
    /// The bar is gone.
    Eof,
}

/// Read stdin on a thread and hand the main loop the one question this sidecar
/// answers. The loop itself is the mtime poll, which cannot block on a read.
/// Read `input` to its end and discard it: how a sidecar with nothing to do
/// waits for the bar to close its pipe without being restarted for exiting.
pub fn idle_until_eof(input: &mut impl io::Read) {
    let _ = io::copy(input, &mut io::sink());
}

fn listen(into: mpsc::Sender<Input>) {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    loop {
        let sent = match wire::read(&mut input) {
            // Both ways this stream ends: cleanly on a frame boundary, and —
            // a bad magic, an impossible length, an EOF mid-frame — not. The
            // loop returns on either, because a stream that cannot be
            // resynchronised is not one to keep reading (the bar's `pump`
            // treats its side the same way).
            Ok(None) => {
                let _ = into.send(Input::Eof);
                return;
            }
            Err(err) => {
                witness(&format!("stdin: {err}"));
                let _ = into.send(Input::Eof);
                return;
            }
            Ok(Some(Frame::Request(request))) if request.kind == REQ_PROGRAM => {
                into.send(Input::Program(request.name))
            }
            Ok(Some(Frame::Request(request))) => {
                witness(&format!(
                    "request kind {} is not this sidecar's",
                    request.kind
                ));
                Ok(())
            }
            Ok(Some(Frame::Skipped(skipped))) => {
                witness(&format!("stdin: {skipped}"));
                Ok(())
            }
            Ok(Some(other)) => {
                witness(&format!(
                    "stdin: frame kind {} has no meaning here",
                    other.kind()
                ));
                Ok(())
            }
        };
        if sent.is_err() {
            // The main loop is gone; so is the reason to keep reading.
            return;
        }
    }
}

/// Write one generation's frames. A closed pipe is the bar leaving, not an
/// error.
///
/// A module is one frame where it fits in [`WIRE_MAX_BODY`] and several, in
/// order, where it does not — `bar_shape`, at about 1.7 MiB, is two. The cut is
/// `Module::chunks`'s and the reassembly is `wire::ModuleAssembly`'s; this loop
/// knows only that a module has pieces.
fn send(program: &Program) -> io::Result<()> {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    for module in &program.modules {
        for chunk in module.chunks(WIRE_MAX_BODY) {
            wire::write(&mut out, &Frame::Module(chunk))?;
        }
    }
    Ok(())
}

/// The whole program. Returns the process's exit status.
pub fn run() -> i32 {
    let dir = caustics_dir();
    if !dir.is_dir() {
        witness(&format!(
            "no caustics directory at {}; there is nothing to watch",
            dir.display()
        ));
        // A set installed without its source tree (a distribution image) has
        // nothing to reload, and the bar's baked caustic is the one it runs.
        // A sidecar that exits is restarted with a backoff for as long as the
        // bar runs (host/src/pipe.rs), so this one stays, idle, until
        // the bar closes the pipe.
        idle_until_eof(&mut io::stdin().lock());
        return 0;
    }
    if ENTRIES.len() > MAX_MODULES {
        witness(&format!(
            "{} entry points is more than one generation carries",
            ENTRIES.len()
        ));
        return 1;
    }
    let mut watch = watch(&dir);
    witness(&format!(
        "watching {} ({} entry points); waiting for the bar's program",
        dir.display(),
        ENTRIES.len()
    ));

    let (sender, inbox) = mpsc::channel();
    let listener = std::thread::Builder::new()
        .name("stdin".to_owned())
        .spawn(move || {
            listen(sender);
        });
    if let Err(err) = listener {
        witness(&format!("could not read stdin: {err}"));
        return 1;
    }

    let tools = Toolchain::new();
    witness(&format!(
        "{}; cache {}",
        tools.version,
        tools
            .cache
            .as_ref()
            .map_or_else(|| "off".to_owned(), |dir| dir.display().to_string())
    ));

    let mut define: Option<String> = None;
    let mut generation = 0u32;
    // Generation 0 never goes out: it is what the bar's collector holds before
    // the first frame, so a wrap skips straight past it.
    let next = |generation: u32| generation.wrapping_add(1).max(1);
    loop {
        // Every generation the loop decides to send, from either arm: the
        // format arriving (the cache only), or a file moving under a format
        // already known (a compile).
        let program = match wait(&inbox) {
            Ok(Some(Input::Program(asked))) => {
                witness(&format!("the bar's program is {asked}"));
                generation = next(generation);
                let program = warm_start(&tools, &dir, &asked, generation, &ENTRIES);
                if program.is_some() {
                    witness("the cache holds this directory's program; sending it");
                } else {
                    witness(
                        "nothing cached for this directory; the bar runs what it was built \
                         from until a save",
                    );
                }
                define = Some(asked);
                program
            }
            Ok(Some(Input::Eof)) => return 0,
            // The listener thread ended without saying so: treat it as EOF
            // rather than spinning on a dead channel.
            Err(()) => return 0,
            Ok(None) => {
                // Read on every poll, the format known or not, so a save made
                // before the format arrived is not a change afterwards.
                let changed = watch.changed();
                match define.as_deref() {
                    Some(define) if changed => {
                        generation = next(generation);
                        compile(&tools, &dir, define, generation)
                    }
                    _ => None,
                }
            }
        };
        let Some(program) = program else { continue };
        if let Err(err) = send(&program) {
            if err.kind() == io::ErrorKind::BrokenPipe {
                witness("stdout closed; exiting");
                return 0;
            }
            witness(&format!("writing generation {generation} failed: {err}"));
            return 1;
        }
        // After the write, not before it: the line says "sent".
        witness(&format!(
            "generation {generation} sent ({} modules, {} KiB, {} ms, {})",
            program.modules.len(),
            program.bytes / 1024,
            program.millis,
            if program.warm { "warm" } else { "cold" },
        ));
        // And the cache last of all: the bar has its frames before this
        // process spends a megabyte and a half of disk on never compiling
        // them again.
        tools.store(&program);
    }
}

/// One poll interval of waiting: an input if one arrived, `None` on the
/// timeout (which is when the mtimes are read), `Err` when the listener is
/// gone for good.
fn wait(inbox: &Receiver<Input>) -> Result<Option<Input>, ()> {
    match inbox.recv_timeout(POLL_INTERVAL) {
        Ok(input) => Ok(Some(input)),
        Err(RecvTimeoutError::Timeout) => Ok(None),
        Err(RecvTimeoutError::Disconnected) => Err(()),
    }
}

#[cfg(test)]
mod tests;
