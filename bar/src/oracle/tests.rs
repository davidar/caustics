use super::*;

use crate::abi::wire::{self, Row, Rows, RowsBuilder, Samples};
use crate::abi::{Command, FrameInputs};
use crate::model::{Caffeine, Update, Window};
use crate::witness;

/// The sidecar the row fixtures come from, and the slot it takes.
const DEMO: &str = "rows_demo";

fn slots() -> Vec<String> {
    vec![DEMO.to_owned()]
}

fn window(id: u64, app_id: &str, title: &str, focused: bool, stamp: u64) -> Update {
    Update::Window(Window {
        id,
        app_id: app_id.to_owned(),
        title: title.to_owned(),
        focused,
        urgent: false,
        focus_stamp: stamp,
    })
}

fn samples(kinds: &[u32]) -> Box<Samples> {
    let mut body = Samples::ZERO;
    body.chart_count = kinds.len() as u32;
    for (index, kind) in kinds.iter().enumerate() {
        body.charts[index].kind = *kind;
        body.charts[index].series = 1;
        body.charts[index].ceiling = 1.0;
    }
    Box::new(body)
}

// ---------------------------------------------------------------------------
// The caustic's own words, read out of the caustic
// ---------------------------------------------------------------------------

/// `caustics/bar_model.slang`, baked into this binary.
///
/// Baked rather than read: this binary can be copied into a VM and run where the source tree is not (the VM target, bar/vm-run.sh). The caustic's
/// compile is a different matter — it needs the files, and `caustics_dir`
/// finds the copy there.
const MODEL: &str = include_str!("../../../caustics/bar_model.slang");

/// `caustics/bar_tunables.slang`, likewise.
const TUNABLES: &str = include_str!("../../../caustics/bar_tunables.slang");

/// One `public static const uint NAME = <n>u;` of `bar_model.slang`.
///
/// The caustic's vocabulary is the caustic's — the ABI declares
/// no `ID_*` at all (docs/abi.md, *The private buffer*) — so a test that wants to name an
/// open menu reads the number out of the module's own text. It is the trick
/// `shape/tests/layout.rs` still uses: a test's legibility, not a contract.
fn model_const(name: &str) -> u32 {
    let needle = format!("public static const uint {name} = ");
    let rest = MODEL
        .split(&needle)
        .nth(1)
        .unwrap_or_else(|| panic!("bar_model.slang declares no {name}"));
    let digits: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || c.is_ascii_hexdigit() || *c == 'x')
        .collect();
    let text = digits.trim_end_matches('u');
    match text.strip_prefix("0x") {
        Some(hex) => u32::from_str_radix(hex, 16).expect("a hex literal"),
        None => text
            .parse()
            .unwrap_or_else(|_| panic!("{name} is {text:?}")),
    }
}

/// The caustic's `id_tagged`, in Rust: an `ID_*` tag, 24 bits of `aux`, a
/// 32-bit payload, as `(lo, hi)`.
fn id_tagged(tag: &str, aux: u32, payload: u32) -> (u32, u32) {
    let shift = model_const("ID_TAG_SHIFT");
    let mask = model_const("ID_AUX_MASK");
    (payload, (model_const(tag) << shift) | (aux & mask))
}

/// The caustic's own `hash_bytes`, in Rust: FNV-1a 32 over a run of bytes with
/// the length folded in (docs/abi.md, *The arenas*).
///
/// Private to the caustic in the strict sense — the embassy hashes nothing at
/// all — so the constants are read out of `bar_model.slang`'s own
/// text, as every other number this file names is.
fn hash_bytes(bytes: &[u8]) -> u32 {
    let prime = model_const("FNV_PRIME");
    let mut h = model_const("FNV_OFFSET");
    for byte in bytes {
        h = (h ^ u32::from(*byte)).wrapping_mul(prime);
    }
    (h ^ bytes.len() as u32).wrapping_mul(prime)
}

/// A dock entry's level: the caustic's hash of the app id's bytes, which is
/// what `bar_build` names an `ID_ENTRY` by.
fn entry_level(app_id: &str) -> (u32, u32) {
    id_tagged("ID_ENTRY", 0, hash_bytes(app_id.as_bytes()))
}

/// A sidecar row's level: the slot and the row id.
fn applet_level(slot: u32, row: u32) -> (u32, u32) {
    id_tagged("ID_APPLET", slot, row)
}

/// The four windows and the two favourites every other scenario is a variation
/// of: two apps pinned, one of them with two windows, and one running stray.
fn fixture() -> Model {
    let mut model = Model::default();
    model.favourites = vec!["kitty".to_owned(), "org.mozilla.firefox".to_owned()];
    model.apply(window(1, "kitty", "one", false, 1));
    model.apply(window(2, "kitty", "two", true, 9));
    model.apply(window(3, "org.mozilla.firefox", "web", false, 4));
    model.apply(window(4, "stray", "a stray\nsecond line", false, 2));
    model
}

/// A sidecar's frame: one chip with a menu, a checkable row, a separator, a
/// submenu and a slider under it.
fn demo_rows(value: f32) -> Rows {
    let mut builder = RowsBuilder::new(abi::APPLET_AUDIO, 3);
    let chip = builder.item(
        1,
        Some("audio"),
        Some("audio-volume-high-symbolic"),
        0.5,
        wire::RFLAG_HAS_MENU,
    );
    builder.menu(
        chip,
        2,
        Some("Mute"),
        None,
        wire::RFLAG_CHECKABLE | wire::RFLAG_CHECKED,
    );
    builder.separator(chip, 3);
    let more = builder.menu(chip, 4, Some("Outputs"), None, wire::RFLAG_SUBMENU);
    builder.menu(more, 5, Some("Speakers\nbuilt in"), None, 0);
    builder.slider(chip, 6, Some("Volume"), value, 0.0, 1.0, 0);
    builder.build()
}

fn with_rows(mut model: Model, rows: Rows) -> Model {
    model.apply(Update::Rows {
        sidecar: DEMO.to_owned(),
        rows,
    });
    model
}

/// Every scenario the corpus runs, each named for the report line.
fn corpus() -> Vec<Scenario> {
    let mut out: Vec<Scenario> = Vec::new();
    let none = (0, 0);

    let mut full = fixture();
    full.apply(Update::Samples {
        sidecar: "caustics-sysmon".into(),
        body: samples(&[abi::CHART_CPU]),
    });
    out.push(Scenario {
        name: "the full fixture",
        model: full,
        level: none,
        capped: false,
    });

    let mut stray = Model::default();
    stray.favourites = vec!["kitty".to_owned()];
    stray.apply(window(1, "stray", "a stray", true, 1));
    out.push(Scenario {
        name: "a favourite and a stray",
        model: stray,
        level: none,
        capped: false,
    });

    // A dock entry's menu, open: running and pinned, then running and not.
    out.push(Scenario {
        name: "a dock menu, running and pinned",
        model: fixture(),
        level: entry_level("kitty"),
        capped: false,
    });
    let mut unpinned = fixture();
    unpinned.favourites = vec!["org.mozilla.firefox".to_owned()];
    out.push(Scenario {
        name: "a dock menu, running and unpinned",
        model: unpinned,
        level: entry_level("kitty"),
        capped: false,
    });
    let mut pinned_idle = Model::default();
    pinned_idle.favourites = vec!["kitty".to_owned()];
    out.push(Scenario {
        name: "a dock menu, pinned and not running",
        model: pinned_idle,
        level: entry_level("kitty"),
        capped: false,
    });
    // A menu asked for an app that is not in the dock at all.
    out.push(Scenario {
        name: "a dock menu for an entry that left",
        model: fixture(),
        level: entry_level("gone.example"),
        capped: false,
    });

    // Charts: six, a repeat, and one after six.
    let six = [
        abi::CHART_CPU,
        abi::CHART_TEMP,
        abi::CHART_MEM,
        abi::CHART_NET,
        abi::CHART_DISK,
        abi::CHART_GPU,
    ];
    let mut many = fixture();
    many.apply(Update::Samples {
        sidecar: "caustics-sysmon".into(),
        body: samples(&six),
    });
    out.push(Scenario {
        name: "six charts",
        model: many,
        level: none,
        capped: false,
    });
    // Two ticks of the six readings: a chart draws a segment between two
    // samples and nothing through one, so the fill check needs a second column.
    let mut two_ticks = six_readings(1.0);
    two_ticks.sample_count = 2;
    let last = wire::CHART_SAMPLES - 1;
    for row in 0..2 * two_ticks.chart_count as usize {
        two_ticks.samples[row * wire::CHART_SAMPLES + last - 1] =
            two_ticks.samples[row * wire::CHART_SAMPLES + last];
    }
    let mut readings = fixture();
    readings.apply(Update::Samples {
        sidecar: "caustics-sysmon".into(),
        body: two_ticks,
    });
    out.push(Scenario {
        name: "six charts, with readings",
        model: readings,
        level: none,
        capped: false,
    });
    let mut repeat = fixture();
    repeat.apply(Update::Samples {
        sidecar: "caustics-sysmon".into(),
        body: samples(&[abi::CHART_GPU, abi::CHART_GPU]),
    });
    out.push(Scenario {
        name: "a repeated chart kind",
        model: repeat,
        level: none,
        capped: false,
    });
    let mut after = fixture();
    after.apply(Update::Samples {
        sidecar: "caustics-sysmon".into(),
        body: samples(&six),
    });
    after.apply(Update::Samples {
        sidecar: "caustics-sysmon".into(),
        body: samples(&[abi::CHART_CPU]),
    });
    out.push(Scenario {
        name: "one chart after six",
        model: after,
        level: none,
        capped: false,
    });

    // An applet, closed and open, and a slider at three values.
    out.push(Scenario {
        name: "an applet with its menu closed",
        model: with_rows(fixture(), demo_rows(0.5)),
        level: none,
        capped: false,
    });
    for value in [0.0, 0.5, 1.0] {
        out.push(Scenario {
            name: "an applet with its menu open",
            model: with_rows(fixture(), demo_rows(value)),
            level: applet_level(0, 1),
            capped: false,
        });
    }
    // A row added under an open menu.
    let mut grown = demo_rows(0.5);
    grown.rows.push(Row {
        id: 99,
        parent: 0,
        role: wire::ROW_MENU,
        ..Row::default()
    });
    out.push(Scenario {
        name: "a row added under an open menu",
        model: with_rows(fixture(), grown),
        level: applet_level(0, 1),
        capped: false,
    });
    // A sidecar gone: the chips and the rows go with it.
    let mut gone = with_rows(fixture(), demo_rows(0.5));
    gone.apply(Update::SidecarGone(DEMO.to_owned()));
    out.push(Scenario {
        name: "a sidecar gone",
        model: gone,
        level: applet_level(0, 1),
        capped: false,
    });

    // Caffeine's three states.
    for (name, state) in [
        ("caffeine absent", Caffeine::Absent),
        ("caffeine off", Caffeine::Off),
        ("caffeine on", Caffeine::On),
    ] {
        let mut model = fixture();
        model.apply(Update::Caffeine(state));
        out.push(Scenario {
            name,
            model,
            level: none,
            capped: false,
        });
    }

    // The clock's two reservations, on their own.
    out.push(Scenario {
        name: "the empty model",
        model: Model::default(),
        level: none,
        capped: false,
    });

    // More app ids than the facts table is wide.
    let mut crowd = Model::default();
    for id in 0..(abi::MAX_APPS as u64 + 40) {
        crowd.apply(window(id + 1, &format!("app{id}"), "w", false, id));
    }
    out.push(Scenario {
        name: "more app ids than the facts hold",
        model: crowd,
        level: none,
        capped: true,
    });

    // A tray chip whose icon nothing has answered for: its title's letter in
    // the icon's place, at the icon's size.
    let mut tray = RowsBuilder::new(abi::APPLET_TRAY, 1);
    tray.item(
        1,
        Some("Steam"),
        Some("steam_tray_mono"),
        -1.0,
        wire::RFLAG_LABEL_TITLE,
    );
    tray.item(2, None, None, -1.0, wire::RFLAG_LABEL_TITLE);
    out.push(Scenario {
        name: "tray chips with no icon yet",
        model: with_rows(fixture(), tray.build()),
        level: none,
        capped: false,
    });

    // The clock's calendar, open: a title, seven initials and the 31 days of the month `run_model_passes` dates its inputs in.
    out.push(Scenario {
        name: "the clock's calendar, open",
        model: fixture(),
        level: id_tagged("ID_CLOCK", 0, 0),
        capped: false,
    });

    // The chart numbers, open on the third chart: six lines of a name and a
    // composed value.
    let mut numbers = fixture();
    numbers.apply(Update::Samples {
        sidecar: "caustics-sysmon".into(),
        body: samples(&six),
    });
    out.push(Scenario {
        name: "the chart numbers, open",
        model: numbers,
        level: id_tagged("ID_CHART", 0, 2),
        capped: false,
    });

    // Notifications with bodies, the bell's menu open: a 300-character body in chunks, one with a newline, one without.
    out.push(Scenario {
        name: "notification bodies, open",
        model: with_rows(fixture(), body_rows(false)),
        level: applet_level(0, BELL),
        capped: false,
    });

    // A menu longer than the item table: tailscale's is 143 rows, and this is
    // the wire's own cap.
    let mut builder = RowsBuilder::new(abi::APPLET_OTHER, 1);
    let chip = builder.item(1, Some("big"), None, -1.0, wire::RFLAG_HAS_MENU);
    for id in 0..(wire::MAX_ROWS as u32 - 1) {
        builder.menu(chip, id + 2, Some("row"), None, 0);
    }
    out.push(Scenario {
        name: "a menu past MAX_ITEMS",
        model: with_rows(fixture(), builder.build()),
        level: applet_level(0, 1),
        capped: true,
    });

    out
}

/// The em sizes and the icon's square, out of the tunables' own text: what
/// `bar_build` draws at, and what a `check: block` line is measured against.
///
/// The one check in the corpus that must not be the caustic checking itself —
/// the caustic reports the px it used, and this is an independent reading of
/// the table it was supposed to use.
struct Sizes {
    icon_px: u32,
    text_px: Vec<u32>,
}

fn sizes() -> Sizes {
    let table = TUNABLES
        .split("public static const float TEXT_PX[ITEM_KIND_COUNT] = {")
        .nth(1)
        .expect("TEXT_PX is a table")
        .split("};")
        .next()
        .expect("it closes");
    let text_px: Vec<u32> = table
        .lines()
        .filter(|line| line.contains(','))
        .map(|line| {
            let number = line.trim().split(',').next().unwrap_or("0");
            number.trim().parse::<f32>().unwrap_or(0.0) as u32
        })
        .collect();
    assert_eq!(
        text_px.len() as u32,
        model_const("ITEM_KIND_COUNT"),
        "TEXT_PX is one entry per item kind"
    );
    Sizes {
        icon_px: crate::wayland::ICON_LOGICAL,
        text_px,
    }
}

/// One `check:` line, rendered.
struct Line {
    text: String,
    complete: bool,
    level: u32,
}

/// Every witness the ring carries, rendered through the printer the embassy
/// uses. `None` for a head this generation's table cannot name at all, which
/// is itself a complaint.
fn lines(state: &State, strings: &Strings, bytes: &[u8]) -> Vec<Option<Line>> {
    state
        .live_witnesses()
        .iter()
        .map(|note| {
            let format = strings.get(note.head)?;
            let (text, complete) = witness::render(format, note, strings, bytes);
            Some(Line {
                text,
                complete,
                level: note.aux,
            })
        })
        .collect()
}

/// **The gate**: every scenario of the corpus built on this adapter, and every
/// invariant the built table has to satisfy on its own.
///
/// Gated on `CAUSTICS_BAR_ORACLE=1` so `cargo test --workspace` never creates a
/// Vulkan instance and never runs slangc (the module doc says why). The run
/// prints one line per scenario and one tally, and fails on the first
/// invariant it can name.
#[test]
fn the_caustic_builds_a_table_that_holds_together() {
    if std::env::var("CAUSTICS_BAR_ORACLE").as_deref() != Ok("1") {
        eprintln!("skipping the oracle: set CAUSTICS_BAR_ORACLE=1 to run it");
        return;
    }
    let mut oracle = match Oracle::new() {
        Ok(oracle) => oracle,
        Err(err) => {
            eprintln!("skipping the oracle: {err:#}");
            return;
        }
    };
    let sizes = sizes();
    // The literal table the *running* caustic names — this compile's, which is
    // the baked modules plus `bar_check`'s own formats.
    let strings = Strings::baked();
    let scenarios = corpus();
    let mut complaints: Vec<String> = Vec::new();
    let started = std::time::Instant::now();

    for scenario in &scenarios {
        let mut facts = Facts::new(slots());
        let mut images = Images::new();
        let (state, bytes) = oracle
            .build(&mut facts, &mut images, &strings, scenario)
            .expect("the build pass runs");
        let rendered = lines(&state, &oracle.strings, &bytes);

        let mut items = 0usize;
        let mut blocks = 0usize;
        let mut swatches = 0usize;
        let mut fills = 0usize;
        let mut mine: Vec<String> = Vec::new();
        for (index, line) in rendered.iter().enumerate() {
            let Some(line) = line else {
                mine.push(format!(
                    "witness {index} names a literal this build cannot resolve"
                ));
                continue;
            };
            if !line.text.starts_with("check:") {
                continue;
            }
            if line.text.starts_with("check: swatch temp ") {
                swatches += 1;
            }
            if line.text.starts_with("check: chart item ") {
                fills += 1;
            }
            // The caustic's own complaints: an invariant it found violated.
            if line.level == abi::WITNESS_WARN {
                mine.push(line.text.clone());
                continue;
            }
            if let Some(rest) = line.text.strip_prefix("check: item ") {
                items += 1;
                if !line.complete {
                    mine.push(format!("an item line cannot be spelled: {rest}"));
                }
                continue;
            }
            if line.text.starts_with("check: block ") {
                blocks += 1;
                if let Some(complaint) = block_size(&line.text, &sizes) {
                    mine.push(complaint);
                }
            }
        }
        // The TEMP swatch check is not vacuous: the one scenario that builds
        // the numbers with a temperature among them drew its swatch.
        if scenario.name == "the chart numbers, open" {
            assert_eq!(swatches, 1, "the TEMP line's swatch was looked at");
        }
        // Nor is the fill check: the scenario with readings has charts whose
        // newest sample sits between zero and the ceiling.
        if scenario.name == "six charts, with readings" {
            assert!(fills >= 1, "no chart's fill was looked at");
        }

        eprintln!(
            "{:<40} {:>3} items, {:>3} blocks, {} complaints",
            scenario.name,
            items,
            blocks,
            mine.len()
        );
        if !scenario.capped {
            assert!(
                items > 0,
                "{}: the pass published no nameable item",
                scenario.name
            );
            assert_eq!(
                state.surface.witness_lost, 0,
                "{}: the ring dropped {} of its own descriptions",
                scenario.name, state.surface.witness_lost
            );
        }
        for complaint in mine {
            eprintln!("    {complaint}");
            complaints.push(format!("{}: {complaint}", scenario.name));
        }
    }

    eprintln!(
        "oracle: {} scenarios / {} complaints in {:.0} ms",
        scenarios.len(),
        complaints.len(),
        started.elapsed().as_secs_f64() * 1000.0
    );
    assert!(
        scenarios.len() >= 25,
        "the corpus has at least its twenty-five scenarios"
    );
    assert!(
        complaints.is_empty(),
        "the built table breaks an invariant of its own"
    );
}

