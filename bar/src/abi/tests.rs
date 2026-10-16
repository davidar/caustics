use super::*;
use core::mem::offset_of;

// Every assertion below is a cell of docs/abi.md, *Structs*'s table.

#[test]
fn frame_inputs_layout() {
    // Every word of it is something only the embassy can know. The pointer
    // and its target are not here — they arrive as `CMD_POINTER_*` and the
    // caustic keeps them — and `string_words` and `literal_count` are
    // `FactsHeader`'s, beside the tables they bound. `caffeine` is, because an
    // idle inhibitor is a Wayland object this side owns and not a fact about
    // content. The output's size and two pad words make it 80.
    assert_eq!(size_of::<FrameInputs>(), 80);
    assert_eq!(align_of::<FrameInputs>(), 16);
    assert_eq!(offset_of!(FrameInputs, frame), 0);
    assert_eq!(offset_of!(FrameInputs, dt), 4);
    assert_eq!(offset_of!(FrameInputs, time), 8);
    assert_eq!(offset_of!(FrameInputs, scale), 12);
    assert_eq!(offset_of!(FrameInputs, edge), 16);
    assert_eq!(offset_of!(FrameInputs, caffeine), 20);
    assert_eq!(offset_of!(FrameInputs, command_count), 24);
    // A `uint2` aligns to eight bytes in std430, which is what `_pad0` at 28
    // is for.
    assert_eq!(offset_of!(FrameInputs, bar_size), 32);
    assert_eq!(offset_of!(FrameInputs, popup_size), 40);
    assert_eq!(offset_of!(FrameInputs, preview_size), 48);
    assert_eq!(offset_of!(FrameInputs, clock_time), 56);
    assert_eq!(offset_of!(FrameInputs, clock_date), 60);
    assert_eq!(offset_of!(FrameInputs, output_size), 64);
    assert_eq!(offset_of!(FrameInputs, clock_month), 72);
}

#[test]
fn params_layout() {
    // No width or height: nothing read them, and the surface's size is
    // `FrameInputs`'.
    assert_eq!(size_of::<Params>(), 16);
    assert_eq!(offset_of!(Params, target), 0);
    assert_eq!(offset_of!(Params, cap_height), 4);
}

#[test]
fn command_layout() {
    assert_eq!(size_of::<Command>(), 32);
    assert_eq!(offset_of!(Command, tag), 0);
    assert_eq!(offset_of!(Command, target), 4);
    assert_eq!(offset_of!(Command, pos), 8);
    assert_eq!(offset_of!(Command, args), 16);
    assert_eq!(COMMAND_RING_SIZE, 32 * 256);
    assert_eq!(MAX_COMMANDS, 256);
}

/// The run buffer's stride is `punchcutter_shape::ShapedGlyph`'s size, and
/// `GLYPH_BUFFER_SIZE` spells the 32 as a literal because a sidecar
/// importing `caustics-bar-abi` has no business pulling a font parser in with
/// it. This is the pair, pinned — the note beside the constant names this
/// test.
///
/// Thirty-two because a glyph carries its **cluster**: the byte offset in the
/// request's string a glyph came from, which bidi reordering needs.
#[test]
fn the_glyph_record_is_thirty_two_bytes() {
    assert_eq!(size_of::<punchcutter_shape::ShapedGlyph>(), 32);
    assert_eq!(GLYPH_BUFFER_SIZE, 32 * MAX_GLYPHS as u64);
    assert_eq!(STRINGS_BUFFER_SIZE, 4 * MAX_STRING_WORDS as u64);
    // And a block never asks the shaper for more slots than it will write:
    // `Build::text` caps a block at `MAX_RUN_GLYPHS` slots, and the caustic
    // caps a run at the same number.
    assert!(punchcutter_shape::MAX_RUN_GLYPHS as usize <= MAX_GLYPHS);
}

/// One literal of the pool: 16 B, four plain words.
#[test]
fn literal_layout() {
    assert_eq!(size_of::<Literal>(), 16);
    assert_eq!(offset_of!(Literal, hash), 0);
    assert_eq!(offset_of!(Literal, off), 4);
    assert_eq!(offset_of!(Literal, len), 8);
    assert_eq!(offset_of!(Literal, _pad0), 12);
    assert_eq!(LITERALS_BUFFER_SIZE, 16 * MAX_LITERALS as u64);
}

// The facts, one test per struct (docs/abi.md, *Structs*). What a sidecar sent
// is not among them: a `Rows` body is in its region of the rows arena
// and a `Glyph` or `App` answer in the answers arena, both read through the
// wire's own structs (docs/abi.md, *The arenas*), so `FavFact`, `SidecarFact`, `FactRow` and
// `IconFact` are gone with the transcription that filled them.

#[test]
fn facts_header_layout() {
    assert_eq!(size_of::<FactsHeader>(), 32);
    assert_eq!(offset_of!(FactsHeader, window_count), 0);
    assert_eq!(offset_of!(FactsHeader, app_count), 4);
    assert_eq!(offset_of!(FactsHeader, image_count), 8);
    assert_eq!(offset_of!(FactsHeader, literal_count), 12);
    assert_eq!(offset_of!(FactsHeader, string_words), 16);
    assert_eq!(offset_of!(FactsHeader, compose_base), 20);
    assert_eq!(offset_of!(FactsHeader, answer_bytes), 24);
    assert_eq!(offset_of!(FactsHeader, glyph_bytes), 28);
}

#[test]
fn window_fact_layout() {
    assert_eq!(size_of::<WindowFact>(), 32);
    assert_eq!(align_of::<WindowFact>(), 16);
    assert_eq!(offset_of!(WindowFact, id_lo), 0);
    assert_eq!(offset_of!(WindowFact, id_hi), 4);
    // The join to the dock is `app_row`,
    // which is an index into a table the embassy just wrote.
    assert_eq!(offset_of!(WindowFact, app_row), 8);
    assert_eq!(offset_of!(WindowFact, title_off), 12);
    assert_eq!(offset_of!(WindowFact, title_len), 16);
    assert_eq!(offset_of!(WindowFact, flags), 20);
    assert_eq!(offset_of!(WindowFact, focus_stamp), 24);
}

#[test]
fn app_fact_layout() {
    // 16 B: an app id and one flag. The name, the icon and
    // the key are the apps sidecar's answer, joined by the id's own bytes.
    assert_eq!(size_of::<AppFact>(), 16);
    assert_eq!(offset_of!(AppFact, id_off), 0);
    assert_eq!(offset_of!(AppFact, id_len), 4);
    assert_eq!(offset_of!(AppFact, flags), 8);
}

#[test]
fn image_fact_layout() {
    assert_eq!(size_of::<ImageFact>(), 16);
    assert_eq!(offset_of!(ImageFact, name_off), 0);
    assert_eq!(offset_of!(ImageFact, name_len), 4);
    assert_eq!(offset_of!(ImageFact, slot), 8);
    // A row's icon is a name, found like any other.
    assert_eq!(offset_of!(ImageFact, _pad0), 12);
}

#[test]
fn slot_fact_layout() {
    assert_eq!(size_of::<SlotFact>(), 16);
    assert_eq!(offset_of!(SlotFact, name_off), 0);
    assert_eq!(offset_of!(SlotFact, name_len), 4);
}

/// The arenas are the wire's own bytes, so their capacities are pinned against
/// the wire's rather than against a second copy (docs/abi.md, *The arenas*) — and
/// so are the **offsets the caustic reads them at**.
///
/// The generic layout test cannot do this one. slangc reflects the types a
/// binding's layout needs, and both arenas are bound as
/// `RWStructuredBuffer<uint>` — they have to be, because `shapeRun` takes its
/// text through such a handle and a row's label lives in the arena — so
/// `RowsHeader`, `Row`, `GlyphBody` and `AppBody` have no reflected layout to
/// compare against. `bar.slang` reads them word by word at the literal offsets
/// below, and this is what says those literals are still the wire's.
#[test]
fn the_arenas_hold_what_the_wire_can_send() {
    // One region holds a frame's header, its rows and its blob. A `Rows` frame
    // the reader accepts carries at most `MAX_ROWS` rows; what a region cannot
    // promise is the blob, which is why an over-long body is one witness and
    // the region kept.
    assert!(16 + wire::MAX_ROWS * size_of::<wire::Row>() < ROWS_ARENA_BYTES);
    assert_eq!(ROWS_BUFFER_SIZE, (MAX_SLOTS * ROWS_ARENA_BYTES) as u64);
    assert_eq!(ANSWERS_BUFFER_SIZE, ANSWERS_BYTES as u64);
    // `Item.fact_row` packs a slot and a row index against `MAX_ROWS`.
    assert!(MAX_SLOTS * wire::MAX_ROWS < NO_INDEX as usize);

    // `rows_header` and `row_at` in `bar.slang`.
    assert_eq!(size_of::<wire::RowsHeader>(), 16);
    assert_eq!(offset_of!(wire::RowsHeader, applet), 0);
    assert_eq!(offset_of!(wire::RowsHeader, row_count), 4);
    assert_eq!(offset_of!(wire::RowsHeader, bytes_len), 8);
    assert_eq!(offset_of!(wire::RowsHeader, revision), 12);
    assert_eq!(size_of::<wire::Row>(), 48, "`16 + index * 48`");
    assert_eq!(offset_of!(wire::Row, id), 0);
    assert_eq!(offset_of!(wire::Row, parent), 4);
    assert_eq!(offset_of!(wire::Row, role), 8);
    assert_eq!(offset_of!(wire::Row, flags), 12);
    assert_eq!(offset_of!(wire::Row, label_off), 16);
    assert_eq!(offset_of!(wire::Row, label_len), 20);
    assert_eq!(offset_of!(wire::Row, icon_off), 24);
    assert_eq!(offset_of!(wire::Row, icon_len), 28);
    assert_eq!(offset_of!(wire::Row, value), 32);
    assert_eq!(offset_of!(wire::Row, min), 36);
    assert_eq!(offset_of!(wire::Row, max), 40);

    // `index_answers`, `answer_of` and `build_entry` in `bar.slang`.
    assert_eq!(size_of::<wire::GlyphBody>(), 16, "then `name_len` bytes");
    assert_eq!(offset_of!(wire::GlyphBody, gid), 0);
    assert_eq!(offset_of!(wire::GlyphBody, flags), 4);
    assert_eq!(offset_of!(wire::GlyphBody, name_len), 8);
    assert_eq!(
        size_of::<wire::AppBody>(),
        32,
        "then the five fields, in order"
    );
    assert_eq!(offset_of!(wire::AppBody, flags), 0);
    assert_eq!(offset_of!(wire::AppBody, app_len), 4);
    assert_eq!(offset_of!(wire::AppBody, name_len), 8);
    assert_eq!(offset_of!(wire::AppBody, icon_len), 12);
    assert_eq!(offset_of!(wire::AppBody, path_len), 16);
    assert_eq!(offset_of!(wire::AppBody, exec_len), 20);
}

#[test]
fn facts_layout() {
    assert_eq!(align_of::<Facts>(), 16);
    assert_eq!(offset_of!(Facts, header), 0);
    assert_eq!(offset_of!(Facts, windows), 32);
    assert_eq!(offset_of!(Facts, apps), 4128);
    assert_eq!(offset_of!(Facts, images), 5152);
    assert_eq!(offset_of!(Facts, slots), 6176);
    assert_eq!(offset_of!(Facts, literals), 6432);
    assert_eq!(size_of::<Facts>(), 10528);
    assert_eq!(FACTS_BUFFER_SIZE, 10528);
    // Zero reads as "nothing" where it must: no app row, no image key.
    assert_eq!(Facts::ZERO.windows[0].app_row, NO_INDEX);
    assert_eq!(Facts::ZERO.images[0].name_len, 0);
    // And the byte view round-trips, like the state buffer's.
    let mut facts = Facts::ZERO;
    facts.literals[MAX_LITERALS - 1].hash = 42;
    let bytes = as_bytes(&facts).to_vec();
    assert_eq!(bytes.len(), 10528);
    let mut back = Facts::ZERO;
    assert!(write_from_bytes(&mut back, &bytes));
    assert_eq!(back.literals[MAX_LITERALS - 1].hash, 42);
}

/// The state buffer: the surface's eight words, then the two rings
/// (docs/abi.md, *Structs*). The only buffer the embassy reads back.
#[test]
fn state_layout() {
    assert_eq!(size_of::<Surface>(), 32);
    assert_eq!(offset_of!(Surface, frame), 0);
    assert_eq!(offset_of!(Surface, wants_redraw), 4);
    assert_eq!(offset_of!(Surface, action_frame), 8);
    assert_eq!(offset_of!(Surface, witness_lost), 28);

    assert_eq!(size_of::<Note>(), 48);
    assert_eq!(offset_of!(Note, head), 0);
    assert_eq!(offset_of!(Note, aux), 4);
    assert_eq!(offset_of!(Note, arg), 8);

    assert_eq!(size_of::<State>(), 21536, "32 + 64 x 48 + 384 x 48");
    assert_eq!(offset_of!(State, surface), 0);
    assert_eq!(offset_of!(State, actions), 32);
    assert_eq!(offset_of!(State, witnesses), 3104);
    assert_eq!(STATE_BUFFER_SIZE, 21536);
}

/// Each ring is read against its own count and never past its capacity: a
/// count from a caustic that has since been swapped must name a record this
/// build has, and the drain memo in `wayland.rs` is what keeps a frame's
/// records from being read twice (docs/abi.md, *The three rules the buffer adds*, rule 1).
#[test]
fn the_rings_are_read_against_their_count() {
    let mut state = State::ZERO;
    assert!(
        state.live_actions().is_empty(),
        "a caustic that has not run pushed nothing"
    );
    assert!(state.live_witnesses().is_empty());

    state.surface.action_count = 3;
    state.actions[2].head = ACT_PREVIEW_BEGIN;
    assert_eq!(state.live_actions().len(), 3);
    assert_eq!(state.live_actions()[2].head, ACT_PREVIEW_BEGIN);

    state.surface.action_count = 9999;
    assert_eq!(
        state.live_actions().len(),
        MAX_ACTIONS,
        "never past the ring"
    );
    state.surface.witness_count = 9999;
    assert_eq!(state.live_witnesses().len(), MAX_WITNESSES);
}