/// One `check: block N kind=K px=P icon=I` line, against this file's own
/// reading of the tunables.
///
/// A **label** is sized from `TEXT_PX` by its item's kind; an icon — curves, a
/// slot, or the placeholder letter a dock entry wears, which is a `BLOCK_TEXT`
/// like any other — is always the icon's own square, whatever the label beside
/// it is. The line carries which role the block has because nothing in the
/// block itself says, and the caustic is the side that knows.
///
/// A label's *kind* is not in the line, so what a text block is asked is that
/// its size is one of the table's rows. The headless device has no font, so
/// `Params::cap_height` is 0 and `snap_px` moves nothing.
fn block_size(line: &str, sizes: &Sizes) -> Option<String> {
    let px: f32 = field(line, "px=")?.parse().ok()?;
    let px = px as u32;
    let icon = field(line, "icon=")? == "1";
    if icon {
        if px != sizes.icon_px {
            return Some(format!(
                "an icon block is {px} px, not the tunables' ICON_LOGICAL of {}",
                sizes.icon_px
            ));
        }
        return None;
    }
    let kind: u32 = field(line, "kind=")?.parse().ok()?;
    if kind == model_const("BLOCK_TEXT") && px > 0 && !sizes.text_px.contains(&px) {
        return Some(format!("a label is {px} px, which is in no TEXT_PX row"));
    }
    None
}

/// The value after `name` in a rendered line, up to the next space.
fn field<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let rest = line.split(name).nth(1)?;
    Some(rest.split(' ').next().unwrap_or(rest))
}

// ---------------------------------------------------------------------------
// The shaper, against a real font (docs/architecture.md, *Rules*)
// ---------------------------------------------------------------------------

/// Every glyph of an all-ASCII bar comes out of the font.
///
/// A driver's miscompile once drew the **first** glyph of every run as `.notdef` on RADV
/// — the clock read `▮ed 16 Sep ▮0:54` — while the text engine's conformance runner
/// shaped the same strings through the same `shapeRun` at 792/0 and
/// `bar/bar-test.sh` passed 203/0 on lavapipe. Neither gate could see it: the
/// harness dispatches its own entry point with its own requests and never the
/// bar's three passes, and the harness's count of flagged runs is a *count*,
/// which the bug raised uniformly and which had nothing to compare against.
///
/// This is the check that would have: the bar's own build/compose/shape
/// submission, on this machine's GPU, with the font `caustics-glyphs` actually
/// sends — and then the glyph buffer read back and asked whether the run the
/// caustic says it wrote is drawable. Latin labels in a Latin font: `.notdef`
/// is never the answer, and *where* in the run it appears is the whole of the
/// symptom.
#[test]
fn every_glyph_of_an_ascii_bar_comes_out_of_the_font() {
    if std::env::var("CAUSTICS_BAR_ORACLE").as_deref() != Ok("1") {
        eprintln!("skipping the oracle: set CAUSTICS_BAR_ORACLE=1 to run it");
        return;
    }
    let mut oracle = match Oracle::new() {
        Ok(oracle) => oracle,
        Err(err) => {
            eprintln!("skipping the oracle: {err:#}");
            return;
        }
    };
    if let Err(err) = oracle.load_font() {
        eprintln!("skipping the oracle: no font to shape with ({err:#})");
        return;
    }

    // ASCII throughout, and no applet: a sidecar's row labels are this file's
    // to choose, and a tunables literal that is not Latin (the overview's
    // `☷`) is a legitimate `.notdef` until a cover opens a fallback face,
    // which no headless pass does.
    let mut model = Model::default();
    model.favourites = vec!["kitty".to_owned(), "org.mozilla.firefox".to_owned()];
    model.apply(window(1, "kitty", "one", true, 1));
    model.apply(window(2, "org.mozilla.firefox", "web", false, 2));
    let scenario = Scenario {
        name: "an all-ascii bar",
        model,
        level: (0, 0),
        capped: false,
    };

    let strings = Strings::baked();
    let mut facts = Facts::new(Vec::new());
    let mut images = Images::new();
    let (state, bytes) = oracle
        .build(&mut facts, &mut images, &strings, &scenario)
        .expect("the build pass runs");
    let rendered = lines(&state, &oracle.strings, &bytes);
    let runs = oracle.gpu.read_runs();

    let mut checked = 0usize;
    let mut complaints: Vec<String> = Vec::new();
    for line in rendered.iter().flatten() {
        if !line.text.starts_with("check: block ") {
            continue;
        }
        // Labels only. A block in the *icon* role may be a tunables literal
        // the primary face has no glyph for — the overview's `☷` is one — and
        // headless there is no cover and so no fallback face to answer it
        // with. That is the one honest `.notdef` on this bar, and it is not
        // what this test is about.
        if field(&line.text, "icon=") != Some("0") {
            continue;
        }
        let Some(run) = field(&line.text, "run=") else {
            continue;
        };
        let Some((start, count)) = run.split_once('+') else {
            continue;
        };
        let (Ok(start), Ok(count)) = (start.parse::<usize>(), count.parse::<usize>()) else {
            continue;
        };
        if count == 0 || start + count > runs.len() {
            continue;
        }
        checked += 1;
        for (offset, glyph) in runs[start..start + count].iter().enumerate() {
            if glyph.flags & punchcutter_shape::abi::SHAPE_GLYPH_NOTDEF == 0 {
                continue;
            }
            complaints.push(format!(
                "glyph {offset} of {count} (cluster {}) is .notdef in a Latin label — {}",
                glyph.cluster, line.text
            ));
        }
    }

    eprintln!(
        "oracle: {checked} runs shaped, {} notdef glyphs",
        complaints.len()
    );
    for complaint in &complaints {
        eprintln!("    {complaint}");
    }
    assert!(
        checked >= 4,
        "the scenario published no label run to check ({checked})"
    );
    assert!(
        complaints.is_empty(),
        "an ASCII bar drew {} .notdef glyphs",
        complaints.len()
    );
}

/// A sidecar's menu row the font cannot draw is a cover question.
///
/// Tailscale's exit-node rows read `🇦🇺 Australia`: two regional indicators,
/// which Adwaita Sans lacks and Noto Emoji ligates to a flag. Without the
/// source `ACT_COVER` carries, `bar_apply`'s cover loop would skip every block
/// whose bytes are not the strings buffer's, so a row label would be flagged
/// and never asked about. This runs the bar's own build/compose/shape submission with the font
/// `caustics-glyphs` sends, then one `bar_apply`, and reads the action ring: the
/// row's block comes out as an `ACT_COVER` naming the sidecar's slot, and the
/// embassy's reader turns that range into exactly the label's bytes.
#[test]
fn a_menu_rows_flag_is_asked_about_from_its_slot() {
    const FLAG: &str = "🇦🇺 Australia";
    if std::env::var("CAUSTICS_BAR_ORACLE").as_deref() != Ok("1") {
        eprintln!("skipping the oracle: set CAUSTICS_BAR_ORACLE=1 to run it");
        return;
    }
    let mut oracle = match Oracle::new() {
        Ok(oracle) => oracle,
        Err(err) => {
            eprintln!("skipping the oracle: {err:#}");
            return;
        }
    };
    if let Err(err) = oracle.load_font() {
        eprintln!("skipping the oracle: no font to shape with ({err:#})");
        return;
    }

    let mut builder = RowsBuilder::new(abi::APPLET_OTHER, 1);
    let chip = builder.item(1, Some("tailscale"), None, -1.0, wire::RFLAG_HAS_MENU);
    builder.menu(chip, 2, Some("Exit node"), None, 0);
    builder.menu(chip, 3, Some(FLAG), None, 0);
    let rows = builder.build();
    let mut model = Model::default();
    model.favourites = vec!["kitty".to_owned()];
    model.apply(window(1, "kitty", "one", true, 1));
    let model = with_rows(model, rows.clone());

    let strings = Strings::baked();
    let mut facts = Facts::new(slots());
    let slot = facts.slot_of(DEMO).expect("the demo has a slot");
    let scenario = Scenario {
        name: "a flag in a menu row",
        model,
        level: applet_level(slot, 1),
        capped: false,
    };
    let mut images = Images::new();
    oracle
        .build(&mut facts, &mut images, &strings, &scenario)
        .expect("the build pass runs");
    let (width, height) = oracle.gpu.bar_size();
    let inputs = FrameInputs {
        caffeine: abi::CAFFEINE_ABSENT,
        scale: 1,
        bar_size: [width, height],
        ..FrameInputs::default()
    };
    let state = oracle.gpu.run_apply(&inputs, &[]);

    let covers: Vec<abi::Cover> = state
        .live_actions()
        .iter()
        .filter(|note| note.head == abi::ACT_COVER)
        .map(|note| note.cover())
        .collect();
    for cover in &covers {
        eprintln!(
            "oracle: ACT_COVER source={} {:?}",
            cover.source,
            facts.bytes_at(cover.source, cover.off, cover.len)
        );
    }
    let blob = (16 + rows.rows.len() * size_of::<Row>()) as u32;
    let row = rows.rows[2];
    let want = abi::Cover {
        off: blob + row.label_off,
        len: row.label_len,
        source: slot,
    };
    assert!(
        covers.contains(&want),
        "the flag row's block is asked about from slot {slot}: {covers:?}"
    );
    assert!(
        covers
            .iter()
            .all(|cover| cover.source != slot || *cover == want),
        "and the slot's Latin rows are not"
    );
    assert_eq!(facts.cover(want).as_deref(), Some(FLAG));
    assert_eq!(
        facts.take_requests(),
        vec![(
            crate::feeders::pipe::GLYPHS_SIDECAR,
            wire::REQ_COVER,
            FLAG.to_owned()
        )]
    );
}

/// The face fontconfig lists under exactly this family, as a chain entry —
/// `fc-list`, because `fc-match "Noto Emoji"` answers Noto Color Emoji.
fn family_face(family: &str) -> Option<(String, std::path::PathBuf, u32)> {
    let output = std::process::Command::new("fc-list")
        .args([family, "--format=%{family}\t%{file}\t%{index}\n"])
        .output()
        .ok()?;
    let text = String::from_utf8(output.stdout).ok()?;
    let mut lines: Vec<&str> = text.lines().collect();
    lines.sort_unstable();
    lines.into_iter().find_map(|line| {
        let mut fields = line.split('\t');
        if !fields.next()?.split(',').any(|name| name.trim() == family) {
            return None;
        }
        let path = std::path::PathBuf::from(fields.next()?.trim());
        let index = fields
            .next()
            .and_then(|word| word.trim().parse().ok())
            .unwrap_or(0);
        path.is_file().then(|| (family.to_owned(), path, index))
    })
}

/// A second flag draws.
///
/// The first cover opens Noto Emoji and fills its records inside the new tail;
/// the second fills records **below** what was sent. If the arena's bitmap
/// went out and those records did not, the shaper would say filled and the
/// GPU's record would be the blank: `🇦🇺` drawn and `🇯🇵` not. This feeds the
/// sidecar's own frames for both covers through `write_bytes`, shapes a menu
/// holding both rows with the bar's own passes, and asks the GPU's glyph table
/// about flagJapan.
#[test]
fn a_second_flag_draws_after_the_first_opened_its_face() {
    const FLAGS: [&str; 2] = ["🇦🇺 Australia", "🇯🇵 Japan"];
    if std::env::var("CAUSTICS_BAR_ORACLE").as_deref() != Ok("1") {
        eprintln!("skipping the oracle: set CAUSTICS_BAR_ORACLE=1 to run it");
        return;
    }
    let Some(face) = family_face("Noto Emoji") else {
        eprintln!("skipping: Noto Emoji is not installed");
        return;
    };
    let mut oracle = match Oracle::new() {
        Ok(oracle) => oracle,
        Err(err) => {
            eprintln!("skipping the oracle: {err:#}");
            return;
        }
    };
    let sidecar =
        match oracle.load_font_covered(punchcutter_fonts::fallback::Chain::of([face]), &FLAGS) {
            Ok(sidecar) => sidecar,
            Err(err) => {
                eprintln!("skipping the oracle: no font to shape with ({err:#})");
                return;
            }
        };
    let info = *sidecar.loaded().arena.faces().last().expect("a face");
    if info.glyph_count != 1895 {
        eprintln!(
            "skipping: Noto Emoji has {} glyphs, not the 1895 of the build flagJapan (1734) was read from",
            info.glyph_count
        );
        return;
    }
    let flag_japan = info.glyph_base + 1734;

    let mut builder = RowsBuilder::new(abi::APPLET_OTHER, 1);
    let chip = builder.item(1, Some("tailscale"), None, -1.0, wire::RFLAG_HAS_MENU);
    builder.menu(chip, 2, Some(FLAGS[0]), None, 0);
    builder.menu(chip, 3, Some(FLAGS[1]), None, 0);
    let rows = builder.build();
    let mut model = Model::default();
    model.favourites = vec!["kitty".to_owned()];
    model.apply(window(1, "kitty", "one", true, 1));
    let model = with_rows(model, rows);

    let strings = Strings::baked();
    let mut facts = Facts::new(slots());
    let slot = facts.slot_of(DEMO).expect("the demo has a slot");
    let scenario = Scenario {
        name: "two flags in a menu",
        model,
        level: applet_level(slot, 1),
        capped: false,
    };
    let mut images = Images::new();
    oracle
        .build(&mut facts, &mut images, &strings, &scenario)
        .expect("the build pass runs");
    let runs = oracle.gpu.read_runs();

    let shaped: Vec<_> = runs
        .iter()
        .filter(|glyph| glyph.gid == flag_japan)
        .collect();
    eprintln!(
        "oracle: flagJapan (merged id {flag_japan}) shaped {} times",
        shaped.len()
    );
    assert!(!shaped.is_empty(), "no run shaped 🇯🇵 to flagJapan");
    let bad =
        punchcutter_shape::abi::SHAPE_GLYPH_NOTDEF | punchcutter_shape::abi::SHAPE_GLYPH_UNFILLED;
    assert!(
        shaped.iter().all(|glyph| glyph.flags & bad == 0),
        "flagJapan came out flagged: {shaped:?}"
    );

    // The GPU's record table is the sidecar's, and flagJapan's record has ink.
    let want = caustics_glyphs::buffer(sidecar.loaded(), wire::BUF_GLYPHS).expect("a known buffer");
    let have = oracle.gpu.read_buffer(wire::BUF_GLYPHS, want.len() as u64);
    assert_eq!(have.len(), want.len());
    assert!(
        have == *want,
        "the GPU's glyph records differ from the sidecar's"
    );
    let record = flag_japan as usize * size_of::<punchcutter_text::Glyph>();
    let hbands = word(&have, record + 4);
    let vbands = word(&have, record + 8);
    eprintln!("oracle: flagJapan's record on the GPU has {hbands} x {vbands} bands");
    assert!(
        hbands > 0 && vbands > 0,
        "flagJapan's record on the GPU is the blank"
    );
}

// ---------------------------------------------------------------------------
// A menu no taller than the screen, and a wheel to scroll it
// ---------------------------------------------------------------------------

/// `caustics/bar.slang`, for the one number of its own this section names: the
/// popup state that means "up". Not a module constant, so not in `MODEL`.
const BAR: &str = include_str!("../../../caustics/bar.slang");

fn bar_const(name: &str) -> u32 {
    let needle = format!("static const uint {name} = ");
    let rest = BAR
        .split(&needle)
        .nth(1)
        .unwrap_or_else(|| panic!("bar.slang declares no {name}"));
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits
        .parse()
        .unwrap_or_else(|_| panic!("{name} is {digits:?}"))
}

/// One `public static const float NAME = <f>;` of the tunables.
fn tunable(name: &str) -> f32 {
    let needle = format!("public static const float {name} = ");
    let rest = TUNABLES
        .split(&needle)
        .nth(1)
        .unwrap_or_else(|| panic!("bar_tunables.slang declares no {name}"));
    rest.split(';')
        .next()
        .unwrap_or("")
        .trim()
        .parse()
        .expect("a float literal")
}

/// A sidecar with one chip whose menu is `rows` plain rows.
fn tall_menu(rows: u32) -> Rows {
    let mut builder = RowsBuilder::new(abi::APPLET_OTHER, 1);
    let chip = builder.item(1, Some("exit"), None, -1.0, wire::RFLAG_HAS_MENU);
    for id in 0..rows {
        builder.menu(chip, id + 2, Some("Australia"), None, 0);
    }
    builder.build()
}

/// What the layout test reads of `private` after a frame.
struct Laid {
    want: [f32; 2],
    scroll: f32,
    /// The popup's menu rows, as placed, in element order: `(item, rect)`.
    rows: Vec<(u32, [f32; 4])>,
}

fn word(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("four bytes"))
}

fn float(bytes: &[u8], at: usize) -> f32 {
    f32::from_bits(word(bytes, at))
}