/// One record's operands, read by the meanings *The action ring* gives them. The embassy
/// forwards these words and resolves nothing, so the accessors are the whole
/// of what it knows about a kind.
#[test]
fn a_notes_operands_are_read_by_meaning() {
    let mut note = Note::ZERO;
    note.arg[0] = 0x89ab_cdef;
    note.arg[1] = 0x0123_4567;
    assert_eq!(note.window_id(), 0x0123_4567_89ab_cdef);
    assert_eq!(note.string_range(), (0x89ab_cdef, 0x0123_4567));
    assert_eq!(note.anchor(), (0x89ab_cdef, 0x0123_4567));

    let mut sidecar = Note::ZERO;
    sidecar.arg = [
        5,
        7,
        ACTION_SET_VALUE,
        BTN_LEFT,
        3.0f32.to_bits(),
        4.0f32.to_bits(),
        0.25f32.to_bits(),
        0,
        0,
        0,
    ];
    let args = sidecar.sidecar();
    assert_eq!(
        (args.slot, args.row, args.kind, args.button),
        (5, 7, ACTION_SET_VALUE, BTN_LEFT)
    );
    assert_eq!(args.pos, [3.0, 4.0]);
    assert_eq!(args.value, 0.25);

    let mut popup = Note::ZERO;
    popup.arg = [
        POPUP_MENU,
        2,
        9,
        1.0f32.to_bits(),
        2.0f32.to_bits(),
        3.0f32.to_bits(),
        4.0f32.to_bits(),
        300,
        200,
        0,
    ];
    let open = popup.popup_open();
    assert_eq!((open.kind, open.slot, open.row), (POPUP_MENU, 2, 9));
    assert_eq!(open.anchor, [1.0, 2.0, 3.0, 4.0]);
    assert_eq!(open.want, [300, 200]);

    let mut want = Note::ZERO;
    want.arg[0] = 11;
    want.arg[1] = 4;
    want.arg[2] = wire::REQ_GLYPH;
    let asked = want.want_icon();
    assert_eq!((asked.off, asked.len, asked.kind), (11, 4, wire::REQ_GLYPH));
}

#[test]
fn byte_views_round_trip() {
    let mut state = State::ZERO;
    state.surface.frame = 7;
    state.surface.action_count = 3;
    state.actions[2].head = ACT_PREVIEW_BEGIN;
    state.witnesses[300].arg[9] = 0xffff;
    let bytes = as_bytes(&state).to_vec();
    assert_eq!(bytes.len(), 21536);
    let mut back = State::ZERO;
    assert!(write_from_bytes(&mut back, &bytes));
    assert_eq!(back, state);
    assert!(!write_from_bytes(&mut back, &bytes[..100]));
}

// -----------------------------------------------------------------------
// The caustic. Everything below runs slangc over `caustics/bar.slang` — the
// two halves of the ABI cannot drift silently, and the entry points cannot rot in either swapchain format, without one of these two tests
// going red. Modelled on `punchcutter_core::slang`'s own compile tests.
//
// The slangc invocation *is* `punchcutter_core::slang`'s, not a copy of it: the
// same flags in the same order the build script baked with and
// the reload loop recompiles with, so what is tested and what runs cannot drift.
// -----------------------------------------------------------------------

use std::path::{Path, PathBuf};

use punchcutter_core::slang::{self, SlangError};

/// The repository's `caustics/` directory, resolved against this crate so
/// the tests do not care about the harness's working directory.
fn caustics_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../caustics")
}

fn bar_slang() -> PathBuf {
    caustics_dir().join("bar.slang")
}

/// The include path every compile of the caustic takes: its own directory,
/// then the text engine's modules.
fn includes() -> [PathBuf; 2] {
    [caustics_dir(), punchcutter_core::slang_dir().to_path_buf()]
}

/// slangc's reflection of `bar.slang` for one entry point, from a compile
/// with the caustic's include path ([`includes`]): the JSON
/// `slang::struct_offsets` and `slang::bindings` read.
fn reflect_bar(entry: &str, defines: &[&str]) -> Result<String, SlangError> {
    let includes = includes();
    let includes: Vec<&Path> = includes.iter().map(PathBuf::as_path).collect();
    slang::compile_capturing(
        &slang::slangc_program(),
        &bar_slang(),
        entry,
        defines,
        &[],
        &includes,
    )
    .map(|compiled| compiled.reflection)
}

/// Whether a failure means "slangc is not installed", in which case every
/// caller skips rather than fails — the same posture `punchcutter_core::slang`
/// takes for `spirv-val`, and what lets a checkout without the Slang
/// toolchain still `cargo test` green.
fn is_missing_toolchain(err: &SlangError) -> Option<String> {
    match err {
        SlangError::Spawn { program, source } => Some(format!("{program}: {source}")),
        _ => None,
    }
}

/// slangc's own per-field byte offsets for every struct in the bar ABI,
/// against `offset_of!` on this side — **from the declaration tables**, not
/// from a hand-written copy of them (docs/abi.md, *The two contracts*).
///
/// `caustics_bar_abi::buffer::STRUCTS` and `wire::STRUCTS` are what
/// `caustics/bar_abi.slang` and `caustics/bar_wire.slang` were generated from,
/// and each entry carries the offsets `offset_of!` gives its fields. So this
/// walks both tables and compares each struct against the reflection **both
/// ways**: a field on one side only fails. A table written out again here would leave a struct added to the ABI untested until someone remembered this file.
///
/// The structs the bar shares with `punchcutter-shape` and `punchcutter-text`
/// are not in either table — they are the text engine's contracts — so they
/// stay as the explicit list below.
///
/// `bar_apply` is enough for all of them: slangc reflects the whole global
/// parameter block of the module, not only what one entry point happens to
/// read, so `Samples` (which only `bar_draw` touches) comes back too.
#[test]
fn the_slang_layout_matches_this_one() {
    use std::collections::BTreeMap;

    let json = match reflect_bar("bar_apply", &["DST_FORMAT_RGBA8"]) {
        Ok(json) => json,
        Err(err) => match is_missing_toolchain(&err) {
            Some(why) => {
                eprintln!("skipping the Slang layout check: slangc is not runnable ({why})");
                return;
            }
            None => panic!("reflection failed:\n{err}"),
        },
    };

    let reflected = slang::struct_offsets(&json);

    // The two declaration tables, and the structs that belong to the other
    // crates' contracts. Every line of the second list is a struct `bar.slang`
    // binds but `caustics-bar-abi` does not declare.
    let mut expected: Vec<(&str, Vec<(&str, usize)>)> = Vec::new();
    for entry in STRUCTS.iter().chain(wire::STRUCTS) {
        if !entry.bound {
            // A wire struct the GPU does not read yet (docs/abi.md, *Rows*, *Glyph*, *App*):
            // slangc lays out the types a binding needs and no others, and
            // the arenas are bound as `uint` buffers, so there is nothing to
            // compare. The crate's own `wire` tests pin these offsets.
            assert!(
                !reflected.contains_key(entry.decl.name),
                "`{}` is reflected now — set `bound: true` in the wire's table \
                 so this test checks it",
                entry.decl.name
            );
            continue;
        }
        expected.push((entry.decl.name, (entry.offsets)()));
    }
    expected.extend([
        (
            "ShapedGlyph",
            vec![
                ("gid", offset_of!(punchcutter_shape::ShapedGlyph, gid)),
                ("x", offset_of!(punchcutter_shape::ShapedGlyph, x)),
                ("y", offset_of!(punchcutter_shape::ShapedGlyph, y)),
                (
                    "cluster",
                    offset_of!(punchcutter_shape::ShapedGlyph, cluster),
                ),
                ("flags", offset_of!(punchcutter_shape::ShapedGlyph, flags)),
                ("_rsvd0", offset_of!(punchcutter_shape::ShapedGlyph, _rsvd0)),
                ("_rsvd1", offset_of!(punchcutter_shape::ShapedGlyph, _rsvd1)),
                ("_rsvd2", offset_of!(punchcutter_shape::ShapedGlyph, _rsvd2)),
            ],
        ),
        // Every word of `ShapeFont`, not a subset. It is four words: the
        // OpenType arena carries its own header, so no counts of flattened
        // tables are needed.
        (
            "ShapeFont",
            vec![
                ("upem", offset_of!(punchcutter_shape::ShapeFont, upem)),
                (
                    "glyph_count",
                    offset_of!(punchcutter_shape::ShapeFont, glyph_count),
                ),
                (
                    "ascender",
                    offset_of!(punchcutter_shape::ShapeFont, ascender),
                ),
                (
                    "descender",
                    offset_of!(punchcutter_shape::ShapeFont, descender),
                ),
            ],
        ),
        (
            "TextGlyph",
            vec![
                ("band_base", offset_of!(punchcutter_text::Glyph, band_base)),
                (
                    "hband_count",
                    offset_of!(punchcutter_text::Glyph, hband_count),
                ),
                (
                    "vband_count",
                    offset_of!(punchcutter_text::Glyph, vband_count),
                ),
                ("flags", offset_of!(punchcutter_text::Glyph, flags)),
                (
                    "band_transform",
                    offset_of!(punchcutter_text::Glyph, band_transform),
                ),
                ("bounds", offset_of!(punchcutter_text::Glyph, bounds)),
                ("advance", offset_of!(punchcutter_text::Glyph, advance)),
                ("_rsvd0", offset_of!(punchcutter_text::Glyph, _rsvd0)),
                ("_rsvd1", offset_of!(punchcutter_text::Glyph, _rsvd1)),
                ("_rsvd2", offset_of!(punchcutter_text::Glyph, _rsvd2)),
            ],
        ),
    ]);

    for (name, fields) in &expected {
        let actual = reflected
            .get(*name)
            .unwrap_or_else(|| panic!("slangc reflected no struct named `{name}`"));
        for (field, offset) in fields {
            let reported = actual.get(*field).copied().unwrap_or_else(|| {
                panic!("`{name}.{field}` is missing from the Slang declaration")
            });
            assert_eq!(
                reported, *offset,
                "`{name}.{field}`: Slang says {reported}, this module says {offset}"
            );
        }
        // A field added on one side only is exactly the drift this test
        // exists to catch, so the sets have to match, not merely overlap.
        let ours: BTreeMap<&str, usize> = fields.iter().copied().collect();
        for field in actual.keys() {
            assert!(
                ours.contains_key(field.as_str()),
                "`{name}.{field}` exists in Slang but not in this module"
            );
        }
    }

    // Both tables were walked: a struct declared but left out of `STRUCTS`
    // would be a declaration nothing renders and nothing checks.
    assert_eq!(STRUCTS.len(), 13, "the buffer declares thirteen structs");
    assert_eq!(
        wire::STRUCTS.len(),
        6,
        "the wire declares six the GPU reads or will"
    );
}