fn laid(private: &[u8]) -> Laid {
    use crate::gpu::{private_field_offset as field, reflected_offset};
    let elements = field("elements");
    let stride = (field("items") - elements) / model_const("MAX_ELEMENTS") as usize;
    let count = word(private, field("element_count")).min(model_const("MAX_ELEMENTS"));
    // `1u << 2` in the module's text, which `model_const` reads as its first
    // digit: the shift is spelled out here instead.
    let present = {
        let needle = "public static const uint EFLAG_PRESENT = 1u << ";
        let rest = MODEL
            .split(needle)
            .nth(1)
            .expect("EFLAG_PRESENT is a shifted bit");
        let shift: String = rest.chars().take_while(char::is_ascii_digit).collect();
        1u32 << shift.parse::<u32>().expect("a shift")
    };
    let mut rows = Vec::new();
    for index in 0..count as usize {
        let at = elements + index * stride;
        let target = word(private, at + reflected_offset("Element", "target"));
        let flags = word(private, at + reflected_offset("Element", "flags"));
        let item = word(private, at + reflected_offset("Element", "item"));
        if target != abi::TARGET_POPUP || flags & present == 0 || item >= model_const("MAX_ITEMS") {
            continue;
        }
        let rect = at + reflected_offset("Element", "rect");
        rows.push((
            item,
            [
                float(private, rect),
                float(private, rect + 4),
                float(private, rect + 8),
                float(private, rect + 12),
            ],
        ));
    }
    let want = field("popup_want");
    Laid {
        want: [float(private, want), float(private, want + 4)],
        scroll: float(private, field("menu_scroll")),
        rows,
    }
}

/// Open a menu of `rows` rows, headless: build the model, put the popup "up"
/// by two writes to `private`, run one frame to measure, then one at the size
/// the compositor would have given. Returns that frame's inputs and layout.
fn open_menu(oracle: &mut Oracle, base: &FrameInputs, rows: u32) -> (FrameInputs, Laid) {
    let scenario = Scenario {
        name: "a tall menu",
        model: with_rows(fixture(), tall_menu(rows)),
        level: applet_level(0, 1),
        capped: false,
    };
    let strings = Strings::baked();
    let mut facts = Facts::new(slots());
    let mut images = Images::new();
    oracle
        .build(&mut facts, &mut images, &strings, &scenario)
        .expect("the build pass runs");
    oracle
        .gpu
        .set_private_word("popup_state", bar_const("POPUP_UP"));
    oracle.gpu.set_private_word("popup_kind", abi::POPUP_MENU);
    oracle.gpu.run_apply(base, &[]);
    let want = laid(&oracle.gpu.read_private()).want;
    let inputs = FrameInputs {
        popup_size: [want[0].round() as u32, want[1].round() as u32],
        ..*base
    };
    oracle.gpu.run_apply(&inputs, &[]);
    (inputs, laid(&oracle.gpu.read_private()))
}

/// A 150-row level on a 1000 px output asks for no more than the output less
/// the bar less the margin, places only the rows that fit, scrolls three rows
/// on a wheel of three steps and back on the px, stops at the last row, and
/// forgets the scroll when the popup closes; a level of five rows ignores the
/// wheel.
///
/// The one test of `bar_apply`'s layout on the GPU: `Gpu::run_apply` runs the
/// production entry point headless and the element table comes back by the
/// reflection's offsets. The popup is put "up" by one write to `private`,
/// because the click that would open it wants a bar laid out first and is not
/// what this is about.
#[test]
fn a_tall_menu_is_clamped_to_the_output_and_scrolls() {
    if std::env::var("CAUSTICS_BAR_ORACLE").as_deref() != Ok("1") {
        eprintln!("skipping the oracle: set CAUSTICS_BAR_ORACLE=1 to run it");
        return;
    }
    let mut oracle = match Oracle::new() {
        Ok(oracle) => oracle,
        Err(err) => {
            eprintln!("skipping the oracle: {err:#}");
            return;
        }
    };

    let row_h = tunable("MENU_ROW_H");
    let pad = tunable("POPUP_PAD");
    let margin = tunable("POPUP_MARGIN");
    let bar_h = 32u32;
    let output = [1920u32, 1000u32];
    let base = FrameInputs {
        scale: 1,
        bar_size: [640, bar_h],
        output_size: output,
        ..FrameInputs::default()
    };

    // -- 150 rows ------------------------------------------------------------
    let (inputs, first) = open_menu(&mut oracle, &base, 150);
    let bound = output[1] as f32 - bar_h as f32 - margin;
    eprintln!(
        "a tall menu: want {:?}, {} rows placed of 150, bound {bound}",
        first.want,
        first.rows.len()
    );
    assert!(
        first.want[1] <= bound,
        "the popup asks for {} px, past {bound}",
        first.want[1]
    );
    assert_eq!(
        first.want[1], bound,
        "a level taller than the room fills it"
    );
    let inner = first.want[1] - 2.0 * pad;
    let fit = (inner / row_h).ceil() as usize;
    assert_eq!(
        first.rows.len(),
        fit,
        "only the rows that reach into the popup are placed"
    );
    assert!(first.rows.len() < 150);
    assert_eq!(first.rows[0].1[1], pad, "the first row sits under the pad");
    for (_, rect) in &first.rows {
        assert!(
            rect[1] < first.want[1] - pad,
            "a row starting past the bottom pad was placed"
        );
    }
    assert_eq!(first.scroll, 0.0);

    // Three steps of the wheel: the fourth row is at the top.
    let wheel = Command::axis(abi::TARGET_POPUP, [10.0, 10.0], 0, 3);
    oracle.gpu.run_apply(&inputs, &[wheel]);
    let scrolled = laid(&oracle.gpu.read_private());
    assert_eq!(scrolled.scroll, 3.0 * row_h, "a step is a row");
    assert_eq!(
        scrolled.rows[0].0, first.rows[3].0,
        "the first placed row is the fourth"
    );
    assert_eq!(scrolled.rows[0].1[1], pad);

    // A wheel on the bar scrolls nothing; px back up to the top does.
    oracle.gpu.run_apply(
        &inputs,
        &[Command::axis(abi::TARGET_BAR, [1.0, 1.0], 500, 0)],
    );
    assert_eq!(laid(&oracle.gpu.read_private()).scroll, 3.0 * row_h);
    let up = Command::axis(abi::TARGET_POPUP, [10.0, 10.0], -(3.0 * row_h) as i32, 0);
    oracle.gpu.run_apply(&inputs, &[up]);
    let back = laid(&oracle.gpu.read_private());
    assert_eq!(back.scroll, 0.0);
    assert_eq!(back.rows[0].0, first.rows[0].0);

    // Half a row: the row cut at the top is still placed — the carve, the
    // draw and the hit test cut it at the band (`element_clipped`) — and its
    // rect starts above the pad.
    let half = Command::axis(abi::TARGET_POPUP, [10.0, 10.0], (row_h / 2.0) as i32, 0);
    oracle.gpu.run_apply(&inputs, &[half]);
    let cut = laid(&oracle.gpu.read_private());
    assert_eq!(
        cut.rows[0].0, first.rows[0].0,
        "a half-hidden row is placed"
    );
    assert!(cut.rows[0].1[1] < pad, "and starts above the band");
    assert_eq!(
        cut.rows.len(),
        fit + 1,
        "with one more half row at the bottom"
    );
    let whole = Command::axis(abi::TARGET_POPUP, [10.0, 10.0], -((row_h / 2.0) as i32), 0);
    oracle.gpu.run_apply(&inputs, &[whole]);

    // Past the end: clamped, and the last row is reachable and whole.
    oracle.gpu.run_apply(
        &inputs,
        &[Command::axis(abi::TARGET_POPUP, [0.0, 0.0], 100_000, 0)],
    );
    let end = laid(&oracle.gpu.read_private());
    let content = 150.0 * row_h;
    assert_eq!(
        end.scroll,
        content - inner,
        "the scroll stops at the last row"
    );
    let last = end.rows.last().expect("rows at the end");
    assert_eq!(
        last.1[1] + last.1[3],
        first.want[1] - pad,
        "the last row ends on the bottom pad"
    );
    // ...and one tick back from past the end moves back from the end.
    let tick = Command::axis(abi::TARGET_POPUP, [0.0, 0.0], 0, -1);
    oracle.gpu.run_apply(
        &inputs,
        &[
            Command::axis(abi::TARGET_POPUP, [0.0, 0.0], 100_000, 0),
            tick,
        ],
    );
    assert_eq!(
        laid(&oracle.gpu.read_private()).scroll,
        content - inner - row_h
    );

    // Closing forgets it.
    oracle.gpu.run_apply(&inputs, &[Command::popup_done()]);
    assert_eq!(
        laid(&oracle.gpu.read_private()).scroll,
        0.0,
        "a closed popup keeps no scroll"
    );

    // No output known: no bound.
    oracle
        .gpu
        .set_private_word("popup_state", bar_const("POPUP_UP"));
    oracle.gpu.set_private_word("popup_kind", abi::POPUP_MENU);
    let level = applet_level(0, 1);
    oracle.gpu.set_menu_level(level.0, level.1);
    oracle.gpu.run_apply(
        &FrameInputs {
            output_size: [0, 0],
            ..base
        },
        &[],
    );
    let unbounded = laid(&oracle.gpu.read_private());
    assert_eq!(unbounded.want[1], content + 2.0 * pad, "zero is no bound");
    oracle.gpu.run_apply(&inputs, &[Command::popup_done()]);

    // -- 5 rows --------------------------------------------------------------
    let (inputs, short) = open_menu(&mut oracle, &base, 5);
    assert_eq!(
        short.want[1],
        5.0 * row_h + 2.0 * pad,
        "a short level asks for its height"
    );
    assert_eq!(short.rows.len(), 5);
    oracle.gpu.run_apply(
        &inputs,
        &[Command::axis(abi::TARGET_POPUP, [0.0, 0.0], 78, 0)],
    );
    let still = laid(&oracle.gpu.read_private());
    assert_eq!(still.scroll, 0.0, "a level that fits ignores the wheel");
    assert_eq!(still.rows[0].1[1], pad);
    eprintln!("a short menu: want {:?}, the wheel ignored", short.want);
}

// ---------------------------------------------------------------------------
// A tray chip whose icon nothing answers is a letter
// ---------------------------------------------------------------------------

/// One built item, as the test reads it out of `private`.
#[derive(Debug)]
struct BuiltItem {
    kind: u32,
    block: u32,
    icon_block: u32,
    id: (u32, u32),
}

fn built_items(private: &[u8]) -> Vec<BuiltItem> {
    use crate::gpu::{private_field_offset as field, reflected_offset};
    let items = field("items");
    let stride = (field("blocks") - items) / model_const("MAX_ITEMS") as usize;
    let count = word(private, field("built_items")).min(model_const("MAX_ITEMS"));
    let id = reflected_offset("Item", "id");
    (0..count as usize)
        .map(|index| {
            let at = items + index * stride;
            BuiltItem {
                kind: word(private, at + reflected_offset("Item", "kind")),
                block: word(private, at + reflected_offset("Item", "block")),
                icon_block: word(private, at + reflected_offset("Item", "icon_block")),
                id: (word(private, at + id), word(private, at + id + 4)),
            }
        })
        .collect()
}

/// `(kind, a, b)` of one text block: its `BLOCK_*`, and for a `BLOCK_TEXT` the
/// byte range in the strings buffer (for a `BLOCK_GLYPH`, `a` is the glyph).
fn built_block(private: &[u8], block: u32) -> (u32, u32, u32) {
    use crate::gpu::{private_field_offset as field, reflected_offset};
    let blocks = field("blocks");
    let stride = (private.len() - blocks) / model_const("MAX_BLOCKS") as usize;
    let at = blocks + block as usize * stride;
    (
        word(private, at + reflected_offset("TextBlock", "kind")),
        word(private, at + reflected_offset("TextBlock", "a")),
        word(private, at + reflected_offset("TextBlock", "b")),
    )
}

/// Steam's tray chip, and the cases beside it: a title chip whose
/// icon `caustics-glyphs` answered `GLYPH_MISSING` for and nothing else has wears
/// the title's initial in the icon's place; one with no title at all wears
/// `?`; one whose icon is curves wears the curves. **None of them draws the
/// title as a label**, which is what `RFLAG_LABEL_TITLE` is for, and an
/// applet without the flag still draws its label beside its icon.
#[test]
fn a_tray_chip_whose_icon_nothing_answers_wears_its_titles_letter() {
    const MISSING: &str = "caustics-test-no-theme-has-this";
    const CURVES: &str = "caustics-test-curves";
    if std::env::var("CAUSTICS_BAR_ORACLE").as_deref() != Ok("1") {
        eprintln!("skipping the oracle: set CAUSTICS_BAR_ORACLE=1 to run it");
        return;
    }
    let mut oracle = match Oracle::new() {
        Ok(oracle) => oracle,
        Err(err) => {
            eprintln!("skipping the oracle: {err:#}");
            return;
        }
    };
    let title = wire::RFLAG_LABEL_TITLE;
    let mut builder = RowsBuilder::new(abi::APPLET_TRAY, 1);
    builder.item(
        1,
        Some("steam"),
        Some(MISSING),
        -1.0,
        wire::RFLAG_HAS_MENU | title,
    );
    builder.item(2, None, Some(MISSING), -1.0, title);
    builder.item(3, Some("Dropbox"), Some(CURVES), -1.0, title);
    builder.item(4, Some("42%"), Some(CURVES), -1.0, 0);
    let model = with_rows(fixture(), builder.build());

    let strings = Strings::baked();
    let mut facts = Facts::new(slots());
    let slot = facts.slot_of(DEMO).expect("the demo has a slot");
    // What the two sidecars would have said: no curves for the one, a glyph
    // for the other. `caustics-apps`' answer to the first is silence.
    facts.learn_glyph(MISSING, None);
    facts.learn_glyph(CURVES, Some(3));
    let scenario = Scenario {
        name: "tray title chips",
        model,
        level: (0, 0),
        capped: false,
    };
    let mut images = Images::new();
    let (_, bytes) = oracle
        .build(&mut facts, &mut images, &strings, &scenario)
        .expect("the build pass runs");
    let private = oracle.gpu.read_private();
    let items = built_items(&private);
    let chip = |row: u32| {
        let id = applet_level(slot, row);
        items
            .iter()
            .find(|it| it.kind == model_const("ITEM_APPLET") && it.id == id)
            .unwrap_or_else(|| panic!("chip {row} was not built: {items:?}"))
    };
    let letter = |row: u32| -> String {
        let it = chip(row);
        assert_eq!(
            it.block,
            abi::NO_INDEX,
            "chip {row} draws its title as a label"
        );
        assert_ne!(it.icon_block, abi::NO_INDEX, "chip {row} is empty");
        let (kind, a, b) = built_block(&private, it.icon_block);
        assert_eq!(
            kind,
            model_const("BLOCK_TEXT"),
            "chip {row}'s placeholder is text"
        );
        String::from_utf8_lossy(&bytes[a as usize..(a + b) as usize]).into_owned()
    };
    assert_eq!(letter(1), "S", "the title's initial, uppercased");
    assert_eq!(letter(2), "?", "no title: a question mark, not a gap");

    let dropbox = chip(3);
    assert_eq!(
        dropbox.block,
        abi::NO_INDEX,
        "a resolved icon has no title beside it"
    );
    let (kind, gid, _) = built_block(&private, dropbox.icon_block);
    assert_eq!(
        (kind, gid),
        (model_const("BLOCK_GLYPH"), 3),
        "the curves, not the letter"
    );

    let labelled = chip(4);
    assert_ne!(
        labelled.block,
        abi::NO_INDEX,
        "an applet's own label is still drawn"
    );
    eprintln!("oracle: the title chips wear S and ?, the curves win, a label stays a label");
}

// ---------------------------------------------------------------------------
// Toasts: a chip that carries `RFLAG_SHOW` is shown without a click
// ---------------------------------------------------------------------------

/// The chip's id, as `caustics-notify` numbers it.
const BELL: u32 = 0xFFFF_0000;

/// `caustics-notify`'s frame, cut down: the bell, do-not-disturb, a separator and
/// three notifications newest first, two of them unread — five rows under the
/// chip, two of them `RFLAG_ATTENTION`. The separator is flagged too, which no
/// sidecar does and a toast must not show anyway. `extra` puts a newer unread
/// notification on top.
fn notify_rows(show: bool, attention: bool, extra: bool) -> Rows {
    let unread = if attention { wire::RFLAG_ATTENTION } else { 0 };
    let mut builder = RowsBuilder::new(abi::APPLET_NOTIFY, 1);
    let flags = wire::RFLAG_HAS_MENU | if show { wire::RFLAG_SHOW } else { 0 };
    let chip = builder.item(BELL, Some("2"), None, -1.0, flags);
    builder.menu(
        chip,
        0xFFFF_0001,
        Some("Do Not Disturb"),
        None,
        wire::RFLAG_CHECKABLE,
    );
    builder.separator(chip, 0xFFFF_0004);
    if extra {
        builder.menu(chip, 13, Some("Newer — caustics · now"), None, unread);
    }
    builder.menu(
        chip,
        12,
        Some("Build finished — caustics · now"),
        None,
        unread,
    );
    builder.menu(
        chip,
        11,
        Some("Battery low — 10% · 1 min ago"),
        None,
        unread,
    );
    builder.menu(chip, 10, Some("Older — read · 1 day ago"), None, 0);
    let mut rows = builder.build();
    for row in rows
        .rows
        .iter_mut()
        .filter(|row| row.role == wire::ROW_SEPARATOR)
    {
        row.flags |= wire::RFLAG_ATTENTION;
    }
    rows
}

/// Every action of one kind in a frame's ring. A ring belongs to the frame
/// stamped on it (docs/abi.md, *The three rules the buffer adds*, rule 1): a frame that pushed nothing
/// leaves the last one's records there, and they are not this frame's.
fn actions_of(state: &State, kind: u32) -> Vec<crate::abi::Note> {
    if state.surface.action_frame != state.surface.frame {
        return Vec::new();
    }
    state
        .live_actions()
        .iter()
        .filter(|note| note.head == kind)
        .copied()
        .collect()
}