/// The samples body (docs/abi.md, *Samples*): the Rust half of its layout,
/// asserted here beside the buffer's because this is the one wire body the
/// GPU reads and the reflection rows above pin it against the Slang half.
#[test]
fn the_samples_body_is_8336_bytes_in_the_documented_layout() {
    assert_eq!(size_of::<wire::ChartRow>(), 16);
    assert_eq!(offset_of!(wire::ChartRow, ceiling), 0);
    assert_eq!(offset_of!(wire::ChartRow, series), 4);
    assert_eq!(offset_of!(wire::ChartRow, kind), 8);
    assert_eq!(offset_of!(wire::ChartRow, _rsvd0), 12);
    assert_eq!(size_of::<wire::Samples>(), wire::SAMPLES_SIZE);
    assert_eq!(wire::SAMPLES_SIZE, 8336);
    assert_eq!(offset_of!(wire::Samples, sample_count), 0);
    assert_eq!(offset_of!(wire::Samples, chart_count), 4);
    assert_eq!(offset_of!(wire::Samples, t), 8);
    assert_eq!(offset_of!(wire::Samples, _rsvd0), 12);
    assert_eq!(offset_of!(wire::Samples, charts), 16);
    assert_eq!(offset_of!(wire::Samples, samples), 144);

    let body = wire::Samples::ZERO;
    assert!(body.is_valid());
    assert_eq!(as_bytes(&body).len(), wire::SAMPLES_SIZE);
    assert_eq!(body.sample(0, 0, 0), -1.0, "an empty body is all gaps");
    assert_eq!(body.sample(MAX_CHARTS, 0, 0), -1.0, "out of range is a gap");
}

/// Every entry point, in both swapchain-format variants, with zero
/// warnings and a clean `spirv-val`.
///
/// This is the compile check turned into a test: the BGRA variant in particular is easy to break by hand (SPIR-V
/// has no BGRA storage format, so `dst` is declared format-unknown and the
/// `-default-image-format-unknown` flag has to come with it) and nothing
/// else in the workspace exercises it for the bar.
///
/// The variants come from `DstFormat`, which is what `bar/build.rs` bakes
/// and what `gpu.rs` selects between at run time — so a format added there
/// is tested here without anyone remembering to add it. The entry names
/// come from [`ENTRIES`], which is the one list all four readers share.
#[test]
fn the_caustic_compiles_for_every_variant() {
    let source = bar_slang();
    let slangc = slang::slangc_program();
    let includes = includes();
    let includes: Vec<&Path> = includes.iter().map(PathBuf::as_path).collect();

    for entry in ENTRIES {
        for format in caustics_bar_abi::DstFormat::ALL {
            let (define, extra) = format.args();
            // `compile_capturing` runs `spirv-val` over the module itself
            // where SPIRV-Tools is installed, so a rejected module is an
            // `Err` here rather than something to re-check by hand.
            let compiled = match slang::compile_capturing(
                &slangc,
                &source,
                entry,
                &[define],
                extra,
                &includes,
            ) {
                Ok(compiled) => compiled,
                Err(err) => match is_missing_toolchain(&err) {
                    Some(why) => {
                        eprintln!(
                            "skipping the caustic compile check: slangc is not runnable ({why})"
                        );
                        return;
                    }
                    None => panic!("{define}: {err}"),
                },
            };

            // The gate is zero warnings, not merely a zero exit code.
            assert!(
                compiled.warnings.is_empty(),
                "slangc warned on {entry}/{define} (the gate is zero warnings):\n{}",
                compiled.warnings
            );

            // The SPIR-V magic number, little-endian on every target.
            let module = &compiled.spirv;
            assert!(
                module.len() > 20,
                "{entry}/{define}: suspiciously short module"
            );
            assert_eq!(
                &module[..4],
                &[0x03, 0x02, 0x23, 0x07],
                "{entry}/{define}: output does not start with the SPIR-V magic number"
            );
        }
    }
}

/// One row of the tunables' `DOCK_MENU`, as its source spells it.
struct TunableRow {
    label: String,
    verb: String,
    when: String,
}

/// `DOCK_MENU` out of `caustics/bar_tunables.slang`, parsed.
fn dock_menu_rows() -> Vec<TunableRow> {
    let path = caustics_dir().join("bar_tunables.slang");
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("could not read {}: {err}", path.display()));
    let body = source
        .split_once("DOCK_MENU[DOCK_MENU_ROWS] = {")
        .unwrap_or_else(|| panic!("no DOCK_MENU table in {}", path.display()))
        .1
        .split_once("};")
        .expect("the table is closed")
        .0;
    body.lines()
        .filter_map(|line| line.trim().strip_prefix("uint4("))
        .map(|row| {
            let open = row.find('"').expect("a getStringHash literal") + 1;
            let close = open + row[open..].find('"').expect("the literal is closed");
            let words: Vec<&str> = row[close + 1..]
                .trim_start_matches(')')
                .split(',')
                .map(str::trim)
                .filter(|word| !word.is_empty())
                .collect();
            TunableRow {
                label: row[open..close].to_owned(),
                verb: words[0].to_owned(),
                when: words[1].to_owned(),
            }
        })
        .collect()
}