/// The frame's witness lines that start with `prefix`, rendered.
fn said(oracle: &Oracle, state: &State, prefix: &str) -> Vec<String> {
    if state.surface.witness_frame != state.surface.frame {
        return Vec::new();
    }
    lines(state, &oracle.strings, &[])
        .into_iter()
        .flatten()
        .map(|line| line.text)
        .filter(|text| text.starts_with(prefix))
        .collect()
}

/// A `Rows` frame arriving: the region rewritten, then `CMD_ROWS`.
fn rows_arrive(oracle: &mut Oracle, facts: &mut Facts, slot: u32, rows: &Rows) -> Command {
    let body = facts
        .rows_region(slot, rows)
        .expect("the frame fits its region");
    oracle.gpu.write_rows_region(slot, &body);
    Command::rows(slot)
}

/// Run frames until a toast opens, as the embassy runs them: each frame's
/// `ACT_REBUILD` answered by marking the shape submission pending, so the next
/// frame builds the level the toast set (a toast waits for its rows to be
/// built and opens at their size). `first` goes with the first
/// frame. At most three frames; exactly one open across them. Returns the open
/// and the `toast:` lines of the frame it opened in.
fn until_toast_opens(
    oracle: &mut Oracle,
    inputs: &FrameInputs,
    first: &[Command],
) -> (crate::abi::PopupOpen, Vec<String>) {
    let mut commands = first.to_vec();
    for _ in 0..3 {
        let state = oracle.gpu.run_apply(inputs, &commands);
        commands.clear();
        let opens = actions_of(&state, abi::ACT_POPUP_OPEN);
        if !opens.is_empty() {
            assert_eq!(opens.len(), 1, "one open: {opens:?}");
            return (opens[0].popup_open(), said(oracle, &state, "toast:"));
        }
        assert!(
            said(oracle, &state, "toast: open").is_empty(),
            "an open said without one"
        );
        oracle.gpu.mark_shape_pending();
    }
    panic!("no toast opened in three frames");
}

/// A chip with `RFLAG_SHOW` and two attention rows among five opens **one**
/// `POPUP_TOAST` at the chip, two rows tall, once those rows are built; the
/// popup lays out exactly those two, newest first, at the size it opened at; a
/// click on one is the row's own `ACT_SIDECAR` and ends it; the bit leaving the
/// frame ends it; with a menu up the bit opens nothing; a toast the compositor
/// dismissed is not asked for again until something newer arrives; a chip with
/// nothing flagged opens nothing.
///
/// The bar's own `bar_apply` on the bar's own GPU (`Gpu::run_apply`), as the
/// menu-scroll layout test runs it. The embassy's side of an open is one line —
/// `open_popup` takes the grab for `POPUP_MENU` alone — so the action ring is
/// the whole of what is asserted.
#[test]
fn a_chip_that_asks_is_shown_as_a_toast() {
    if std::env::var("CAUSTICS_BAR_ORACLE").as_deref() != Ok("1") {
        eprintln!("skipping the oracle: set CAUSTICS_BAR_ORACLE=1 to run it");
        return;
    }
    let mut oracle = match Oracle::new() {
        Ok(oracle) => oracle,
        Err(err) => {
            eprintln!("skipping the oracle: {err:#}");
            return;
        }
    };
    let row_h = tunable("MENU_ROW_H");
    let pad = tunable("POPUP_PAD");
    let max_w = tunable("MENU_MAX_W");
    let base = FrameInputs {
        scale: 1,
        bar_size: [640, 32],
        output_size: [1920, 1000],
        ..FrameInputs::default()
    };

    let strings = Strings::baked();
    let mut facts = Facts::new(slots());
    let slot = facts.slot_of(DEMO).expect("the demo has a slot");
    let mut images = Images::new();
    let scenario = Scenario {
        name: "a toast",
        model: with_rows(fixture(), notify_rows(true, true, false)),
        level: (0, 0),
        capped: false,
    };
    oracle
        .build(&mut facts, &mut images, &strings, &scenario)
        .expect("the build pass runs");

    // -- the open ------------------------------------------------------------
    let (open, toast_said) = until_toast_opens(&mut oracle, &base, &[]);
    eprintln!("toast: {open:?}");
    assert_eq!(open.kind, abi::POPUP_TOAST);
    assert_eq!((open.slot, open.row), (slot, BELL), "anchored at the bell");
    assert_eq!(
        open.want[1] as f32,
        2.0 * row_h + 2.0 * pad,
        "two rows tall"
    );
    assert_eq!(
        open.want[0] as f32, max_w,
        "a toast is as wide as a menu may be"
    );
    assert!(
        open.anchor[2] > 0.0 && open.anchor[3] > 0.0,
        "the chip's rect: {:?}",
        open.anchor
    );
    assert_eq!(
        toast_said,
        [format!("toast: open slot={slot} rows=2 ids=12,11")]
    );

    // Mapped at the size it asked for: the rows are already built.
    let mapped = FrameInputs {
        popup_size: open.want,
        ..base
    };
    let state = oracle.gpu.run_apply(
        &mapped,
        &[Command::popup_mapped(open.want[0], open.want[1])],
    );
    assert!(
        actions_of(&state, abi::ACT_POPUP_OPEN).is_empty(),
        "a toast up is not opened again"
    );
    let state = oracle.gpu.run_apply(&mapped, &[]);
    assert!(
        actions_of(&state, abi::ACT_POPUP_CLOSE).is_empty(),
        "nothing ended it"
    );
    assert!(
        actions_of(&state, abi::ACT_POPUP_MOVE).is_empty(),
        "the open was its size"
    );
    let private = oracle.gpu.read_private();
    let toast = laid(&private);
    let items = built_items(&private);
    let ids: Vec<u32> = toast
        .rows
        .iter()
        .map(|(item, _)| items[*item as usize].id.0)
        .collect();
    eprintln!("toast: want {:?}, rows {ids:?}", toast.want);
    assert_eq!(
        ids,
        [12, 11],
        "the two attention rows, newest first, no separator"
    );
    assert_eq!(
        toast.want,
        [open.want[0] as f32, open.want[1] as f32],
        "the layout keeps the open"
    );
    assert_eq!(toast.rows[0].1[1], pad, "no back row above them");

    // -- a click on a row: the row's own action, and the toast is over -------
    let rect = toast.rows[0].1;
    let at = [rect[0] + rect[2] / 2.0, rect[1] + rect[3] / 2.0];
    let state = oracle.gpu.run_apply(
        &mapped,
        &[
            Command::pointer_motion(abi::TARGET_POPUP, at),
            Command::pointer_down(abi::TARGET_POPUP, at, abi::BTN_LEFT),
            Command::pointer_up(abi::TARGET_POPUP, at, abi::BTN_LEFT),
        ],
    );
    let all: Vec<_> = actions_of(&state, abi::ACT_SIDECAR)
        .iter()
        .map(|note| note.sidecar())
        .collect();
    let (clicks, shown) = split_shown(all.clone());
    assert_eq!(clicks.len(), 1, "one action for one click: {all:?}");
    let click = clicks[0];
    assert_eq!(
        (click.slot, click.row, click.kind),
        (slot, 12, wire::ACTION_ACTIVATE)
    );
    assert!(
        !click.token,
        "a row without RFLAG_ACTIVATION asks for no token"
    );
    assert_eq!(all[0], click, "the click first, then what the toast showed");
    assert_eq!(shown, [12, 11], "the ending toast names the rows it showed");
    assert!(
        all[1..]
            .iter()
            .all(|args| args.slot == slot && args.button == 0 && !args.token)
    );
    assert_eq!(
        actions_of(&state, abi::ACT_POPUP_CLOSE).len(),
        1,
        "the click ends it"
    );
    assert_eq!(
        said(&oracle, &state, "toast:"),
        [
            "toast: closed (click)".to_owned(),
            format!("toast: shown slot={slot} rows=2")
        ]
    );
    // The click holds it: the frames before the sidecar answers still carry
    // the bit and the same rows, and open nothing.
    let state = oracle.gpu.run_apply(&base, &[Command::popup_done()]);
    assert!(
        actions_of(&state, abi::ACT_POPUP_OPEN).is_empty(),
        "held after a click"
    );
    oracle.gpu.mark_shape_pending();
    let state = oracle.gpu.run_apply(&base, &[]);
    assert!(
        actions_of(&state, abi::ACT_POPUP_OPEN).is_empty(),
        "and the frame after"
    );
    // The sidecar's answer: the bit gone. A frame without the bit opens nothing.
    let gone = rows_arrive(
        &mut oracle,
        &mut facts,
        slot,
        &notify_rows(false, true, false),
    );
    let state = oracle.gpu.run_apply(&base, &[gone]);
    assert!(
        actions_of(&state, abi::ACT_POPUP_OPEN).is_empty(),
        "no bit, no toast"
    );

    // -- the bit leaving the frame ends it -----------------------------------
    let back = rows_arrive(
        &mut oracle,
        &mut facts,
        slot,
        &notify_rows(true, true, false),
    );
    let (again, _) = until_toast_opens(&mut oracle, &base, &[back]);
    assert_eq!(again.want, open.want, "the same rows, the same toast");
    oracle.gpu.run_apply(
        &mapped,
        &[Command::popup_mapped(open.want[0], open.want[1])],
    );
    let gone = rows_arrive(
        &mut oracle,
        &mut facts,
        slot,
        &notify_rows(false, true, false),
    );
    let state = oracle.gpu.run_apply(&mapped, &[gone]);
    assert_eq!(
        actions_of(&state, abi::ACT_POPUP_CLOSE).len(),
        1,
        "the bit gone: closed"
    );
    assert_eq!(
        said(&oracle, &state, "toast:"),
        [
            "toast: closed (flag)".to_owned(),
            format!("toast: shown slot={slot} rows=2")
        ]
    );
    oracle.gpu.run_apply(&base, &[Command::popup_done()]);

    // -- with a menu up the bit opens nothing --------------------------------
    let back = rows_arrive(
        &mut oracle,
        &mut facts,
        slot,
        &notify_rows(true, true, false),
    );
    let level = applet_level(slot, BELL);
    oracle.gpu.set_menu_level(level.0, level.1);
    oracle
        .gpu
        .set_private_word("popup_state", bar_const("POPUP_UP"));
    oracle.gpu.set_private_word("popup_kind", abi::POPUP_MENU);
    for _ in 0..3 {
        let state = oracle.gpu.run_apply(&mapped, &[back]);
        assert!(
            actions_of(&state, abi::ACT_POPUP_OPEN).is_empty(),
            "a menu up: no toast"
        );
        assert!(
            actions_of(&state, abi::ACT_POPUP_CLOSE).is_empty(),
            "and the menu stays"
        );
    }

    // -- the compositor ending it holds it until something newer arrives -----
    // (The menu's own end is the first frame the toast may be asked for.)
    // Refused, here — never mapped — so it showed nothing and says nothing
    // (`ACTION_TOAST_SHOWN`); the sidecar's clock retires those rows.
    let (_, _) = until_toast_opens(&mut oracle, &base, &[Command::popup_done()]);
    let state = oracle.gpu.run_apply(&base, &[Command::popup_done()]);
    assert_eq!(
        said(&oracle, &state, "toast:"),
        ["toast: closed (dismissed)"]
    );
    assert!(
        actions_of(&state, abi::ACT_SIDECAR).is_empty(),
        "a refused toast showed nothing"
    );
    assert!(
        actions_of(&state, abi::ACT_POPUP_OPEN).is_empty(),
        "not asked again at once"
    );
    oracle.gpu.mark_shape_pending();
    let state = oracle.gpu.run_apply(&base, &[]);
    assert!(
        actions_of(&state, abi::ACT_POPUP_OPEN).is_empty(),
        "nor the frame after"
    );
    let newer = rows_arrive(
        &mut oracle,
        &mut facts,
        slot,
        &notify_rows(true, true, true),
    );
    let (three, said_three) = until_toast_opens(&mut oracle, &base, &[newer]);
    assert_eq!(
        three.want[1] as f32,
        3.0 * row_h + 2.0 * pad,
        "something newer: three rows"
    );
    assert_eq!(
        said_three,
        [format!("toast: open slot={slot} rows=3 ids=13,12,11")]
    );
    oracle.gpu.run_apply(&base, &[Command::popup_done()]);

    // -- nothing flagged is no toast -----------------------------------------
    let quiet = rows_arrive(
        &mut oracle,
        &mut facts,
        slot,
        &notify_rows(true, false, false),
    );
    oracle.gpu.mark_shape_pending();
    for _ in 0..2 {
        let state = oracle.gpu.run_apply(&base, &[quiet]);
        assert!(
            actions_of(&state, abi::ACT_POPUP_OPEN).is_empty(),
            "no attention row: no toast"
        );
        oracle.gpu.mark_shape_pending();
    }
    eprintln!("oracle: the toast opens without a grab, shows two rows, and ends three ways");
}

// ---------------------------------------------------------------------------
// A toast tells the truth
// ---------------------------------------------------------------------------

/// `caustics-notify`'s frame as it sends it: the bell (asking when `show`), and
/// the given notifications in order, each `(id, attention)`. Row 11 carries
/// `RFLAG_ACTIVATION` (a notification with a default action), the others not.
fn toast_frame(show: bool, rows: &[(u32, bool)]) -> Rows {
    let mut builder = RowsBuilder::new(abi::APPLET_NOTIFY, 1);
    let flags = wire::RFLAG_HAS_MENU | if show { wire::RFLAG_SHOW } else { 0 };
    let chip = builder.item(BELL, Some("3"), None, -1.0, flags);
    builder.menu(
        chip,
        0xFFFF_0001,
        Some("Do Not Disturb"),
        None,
        wire::RFLAG_CHECKABLE,
    );
    builder.separator(chip, 0xFFFF_0004);
    for &(id, attention) in rows {
        let mut flags = if attention { wire::RFLAG_ATTENTION } else { 0 };
        if id == 11 {
            flags |= wire::RFLAG_ACTIVATION;
        }
        builder.menu(
            chip,
            id,
            Some(&format!("Notification {id} · now")),
            None,
            flags,
        );
    }
    builder.build()
}

/// The open toast's rows, as placed: row id → rect.
fn toast_rects(oracle: &mut Oracle) -> Vec<(u32, [f32; 4])> {
    let private = oracle.gpu.read_private();
    let items = built_items(&private);
    laid(&private)
        .rows
        .iter()
        .map(|(item, rect)| (items[*item as usize].id.0, *rect))
        .collect()
}

/// A frame's `ACT_SIDECAR`s split in two: everything else, and the rows an
/// ending toast says it showed (`ACTION_TOAST_SHOWN`).
fn split_shown(all: Vec<crate::abi::SidecarArgs>) -> (Vec<crate::abi::SidecarArgs>, Vec<u32>) {
    let (shown, rest): (Vec<_>, Vec<_>) = all
        .into_iter()
        .partition(|args| args.kind == wire::ACTION_TOAST_SHOWN);
    (rest, shown.iter().map(|args| args.row).collect())
}

/// A click, press and release, at the centre of `rect` in the popup; the
/// frame's `ACT_SIDECAR`s.
fn click_rect(
    oracle: &mut Oracle,
    inputs: &FrameInputs,
    rect: [f32; 4],
    button: u32,
) -> (Vec<crate::abi::SidecarArgs>, Box<State>) {
    let at = [rect[0] + rect[2] / 2.0, rect[1] + rect[3] / 2.0];
    let state = oracle.gpu.run_apply(
        inputs,
        &[
            Command::pointer_motion(abi::TARGET_POPUP, at),
            Command::pointer_down(abi::TARGET_POPUP, at, button),
            Command::pointer_up(abi::TARGET_POPUP, at, button),
        ],
    );
    let clicks = actions_of(&state, abi::ACT_SIDECAR)
        .iter()
        .map(|note| note.sidecar())
        .collect();
    (clicks, state)
}

/// A click on a toast lands on the row under the pointer, never on a row the
/// toast has re-indexed since it opened.
///
/// A three-row toast: a click on each row's rect names that row, every time
/// (the toast reopened between, at the same rects); the click asks for an
/// activation token only for the row flagged `RFLAG_ACTIVATION`; a right click
/// is `ACTION_SECONDARY` and ends the toast too. With the toast up, a frame
/// that adds a fourth attention row on top changes nothing it shows — the three
/// rects do not move, the new row is not placed, `toast: held` says so — and a
/// click in the very frame whose build re-indexed the item table still names
/// the row that was drawn under the pointer. A frame that removes one of its
/// rows leaves a gap and moves nothing; the toast ends when none of its own
/// rows is left flagged. The open is the built size: no `ACT_POPUP_MOVE`, ever.
#[test]
fn a_toast_is_the_rows_it_opened_with() {
    if std::env::var("CAUSTICS_BAR_ORACLE").as_deref() != Ok("1") {
        eprintln!("skipping the oracle: set CAUSTICS_BAR_ORACLE=1 to run it");
        return;
    }
    let mut oracle = match Oracle::new() {
        Ok(oracle) => oracle,
        Err(err) => {
            eprintln!("skipping the oracle: {err:#}");
            return;
        }
    };
    let row_h = tunable("MENU_ROW_H");
    let pad = tunable("POPUP_PAD");
    let base = FrameInputs {
        scale: 1,
        bar_size: [640, 32],
        output_size: [1920, 1000],
        ..FrameInputs::default()
    };
    let strings = Strings::baked();
    let mut facts = Facts::new(slots());
    let slot = facts.slot_of(DEMO).expect("the demo has a slot");
    let mut images = Images::new();
    let three = toast_frame(true, &[(12, true), (11, true), (10, true)]);
    let scenario = Scenario {
        name: "a three-row toast",
        model: with_rows(fixture(), three.clone()),
        level: (0, 0),
        capped: false,
    };
    oracle
        .build(&mut facts, &mut images, &strings, &scenario)
        .expect("the build pass runs");

    let mut moves = 0usize;
    let open_three = |oracle: &mut Oracle, first: Vec<Command>| {
        let (open, said_open) = until_toast_opens(oracle, &base, &first);
        assert_eq!(
            said_open,
            [format!("toast: open slot={slot} rows=3 ids=12,11,10")]
        );
        assert_eq!(
            open.want[1] as f32,
            3.0 * row_h + 2.0 * pad,
            "three plain rows"
        );
        let mapped = FrameInputs {
            popup_size: open.want,
            ..base
        };
        let state = oracle.gpu.run_apply(
            &mapped,
            &[Command::popup_mapped(open.want[0], open.want[1])],
        );
        assert!(
            actions_of(&state, abi::ACT_POPUP_MOVE).is_empty(),
            "no resize after the open"
        );
        mapped
    };

    // -- each row, clicked by its own rect, names itself -------------------------
    let mapped = open_three(&mut oracle, Vec::new());
    let rects = toast_rects(&mut oracle);
    eprintln!("a three-row toast: {rects:?}");
    assert_eq!(
        rects.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        [12, 11, 10]
    );
    let rect_of = |rects: &[(u32, [f32; 4])], id: u32| {
        rects
            .iter()
            .find(|(r, _)| *r == id)
            .unwrap_or_else(|| panic!("row {id} not placed"))
            .1
    };
    for (round, id) in [12u32, 11, 10].into_iter().enumerate() {
        if round > 0 {
            // Reopen: the sidecar stops asking (which lifts the click's
            // hold), then asks again with the same three.
            let quiet = rows_arrive(&mut oracle, &mut facts, slot, &toast_frame(false, &[]));
            oracle.gpu.run_apply(&base, &[Command::popup_done(), quiet]);
            let back = rows_arrive(&mut oracle, &mut facts, slot, &three);
            open_three(&mut oracle, vec![back]);
            assert_eq!(
                toast_rects(&mut oracle),
                rects,
                "the same rows at the same rects"
            );
        }
        let (all, state) = click_rect(&mut oracle, &mapped, rect_of(&rects, id), abi::BTN_LEFT);
        let (clicks, shown) = split_shown(all);
        assert_eq!(clicks.len(), 1, "one click, one action: {clicks:?}");
        assert_eq!(
            (clicks[0].slot, clicks[0].row, clicks[0].kind),
            (slot, id, wire::ACTION_ACTIVATE)
        );
        assert_eq!(
            clicks[0].token,
            id == 11,
            "a token only for the row that asked"
        );
        assert_eq!(
            shown,
            [12, 11, 10],
            "and the toast's end names all three it showed"
        );
        assert_eq!(
            said(&oracle, &state, "toast:"),
            [
                "toast: closed (click)".to_owned(),
                format!("toast: shown slot={slot} rows=3")
            ]
        );
        eprintln!(
            "a toast: the click on row {id}'s rect named row {}",
            clicks[0].row
        );
    }

    // -- a right click is a dismissal, and ends it too ---------------------------
    let quiet = rows_arrive(&mut oracle, &mut facts, slot, &toast_frame(false, &[]));
    oracle.gpu.run_apply(&base, &[Command::popup_done(), quiet]);
    let back = rows_arrive(&mut oracle, &mut facts, slot, &three);
    open_three(&mut oracle, vec![back]);
    let (all, state) = click_rect(&mut oracle, &mapped, rect_of(&rects, 10), abi::BTN_RIGHT);
    let (clicks, shown) = split_shown(all);
    assert_eq!(clicks.len(), 1, "{clicks:?}");
    assert_eq!(shown, [12, 11, 10]);
    assert_eq!(
        (clicks[0].row, clicks[0].kind, clicks[0].token),
        (10, wire::ACTION_SECONDARY, false)
    );
    assert_eq!(actions_of(&state, abi::ACT_POPUP_CLOSE).len(), 1);

    // -- a fourth row while it is up: nothing moves, the click names its row -----
    let quiet = rows_arrive(&mut oracle, &mut facts, slot, &toast_frame(false, &[]));
    oracle.gpu.run_apply(&base, &[Command::popup_done(), quiet]);
    let back = rows_arrive(&mut oracle, &mut facts, slot, &three);
    open_three(&mut oracle, vec![back]);
    let four = toast_frame(true, &[(13, true), (12, true), (11, true), (10, true)]);
    let arrive = rows_arrive(&mut oracle, &mut facts, slot, &four);
    let state = oracle.gpu.run_apply(&mapped, &[arrive]);
    assert!(
        actions_of(&state, abi::ACT_POPUP_CLOSE).is_empty(),
        "a new row ends nothing"
    );
    assert_eq!(
        said(&oracle, &state, "toast:"),
        ["toast: held (new row while up) row=13"]
    );
    oracle.gpu.mark_shape_pending();
    let state = oracle.gpu.run_apply(&mapped, &[]);
    moves += actions_of(&state, abi::ACT_POPUP_MOVE).len();
    let after = toast_rects(&mut oracle);
    eprintln!("a fourth row arrived: {after:?}");
    assert_eq!(
        after, rects,
        "the three rects did not move, and the fourth is not placed"
    );
    assert!(said(&oracle, &state, "toast: held").is_empty(), "said once");
    // A fifth on top: the frame that lays out still has the old table's
    // indices, and the click lands in the frame whose build re-indexed it.
    let five = toast_frame(
        true,
        &[(14, true), (13, true), (12, true), (11, true), (10, true)],
    );
    let arrive = rows_arrive(&mut oracle, &mut facts, slot, &five);
    let state = oracle.gpu.run_apply(&mapped, &[arrive]);
    assert_eq!(
        said(&oracle, &state, "toast:"),
        ["toast: held (new row while up) row=14"]
    );
    oracle.gpu.mark_shape_pending();
    let (all, _) = click_rect(&mut oracle, &mapped, rect_of(&rects, 11), abi::BTN_LEFT);
    let (clicks, shown) = split_shown(all);
    assert_eq!(clicks.len(), 1, "{clicks:?}");
    assert_eq!(
        clicks[0].row, 11,
        "the click names the row drawn under the pointer"
    );
    eprintln!("a toast: across a re-indexing build the click still named row 11");
    // What it showed is its own three, not the two held behind it.
    assert_eq!(
        shown,
        [12, 11, 10],
        "13 and 14 arrived while it was up and were never shown"
    );

    // -- the rows held behind it are the next toast ------------------------------
    // Until the sidecar has heard, its frame still flags the toast's three, and
    // the click's hold keeps them from reopening with the two.
    let state = oracle.gpu.run_apply(&base, &[Command::popup_done()]);
    assert!(
        actions_of(&state, abi::ACT_POPUP_OPEN).is_empty(),
        "held until the sidecar answers"
    );
    oracle.gpu.mark_shape_pending();
    let state = oracle.gpu.run_apply(&base, &[]);
    assert!(
        actions_of(&state, abi::ACT_POPUP_OPEN).is_empty(),
        "and the frame after"
    );
    // Its answer: 11 gone (the click), 12 and 10 shown, 14 and 13 still asking.
    let heard = toast_frame(true, &[(14, true), (13, true), (12, false), (10, false)]);
    let heard = rows_arrive(&mut oracle, &mut facts, slot, &heard);
    let (late, said_late) = until_toast_opens(&mut oracle, &base, &[heard]);
    assert_eq!(
        said_late,
        [format!("toast: open slot={slot} rows=2 ids=14,13")]
    );
    assert_eq!(
        late.want[1] as f32,
        2.0 * row_h + 2.0 * pad,
        "the two it never showed"
    );
    eprintln!("a toast: the rows held behind it opened next, alone");

    // -- a row removed leaves a gap; the last one gone ends it -------------------
    let quiet = rows_arrive(&mut oracle, &mut facts, slot, &toast_frame(false, &[]));
    oracle.gpu.run_apply(&base, &[Command::popup_done(), quiet]);
    let back = rows_arrive(&mut oracle, &mut facts, slot, &three);
    open_three(&mut oracle, vec![back]);
    let less = rows_arrive(
        &mut oracle,
        &mut facts,
        slot,
        &toast_frame(true, &[(11, true), (10, true)]),
    );
    let state = oracle.gpu.run_apply(&mapped, &[less]);
    assert!(
        actions_of(&state, abi::ACT_POPUP_CLOSE).is_empty(),
        "two of its rows are left"
    );
    oracle.gpu.mark_shape_pending();
    let state = oracle.gpu.run_apply(&mapped, &[]);
    moves += actions_of(&state, abi::ACT_POPUP_MOVE).len();
    let gap = toast_rects(&mut oracle);
    eprintln!("row 12 removed: {gap:?}");
    assert_eq!(gap, rects[1..], "11 and 10 where they were; 12 a gap");
    let fading = rows_arrive(
        &mut oracle,
        &mut facts,
        slot,
        &toast_frame(true, &[(11, false), (10, true)]),
    );
    let state = oracle.gpu.run_apply(&mapped, &[fading]);
    assert!(
        actions_of(&state, abi::ACT_POPUP_CLOSE).is_empty(),
        "10 is still flagged"
    );
    let done = rows_arrive(
        &mut oracle,
        &mut facts,
        slot,
        &toast_frame(true, &[(11, false), (10, false)]),
    );
    let state = oracle.gpu.run_apply(&mapped, &[done]);
    assert_eq!(
        actions_of(&state, abi::ACT_POPUP_CLOSE).len(),
        1,
        "none of its rows is left"
    );
    assert_eq!(
        said(&oracle, &state, "toast:"),
        [
            "toast: closed (empty)".to_owned(),
            format!("toast: shown slot={slot} rows=3")
        ]
    );
    let all: Vec<_> = actions_of(&state, abi::ACT_SIDECAR)
        .iter()
        .map(|note| note.sidecar())
        .collect();
    assert_eq!(
        split_shown(all).1,
        [12, 11, 10],
        "a row that left still showed"
    );
    oracle.gpu.run_apply(&base, &[Command::popup_done()]);
    assert_eq!(moves, 0, "a toast never resizes");
    eprintln!("oracle: a toast is the rows it opened with, and a click names the row under it");
}

// ---------------------------------------------------------------------------
// A notification is a title and a wrapped body
// ---------------------------------------------------------------------------

/// Three hundred characters of words — no `fi`, `fl` or `ff`, so no ligature
/// merges two characters into one glyph and a line's text can be read back a
/// glyph a character.
fn body_300() -> String {
    let words = "The quick brown dog jumps over the lazy cat while seven more \
                 run along the old road towards the market, where people \
                 trade apples, bread and stories about the weather. ";
    let mut body = words.repeat(3);
    body.truncate(300);
    body.trim_end().to_owned()
}

/// `caustics-notify`'s frame with bodies: the bell, and three unread
/// notifications newest first — one with a 300-character body, one whose
/// body has a newline of its own, one with none.
fn body_rows(show: bool) -> Rows {
    let mut builder = RowsBuilder::new(abi::APPLET_NOTIFY, 1);
    let flags = wire::RFLAG_HAS_MENU | if show { wire::RFLAG_SHOW } else { 0 };
    let chip = builder.item(BELL, Some("3"), None, -1.0, flags);
    let long = builder.menu(
        chip,
        12,
        Some("Build finished · now"),
        None,
        wire::RFLAG_ATTENTION,
    );
    assert!(builder.body(long, &body_300()));
    let two = builder.menu(
        chip,
        11,
        Some("Two lines · now"),
        None,
        wire::RFLAG_ATTENTION,
    );
    assert!(builder.body(two, "Line one\nLine two"));
    builder.menu(chip, 10, Some("No body · now"), None, wire::RFLAG_ATTENTION);
    builder.build()
}

/// One glyph of a body, flattened across its blocks: its pen and far edge in
/// the paragraph's x, the byte its cluster starts at, and where it lives.
#[derive(Debug)]
struct BodyGlyph {
    x: f32,
    end: f32,
    byte: u8,
    block: u32,
    glyph: u32,
    /// The first glyph of a paragraph: the body's first, or the first after
    /// one of its newlines.
    paragraph: bool,
}

/// One wrapped line as the pool holds it.
#[derive(Debug)]
struct WrappedLine {
    block: u32,
    glyph: u32,
    count: u32,
    fit: u32,
    width: f32,
    fit_w: f32,
}

/// A built row's body, read back: its glyphs in order and its wrapped lines,
/// with the `body_lines` word's count and BODY_MORE bit.
struct ReadBody {
    glyphs: Vec<BodyGlyph>,
    lines: Vec<WrappedLine>,
    more: bool,
}

impl ReadBody {
    fn start(&self, line: &WrappedLine) -> usize {
        self.glyphs
            .iter()
            .position(|g| g.block == line.block && g.glyph == line.glyph)
            .unwrap_or_else(|| panic!("line {line:?} starts at no glyph of the body"))
    }

    fn text(&self, line: &WrappedLine) -> String {
        let at = self.start(line);
        self.glyphs[at..at + line.count as usize]
            .iter()
            .map(|g| g.byte as char)
            .collect()
    }
}

fn read_body(
    private: &[u8],
    runs: &[punchcutter_shape::abi::ShapedGlyph],
    region: &[u8],
    item: u32,
) -> ReadBody {
    use crate::gpu::{private_field_offset as field, reflected_offset};
    let first_block = item_word(private, item, "aux_block");
    let blocks = item_word(private, item, "body_blocks") & 0xff;
    assert!(blocks > 0, "item {item} has no body");
    let mut glyphs = Vec::new();
    let mut base = 0.0f32;
    for block in first_block..first_block + blocks {
        let bflags = block_word(private, block, "bflags");
        assert_ne!(
            bflags & model_const("BLOCK_BODY"),
            0,
            "block {block} is not a body's"
        );
        if bflags & model_const("BLOCK_PARAGRAPH") != 0 {
            base = 0.0;
        }
        assert_eq!(
            block_word(private, block, "flags") & punchcutter_shape::abi::SHAPE_RUN_NOTDEF,
            0,
            "body block {block} drew a .notdef"
        );
        let (_, a, _) = built_block(private, block);
        let first = block_word(private, block, "first_glyph") as usize;
        let count = block_word(private, block, "glyph_count");
        let width = f32::from_bits(block_word(private, block, "size"));
        for k in 0..count {
            let g = runs[first + k as usize];
            let x = g.x as f32 / 64.0;
            let end = if k + 1 < count {
                runs[first + k as usize + 1].x as f32 / 64.0
            } else {
                width
            };
            glyphs.push(BodyGlyph {
                x: base + x,
                end: base + end,
                byte: region[(a + g.cluster) as usize],
                block,
                glyph: k,
                paragraph: k == 0 && bflags & model_const("BLOCK_PARAGRAPH") != 0,
            });
        }
        base += width;
    }
    let lines_word = item_word(private, item, "body_lines");
    let first_line = lines_word & 0xffff;
    let count = (lines_word >> 16) & 0xff;
    let pool = field("body_lines");
    let stride = (field("elements") - pool) / bar_const("MAX_BODY_POOL") as usize;
    let lines = (first_line..first_line + count)
        .map(|index| {
            let at = pool + index as usize * stride;
            let w = |name: &str| word(private, at + reflected_offset("BodyLine", name));
            WrappedLine {
                block: w("block"),
                glyph: w("glyph"),
                count: w("count"),
                fit: w("fit"),
                width: f32::from_bits(w("width")),
                fit_w: f32::from_bits(w("fit_w")),
            }
        })
        .collect();
    ReadBody {
        glyphs,
        lines,
        more: lines_word & (1 << 31) != 0,
    }
}

/// The item a notification row was built as.
fn row_item(private: &[u8], slot: u32, row: u32) -> u32 {
    let id = applet_level(slot, row);
    built_items(private)
        .iter()
        .position(|it| it.kind == model_const("ITEM_MENU_ROW") && it.id == id)
        .unwrap_or_else(|| panic!("row {row} was not built")) as u32
}

/// Every wrapped line of a body against the rule: no line starts or ends
/// with a space, none is wider than the wrap width, each break is at a space
/// — the glyph after a line and the one before the next are spaces — and the
/// break is greedy: the next line's first word would not have fitted after
/// this one. Returns the lines' texts.
fn check_wrap(body: &ReadBody, wrap_w: f32) -> Vec<String> {
    let mut texts = Vec::new();
    for (index, line) in body.lines.iter().enumerate() {
        let text = body.text(line);
        assert!(
            !text.starts_with(' ') && !text.ends_with(' '),
            "line {index}: {text:?}"
        );
        assert!(
            line.width <= wrap_w + 0.01,
            "line {index} is {} px of {wrap_w}",
            line.width
        );
        let start = body.start(line);
        let end = start + line.count as usize;
        let measured = body.glyphs[end - 1].end - body.glyphs[start].x;
        assert!((measured - line.width).abs() < 0.01, "line {index}'s width");
        if let Some(next) = body.lines.get(index + 1) {
            let next_start = body.start(next);
            if !body.glyphs[next_start].paragraph {
                assert_eq!(
                    body.glyphs[end].byte, b' ',
                    "line {index} breaks on a space"
                );
                assert_eq!(body.glyphs[next_start - 1].byte, b' ');
                let word_end = body.glyphs[next_start..]
                    .iter()
                    .position(|g| g.byte == b' ')
                    .map_or(body.glyphs.len(), |k| next_start + k);
                let would_be = body.glyphs[word_end - 1].end - body.glyphs[start].x;
                assert!(
                    would_be > wrap_w,
                    "line {index} broke early: the next word fits ({would_be} of {wrap_w})"
                );
            }
        }
        texts.push(text);
    }
    texts
}