/// The dock's menu is four rows of the tunables, and slangc knows their words.
///
/// This is the dock menu's seam end to end on this side: the rows come out of
/// `bar_tunables.slang`, the labels come back out of slangc's reflection as
/// `getStringHash` literals (which is the only way the embassy ever learns
/// them — nothing in the bar holds these strings), and the verbs and the
/// `when`s are `bar_abi.slang`'s. the dock-menu step of `bar/bar-test.sh` reads the same four rows off a real screen.
#[test]
fn the_dock_menu_is_four_tunable_rows_whose_words_slangc_knows() {
    let rows = dock_menu_rows();
    let spelled: Vec<(&str, &str, &str)> = rows
        .iter()
        .map(|row| (row.label.as_str(), row.verb.as_str(), row.when.as_str()))
        .collect();
    assert_eq!(
        spelled,
        vec![
            ("New window", "ACT_SPAWN", "WHEN_ALWAYS"),
            ("Close", "ACT_CLOSE_WINDOW", "WHEN_RUNNING"),
            ("Pin", "ACT_PIN", "WHEN_UNPINNED"),
            ("Unpin", "ACT_UNPIN", "WHEN_PINNED"),
        ],
        "the shipped dock menu; the verb column is an ACT_*, so a \
         click on a row is that action with the entry's own operands and the \
         embassy translates nothing"
    );
    assert_eq!(
        rows.len() as u32,
        model_const("DOCK_MENU_ROWS"),
        "the table is as long as the caustic walks it"
    );

    // ...and every one of those labels reaches the embassy the only way it
    // can: through the reflection of the program `bar_apply` is compiled from.
    let json = match reflect_bar("bar_apply", &["DST_FORMAT_RGBA8"]) {
        Ok(json) => json,
        Err(err) => match is_missing_toolchain(&err) {
            Some(why) => {
                eprintln!("skipping the string table check: slangc is not runnable ({why})");
                return;
            }
            None => panic!("reflection failed:\n{err}"),
        },
    };
    let table = slang::hashed_strings(&json).expect("slangc's hashedStrings");
    for row in &rows {
        assert!(
            table.values().any(|literal| *literal == row.label),
            "slangc's reflection does not carry {:?}; the bar would draw no row",
            row.label
        );
    }
}

/// The chart tunables the texel probe needs, out of the caustic's own
/// module: a change to CHART_ASPECT, LINE_PX or the CPU colour moves the
/// probe with it rather than leaving it asserting yesterday's numbers.
struct ChartTunables {
    aspect: f32,
    frame_px: f32,
    line_px: f32,
    cpu_rgb: [f32; 3],
}

fn chart_tunables() -> ChartTunables {
    let path = caustics_dir().join("bar_tunables.slang");
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("could not read {}: {err}", path.display()));
    let value = |name: &str| -> String {
        let needle = format!(" {name} = ");
        source
            .lines()
            .find(|line| line.starts_with("public static const float") && line.contains(&needle))
            .and_then(|line| line.split_once(&needle))
            .map(|(_, tail)| tail.trim().trim_end_matches(';').to_string())
            .unwrap_or_else(|| panic!("no `{name}` in {}", path.display()))
    };
    let scalar = |name: &str| -> f32 {
        value(name)
            .parse()
            .unwrap_or_else(|err| panic!("`{name}` is not a float literal: {err}"))
    };
    let rgb = |name: &str| -> [f32; 3] {
        let raw = value(name);
        let inner = raw
            .trim_start_matches("float3(")
            .trim_end_matches(')')
            .to_string();
        let parts: Vec<f32> = inner
            .split(',')
            .map(|part| part.trim().parse().expect("a float3 of literals"))
            .collect();
        [parts[0], parts[1], parts[2]]
    };
    ChartTunables {
        aspect: scalar("CHART_ASPECT"),
        frame_px: scalar("CHART_FRAME_PX"),
        line_px: scalar("LINE_PX"),
        cpu_rgb: rgb("CHART_CPU_RGB"),
    }
}

/// The CPU stand-in for `draw_chart`/`draw_series` in `caustics/bar.slang`:
/// the same per-texel walk — map x to a fractional column of the valid
/// tail, take the two bracketing samples, and cover the segment between
/// them — returning the line's coverage rather than a colour.
fn chart_line_coverage(
    frame: &wire::Samples,
    chart: usize,
    series: usize,
    rect: [f32; 4],
    scale: f32,
    t: &ChartTunables,
    p: [f32; 2],
) -> f32 {
    // Outside the element there is no chart at all: everything below is
    // clipped by the same rect the visibility pass carves.
    if p[0] < rect[0] || p[0] > rect[0] + rect[2] || p[1] < rect[1] || p[1] > rect[1] + rect[3] {
        return 0.0;
    }
    let fw = t.frame_px * scale;
    let org = [rect[0] + fw, rect[1] + fw];
    let size = [(rect[2] - 2.0 * fw).max(1.0), (rect[3] - 2.0 * fw).max(1.0)];
    if p[0] < org[0] || p[0] > org[0] + size[0] || p[1] < org[1] || p[1] > org[1] + size[1] {
        return 0.0;
    }

    let sc = frame.sample_count.min(CHART_SAMPLES as u32);
    if sc < 2 {
        return 0.0;
    }
    let tail = |j: u32| frame.sample(chart, series, (CHART_SAMPLES as u32 - sc + j) as usize);

    let step = size[0] / (sc - 1) as f32;
    let f = ((p[0] - org[0]) / step).clamp(0.0, (sc - 1) as f32);
    let i0 = (f.floor() as u32).min(sc - 2);
    let (v0, v1) = (tail(i0), tail(i0 + 1));
    if v0 < 0.0 || v1 < 0.0 {
        return 0.0; // a gap breaks the segment on either side
    }

    let ceiling = frame.charts[chart].ceiling.max(1e-6);
    let n0 = (v0 / ceiling).clamp(0.0, 1.0);
    let n1 = (v1 / ceiling).clamp(0.0, 1.0);
    let a = [org[0] + i0 as f32 * step, org[1] + size[1] * (1.0 - n0)];
    let b = [
        org[0] + (i0 + 1) as f32 * step,
        org[1] + size[1] * (1.0 - n1),
    ];

    // `sd_segment` from the caustic, and its one-pixel coverage ramp.
    let (pa, ba) = ([p[0] - a[0], p[1] - a[1]], [b[0] - a[0], b[1] - a[1]]);
    let h = ((pa[0] * ba[0] + pa[1] * ba[1]) / (ba[0] * ba[0] + ba[1] * ba[1]).max(1e-6))
        .clamp(0.0, 1.0);
    let d = ((pa[0] - h * ba[0]).powi(2) + (pa[1] - h * ba[1]).powi(2)).sqrt();
    (0.5 - (d - t.line_px * scale * 0.5)).clamp(0.0, 1.0)
}

/// The texel probe: with a fixture frame, the line
/// lands inside the chart's rect and nowhere else, at the height its
/// samples put it, and a gap leaves a hole rather than a dive to zero.
#[test]
fn the_chart_line_lands_inside_its_rect_and_nowhere_else() {
    let t = chart_tunables();
    let scale: f32 = 2.0;

    // A chart at scale 2 on a 32 px bar: 56 px of band inside the chip
    // margins, times CHART_ASPECT.
    let row_h: f32 = (32.0 * scale - 2.0 * 2.0 * scale).max(1.0);
    let rect = [10.0, 4.0, (row_h * t.aspect).floor(), row_h];
    assert_eq!(
        (rect[2], rect[3]),
        (89.0, 56.0),
        "the chart rect at scale 2"
    );

    // Eight columns of CPU busy: two gaps (a rate with no delta yet) and
    // then a flat half.
    let mut frame = wire::Samples::ZERO;
    frame.sample_count = 8;
    frame.chart_count = 1;
    frame.charts[0] = wire::ChartRow {
        ceiling: 1.0,
        series: 1,
        kind: CHART_CPU,
        _rsvd0: 0,
    };
    for j in 0..8usize {
        let column = CHART_SAMPLES - 8 + j;
        frame.samples[column] = if j < 2 { -1.0 } else { 0.5 };
    }

    let (cw, ch) = (140usize, 80usize);
    let mut hits = Vec::new();
    for y in 0..ch {
        for x in 0..cw {
            let p = [x as f32 + 0.5, y as f32 + 0.5];
            let cov = chart_line_coverage(&frame, 0, 0, rect, scale, &t, p);
            if cov > 0.5 {
                hits.push(p);
            }
        }
    }

    assert!(!hits.is_empty(), "the fixture drew no line at all");
    for p in &hits {
        assert!(
            p[0] >= rect[0]
                && p[0] <= rect[0] + rect[2]
                && p[1] >= rect[1]
                && p[1] <= rect[1] + rect[3],
            "a line texel at {p:?} is outside the chart rect {rect:?}"
        );
    }

    // The flat half of the ceiling puts the line across the middle of the
    // plot area, and nowhere near the top or the bottom.
    let fw = t.frame_px * scale;
    let mid = rect[1] + fw + (rect[3] - 2.0 * fw) * 0.5;
    for p in &hits {
        assert!(
            (p[1] - mid).abs() <= 1.5,
            "a line texel at {p:?} is not on the mid-height line at y={mid}"
        );
    }

    // The two oldest columns are gaps, so the leftmost two steps of the
    // plot draw nothing — a hole, not a dive to zero.
    let step = (rect[2] - 2.0 * fw) / 7.0;
    let gap_end = rect[0] + fw + 2.0 * step;
    for p in &hits {
        assert!(
            p[0] > gap_end - 1.0,
            "a line texel at {p:?} is inside the gap"
        );
    }

    // And the ink is minimon's CPU red, composited at full coverage.
    assert_eq!(t.cpu_rgb.map(|c| (c * 255.0).round() as u8), [255, 6, 0]);
    println!(
        "chart rect {rect:?} at scale 2: {} line texels, mid-height {mid}",
        hits.len()
    );
}

/// The `TEXT_PX` table out of `caustics/bar_tunables.slang`, parsed.
fn text_px_tunables() -> Vec<f32> {
    let path = caustics_dir().join("bar_tunables.slang");
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("could not read {}: {err}", path.display()));
    let body = source
        .split_once("TEXT_PX[ITEM_KIND_COUNT] = {")
        .unwrap_or_else(|| panic!("no TEXT_PX table in {}", path.display()))
        .1
        .split_once("};")
        .expect("the table is closed")
        .0;
    body.lines()
        .filter_map(|line| line.trim().split_once(','))
        .map(|(value, _)| value.trim().parse().expect("a float literal"))
        .collect()
}

/// The text size per kind is a table in the tunables, one entry per item kind
/// the **caustic** declares, and the shipped one gives chrome 16 px and prose 14 px.
///
/// `bar_build` indexes
/// `TEXT_PX` by the kind of the item a block belongs to, and there is no rule in Rust about which text is big. The kinds come out of
/// `bar_model.slang`'s text, because that is where they live (docs/abi.md, *The private buffer*).
#[test]
fn the_text_size_is_one_tunable_table_per_item_kind() {
    let table = text_px_tunables();
    let count = model_const("ITEM_KIND_COUNT");
    assert_eq!(
        table.len() as u32,
        count,
        "the table is as long as the caustic walks it"
    );

    // Kind by kind: chrome that says a number or a state, and prose someone else
    // wrote. Eleven entries: the calendar's cell is the tenth and the chart's
    // numbers line the eleventh.
    let chrome = ["ITEM_CLOCK", "ITEM_OVERVIEW", "ITEM_APPLET"];
    let prose = [
        "ITEM_DOCK",
        "ITEM_WINDOW",
        "ITEM_MENU_ROW",
        "ITEM_CHART",
        "ITEM_SLIDER",
        "ITEM_CAL_CELL",
        "ITEM_CHART_LINE",
    ];
    for kind in chrome {
        assert_eq!(table[model_const(kind) as usize], 16.0, "{kind} is chrome");
    }
    for kind in prose {
        assert_eq!(table[model_const(kind) as usize], 14.0, "{kind} is prose");
    }
    assert_eq!(
        (chrome.len() + prose.len() + 1) as u32,
        count,
        "every kind but ITEM_NONE has a size, and no retired one has a slot"
    );
}

/// One `public static const uint NAME = <n>u;` of `caustics/bar_model.slang`.
///
/// The item kinds, the element slots, the block kinds and the capacities are
/// the **caustic's** — the ABI declares none of them (docs/abi.md, *The private buffer*) —
/// so a test that wants one reads it out of the module's own text. It is the
/// trick `shape/tests/layout.rs` still uses: a test's legibility, not a
/// contract.
fn model_const(name: &str) -> u32 {
    let path = caustics_dir().join("bar_model.slang");
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("could not read {}: {err}", path.display()));
    let needle = format!("public static const uint {name} = ");
    let rest = source
        .split(&needle)
        .nth(1)
        .unwrap_or_else(|| panic!("bar_model.slang declares no {name}"))
        .trim();
    let text: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    text.parse()
        .unwrap_or_else(|_| panic!("{name} is {rest:.20?}"))
}

/// The `bar_shape` dispatch covers the caustic's whole block table.
///
/// `MAX_BLOCKS` is `bar_model.slang`'s and the ABI describes no word of the
/// caustic's memory, so the embassy has a number it must *cover* rather than
/// know (`gpu::SHAPE_BLOCKS`). A thread past `live_blocks()` returns on its
/// first line, so over-dispatching is free; under-dispatching would leave the
/// tail of the table unshaped, with nothing to say so.
#[test]
fn the_shape_dispatch_covers_the_caustics_block_table() {
    let blocks = model_const("MAX_BLOCKS");
    assert!(
        crate::gpu::SHAPE_BLOCKS >= blocks,
        "the caustic has {blocks} block slots and the dispatch covers {}",
        crate::gpu::SHAPE_BLOCKS
    );
}

/// The three snap constants out of `caustics/bar.slang`, so this moves with
/// them rather than asserting yesterday's numbers.
fn snap_constants() -> (i32, f32, u32) {
    let path = bar_slang();
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("could not read {}: {err}", path.display()));
    let value = |name: &str| -> String {
        let needle = format!(" {name} = ");
        source
            .lines()
            .find(|line| line.starts_with("static const") && line.contains(&needle))
            .and_then(|line| line.split_once(&needle))
            .map(|(_, tail)| tail.trim().trim_end_matches([';', 'u']).to_string())
            .unwrap_or_else(|| panic!("no `{name}` in {}", path.display()))
    };
    (
        value("SNAP_WINDOW_PX").parse().expect("an int literal"),
        value("SNAP_TOLERANCE").parse().expect("a float literal"),
        value("MIN_TEXT_PX").parse().expect("a uint literal"),
    )
}