/// A row carrying a body draws it under its summary, wrapped: the 300-char
/// body of `body_300` lays out as lines that each break at a space, greedily,
/// at the popup's widest less its pads and gutters; the menu shows at most
/// `MENU_BODY_LINES` and the row is one plain row tall plus a body line per
/// line shown plus the pad; a body with a newline breaks there and nowhere
/// else; a row with no body is a plain row. As a toast the same row shows
/// `TOAST_BODY_LINES`, its last cut to leave room for the ellipsis, and says
/// so in a `body:` witness.
///
/// The bar's own build/shape submission and `bar_apply` on the bar's own GPU,
/// with the font `caustics-glyphs` sends — a wrap measures real advances.
#[test]
fn a_notifications_body_wraps_under_its_summary() {
    if std::env::var("CAUSTICS_BAR_ORACLE").as_deref() != Ok("1") {
        eprintln!("skipping the oracle: set CAUSTICS_BAR_ORACLE=1 to run it");
        return;
    }
    let mut oracle = match Oracle::new() {
        Ok(oracle) => oracle,
        Err(err) => {
            eprintln!("skipping the oracle: {err:#}");
            return;
        }
    };
    if let Err(err) = oracle.load_font() {
        eprintln!("skipping the oracle: no font to shape with ({err:#})");
        return;
    }
    let row_h = tunable("MENU_ROW_H");
    let line_h = tunable("MENU_BODY_LINE_H");
    let body_pad = tunable("MENU_BODY_PAD");
    let pad = tunable("POPUP_PAD");
    let wrap_w = tunable("MENU_MAX_W") - 2.0 * (pad + tunable("MENU_GUTTER"));
    let uint_tunable = |name: &str| -> u32 {
        let needle = format!("public static const uint {name} = ");
        let rest = TUNABLES
            .split(&needle)
            .nth(1)
            .unwrap_or_else(|| panic!("no {name}"));
        rest.chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
            .parse()
            .unwrap()
    };
    let menu_lines = uint_tunable("MENU_BODY_LINES");
    let toast_lines = uint_tunable("TOAST_BODY_LINES");
    let base = FrameInputs {
        scale: 1,
        bar_size: [640, 32],
        output_size: [1920, 1000],
        ..FrameInputs::default()
    };
    let strings = Strings::baked();
    let mut facts = Facts::new(slots());
    let slot = facts.slot_of(DEMO).expect("the demo has a slot");
    let mut images = Images::new();
    let rows = body_rows(false);
    let region = facts
        .rows_region(slot, &rows)
        .expect("the frame fits its region");

    // -- the menu ------------------------------------------------------------
    let scenario = Scenario {
        name: "bodies, in a menu",
        model: with_rows(fixture(), rows.clone()),
        level: applet_level(slot, BELL),
        capped: false,
    };
    oracle
        .build(&mut facts, &mut images, &strings, &scenario)
        .expect("the build pass runs");
    // Once more with the font's cap height in `params`, which the first
    // headless build ran without.
    oracle.gpu.run_apply(&base, &[]);
    oracle.gpu.mark_shape_pending();
    oracle
        .gpu
        .set_private_word("popup_state", bar_const("POPUP_UP"));
    oracle.gpu.set_private_word("popup_kind", abi::POPUP_MENU);
    oracle.gpu.run_apply(&base, &[]);
    let want = laid(&oracle.gpu.read_private()).want;
    let mapped = FrameInputs {
        popup_size: [want[0] as u32, want[1] as u32],
        ..base
    };
    oracle.gpu.run_apply(&mapped, &[]);
    let private = oracle.gpu.read_private();
    let runs = oracle.gpu.read_runs();

    let long_item = row_item(&private, slot, 12);
    let long = read_body(&private, &runs, &region, long_item);
    let texts = check_wrap(&long, wrap_w);
    eprintln!(
        "the 300-char body, {} lines (more={}):",
        texts.len(),
        long.more
    );
    for text in &texts {
        eprintln!("    {text:?}");
    }
    assert!(
        texts.len() >= 2,
        "300 characters at {wrap_w} px are more than one line"
    );
    assert!(texts.len() as u32 <= menu_lines.max(toast_lines));
    // Joined at the spaces it broke on, the lines are the body from its start.
    let joined = texts.join(" ");
    assert!(
        body_300().starts_with(&joined),
        "the lines are the body in order: {joined:?}"
    );
    let shown = (texts.len() as u32).min(menu_lines);
    let cut = shown < texts.len() as u32 || long.more;
    assert_eq!(
        cut,
        joined.len() < body_300().len(),
        "cut exactly when the lines stop short"
    );

    let laid_out = laid(&private);
    let rect_of = |item: u32| -> [f32; 4] {
        laid_out
            .rows
            .iter()
            .find(|(it, _)| *it == item)
            .unwrap_or_else(|| panic!("item {item} was not placed"))
            .1
    };
    let long_h = row_h + shown as f32 * line_h + body_pad;
    assert_eq!(
        rect_of(long_item)[3],
        long_h,
        "the row is its summary and {shown} lines tall"
    );

    let two_item = row_item(&private, slot, 11);
    let two = read_body(&private, &runs, &region, two_item);
    assert_eq!(
        check_wrap(&two, wrap_w),
        ["Line one", "Line two"],
        "the newline breaks"
    );
    assert!(!two.more);
    assert_eq!(rect_of(two_item)[3], row_h + 2.0 * line_h + body_pad);
    let bare_item = row_item(&private, slot, 10);
    assert_eq!(item_word(&private, bare_item, "body_blocks"), 0);
    assert_eq!(rect_of(bare_item)[3], row_h, "no body is a plain row");
    // DND-less frame: the bell's three rows, nothing else, and the popup asks
    // for exactly them.
    assert_eq!(
        laid_out.want[1],
        long_h + (row_h + 2.0 * line_h + body_pad) + row_h + 2.0 * pad,
        "the popup is its rows' height"
    );
    if cut {
        let last = &long.lines[shown as usize - 1];
        let ell = f32::from_bits(block_word(
            &private,
            word(&private, crate::gpu::private_field_offset("ellipsis_block")),
            "size",
        ));
        assert!(last.fit <= last.count && last.fit_w + ell <= wrap_w + 0.01);
    }
    oracle.gpu.run_apply(&mapped, &[Command::popup_done()]);

    // -- the toast -----------------------------------------------------------
    let scenario = Scenario {
        name: "bodies, as a toast",
        model: with_rows(fixture(), body_rows(true)),
        level: (0, 0),
        capped: false,
    };
    oracle
        .build(&mut facts, &mut images, &strings, &scenario)
        .expect("the build pass runs");
    // The toast waits for its rows to be built and wrapped, then opens at
    // their height — the 300-character row at TOAST_BODY_LINES, the newline
    // row at two, the bare row plain — and never moves.
    let (open, _) = until_toast_opens(&mut oracle, &base, &[]);
    assert_eq!(open.kind, abi::POPUP_TOAST);
    let rows_h = (row_h + toast_lines as f32 * line_h + body_pad)
        + (row_h + 2.0 * line_h + body_pad)
        + row_h;
    assert_eq!(
        open.want[1] as f32,
        rows_h + 2.0 * pad,
        "the open is the rows' height"
    );
    let size = open.want;
    let mut bodies: Vec<String> = Vec::new();
    let state = oracle.gpu.run_apply(
        &FrameInputs {
            popup_size: size,
            ..base
        },
        &[Command::popup_mapped(size[0], size[1])],
    );
    bodies.extend(said(&oracle, &state, "body:"));
    assert!(
        actions_of(&state, abi::ACT_POPUP_MOVE).is_empty(),
        "no resize"
    );
    for _ in 0..2 {
        let state = oracle.gpu.run_apply(
            &FrameInputs {
                popup_size: size,
                ..base
            },
            &[],
        );
        bodies.extend(said(&oracle, &state, "body:"));
        assert!(
            actions_of(&state, abi::ACT_POPUP_MOVE).is_empty(),
            "no resize"
        );
    }

    let private = oracle.gpu.read_private();
    let toast = laid(&private);
    let long_item = row_item(&private, slot, 12);
    let long = read_body(&private, &oracle.gpu.read_runs(), &region, long_item);
    let lines = long.lines.len() as u32;
    assert!(
        lines > toast_lines || long.more,
        "300 characters are more than a toast shows"
    );
    let rect = toast
        .rows
        .iter()
        .find(|(item, _)| *item == long_item)
        .expect("the long row is in the toast")
        .1;
    assert_eq!(
        rect[3],
        row_h + toast_lines as f32 * line_h + body_pad,
        "a toast's lines"
    );
    let last = &long.lines[toast_lines as usize - 1];
    let ell_block = word(&private, crate::gpu::private_field_offset("ellipsis_block"));
    let ell = f32::from_bits(block_word(&private, ell_block, "size"));
    assert!(ell > 0.0, "the ellipsis was shaped");
    assert!(
        last.fit <= last.count,
        "the last line gives way to the ellipsis"
    );
    assert!(
        last.fit_w + ell <= wrap_w + 0.01,
        "and the ellipsis fits after it"
    );
    let (_, a, b) = built_block(&private, ell_block);
    let strings_bytes = oracle.gpu.read_strings();
    assert_eq!(
        &strings_bytes[a as usize..(a + b) as usize],
        "…".as_bytes(),
        "one literal"
    );
    assert_eq!(
        toast.want,
        [size[0] as f32, size[1] as f32],
        "the layout keeps the open's size"
    );
    eprintln!("toast: {bodies:?}, size {size:?}");
    assert!(
        bodies.contains(&format!(
            "body: slot={slot} row=12 lines={toast_lines} cut=1"
        )),
        "the toast says how many lines it shows: {bodies:?}"
    );
    assert!(
        bodies.contains(&format!("body: slot={slot} row=11 lines=2 cut=0")),
        "{bodies:?}"
    );
    oracle.gpu.run_apply(&base, &[Command::popup_done()]);
    eprintln!("oracle: a body wraps at spaces, {shown} lines in a menu, {toast_lines} in a toast");
}

// ---------------------------------------------------------------------------
// The calendar
// ---------------------------------------------------------------------------

/// One `public static const uint NAME = 1u << n;` of `bar_model.slang`.
fn model_bit(name: &str) -> u32 {
    let needle = format!("public static const uint {name} = 1u << ");
    let rest = MODEL
        .split(&needle)
        .nth(1)
        .unwrap_or_else(|| panic!("{name} is no shifted bit"));
    let shift: String = rest.chars().take_while(char::is_ascii_digit).collect();
    1u32 << shift.parse::<u32>().expect("a shift")
}

/// One present element, as the calendar test reads the table back.
#[derive(Debug)]
struct Placed {
    target: u32,
    item: u32,
    rect: [f32; 4],
}

fn placed(private: &[u8]) -> Vec<Placed> {
    use crate::gpu::{private_field_offset as field, reflected_offset};
    let elements = field("elements");
    let stride = (field("items") - elements) / model_const("MAX_ELEMENTS") as usize;
    let count = word(private, field("element_count")).min(model_const("MAX_ELEMENTS"));
    let present = model_bit("EFLAG_PRESENT");
    (0..count as usize)
        .filter_map(|index| {
            let at = elements + index * stride;
            let flags = word(private, at + reflected_offset("Element", "flags"));
            let item = word(private, at + reflected_offset("Element", "item"));
            if flags & present == 0 || item >= model_const("MAX_ITEMS") {
                return None;
            }
            let rect = at + reflected_offset("Element", "rect");
            Some(Placed {
                target: word(private, at + reflected_offset("Element", "target")),
                item,
                rect: [0, 4, 8, 12].map(|k| float(private, rect + k)),
            })
        })
        .collect()
}

/// An item's `flags`, which `BuiltItem` does not carry.
fn item_flags(private: &[u8], item: u32) -> u32 {
    use crate::gpu::{private_field_offset as field, reflected_offset};
    let items = field("items");
    let stride = (field("blocks") - items) / model_const("MAX_ITEMS") as usize;
    word(
        private,
        items + item as usize * stride + reflected_offset("Item", "flags"),
    )
}

/// One month as the embassy publishes it: `clock_date` (today) and
/// `clock_month` (the year, the 1st's weekday, the length), written out by
/// hand from a printed calendar rather than by `clock::month_word` — the
/// oracle does not grade the Rust with the Rust.
struct Month {
    year: u32,
    month: u32,
    today: u32,
    /// Today's weekday, 0 = Sunday.
    weekday: u32,
    /// The 1st's weekday, 0 = Sunday.
    first: u32,
    days: u32,
}

impl Month {
    fn inputs(&self, base: &FrameInputs) -> FrameInputs {
        FrameInputs {
            clock_time: 9 * 60 + 30,
            clock_date: self.weekday << 16 | self.month << 8 | self.today,
            clock_month: self.year << 16 | self.first << 8 | self.days,
            ..*base
        }
    }

    /// The column the 1st sits in, Monday first (`CAL_WEEK_STARTS_MONDAY`).
    fn first_column(&self) -> u32 {
        (self.first + 6) % 7
    }

    fn weeks(&self) -> u32 {
        (self.first_column() + self.days - 1) / 7 + 1
    }
}

/// What the calendar test found for one month: every cell by its text, with
/// its column, its week under the weekday row and whether it is marked.
#[derive(Debug, Default)]
struct Grid {
    title: String,
    /// `(text, column)`, in element order.
    weekdays: Vec<(String, u32)>,
    /// `(text, column, week, marked)`, in element order; week 1 is the first.
    days: Vec<(String, u32, u32, bool)>,
    /// The frame's `layout: calendar-…` lines.
    laid: Vec<String>,
}

/// Open the calendar for `month` the way a click does, map it, let the
/// embassy's answer to `ACT_REBUILD` run the build, read the grid back, and
/// close it with a second click.
fn open_calendar(oracle: &mut Oracle, base: &FrameInputs, month: &Month) -> Grid {
    let cell = tunable("CAL_CELL_PX");
    let gap = tunable("CAL_GAP");
    let pad = tunable("POPUP_PAD");
    let row_h = tunable("MENU_ROW_H");
    let inputs = month.inputs(base);

    // The bar, laid out at this month's clock.
    oracle.gpu.mark_shape_pending();
    oracle.gpu.run_apply(&inputs, &[]);
    let private = oracle.gpu.read_private();
    let items = built_items(&private);
    let clock = placed(&private)
        .into_iter()
        .find(|e| {
            e.target == abi::TARGET_BAR && items[e.item as usize].kind == model_const("ITEM_CLOCK")
        })
        .expect("the clock is laid out on the bar");
    let at = [
        clock.rect[0] + clock.rect[2] / 2.0,
        clock.rect[1] + clock.rect[3] / 2.0,
    ];
    let click = [
        Command::pointer_motion(abi::TARGET_BAR, at),
        Command::pointer_down(abi::TARGET_BAR, at, abi::BTN_LEFT),
        Command::pointer_up(abi::TARGET_BAR, at, abi::BTN_LEFT),
    ];

    // The click: one menu-kind open at the clock, sized from the facts alone.
    let state = oracle.gpu.run_apply(&inputs, &click);
    let opens = actions_of(&state, abi::ACT_POPUP_OPEN);
    assert_eq!(
        opens.len(),
        1,
        "one open for one click on the clock: {opens:?}"
    );
    let open = opens[0].popup_open();
    assert_eq!(
        open.kind,
        abi::POPUP_MENU,
        "the calendar is a menu: it takes the grab"
    );
    assert_eq!(open.slot, abi::NO_INDEX, "no sidecar is told about it");
    assert_eq!(open.anchor, clock.rect, "anchored at the clock");
    let weeks = month.weeks();
    let want = [
        7.0 * cell + 6.0 * gap + 2.0 * pad,
        2.0 * pad + row_h + (weeks + 1) as f32 * cell + weeks as f32 * gap,
    ];
    assert_eq!(
        open.want,
        want.map(|v| v.round() as u32),
        "the grid's size, before a cell exists"
    );
    let opened = said(oracle, &state, "calendar:");
    eprintln!("{opened:?}, want {:?}", open.want);
    assert_eq!(opened.len(), 1, "one open line: {opened:?}");

    // Mapped at what it asked for; the embassy answers `ACT_REBUILD` by
    // marking the model passes pending, which builds the clock's level.
    let mapped = FrameInputs {
        popup_size: open.want,
        ..inputs
    };
    oracle.gpu.mark_shape_pending();
    let state = oracle.gpu.run_apply(
        &mapped,
        &[Command::popup_mapped(open.want[0], open.want[1])],
    );
    let laid_lines = said(oracle, &state, "layout: calendar-");
    let state = oracle.gpu.run_apply(&mapped, &[]);
    assert!(
        actions_of(&state, abi::ACT_POPUP_CLOSE).is_empty(),
        "nothing ended it"
    );
    assert!(
        actions_of(&state, abi::ACT_REBUILD).is_empty(),
        "the level is built"
    );
    assert!(
        actions_of(&state, abi::ACT_POPUP_MOVE).is_empty(),
        "the open's size was right"
    );

    let private = oracle.gpu.read_private();
    let strings = oracle.gpu.read_strings();
    let items = built_items(&private);
    let text = |block: u32| -> String {
        assert_ne!(block, abi::NO_INDEX, "a cell with no text");
        let (kind, a, b) = built_block(&private, block);
        assert_eq!(kind, model_const("BLOCK_TEXT"));
        String::from_utf8_lossy(&strings[a as usize..(a + b) as usize]).into_owned()
    };
    let x0 = pad;
    let y0 = pad + row_h;
    let step = cell + gap;
    let attention = model_bit("IFLAG_ATTENTION");
    let mut grid = Grid {
        laid: laid_lines,
        ..Grid::default()
    };
    assert_eq!(laid(&private).want, want, "the layout agrees with the open");
    for e in placed(&private)
        .into_iter()
        .filter(|e| e.target == abi::TARGET_POPUP)
    {
        let it = &items[e.item as usize];
        assert_eq!(
            it.kind,
            model_const("ITEM_CAL_CELL"),
            "only cells in the calendar: {it:?}"
        );
        assert_eq!(
            it.id.1 >> model_const("ID_TAG_SHIFT"),
            model_const("ID_CALENDAR")
        );
        let part = it.id.1 & model_const("ID_AUX_MASK");
        if part == model_const("CAL_TITLE") {
            assert_eq!(
                e.rect,
                [pad, pad, want[0] - 2.0 * pad, row_h],
                "the title's row"
            );
            grid.title = text(it.block);
            continue;
        }
        let column = ((e.rect[0] - x0) / step).round() as u32;
        let row = ((e.rect[1] - y0) / step).round() as u32;
        assert_eq!(
            e.rect,
            [
                x0 + column as f32 * step,
                y0 + row as f32 * step,
                cell,
                cell
            ],
            "a cell off the grid"
        );
        if part == model_const("CAL_WEEKDAY") {
            assert_eq!(row, 0, "the weekday row first");
            grid.weekdays.push((text(it.block), column));
        } else {
            let marked = item_flags(&private, e.item) & attention != 0;
            grid.days.push((text(it.block), column, row, marked));
        }
    }

    // A second click on the clock closes it, as an applet's chip does.
    let state = oracle.gpu.run_apply(&mapped, &click);
    assert_eq!(
        actions_of(&state, abi::ACT_POPUP_CLOSE).len(),
        1,
        "a second click closes it"
    );
    assert!(
        actions_of(&state, abi::ACT_POPUP_OPEN).is_empty(),
        "and opens nothing"
    );
    oracle.gpu.run_apply(&inputs, &[Command::popup_done()]);
    grid
}

/// Check one month's grid against a printed calendar: the title, the seven
/// initials Monday first, every day once in its column and week, today alone
/// marked, and the title named in the layout witness.
fn check_grid(grid: &Grid, month: &Month, title: &str) {
    assert_eq!(grid.title, title);
    let initials: Vec<&str> = grid.weekdays.iter().map(|(t, _)| t.as_str()).collect();
    assert_eq!(
        initials,
        ["M", "T", "W", "T", "F", "S", "S"],
        "Monday first"
    );
    let columns: Vec<u32> = grid.weekdays.iter().map(|(_, c)| *c).collect();
    assert_eq!(columns, [0, 1, 2, 3, 4, 5, 6]);
    assert_eq!(
        grid.days.len() as u32,
        month.days,
        "one cell a day: {:?}",
        grid.days
    );
    for (index, (text, column, week, marked)) in grid.days.iter().enumerate() {
        let day = index as u32 + 1;
        assert_eq!(text, &day.to_string(), "the cell of day {day}");
        let at = month.first_column() + day - 1;
        assert_eq!((*column, *week), (at % 7, at / 7 + 1), "day {day}'s place");
        assert_eq!(
            *marked,
            day == month.today,
            "day {day}: only today is marked"
        );
    }
    let name = format!("layout: calendar-{} target=popup ", title.replace(' ', "-"));
    assert!(
        grid.laid.iter().any(|line| line.starts_with(&name)),
        "no `{name}` among {:?}",
        grid.laid
    );
    eprintln!(
        "{title}: {} days, {} weeks, {} layout lines",
        grid.days.len(),
        month.weeks(),
        grid.laid.len()
    );
}

/// The clock's click opens this month's calendar, on the
/// bar's own passes on the bar's own GPU, for three months whose shapes
/// differ: October 2026 (the 1st a Thursday, 31 days, five weeks), March 2026
/// (the 1st a Sunday — the last column of a Monday-first grid — six weeks),
/// and February 2024 (a leap year's 29 days, today the 29th).
///
/// For each: one `ACT_POPUP_OPEN` of `POPUP_MENU` at the clock, already the
/// grid's size; once mapped and built, a title composed from `MONTHS` and the
/// year, the seven initials, the days 1 to N composed from the pool in their
/// columns and weeks, today's cell alone carrying the attention flag; a
/// second click on the clock closes it.
#[test]
fn the_clocks_click_opens_this_months_calendar() {
    if std::env::var("CAUSTICS_BAR_ORACLE").as_deref() != Ok("1") {
        eprintln!("skipping the oracle: set CAUSTICS_BAR_ORACLE=1 to run it");
        return;
    }
    let mut oracle = match Oracle::new() {
        Ok(oracle) => oracle,
        Err(err) => {
            eprintln!("skipping the oracle: {err:#}");
            return;
        }
    };
    let base = FrameInputs {
        scale: 1,
        bar_size: [640, 32],
        output_size: [1920, 1000],
        ..FrameInputs::default()
    };
    let strings = Strings::baked();
    let mut facts = Facts::new(slots());
    let mut images = Images::new();
    let scenario = Scenario {
        name: "a calendar",
        model: fixture(),
        level: (0, 0),
        capped: false,
    };
    oracle
        .build(&mut facts, &mut images, &strings, &scenario)
        .expect("the build pass runs");

    // Saturday 10 October 2026.
    let october = Month {
        year: 2026,
        month: 10,
        today: 10,
        weekday: 6,
        first: 4,
        days: 31,
    };
    let grid = open_calendar(&mut oracle, &base, &october);
    check_grid(&grid, &october, "Oct 2026");
    assert_eq!(grid.days[0].1, 3, "the 1st in Thursday's column");
    assert_eq!(grid.days[30].2, 5, "the 31st in the fifth week");

    // Tuesday 31 March 2026: the 1st a Sunday.
    let march = Month {
        year: 2026,
        month: 3,
        today: 31,
        weekday: 2,
        first: 0,
        days: 31,
    };
    let grid = open_calendar(&mut oracle, &base, &march);
    check_grid(&grid, &march, "Mar 2026");
    assert_eq!(
        (grid.days[0].1, grid.days[0].2),
        (6, 1),
        "the 1st alone in the first week"
    );
    assert_eq!(grid.days[30].2, 6, "six weeks");

    // Thursday 29 February 2024.
    let february = Month {
        year: 2024,
        month: 2,
        today: 29,
        weekday: 4,
        first: 4,
        days: 29,
    };
    let grid = open_calendar(&mut oracle, &base, &february);
    check_grid(&grid, &february, "Feb 2024");
    assert_eq!(grid.days[28].0, "29", "the leap day is there");
    eprintln!("oracle: the calendar opens, lays out three months, and closes");
}

// ---------------------------------------------------------------------------
// The chart numbers
// ---------------------------------------------------------------------------

/// One chart of a samples body whose newest column is known: its kind, its
/// ceiling, and one sample per series (`None` for one series; a negative is
/// the wire's gap). One valid column, so "the newest" is unambiguous.
fn readings(t: f32, charts: &[(u32, f32, f32, Option<f32>)]) -> Box<Samples> {
    let mut body = Samples::ZERO;
    body.chart_count = charts.len() as u32;
    body.sample_count = 1;
    body.t = t;
    let last = wire::CHART_SAMPLES - 1;
    for (c, (kind, ceiling, first, second)) in charts.iter().enumerate() {
        body.charts[c].kind = *kind;
        body.charts[c].ceiling = *ceiling;
        body.charts[c].series = if second.is_some() { 2 } else { 1 };
        body.samples[2 * c * wire::CHART_SAMPLES + last] = *first;
        if let Some(second) = second {
            body.samples[(2 * c + 1) * wire::CHART_SAMPLES + last] = *second;
        }
    }
    Box::new(body)
}

/// The six charts `caustics-sysmon` sends by default, at values that cover every
/// spelling the numbers have: 0, a fraction, 9.5 KiB/s, 12.3 MiB/s, 61 °C — and
/// a gap in a two-series chart.
fn six_readings(t: f32) -> Box<Samples> {
    const MIB: f32 = 1024.0 * 1024.0;
    readings(
        t,
        &[
            (abi::CHART_CPU, 1.0, 0.234, None),
            (abi::CHART_TEMP, 100.0, 61.2, None),
            (abi::CHART_MEM, 1.0, 0.0, None),
            (abi::CHART_NET, 0.0, 9.5 * 1024.0, Some(12.3 * MIB)),
            (abi::CHART_DISK, 0.0, 0.0, Some(512.0)),
            (abi::CHART_GPU, 100.0, 12.0, Some(-1.0)),
        ],
    )
}

/// What the six read as, line by line: the name, then the value.
const SIX_LINES: [(&str, &str); 6] = [
    ("CPU", "23 %"),
    ("TEMP", "61 °C"),
    ("MEM", "0 %"),
    ("NET", "↓ 9.5 KiB/s ↑ 12 MiB/s"),
    ("DISK", "R 0 B/s W 512 B/s"),
    ("GPU", "12 % / –"),
];

/// One word of item `item`'s row of the table, by the reflection's field name.
fn item_word(private: &[u8], item: u32, name: &str) -> u32 {
    use crate::gpu::{private_field_offset as field, reflected_offset};
    let items = field("items");
    let stride = (field("blocks") - items) / model_const("MAX_ITEMS") as usize;
    word(
        private,
        items + item as usize * stride + reflected_offset("Item", name),
    )
}

/// The text of a block, out of the strings buffer the device holds.
fn block_text(private: &[u8], strings: &[u8], block: u32) -> String {
    assert_ne!(block, abi::NO_INDEX, "a line with no text");
    let (kind, a, b) = built_block(private, block);
    assert_eq!(kind, model_const("BLOCK_TEXT"));
    String::from_utf8_lossy(&strings[a as usize..(a + b) as usize]).into_owned()
}

/// Every line of the chart numbers in the built table, in item order: the
/// chart's row, its name and its value.
fn number_lines(private: &[u8], strings: &[u8]) -> Vec<(u32, String, String)> {
    built_items(private)
        .iter()
        .enumerate()
        .filter(|(_, it)| it.kind == model_const("ITEM_CHART_LINE"))
        .map(|(index, it)| {
            let index = index as u32;
            (
                item_word(private, index, "a"),
                block_text(private, strings, it.block),
                block_text(private, strings, item_word(private, index, "aux_block")),
            )
        })
        .collect()
}

/// A samples body with known values composes the expected line for every
/// kind while the level is a chart: the names out of `CHART_NAMES`, the
/// numbers in their units (`compose_chart_value`), read back as the bytes the
/// build pass wrote into its reservations.
#[test]
fn the_chart_numbers_compose_every_kind() {
    if std::env::var("CAUSTICS_BAR_ORACLE").as_deref() != Ok("1") {
        eprintln!("skipping the oracle: set CAUSTICS_BAR_ORACLE=1 to run it");
        return;
    }
    let mut oracle = match Oracle::new() {
        Ok(oracle) => oracle,
        Err(err) => {
            eprintln!("skipping the oracle: {err:#}");
            return;
        }
    };
    let strings = Strings::baked();

    let mut compose = |body: Box<Samples>, level: (u32, u32)| -> Vec<(u32, String, String)> {
        let mut model = fixture();
        model.apply(Update::Samples {
            sidecar: "caustics-sysmon".into(),
            body: body,
        });
        let scenario = Scenario {
            name: "the chart numbers",
            model,
            level,
            capped: false,
        };
        let mut facts = Facts::new(slots());
        let mut images = Images::new();
        let (_, bytes) = oracle
            .build(&mut facts, &mut images, &strings, &scenario)
            .expect("the build pass runs");
        number_lines(&oracle.gpu.read_private(), &bytes)
    };

    let six = compose(six_readings(1.0), id_tagged("ID_CHART", 0, 2));
    eprintln!("{six:?}");
    assert_eq!(six.len(), 6, "one line per chart");
    for (index, ((row, name, value), (want_name, want_value))) in
        six.iter().zip(SIX_LINES).enumerate()
    {
        assert_eq!(*row, index as u32, "the strip's own order");
        assert_eq!((name.as_str(), value.as_str()), (want_name, want_value));
    }

    // The edges: a gap, the prefix climbing at 1023.5, one decimal stopping at
    // 9.95, the top prefix, a third party's bare number, a percent of 100 and
    // the whole of memory.
    const GIB: f32 = 1024.0 * 1024.0 * 1024.0;
    let edges = compose(
        readings(
            2.0,
            &[
                (abi::CHART_CPU, 1.0, -1.0, None),
                (abi::CHART_NET, 0.0, 1023.6, Some(1023.4)),
                (abi::CHART_DISK, 0.0, 10239.0, Some(5.0 * GIB)),
                (abi::CHART_OTHER, 0.0, 3.46, None),
                (abi::CHART_GPU, 100.0, 57.4, None),
                (abi::CHART_MEM, 1.0, 1.0, None),
            ],
        ),
        id_tagged("ID_CHART", 0, 0),
    );
    eprintln!("{edges:?}");
    let got: Vec<(&str, &str)> = edges
        .iter()
        .map(|(_, name, value)| (name.as_str(), value.as_str()))
        .collect();
    assert_eq!(
        got,
        [
            ("CPU", "–"),
            ("NET", "↓ 1.0 KiB/s ↑ 1023 B/s"),
            ("DISK", "R 10 KiB/s W 5.0 GiB/s"),
            ("Other", "3.5"),
            ("GPU", "57 %"),
            ("MEM", "100 %"),
        ]
    );

    // No chart level, no lines.
    assert!(
        compose(six_readings(3.0), (0, 0)).is_empty(),
        "lines with no level"
    );
    eprintln!("oracle: the chart numbers compose every kind");
}

/// One word of block `block` of the table, by the reflection's field name.
fn block_word(private: &[u8], block: u32, name: &str) -> u32 {
    use crate::gpu::{private_field_offset as field, reflected_offset};
    let blocks = field("blocks");
    let stride = (private.len() - blocks) / model_const("MAX_BLOCKS") as usize;
    word(
        private,
        blocks + block as usize * stride + reflected_offset("TextBlock", name),
    )
}

/// The chart numbers shape from the font `caustics-glyphs` sends: every name and
/// every value — `↓`, `↑`, `°C`, `–` among them — is a run with glyphs, a
/// width, and no `.notdef`. Composed bytes are this pass's own and never the
/// embassy's, so a glyph the font lacked could not be covered (a `REQ_COVER`
/// carries the embassy's bytes); this is the check that none is missing.
#[test]
fn the_chart_numbers_draw_from_the_font() {
    if std::env::var("CAUSTICS_BAR_ORACLE").as_deref() != Ok("1") {
        eprintln!("skipping the oracle: set CAUSTICS_BAR_ORACLE=1 to run it");
        return;
    }
    let mut oracle = match Oracle::new() {
        Ok(oracle) => oracle,
        Err(err) => {
            eprintln!("skipping the oracle: {err:#}");
            return;
        }
    };
    if let Err(err) = oracle.load_font() {
        eprintln!("skipping the oracle: no font to shape with ({err:#})");
        return;
    }
    let strings = Strings::baked();
    let mut facts = Facts::new(slots());
    let mut images = Images::new();
    let mut model = fixture();
    model.apply(Update::Samples {
        sidecar: "caustics-sysmon".into(),
        body: six_readings(1.0),
    });
    let level = id_tagged("ID_CHART", 0, 0);
    let scenario = Scenario {
        name: "the chart numbers, shaped",
        model,
        level,
        capped: false,
    };
    oracle
        .build(&mut facts, &mut images, &strings, &scenario)
        .expect("the build pass runs");
    let private = oracle.gpu.read_private();
    let mut checked = 0;
    for (index, it) in built_items(&private).iter().enumerate() {
        if it.kind != model_const("ITEM_CHART_LINE") {
            continue;
        }
        for block in [it.block, item_word(&private, index as u32, "aux_block")] {
            let flags = block_word(&private, block, "flags");
            let glyphs = block_word(&private, block, "glyph_count");
            let width = f32::from_bits(block_word(&private, block, "size"));
            assert_eq!(
                flags & punchcutter_shape::abi::SHAPE_RUN_NOTDEF,
                0,
                "line {index}'s block {block} has a glyph the font lacks"
            );
            assert!(
                glyphs > 0 && width > 0.0,
                "line {index}'s block {block} is empty"
            );
            checked += 1;
        }
    }
    assert_eq!(checked, 12, "six names and six values");
    eprintln!("oracle: the chart numbers' {checked} runs shape with no .notdef");
}

/// The level the caustic holds, as `(lo, hi)`.
fn held_level(private: &[u8]) -> (u32, u32) {
    let at = crate::gpu::private_field_offset("menu_level");
    (word(private, at), word(private, at + 4))
}