/// `snap_px` from `caustics/bar.slang`: the nearest size within the window whose cap height is whole, else
/// the number that was asked for.
fn snap_px(cap_height: f32, want: u32) -> u32 {
    let (window, tolerance, min_px) = snap_constants();
    let cap_error = |px: u32| {
        let cap = cap_height * px as f32;
        (cap - cap.round()).abs()
    };
    let want = want.max(min_px);
    if cap_height <= 0.0 {
        return want;
    }
    let mut best = want;
    let mut best_err = cap_error(want);
    for delta in 1..=window {
        for candidate in [want as i32 - delta, want as i32 + delta] {
            if candidate < min_px as i32 {
                continue;
            }
            let err = cap_error(candidate as u32);
            if err < best_err {
                best_err = err;
                best = candidate as u32;
            }
        }
    }
    if best_err > tolerance { want } else { best }
}

/// The caustic's snap: the sizes the shipped
/// tunables ask for, against the faces the bar actually meets.
///
/// It is unreachable from a test that cannot run a compute pass, so this is the same
/// arithmetic over the caustic's own three constants. What it pins: Adwaita Sans's whole-pixel sizes
/// are near multiples of 11 device px and Noto Sans's of 7, which is what the
/// tunables' own comment says.
#[test]
fn the_snap_gives_whole_cap_heights_on_the_shipped_faces() {
    let adwaita = 0.7275;
    let noto = 0.714;

    // The shipped table at scale 2: 32 device px of chrome and 28 of prose.
    assert_eq!(
        snap_px(adwaita, 32),
        33,
        "a 24 px cap height on Adwaita Sans"
    );
    assert_eq!(snap_px(adwaita, 28), 29, "and a 21 px one");
    assert_eq!(
        snap_px(noto, 32),
        32,
        "nothing nearer is whole on Noto Sans"
    );
    assert_eq!(snap_px(noto, 28), 28, "already whole");

    // ...and at scale 1, where Adwaita's whole-pixel sizes are far enough apart
    // that both of the tunables' numbers land on the same one. It is what "within one pixel" costs on a face whose
    // cap height is 0.7275 em.
    assert_eq!(snap_px(adwaita, 16), 15);
    assert_eq!(snap_px(adwaita, 14), 15);
    assert_eq!(snap_px(noto, 16), 16, "and a tighter face keeps both");
    assert_eq!(snap_px(noto, 14), 14);

    // No font yet — `Params::cap_height` is 0 until one has arrived — is no
    // snap, not a division by zero and not a size of nothing.
    assert_eq!(snap_px(0.0, 32), 32);

    // And the floor holds against an absurd tunable: a size of nothing becomes
    // MIN_TEXT_PX and is then snapped like any other, so the answer is at the
    // floor or just above it — never zero, and never a division by it.
    let (_, _, min_px) = snap_constants();
    for face in [adwaita, noto, 0.0] {
        let got = snap_px(face, 0);
        assert!((min_px..min_px + 2).contains(&got), "{face}: {got}");
    }
}

/// The `WEEKDAYS`, `MONTHS` and `CLOCK_DATE_ORDER` tables out of
/// `caustics/bar_tunables.slang`.
fn clock_tunables() -> (Vec<String>, Vec<String>, Vec<String>) {
    let path = caustics_dir().join("bar_tunables.slang");
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("could not read {}: {err}", path.display()));
    let table = |head: &str| -> String {
        source
            .split_once(head)
            .unwrap_or_else(|| panic!("no `{head}` in {}", path.display()))
            .1
            .split_once("};")
            .expect("the table is closed")
            .0
            .to_owned()
    };
    let literals = |head: &str| -> Vec<String> {
        let body = table(head);
        body.match_indices("getStringHash(\"")
            .map(|(at, needle)| {
                let start = at + needle.len();
                let end = start + body[start..].find('"').expect("the literal is closed");
                body[start..end].to_owned()
            })
            .collect()
    };
    let order: Vec<String> = table("CLOCK_DATE_ORDER[] = {")
        .split(',')
        .map(str::trim)
        .filter(|word| !word.is_empty() && !word.starts_with("//"))
        .map(str::to_owned)
        .collect();
    (
        literals("WEEKDAYS[7] = {"),
        literals("MONTHS[12] = {"),
        order,
    )
}

/// `compose_clock_time` and `compose_clock_date` from `caustics/bar.slang`,
/// over the tunables' own tables: the bytes the compose pass writes.
fn compose(time: u32, date: u32) -> (String, String) {
    let (weekdays, months, order) = clock_tunables();
    let hh_mm = format!("{:02}:{:02}", (time / 60) % 24, time % 60);
    let (weekday, month, day) = ((date >> 16) & 0xff, (date >> 8) & 0xff, date & 0xff);
    let parts: Vec<String> = order
        .iter()
        .filter_map(|token| match token.as_str() {
            "DATE_WEEKDAY" => weekdays.get(weekday as usize).cloned(),
            "DATE_DAY" => (1..=31).contains(&day).then(|| day.to_string()),
            "DATE_MONTH" => months.get(month.wrapping_sub(1) as usize).cloned(),
            _ => None,
        })
        .filter(|part| !part.is_empty())
        .collect();
    (hh_mm, parts.join(" "))
}

/// The clock the caustic composes, for known numbers: what `strftime`'s `%H:%M` and `%a %-d %b` would give.
///
/// The date's words and its order are the tunables', so this reads them out of `bar_tunables.slang` and asserts the *shipped* result. `bar/bar-test.sh` reads the rendered clock off a real screen.
#[test]
fn the_clock_composes_what_strftime_would() {
    // 14:03 on Friday 5 September, the example in docs/abi.md.
    let (time, date) = compose(14 * 60 + 3, 5 << 16 | 9 << 8 | 5);
    assert_eq!(time, "14:03");
    assert_eq!(date, "Fri 5 Sep");

    // Midnight, a zero-padded hour and minute, and the first of the month with
    // no leading zero — `%-d`, which is where the composition could differ.
    let (time, date) = compose(9, 4 << 16 | 1 << 8 | 1);
    assert_eq!(time, "00:09");
    assert_eq!(date, "Thu 1 Jan");

    // The last minute of the year, and a two-digit day.
    let (time, date) = compose(23 * 60 + 59, 6 << 16 | 12 << 8 | 31);
    assert_eq!(time, "23:59");
    assert_eq!(date, "Sat 31 Dec");

    // A broken clock — `clock::clock_numbers` answers `(0, 0, 0)` when the C
    // library will not give local time — composes midnight and a bare weekday
    // rather than a garbled date or a read past a table. The space between two
    // parts belongs to the part that said something, so there is no double one.
    let (time, date) = compose(0, 0);
    assert_eq!(time, "00:00");
    assert_eq!(date, "Sun");

    // Every one of those words reaches the GPU the only way it can: through the
    // literal pool, whose bytes are the reflection's `hashedStrings`.
    let json = match reflect_bar("bar_compose", &["DST_FORMAT_RGBA8"]) {
        Ok(json) => json,
        Err(err) => match is_missing_toolchain(&err) {
            Some(why) => {
                eprintln!("skipping the clock's string table check: slangc ({why})");
                return;
            }
            None => panic!("reflection failed:\n{err}"),
        },
    };
    let table = slang::hashed_strings(&json).expect("slangc's hashedStrings");
    let (weekdays, months, _) = clock_tunables();
    for word in weekdays.iter().chain(months.iter()) {
        assert!(
            table.values().any(|literal| literal == word),
            "slangc's reflection does not carry {word:?}; the date would be short a word"
        );
    }
    assert!(
        table.len() <= MAX_LITERALS,
        "the pool holds the whole table"
    );
    println!(
        "the caustic names {} literals; the pool holds {MAX_LITERALS}",
        table.len()
    );
}

/// The three icon lists `bar_build` reads are the tunables' own, and every
/// name in them reaches the GPU the only way a name can — through slangc's
/// `hashedStrings` and the pool the facts publish.
///
/// Three Rust `const &[&str]` lists would be exactly the thing the bar process's membership rule (docs/architecture.md, *The bar process*) says the embassy must not hold: which icon a state wears.
#[test]
fn the_overview_and_caffeine_icons_are_the_tunables_own_lists() {
    let names = |table: &str| -> Vec<String> { hash_literals(table) };
    let overview = names("OVERVIEW_ICONS[] = {");
    let on = names("CAFFEINE_ON_ICONS[] = {");
    let off = names("CAFFEINE_OFF_ICONS[] = {");
    assert!(overview.len() >= 2, "the overview has a preference order");
    assert!(
        !on.is_empty() && !off.is_empty(),
        "caffeine's two states are named"
    );
    assert_ne!(
        on, off,
        "and they are told apart by the icon and nothing else"
    );

    let json = match reflect_bar("bar_build", &["DST_FORMAT_RGBA8"]) {
        Ok(json) => json,
        Err(err) => match is_missing_toolchain(&err) {
            Some(why) => {
                eprintln!("skipping the icon lists' string table check: slangc ({why})");
                return;
            }
            None => panic!("reflection failed:\n{err}"),
        },
    };
    let table = slang::hashed_strings(&json).expect("slangc's hashedStrings");
    for name in overview.iter().chain(on.iter()).chain(off.iter()) {
        assert!(
            table.values().any(|literal| literal == name),
            "slangc's reflection does not carry {name:?}; the caustic would ask the \
             directory under a hash the embassy has no bytes for"
        );
    }
    // ...and the overview's last resort, which is a character rather than a
    // theme name and reaches the GPU the same way.
    let glyph = single_hash_literal("OVERVIEW_GLYPH = getStringHash(");
    assert!(
        table.values().any(|literal| *literal == glyph),
        "slangc's reflection does not carry the overview's fallback glyph"
    );
}

/// Every `getStringHash("…")` literal of one tunables table, in order.
fn hash_literals(marker: &str) -> Vec<String> {
    let path = caustics_dir().join("bar_tunables.slang");
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("could not read {}: {err}", path.display()));
    let body = source
        .split_once(marker)
        .unwrap_or_else(|| panic!("no `{marker}` in {}", path.display()))
        .1
        .split_once("};")
        .expect("the table is closed")
        .0;
    body.split("getStringHash(\"")
        .skip(1)
        .filter_map(|tail| tail.split_once('"').map(|(name, _)| name.to_owned()))
        .collect()
}

/// The one-literal form: `public static const uint NAME = getStringHash("…");`
fn single_hash_literal(marker: &str) -> String {
    let path = caustics_dir().join("bar_tunables.slang");
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("could not read {}: {err}", path.display()));
    source
        .split_once(marker)
        .unwrap_or_else(|| panic!("no `{marker}` in {}", path.display()))
        .1
        .split_once('"')
        .expect("a literal opens")
        .1
        .split_once('"')
        .expect("and closes")
        .0
        .to_owned()
}

/// `build_first_line` in `caustics/bar.slang`, reimplemented: a label is one
/// line, trimmed, and the range shrinks rather than any byte moving.
///
/// dbusmenu rows and window titles both arrive multi-line on the live bus, and
/// the truncation is the GPU's.
fn first_line(bytes: &[u8]) -> &[u8] {
    let end = bytes
        .iter()
        .position(|byte| *byte == b'\n' || *byte == b'\r')
        .unwrap_or(bytes.len());
    let space = |byte: u8| matches!(byte, b' ' | b'\t' | 0x0b | 0x0c);
    let mut start = 0;
    let mut end = end;
    while start < end && space(bytes[start]) {
        start += 1;
    }
    while end > start && space(bytes[end - 1]) {
        end -= 1;
    }
    &bytes[start..end]
}

#[test]
fn a_label_is_its_first_line() {
    assert_eq!(first_line(b"plain"), b"plain");
    assert_eq!(first_line(b"  padded  "), b"padded");
    assert_eq!(first_line(b"first\nsecond"), b"first");
    assert_eq!(first_line(b"first\r\nsecond"), b"first");
    assert_eq!(first_line(b"  first line  \n  second"), b"first line");
    assert_eq!(first_line(b"\n"), b"");
    assert_eq!(first_line(b""), b"");
    assert_eq!(first_line(b"   "), b"");
    // A multi-byte codepoint is never split: the rule looks at ASCII bytes
    // only, and every byte of a UTF-8 sequence past the first is >= 0x80.
    assert_eq!(first_line("héllo\nworld".as_bytes()), "héllo".as_bytes());
}

/// The bar's own caustic, reflected for real: the 23 bindings across three
/// spaces that `bar/src/gpu.rs` builds its layouts from, and the two facts
/// about them the layout cannot get from anywhere else.
///
/// `bar/src/gpu/tests.rs` pins the counts against the *baked* JSON; this one
/// stands between a slangc whose output shape moved and a bar that panics at
/// startup.
#[test]
fn the_bars_caustic_reflects_its_bindings() {
    let json = match reflect_bar("bar_shape", &["DST_FORMAT_RGBA8"]) {
        Ok(json) => json,
        Err(err) => match is_missing_toolchain(&err) {
            Some(why) => {
                eprintln!("skipping the binding check: slangc is not runnable ({why})");
                return;
            }
            None => panic!("the bar's caustic should reflect:\n{err}"),
        },
    };
    let bindings = slang::bindings(&json).expect("the bar's own reflection classifies");

    // 23: set 2 holds the font as one arena of verbatim OpenType bytes, and the rows and answers arenas sit
    // in set 0 (docs/abi.md, *The arenas*). A change here is an ABI change.
    assert_eq!(bindings.len(), 23, "{bindings:#?}");
    let spaces: std::collections::BTreeSet<u32> =
        bindings.iter().map(|binding| binding.set).collect();
    assert_eq!(spaces, [0, 1, 2].into_iter().collect());

    // Every set is dense from zero, which is what makes a positional layout
    // list legal at all (wgpu-hal numbers a Vulkan binding by position).
    for space in spaces {
        let indices: Vec<u32> = bindings
            .iter()
            .filter(|binding| binding.set == space)
            .map(|binding| binding.index)
            .collect();
        assert_eq!(
            indices,
            (0..indices.len() as u32).collect::<Vec<u32>>(),
            "space {space} has a gap"
        );
    }

    let named = |name: &str| {
        bindings
            .iter()
            .find(|binding| binding.name == name)
            .unwrap_or_else(|| panic!("`{name}` is not among {bindings:#?}"))
            .clone()
    };
    // The two storage images, which is the pair whose texel format the
    // reflection cannot carry on the Bgra8 variant.
    assert_eq!(
        named("ids").kind,
        slang::BindingKind::StorageTexture { write_only: false }
    );
    assert_eq!(
        named("dst").kind,
        slang::BindingKind::StorageTexture { write_only: true }
    );
    // The fixed-slot image array, and the one plain 2D preview.
    assert_eq!(
        named("images").kind,
        slang::BindingKind::Texture { array: true }
    );
    assert_eq!(
        named("preview").kind,
        slang::BindingKind::Texture { array: false }
    );
    // `State` is one element, so its stride is the whole state buffer. The
    // exact size is `caustics-bar`'s to pin (its ABI test compares slangc's layout
    // against `size_of::<State>()` on every build) and it moves with every ABI
    // bump, so here it is only "read-write, and reflected at all".
    match named("state").kind {
        slang::BindingKind::Storage {
            read_only: false,
            stride: Some(stride),
        } => {
            assert!(stride > 0, "the state buffer's stride is zero");
        }
        other => panic!("`state` is {other:?}, not a read-write storage buffer with a stride"),
    }
}