/// The hover over the strip asks for a `POPUP_PREVIEW` beside it after the
/// dwell; the hovered chart is the level — the marked line — and follows the
/// pointer; the popup lays out six lines in the strip's order; it ends when
/// the pointer leaves the strip. A click asks for a `POPUP_MENU` holding the
/// same lines, each `layout:` line carrying its composed number; a click on a
/// line does nothing; a new samples tick under it is one rebuild; a second
/// click closes it, and resting on the strip does not open it straight back.
///
/// The bar's own `bar_apply` and model passes on the bar's own GPU, as the
/// calendar's test runs them; the embassy's answer to `ACT_REBUILD` is
/// `mark_shape_pending`, written out.
#[test]
fn the_strips_hover_and_click_open_the_numbers() {
    if std::env::var("CAUSTICS_BAR_ORACLE").as_deref() != Ok("1") {
        eprintln!("skipping the oracle: set CAUSTICS_BAR_ORACLE=1 to run it");
        return;
    }
    let mut oracle = match Oracle::new() {
        Ok(oracle) => oracle,
        Err(err) => {
            eprintln!("skipping the oracle: {err:#}");
            return;
        }
    };
    // A real font, so the lines have widths and the popup is measured to
    // them; without one the test still runs, at the minimum width.
    let font = match oracle.load_font() {
        Ok(()) => true,
        Err(err) => {
            eprintln!("the numbers' width goes unchecked: no font ({err:#})");
            false
        }
    };
    let strings = Strings::baked();
    let mut facts = Facts::new(slots());
    let mut images = Images::new();
    let mut model = fixture();
    model.apply(Update::Samples {
        sidecar: "caustics-sysmon".into(),
        body: six_readings(1.0),
    });
    let scenario = Scenario {
        name: "six charts",
        model,
        level: (0, 0),
        capped: false,
    };
    oracle
        .build(&mut facts, &mut images, &strings, &scenario)
        .expect("the build pass runs");

    // A wide bar, so no chart is dropped for room; the caustic clamps a
    // frame's dt to DT_MAX, so the dwell takes a dozen frames. The clock is
    // `run_model_passes`' own (09:30, Saturday 10 October 2026), so a rebuild
    // composes the same date and the strip beside it does not move.
    let base = FrameInputs {
        scale: 1,
        bar_size: [1600, 32],
        output_size: [1920, 1000],
        dt: 0.05,
        clock_time: 9 * 60 + 30,
        clock_date: 6 << 16 | 10 << 8 | 10,
        clock_month: 2026 << 16 | 4 << 8 | 31,
        ..FrameInputs::default()
    };
    // Twice: `run_apply` binds the font's cap height in `params` *after* the
    // model passes it runs, so the first build here sizes the clock unsnapped
    // and every later one snapped — and the strip beside the clock moves.
    for _ in 0..2 {
        oracle.gpu.mark_shape_pending();
        oracle.gpu.run_apply(&base, &[]);
    }
    let private = oracle.gpu.read_private();
    let items = built_items(&private);
    let charts: Vec<Placed> = placed(&private)
        .into_iter()
        .filter(|e| {
            e.target == abi::TARGET_BAR && items[e.item as usize].kind == model_const("ITEM_CHART")
        })
        .collect();
    assert_eq!(charts.len(), 6, "six charts on the strip");
    let centre = |e: &Placed| [e.rect[0] + e.rect[2] / 2.0, e.rect[1] + e.rect[3] / 2.0];
    let lo_x = charts.iter().map(|e| e.rect[0]).fold(f32::MAX, f32::min);
    let hi_x = charts
        .iter()
        .map(|e| e.rect[0] + e.rect[2])
        .fold(f32::MIN, f32::max);
    let strip = [lo_x, charts[0].rect[1], hi_x - lo_x, charts[0].rect[3]];
    let cpu = centre(&charts[0]);
    let mem = centre(&charts[2]);
    let chart_level = |row: u32| id_tagged("ID_CHART", 0, row);
    let mapped = |size: [u32; 2]| FrameInputs {
        popup_size: size,
        ..base
    };

    // -- a rest in a gap between two charts: nothing opens, nothing redraws --
    //
    // The strip is the union of the charts' rects, so a gap is on the strip
    // and on no chart. A dwell clock that ran on there would ask for a frame
    // on every frame, for as long as the pointer rested.
    let mut by_x: Vec<[f32; 4]> = charts.iter().map(|e| e.rect).collect();
    by_x.sort_by(|a, b| a[0].total_cmp(&b[0]));
    let gap_lo = by_x[0][0] + by_x[0][2];
    let gap_hi = by_x[1][0];
    assert!(
        gap_hi > gap_lo,
        "a gap between the first two charts: {gap_lo}..{gap_hi}"
    );
    let gap = [(gap_lo + gap_hi) / 2.0, cpu[1]];
    oracle
        .gpu
        .run_apply(&base, &[Command::pointer_motion(abi::TARGET_BAR, gap)]);
    // The pointer's wake and the springs settle in ~50 frames at this dt; what
    // must not happen is the dwell holding the bar awake after them.
    let mut last_redraw = None;
    for frame in 0..120 {
        let state = oracle.gpu.run_apply(&base, &[]);
        assert!(
            actions_of(&state, abi::ACT_POPUP_OPEN).is_empty(),
            "a gap opens nothing"
        );
        if state.surface.wants_redraw != 0 {
            last_redraw = Some(frame);
        }
    }
    assert!(
        last_redraw.is_none_or(|frame| frame < 80),
        "a pointer resting in a gap, past the dwell, still asks for frames (last at {last_redraw:?})"
    );

    // -- the hover: nothing until the dwell, then one preview at the strip ---
    let state = oracle
        .gpu
        .run_apply(&base, &[Command::pointer_motion(abi::TARGET_BAR, cpu)]);
    assert!(
        actions_of(&state, abi::ACT_POPUP_OPEN).is_empty(),
        "no popup before the dwell"
    );
    let mut open = None;
    for _ in 0..40 {
        let state = oracle.gpu.run_apply(&base, &[]);
        if let Some(note) = actions_of(&state, abi::ACT_POPUP_OPEN).first() {
            assert_eq!(
                said(&oracle, &state, "numbers:"),
                ["numbers: open kind=preview chart=cpu lines=6"]
            );
            assert_eq!(
                actions_of(&state, abi::ACT_REBUILD).len(),
                1,
                "the level is a rebuild"
            );
            open = Some(note.popup_open());
            break;
        }
    }
    let open = open.expect("the hover asked for a popup");
    assert_eq!(
        open.kind,
        abi::POPUP_PREVIEW,
        "the hover's popup takes no grab"
    );
    assert_eq!(open.slot, abi::NO_INDEX, "no sidecar is told about it");
    assert_eq!(open.anchor, strip, "anchored at the whole strip");
    assert_eq!(
        held_level(&oracle.gpu.read_private()),
        chart_level(0),
        "the CPU is marked"
    );

    // Mapped at what it asked for and built; then at whatever width the
    // measured lines move it to.
    oracle.gpu.mark_shape_pending();
    let state = oracle.gpu.run_apply(
        &mapped(open.want),
        &[Command::popup_mapped(open.want[0], open.want[1])],
    );
    let mut size = open.want;
    for note in actions_of(&state, abi::ACT_POPUP_MOVE) {
        size = note.popup_size();
    }
    let state = oracle.gpu.run_apply(&mapped(size), &[]);
    for note in actions_of(&state, abi::ACT_POPUP_MOVE) {
        size = note.popup_size();
    }
    let state = oracle.gpu.run_apply(&mapped(size), &[]);
    assert!(
        actions_of(&state, abi::ACT_POPUP_CLOSE).is_empty(),
        "nothing ended it"
    );
    assert!(
        actions_of(&state, abi::ACT_POPUP_MOVE).is_empty(),
        "the width settled"
    );
    let private = oracle.gpu.read_private();
    let items = built_items(&private);
    let lines: Vec<Placed> = placed(&private)
        .into_iter()
        .filter(|e| e.target == abi::TARGET_POPUP)
        .collect();
    assert_eq!(lines.len(), 6, "six lines: {lines:?}");
    for (row, line) in lines.iter().enumerate() {
        assert_eq!(
            items[line.item as usize].kind,
            model_const("ITEM_CHART_LINE")
        );
        assert_eq!(
            item_word(&private, line.item, "a"),
            row as u32,
            "in the strip's order"
        );
        assert_eq!(line.rect[2], size[0] as f32 - 2.0 * tunable("POPUP_PAD"));
    }
    if font {
        // The widest line, the network's, is wider than a menu's minimum:
        // the width is the lines', measured, and not the floor.
        eprintln!("the numbers popup settled at {size:?}");
        assert!(
            size[0] as f32 > tunable("MENU_MIN_W"),
            "the popup was not measured: {size:?}"
        );
    }
    let shown = number_lines(&private, &oracle.gpu.read_strings());
    let shown: Vec<(&str, &str)> = shown
        .iter()
        .map(|(_, n, v)| (n.as_str(), v.as_str()))
        .collect();
    assert_eq!(shown, SIX_LINES, "the lines say what the samples say");

    // -- the mark follows the pointer; leaving the strip ends it ------------
    let state = oracle.gpu.run_apply(
        &mapped(size),
        &[Command::pointer_motion(abi::TARGET_BAR, mem)],
    );
    assert!(
        actions_of(&state, abi::ACT_POPUP_CLOSE).is_empty(),
        "a move along the strip keeps it"
    );
    assert_eq!(
        actions_of(&state, abi::ACT_REBUILD).len(),
        1,
        "the new level is a rebuild"
    );
    assert_eq!(
        held_level(&oracle.gpu.read_private()),
        chart_level(2),
        "the MEM line is marked"
    );
    let state = oracle.gpu.run_apply(
        &mapped(size),
        &[Command::pointer_motion(
            abi::TARGET_BAR,
            [strip[0] - 40.0, cpu[1]],
        )],
    );
    assert_eq!(
        actions_of(&state, abi::ACT_POPUP_CLOSE).len(),
        1,
        "leaving the strip ends it"
    );
    assert_eq!(
        said(&oracle, &state, "numbers:"),
        ["numbers: closed (left)"]
    );
    oracle.gpu.run_apply(&base, &[Command::popup_done()]);

    // -- the click: a menu holding the same lines ----------------------------
    let click = |at: [f32; 2], target: u32| {
        [
            Command::pointer_motion(target, at),
            Command::pointer_down(target, at, abi::BTN_LEFT),
            Command::pointer_up(target, at, abi::BTN_LEFT),
        ]
    };
    let state = oracle.gpu.run_apply(&base, &click(cpu, abi::TARGET_BAR));
    let opens = actions_of(&state, abi::ACT_POPUP_OPEN);
    assert_eq!(opens.len(), 1, "one open for one click: {opens:?}");
    let menu = opens[0].popup_open();
    assert_eq!(
        menu.kind,
        abi::POPUP_MENU,
        "the click's numbers take the grab"
    );
    assert_eq!(menu.anchor, strip);
    assert_eq!(menu.want, size, "the size the hover's popup measured");
    assert_eq!(
        said(&oracle, &state, "numbers:"),
        ["numbers: open kind=menu chart=cpu lines=6"]
    );
    oracle.gpu.mark_shape_pending();
    let state = oracle.gpu.run_apply(
        &mapped(menu.want),
        &[Command::popup_mapped(menu.want[0], menu.want[1])],
    );
    let laid_menu = said(&oracle, &state, "layout: numbers-");
    eprintln!("{laid_menu:?}");
    assert_eq!(laid_menu.len(), 6, "six lines laid out: {laid_menu:?}");
    for want in [
        "layout: numbers-cpu 23 % target=popup ",
        "layout: numbers-temp 61 °C target=popup ",
        "layout: numbers-net 9.5 KiB/s target=popup ",
        "layout: numbers-gpu 12 % target=popup ",
    ] {
        assert!(
            laid_menu.iter().any(|l| l.starts_with(want)),
            "no `{want}` in {laid_menu:?}"
        );
    }
    let private = oracle.gpu.read_private();
    let lines: Vec<Placed> = placed(&private)
        .into_iter()
        .filter(|e| e.target == abi::TARGET_POPUP)
        .collect();
    assert_eq!(lines.len(), 6);
    assert_eq!(
        held_level(&private),
        chart_level(0),
        "the clicked chart is marked"
    );

    // A click on a line is no click at all: the lines are not buttons.
    let state = oracle.gpu.run_apply(
        &mapped(menu.want),
        &click(centre(&lines[3]), abi::TARGET_POPUP),
    );
    assert!(
        actions_of(&state, abi::ACT_POPUP_CLOSE).is_empty(),
        "a line is not a button"
    );
    assert!(actions_of(&state, abi::ACT_SIDECAR).is_empty());

    // A tick under it is one rebuild, and the rebuild says the new number.
    let mut tick = six_readings(2.0);
    tick.samples[wire::CHART_SAMPLES - 1] = 0.871;
    oracle.gpu.upload_samples(abi::as_bytes(&*tick));
    let state = oracle.gpu.run_apply(&mapped(menu.want), &[]);
    assert_eq!(
        actions_of(&state, abi::ACT_REBUILD).len(),
        1,
        "a tick under the numbers"
    );
    oracle.gpu.mark_shape_pending();
    let state = oracle.gpu.run_apply(&mapped(menu.want), &[]);
    assert!(
        actions_of(&state, abi::ACT_REBUILD).is_empty(),
        "one rebuild per tick"
    );
    let after = number_lines(&oracle.gpu.read_private(), &oracle.gpu.read_strings());
    assert_eq!(after[0].2, "87 %", "the CPU's new number");

    // A second click on the strip closes it, and resting there does not open
    // it again until the pointer has left.
    let state = oracle
        .gpu
        .run_apply(&mapped(menu.want), &click(mem, abi::TARGET_BAR));
    assert_eq!(
        actions_of(&state, abi::ACT_POPUP_CLOSE).len(),
        1,
        "a second click closes it"
    );
    assert!(
        actions_of(&state, abi::ACT_POPUP_OPEN).is_empty(),
        "and opens nothing"
    );
    assert_eq!(
        said(&oracle, &state, "numbers:"),
        ["numbers: closed (click)"]
    );
    oracle.gpu.run_apply(&base, &[Command::popup_done()]);
    for _ in 0..30 {
        let state = oracle.gpu.run_apply(&base, &[]);
        assert!(
            actions_of(&state, abi::ACT_POPUP_OPEN).is_empty(),
            "held while on the strip"
        );
    }
    eprintln!("oracle: the strip's hover and click open the numbers, and they close");
}

/// The shape watchdog does not judge the first pass on new pipelines, and
/// judges every one after it: on lavapipe that pass times llvmpipe building
/// the variant, and every watchdog trip seen in a VM run was such a pass. A threshold no pass can meet makes the second pass trip;
/// the arena's front block releases it; a swap makes the next pass cold again.
#[test]
fn the_shape_watchdog_does_not_judge_a_cold_pass() {
    if std::env::var("CAUSTICS_BAR_ORACLE").as_deref() != Ok("1") {
        eprintln!("skipping the oracle: set CAUSTICS_BAR_ORACLE=1 to run it");
        return;
    }
    let mut oracle = match Oracle::new() {
        Ok(oracle) => oracle,
        Err(err) => {
            eprintln!("skipping the oracle: {err:#}");
            return;
        }
    };
    if !oracle.gpu.set_watchdog_threshold(1.0e-9) {
        eprintln!("skipping the watchdog case: this adapter has no timestamp queries");
        return;
    }
    let strings = Strings::baked();
    let mut facts = Facts::new(slots());
    let mut images = Images::new();
    let scenario = Scenario {
        name: "the full fixture",
        model: fixture(),
        level: (0, 0),
        capped: false,
    };
    let mut pass = |oracle: &mut Oracle| {
        oracle
            .build(&mut facts, &mut images, &strings, &scenario)
            .expect("the build pass runs");
        oracle.gpu.shape_blocked()
    };

    assert!(
        !pass(&mut oracle),
        "the first pass on a new device is cold and not judged"
    );
    assert!(
        pass(&mut oracle),
        "the second is judged, and no pass meets this threshold"
    );

    // The release is the arena's front block, as a restarted `caustics-glyphs`
    // sends it; then a swap, as `caustics-reload` delivers one.
    oracle.load_font().expect("a font for the release");
    assert!(
        !oracle.gpu.shape_blocked(),
        "the arena's front block releases the watchdog"
    );
    oracle
        .gpu
        .reload_baked()
        .expect("the baked modules swap in");
    assert!(
        !pass(&mut oracle),
        "the first pass after a swap is cold and not judged"
    );
    assert!(pass(&mut oracle), "and the one after it is judged again");
    eprintln!("oracle: the shape watchdog skips the cold pass and judges the rest");
}

/// The wake costs frames only while the pointer moves on the bar: a pointer
/// that arrives and rests stirs nothing, a sweep along the bar stirs it and
/// holds `wants_redraw`, and within `WAKE_SETTLE_S` of the last motion it says
/// it is still and the bar stops asking for frames. A click stirs it on its
/// own, and settles the same way.
#[test]
fn the_wake_is_motion_and_settles() {
    if std::env::var("CAUSTICS_BAR_ORACLE").as_deref() != Ok("1") {
        eprintln!("skipping the oracle: set CAUSTICS_BAR_ORACLE=1 to run it");
        return;
    }
    let mut oracle = match Oracle::new() {
        Ok(oracle) => oracle,
        Err(err) => {
            eprintln!("skipping the oracle: {err:#}");
            return;
        }
    };
    let strings = Strings::baked();
    let mut facts = Facts::new(slots());
    let mut images = Images::new();
    let scenario = Scenario {
        name: "the full fixture",
        model: fixture(),
        level: (0, 0),
        capped: false,
    };
    oracle
        .build(&mut facts, &mut images, &strings, &scenario)
        .expect("the build pass runs");

    let dt = 1.0 / 60.0;
    let base = FrameInputs {
        scale: 1,
        bar_size: [1600, 32],
        output_size: [1920, 1000],
        dt,
        ..FrameInputs::default()
    };
    let settle = tunable("WAKE_SETTLE_S");
    // The frames the wake may take to settle after the last motion, and a
    // bound on every loop below that waits for the bar to stop asking.
    let settle_frames = (settle / dt).ceil() as u32 + 2;
    let wait = settle_frames + 60;

    // Run frames with no input until the bar stops asking; the frame count,
    // and whether the wake said it was still on the way.
    let rest = |oracle: &mut Oracle| -> (Option<u32>, Option<u32>) {
        let mut still_at = None;
        for frame in 0..wait {
            let state = oracle.gpu.run_apply(&base, &[]);
            if !said(oracle, &state, "wake: still").is_empty() {
                still_at = Some(frame);
            }
            if state.surface.wants_redraw == 0 {
                return (Some(frame), still_at);
            }
        }
        (None, still_at)
    };

    // Arriving and resting: no source, whatever the hover springs do.
    let arrive = Command::pointer_motion(abi::TARGET_BAR, [200.0, 16.0]);
    let state = oracle.gpu.run_apply(&base, &[arrive]);
    assert!(
        said(&oracle, &state, "wake: stirred").is_empty(),
        "arriving on the bar stirs the wake"
    );
    let (settled, still) = rest(&mut oracle);
    assert!(
        settled.is_some(),
        "a pointer resting on the bar keeps the bar drawing"
    );
    assert!(
        still.is_none(),
        "a pointer resting on the bar stirred the wake"
    );

    // A sweep at 1200 px/s.
    let mut stirred = false;
    for i in 1..=30 {
        let x = 200.0 + 20.0 * i as f32;
        let motion = Command::pointer_motion(abi::TARGET_BAR, [x, 16.0]);
        let state = oracle.gpu.run_apply(&base, &[motion]);
        stirred = stirred || !said(&oracle, &state, "wake: stirred").is_empty();
        if i > 2 {
            assert_ne!(
                state.surface.wants_redraw, 0,
                "the wake is moving at frame {i} of the sweep"
            );
        }
    }
    assert!(stirred, "a sweep along the bar never stirred the wake");
    let (settled, still) = rest(&mut oracle);
    let still = still.expect("the wake never said it was still");
    assert!(
        still <= settle_frames,
        "the wake took {still} frames to settle, past WAKE_SETTLE_S ({settle_frames})"
    );
    assert!(
        settled.is_some(),
        "the bar still asks for frames {wait} frames after the sweep"
    );

    // A click, with the pointer still.
    let at = [700.0, 16.0];
    let state = oracle.gpu.run_apply(
        &base,
        &[Command::pointer_down(abi::TARGET_BAR, at, abi::BTN_LEFT)],
    );
    assert!(
        !said(&oracle, &state, "wake: stirred").is_empty(),
        "a click did not stir the wake"
    );
    oracle.gpu.run_apply(
        &base,
        &[Command::pointer_up(abi::TARGET_BAR, at, abi::BTN_LEFT)],
    );
    let (settled, still) = rest(&mut oracle);
    assert!(
        still.is_some_and(|f| f <= settle_frames),
        "a click's wave did not settle in time ({still:?})"
    );
    assert!(
        settled.is_some(),
        "the bar still asks for frames after a click's wave"
    );
    eprintln!("oracle: the wake stirs with motion and a click, and settles within WAKE_SETTLE_S");
}
