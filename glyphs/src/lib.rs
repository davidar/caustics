//! `caustics-glyphs` — the curves sidecar.
//!
//! Everything Slug fills, filled outside the bar: the font's Bézier curves and
//! band tables, the **arena** of verbatim OpenType tables, and every symbolic
//! icon appended to the same glyph-id space. Finding the font, building its
//! buffers and covering what it lacks are the text engine's
//! (`punchcutter_fonts`); what this process adds is the wire, the icons and the
//! cover protocol over the pipe. It writes one `Font` frame and
//! then a run of `Bytes` frames — one per chunk, six buffers — and the bar
//! copies each run into the GPU buffer the frame names without reading a byte
//! of it (docs/abi.md, *Bytes*–*Glyph*).
//!
//! The shaping data is one buffer, not a set of flattened tables. What this
//! process sends for `BUF_OT` is `cmap`, `hmtx`, `GDEF`, `GSUB` and `GPOS` sliced out
//! of the font file **unread**, behind a directory saying where each landed;
//! the engine's `ot.slang` walks OpenType's own format on the other side, so a
//! new lookup type is a shader edit and this side never flattens a table.
//!
//! Why a sidecar at all: not a decision that has to be out of process, but
//! *two hundred milliseconds of computation* that would otherwise sit on the
//! bar's startup path. It links ttf-parser, usvg and a cache; none of that
//! belongs in the address space of a process whose job is to hold a Wayland
//! surface and dispatch a caustic. The bar links neither `punchcutter-text` nor
//! a font parser (docs/architecture.md, *Sidecars*).
//!
//! # What goes out
//!
//! On start: `Font` (upem, cap height, the glyph count and where the icons will
//! begin), then `Bytes` for each of the six `BUF_*` in ABI order — the four
//! curve buffers, `BUF_SHAPE_FONT`, the arena. A buffer longer than
//! [`CHUNK`] is several frames at consecutive offsets, every one of them
//! carrying the same `total`, so the bar can size the GPU buffer on the first
//! one and never resize mid-font.
//!
//! On a `Request` of kind `REQ_COVER`: the **bytes of a string** whose run the
//! shaper flagged `.notdef` (docs/architecture.md, *Text*; the rest of shaping
//! is on the GPU). [`Merged::cover`] opens the faces that draw what the primary
//! lacks into the arena and fills their glyphs, and the GSUB closure of those
//! glyphs; the shaper flags a glyph whose curves have not arrived and the bar
//! asks again.
//!
//! What goes back is the four curve tails, the arena's front block, the new
//! face's table bytes and the fill bitmap, and `BUF_SHAPE_FONT` — deltas, not
//! the arena again.
//!
//! On a `Request` of kind `REQ_GLYPH`: the theme search (`caustics_sidecar::icons`,
//! shared with `caustics-apps` and `caustics-tray`), then, for a
//! symbolic SVG, [`punchcutter_text::icon::load_file`] into this process's own
//! [`Builder`] and **only the new tail** of each of the four curve buffers back
//! as `Bytes`, followed by a `Glyph` naming the id. A name no theme has, or one
//! that is not curves, is `Glyph { GLYPH_MISSING }` — the bar then takes its
//! pixels path. A repeat is answered from the map without appending, because
//! the bar may have restarted and asked again.
//!
//! # Its own settings, never the bar's
//!
//! `$PUNCHCUTTER_FONT` names a font file (or a fontconfig pattern) to use
//! instead of whatever `fc-match` picks; the engine reads it. The bar does not
//! set it, does not read it and has no opinion about it — the same rule
//! `$CAUSTICS_SYSMON_CHARTS` follows (docs/architecture.md, *Sidecars*). The
//! curve cache is the engine's too, under `$XDG_CACHE_HOME/punchcutter/`.
//!
//! # Witness lines
//!
//! The engine reports through `tracing`; [`run`] installs a subscriber that
//! turns each event into one witness line, so what a load or a cover did
//! reaches the bar's journal as this sidecar's own words.

use std::borrow::Cow;
use std::collections::HashMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use caustics_bar_abi::wire::{
    self, BUF_BAND_CURVES, BUF_BANDS, BUF_GLYPHS, BUF_LAST, BUF_OT, BUF_POINTS, BUF_SHAPE_FONT,
    Bytes, BytesHeader, Font, Frame, GLYPH_MISSING, Glyph, REQ_COVER, REQ_GLYPH, WIRE_MAX_BODY,
};
use caustics_bar_abi::{IMAGE_SLOT_PX, slice_as_bytes};
use caustics_sidecar::inbox::{Inbox, Next};
use punchcutter_fonts::fallback::Chain;
use punchcutter_fonts::{Loaded, Merged};
use punchcutter_shape::OT_TABLES_START;
use punchcutter_text::Builder;

/// The theme search, one copy in `caustics-sidecar` shared with `caustics-apps` and
/// `caustics-tray`.
pub use caustics_sidecar::icons::{FALLBACK_THEMES, is_symbolic, resolve, theme_order};

/// The size the theme search is steered by. It does not bound the answer — a
/// scalable SVG is one file at every size — and it matches the bar's slot size
/// so the two searches land on the same file for a name that turns out to be
/// pixels after all.
pub const ICON_PX: u32 = IMAGE_SLOT_PX;

/// The most bytes one `Bytes` body carries, so a run longer than this is
/// several frames at consecutive offsets. The wire's cap less the body's own
/// header; a multiple of sixteen, which every buffer's element size divides.
pub const CHUNK: usize = WIRE_MAX_BODY - size_of::<BytesHeader>();

/// How often the names this process answered are looked at again
/// (see [`Sidecar::sweep`]): a name no theme had is searched for once more, and
/// a symbolic file whose mtime moved is read again and appended as a new
/// glyph. The same bounded age as `caustics-apps`' and for the same reason — the
/// themes are deep trees a directory watch does not see into, and the
/// remembered misses are a handful of names.
pub const SWEEP: Duration = Duration::from_secs(30);

/// The six buffers in the order the ABI numbers them, which is also the order
/// they are sent in: the four curve buffers, the shape scalars, the arena.
pub const BUFFERS: [u32; BUF_LAST as usize] = [
    BUF_POINTS,
    BUF_BANDS,
    BUF_BAND_CURVES,
    BUF_GLYPHS,
    BUF_SHAPE_FONT,
    BUF_OT,
];

/// The characters the caustic composes into runs of its own: the chart
/// arrows, the degree sign, the dash that stands for no value, and the
/// ellipsis a cut body ends in (`caustics/bar_tunables.slang`'s literals). A
/// label the bar sends whole can be covered by a fallback face when the
/// primary lacks a glyph (docs/architecture.md, *Text*); a run the caustic
/// composes cannot, so the primary face has to carry these itself, and the
/// font is loaded requiring them (`Loaded::load_requiring`). Noto Sans has no
/// arrows, and a bar shaped with it would show two boxes in the network line.
pub const COMPOSED: &[char] = &['\u{2193}', '\u{2191}', '\u{00B0}', '\u{2013}', '\u{2026}'];

/// This sidecar's witness line: `caustics-glyphs: …` on stderr, in **one write**
/// (docs/abi.md, *Writing a sidecar*; the one copy is `caustics_sidecar::Witness`).
pub const WITNESS: caustics_sidecar::Witness = caustics_sidecar::Witness::new("caustics-glyphs");

/// One witness line; see [`WITNESS`].
pub fn witness(line: &str) {
    WITNESS.line(line)
}

/// Where the engine's `tracing` events go: each formatted event, one witness
/// line. The formatter writes an event in one `write_all`, so one event is one
/// line.
struct WitnessWriter;

impl Write for WitnessWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let text = String::from_utf8_lossy(buf);
        let line = text.trim_end_matches('\n');
        if !line.is_empty() {
            witness(line);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Every `tracing` event at `info` and above as a witness line: the message
/// alone, no time, level or target, since the witness prefix already says
/// whose it is and the journal stamps the time.
fn witness_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .without_time()
        .with_level(false)
        .with_target(false)
        .with_ansi(false)
        .with_writer(|| WitnessWriter)
        .try_init();
}

// ---------------------------------------------------------------------------
// The font, as the wire carries it
// ---------------------------------------------------------------------------

/// Icons appended after the font's own glyphs (and after any fallback faces'
/// ranges): every record past the primary's.
pub fn icon_count(loaded: &Loaded) -> u32 {
    loaded.curves.glyphs.len() as u32 - loaded.font_glyphs
}

/// The `Font` frame: what the bar needs in order to size a pixel and label
/// its health line, and nothing about what is in the buffers.
pub fn font_frame(loaded: &Loaded) -> Frame {
    Frame::Font(Font {
        upem: loaded.units_per_em,
        cap_height: loaded.cap_height,
        glyph_count: loaded.font_glyphs,
        icon_base: loaded.font_glyphs,
        family: loaded.family.clone(),
    })
}

/// One buffer's whole contents, as the bytes the GPU holds.
///
/// Borrowed for the curve buffers and the arena — which are already the
/// bytes — and owned only for the sixteen-byte `ShapeFont`.
pub fn buffer(loaded: &Loaded, buffer: u32) -> Option<Cow<'_, [u8]>> {
    let [points, bands, band_curves, glyphs] = loaded.curve_bytes();
    Some(match buffer {
        BUF_POINTS => Cow::Borrowed(points),
        BUF_BANDS => Cow::Borrowed(bands),
        BUF_BAND_CURVES => Cow::Borrowed(band_curves),
        BUF_GLYPHS => Cow::Borrowed(glyphs),
        BUF_SHAPE_FONT => Cow::Owned(caustics_bar_abi::as_bytes(&loaded.shape_font()).to_vec()),
        BUF_OT => Cow::Borrowed(loaded.arena_bytes()),
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// The sidecar's state: the font, what has been sent, and the icons asked for
// ---------------------------------------------------------------------------

/// The four curve buffers, in ABI order, as a `BUF_*` each.
const CURVE_BUFFERS: [u32; 4] = [BUF_POINTS, BUF_BANDS, BUF_BAND_CURVES, BUF_GLYPHS];

/// The font, plus the two things that make a delta possible: how many bytes of
/// each curve buffer the bar has already been told about, and which names have
/// already become glyphs.
pub struct Sidecar {
    /// The font and the fallback chain behind it: the engine's.
    pub fonts: Merged,
    /// Bytes of each curve buffer already sent, in [`CURVE_BUFFERS`] order.
    sent: [usize; 4],
    /// Icon name → its glyph id, or `None` for a name that is not curves, so a
    /// repeat is answered without a second theme search — and the file the
    /// answer was made from, so [`Sidecar::sweep`] can tell a stale one.
    icons: HashMap<String, HeldGlyph>,
    /// Bytes of the arena already sent. Its front block is rewritten whenever a
    /// face is opened and its fill bitmaps whenever a glyph is covered, so a
    /// cover sends those two ranges again and the rest as a tail.
    sent_ot: usize,
    /// Frames and bytes written since the start, for the witness line.
    pub frames: u64,
    pub bytes: u64,
}

impl Sidecar {
    /// `loaded`, with the fallback chain fontconfig gives.
    pub fn new(loaded: Loaded) -> Sidecar {
        Sidecar::of(Merged::new(loaded))
    }

    /// [`Self::new`] with the chain handed in — what the tests use, so a test
    /// names the faces it means instead of whatever the machine has installed.
    pub fn with_chain(loaded: Loaded, fallback: Chain) -> Sidecar {
        Sidecar::of(Merged::with_chain(loaded, fallback))
    }

    fn of(fonts: Merged) -> Sidecar {
        Sidecar {
            fonts,
            sent: [0; 4],
            icons: HashMap::new(),
            sent_ot: 0,
            frames: 0,
            bytes: 0,
        }
    }

    /// The font: the primary's buffers and arena, and whatever covers and
    /// icons have added to them.
    pub fn loaded(&self) -> &Loaded {
        &self.fonts.loaded
    }

    /// The whole font: one `Font`, then every buffer from offset 0.
    pub fn write_font(&mut self, out: &mut impl Write) -> io::Result<()> {
        self.write_frame(out, &font_frame(self.loaded()))?;
        for id in BUFFERS {
            let bytes = match buffer(self.loaded(), id) {
                Some(bytes) => bytes.into_owned(),
                None => continue,
            };
            self.write_run(out, id, 0, &bytes, bytes.len())?;
        }
        self.sent = self.loaded().curve_lengths();
        self.sent_ot = self.loaded().arena_bytes().len();
        // Everything just went out whole, rewritten records included.
        self.fonts.loaded.curves.take_rewritten();
        Ok(())
    }

    /// Answer one `REQ_COVER`: the bytes of a string the bar's shaper could not
    /// draw (docs/abi.md, *Request*).
    ///
    /// [`Merged::cover`] does the work: every codepoint the merged font cannot
    /// draw goes down the chain, the first face that draws one is opened into
    /// the arena, and the glyph and the GSUB closure of what was filled are
    /// filled into the blank records the face reserved. The engine says what
    /// it did in witness lines (`cover "…": …`, `no face has U+…`). What goes
    /// out is the four curve tails, the records filled in place, the arena's
    /// front block, its fill bitmaps and its new tail, and `BUF_SHAPE_FONT`.
    ///
    /// A string with nothing missing writes **nothing**: silence is the answer
    /// to "there was nothing to do", and the bar has already stopped asking.
    pub fn answer_cover(&mut self, out: &mut impl Write, text: &str) -> io::Result<()> {
        if self.fonts.cover(text).glyphs == 0 {
            return Ok(());
        }

        // The curve tails first, then everything that *names* a glyph: a frame
        // between the two would leave the caustic's directory pointing at a
        // record the bar does not have yet, and the wire has no transaction.
        self.write_curve_tails(out)?;
        self.write_arena(out)?;
        let font = buffer(self.loaded(), BUF_SHAPE_FONT)
            .expect("a known buffer")
            .into_owned();
        self.write_run(out, BUF_SHAPE_FONT, 0, &font, font.len())?;
        Ok(())
    }

    /// The arena's changed parts: the front block (rewritten whenever a face is
    /// opened), every open face's fill bitmap, and the tail.
    fn write_arena(&mut self, out: &mut impl Write) -> io::Result<()> {
        let bytes = self.loaded().arena_bytes().to_vec();
        let total = bytes.len();
        self.write_run(
            out,
            BUF_OT,
            0,
            &bytes[..(OT_TABLES_START as usize).min(total)],
            total,
        )?;
        for face in 0..self.loaded().arena.faces().len() {
            let Some((at, len)) = self.loaded().arena.bitmap_range(face) else {
                continue;
            };
            if at + len <= total && at + len <= self.sent_ot {
                self.write_run(out, BUF_OT, at, &bytes[at..at + len], total)?;
            }
        }
        if total > self.sent_ot {
            let from = self.sent_ot;
            self.write_run(out, BUF_OT, from, &bytes[from..], total)?;
        }
        self.sent_ot = total;
        Ok(())
    }

    /// The new tail of each curve buffer, at the offset the bar already has,
    /// then every glyph record filled **in place** below that offset.
    ///
    /// Three buffers only append. `BUF_GLYPHS` does too, except that opening a
    /// face reserves its whole range of blank records and a later cover fills
    /// one where it stands (`Builder::fill_from`): that record is below what
    /// was sent, so no tail carries it, and without it the arena's bitmap says
    /// filled while the bar's record is still the blank and the glyph draws as
    /// nothing. The
    /// records go **after** the tails because a record names bands and curves,
    /// and those arrive in the tails.
    fn write_curve_tails(&mut self, out: &mut impl Write) -> io::Result<()> {
        let now = self.loaded().curve_lengths();
        let before = self.sent;
        for (i, id) in CURVE_BUFFERS.into_iter().enumerate() {
            if now[i] <= before[i] {
                continue;
            }
            let whole = match buffer(self.loaded(), id) {
                Some(bytes) => bytes.into_owned(),
                None => continue,
            };
            let offset = before[i];
            self.write_run(out, id, offset, &whole[offset..now[i]], now[i])?;
        }
        self.sent = now;

        // Indices at or past the old watermark went out in the tail.
        const STRIDE: usize = size_of::<punchcutter_text::Glyph>();
        let below = before[3] / STRIDE;
        let rewritten = self.fonts.loaded.curves.take_rewritten();
        let mut runs: Vec<(usize, usize)> = Vec::new();
        for index in rewritten
            .into_iter()
            .map(|i| i as usize)
            .filter(|&i| i < below)
        {
            match runs.last_mut() {
                Some((first, count)) if *first + *count == index => *count += 1,
                _ => runs.push((index, 1)),
            }
        }
        for (first, count) in runs {
            let (at, end) = (first * STRIDE, (first + count) * STRIDE);
            let bytes = slice_as_bytes(&self.loaded().curves.glyphs)[at..end].to_vec();
            self.write_run(out, BUF_GLYPHS, at, &bytes, now[3])?;
        }
        Ok(())
    }

    /// Answer one `REQ_GLYPH`: the deltas, then the `Glyph`.
    ///
    /// Every request is answered, repeats included — the bar may have restarted
    /// and asked again, and silence is the one thing a caller cannot act on.
    pub fn answer_glyph(&mut self, out: &mut impl Write, name: &str) -> io::Result<()> {
        if let Some(known) = self.icons.get(name) {
            let glyph = known.glyph;
            return self.write_frame(out, &glyph_frame(name, glyph));
        }
        let held = self.append_icon(name);
        let glyph = held.glyph;
        self.icons.insert(name.to_owned(), held);
        if glyph.is_some() {
            // Only the new tail of each curve buffer, at the offset the bar
            // already has, with the buffer's new total. An icon only appends, so its tails carry it; `write_curve_tails` also resends any record a cover filled in place.
            self.write_curve_tails(out)?;
        }
        self.write_frame(out, &glyph_frame(name, glyph))
    }

    /// Look again at every name answered and write a
    /// new `Glyph` — with its curve tails first — for each whose answer
    /// changed. Unasked: the bar replaces the answer it holds for the name,
    /// whether it was `GLYPH_MISSING` or an id the file has since moved under.
    ///
    /// Looked at again, and only that: a name whose search found no file
    /// (one theme walk), and a file whose mtime moved or that went away (one
    /// `stat`). A name that resolved to pixels, or to an SVG that would not
    /// parse, is not re-read until its file changes — so a broken file costs
    /// one parse, not one per sweep.
    pub fn sweep(&mut self, out: &mut impl Write) -> io::Result<usize> {
        let mut names: Vec<String> = self.icons.keys().cloned().collect();
        names.sort();
        let mut changed = 0;
        for name in names {
            let Some(held) = self.icons.get(&name) else {
                continue;
            };
            let stale = match &held.file {
                Some((path, when)) => mtime(path).is_none() || mtime(path) != *when,
                None => resolve(&name, ICON_PX).is_some(),
            };
            if !stale {
                continue;
            }
            let was = held.glyph;
            let fresh = self.append_icon(&name);
            let glyph = fresh.glyph;
            self.icons.insert(name.clone(), fresh);
            if glyph == was {
                continue;
            }
            witness(&format!("{name} changed: {was:?} -> {glyph:?}"));
            if glyph.is_some() {
                self.write_curve_tails(out)?;
            }
            self.write_frame(out, &glyph_frame(&name, glyph))?;
            changed += 1;
        }
        Ok(changed)
    }

    /// The theme search and the parse: a name in, a glyph id or `None` out,
    /// and the file it was read from.
    fn append_icon(&mut self, name: &str) -> HeldGlyph {
        let Some((path, theme)) = resolve(name, ICON_PX) else {
            return HeldGlyph {
                glyph: None,
                file: None,
            };
        };
        let file = Some((path.clone(), mtime(&path)));
        if !is_symbolic(name, &path) {
            witness(&format!(
                "{name} in {theme} is not symbolic ({}); pixels",
                path.display()
            ));
            return HeldGlyph { glyph: None, file };
        }
        let fonts = &mut self.fonts.loaded;
        let mut builder = Builder::resume(std::mem::take(&mut fonts.curves));
        let loaded = punchcutter_text::icon::load_file(&mut builder, &path);
        fonts.curves = builder.finish();
        // An icon is one more merged glyph id, so the watermark moves with it
        // and the next face a cover opens takes a base past it.
        let glyphs = fonts.curves.glyphs.len() as u32;
        fonts.arena.set_glyph_count(glyphs);
        let glyph = match loaded {
            Ok(icon) => {
                if !icon.report.clean() {
                    // One witness, not a special case: Adwaita's one stroked
                    // icon is drawn without its slash and said so.
                    witness(&format!(
                        "{name} is not all filled paths (stroked {}, unfilled {}, gradients {}, \
                         mixed rules {}); drawing what is filled",
                        icon.report.stroked_paths,
                        icon.report.unfilled_paths,
                        icon.report.non_solid_fills,
                        icon.report.mixed_fill_rules
                    ));
                }
                Some(icon.glyph)
            }
            Err(err) => {
                witness(&format!("{name} is not curves ({err:#}); pixels"));
                None
            }
        };
        HeldGlyph { glyph, file }
    }

    /// One run of bytes for one buffer, chunked so no body passes the wire's
    /// cap. Every chunk carries the same `total`, so the bar sizes the GPU
    /// buffer on the first one.
    fn write_run(
        &mut self,
        out: &mut impl Write,
        buffer: u32,
        offset: usize,
        bytes: &[u8],
        total: usize,
    ) -> io::Result<()> {
        if bytes.is_empty() {
            // An empty run says nothing the bar does not already assume.
            return Ok(());
        }
        for (i, chunk) in bytes.chunks(CHUNK).enumerate() {
            let frame = Frame::Bytes(Bytes {
                buffer,
                offset: (offset + i * CHUNK) as u32,
                total: total as u32,
                bytes: chunk.to_vec(),
            });
            self.write_frame(out, &frame)?;
        }
        Ok(())
    }

    fn write_frame(&mut self, out: &mut impl Write, frame: &Frame) -> io::Result<()> {
        wire::write(out, frame)?;
        self.frames += 1;
        if let Frame::Bytes(bytes) = frame {
            self.bytes += bytes.bytes.len() as u64;
        }
        Ok(())
    }

    /// What the sidecar has resolved so far, for the tests.
    pub fn known(&self, name: &str) -> Option<Option<u32>> {
        self.icons.get(name).map(|held| held.glyph)
    }
}

/// One name's answer, and the file it was made from (`None`: no theme had the
/// name) with that file's mtime then.
#[derive(Clone, Debug)]
struct HeldGlyph {
    glyph: Option<u32>,
    file: Option<(PathBuf, Option<SystemTime>)>,
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

fn glyph_frame(name: &str, glyph: Option<u32>) -> Frame {
    match glyph {
        Some(gid) => Frame::Glyph(Glyph {
            gid,
            flags: 0,
            name: name.to_owned(),
        }),
        None => Frame::Glyph(Glyph {
            gid: 0,
            flags: GLYPH_MISSING,
            name: name.to_owned(),
        }),
    }
}

// ---------------------------------------------------------------------------
// The run
// ---------------------------------------------------------------------------

/// The whole program. Returns the process's exit status.
///
/// This sidecar writes the font once and then answers what it is asked — and,
/// every [`SWEEP`], looks again at the icon names it
/// answered, so stdin is read on a thread beside the loop
/// (`caustics_sidecar::inbox`). `wire::read`'s `Ok(None)` is EOF, which is the bar
/// being gone, and the exit is 0.
pub fn run() -> i32 {
    witness_tracing();
    let loaded = match Loaded::load_requiring(COMPOSED) {
        Ok(loaded) => loaded,
        Err(why) => {
            witness(&why);
            return 1;
        }
    };
    witness(&format!(
        "{} ({}) {} glyphs at {} upem, cap {:.3} em, curves {}, {} ms",
        loaded.family,
        loaded.path.display(),
        loaded.font_glyphs,
        loaded.units_per_em,
        loaded.cap_height,
        loaded.start(),
        loaded.load_ms
    ));
    // The arena, by table. A subtable format the caustic will not walk is the *caustic's* witness (docs/abi.md, *The witness ring*); this side reads nothing to have an opinion about.
    witness(&format!(
        "arena: {} — {} KiB",
        loaded.arena.faces()[0].table_sizes(),
        loaded.arena_bytes().len().div_ceil(1024),
    ));

    let mut sidecar = Sidecar::new(loaded);
    let stdout = io::stdout();
    let mut out = stdout.lock();
    if let Err(err) = sidecar.write_font(&mut out) {
        return broken(&err, "the font");
    }
    witness(&format!(
        "font sent: {} frames, {} KiB over the wire",
        sidecar.frames,
        sidecar.bytes / 1024
    ));

    // The read on a thread of its own and a clock beside it: a request is
    // answered as it comes, and every `SWEEP` the names
    // answered so far are looked at again.
    let stdin = io::stdin();
    let mut failed = false;
    let mut inbox = Inbox::spawn(SWEEP, move || {
        if failed {
            return None;
        }
        let read = wire::read(&mut stdin.lock());
        failed = read.is_err();
        read.transpose()
    });
    loop {
        let next = match inbox.next() {
            Next::Closed => Ok(None),
            Next::Tick => {
                if let Err(err) = sidecar.sweep(&mut out) {
                    return broken(&err, "a changed icon");
                }
                continue;
            }
            Next::Item(read) => read.map(Some),
        };
        match next {
            Ok(None) => {
                witness("stdin closed; exiting");
                return 0;
            }
            Ok(Some(Frame::Request(request))) if request.kind == REQ_GLYPH => {
                if let Err(err) = sidecar.answer_glyph(&mut out, &request.name) {
                    return broken(&err, &request.name);
                }
            }
            // A cover: the `name` that is not a name — the bytes of a run the bar's
            // shaper could not draw.
            Ok(Some(Frame::Request(request))) if request.kind == REQ_COVER => {
                if let Err(err) = sidecar.answer_cover(&mut out, &request.name) {
                    return broken(&err, "a cover");
                }
            }
            Ok(Some(Frame::Request(request))) => witness(&format!(
                "request kind {} is not this sidecar's",
                request.kind
            )),
            Ok(Some(Frame::Skipped(skipped))) => witness(&format!("stdin: {skipped}")),
            Ok(Some(other)) => witness(&format!(
                "stdin: frame kind {} has no meaning here",
                other.kind()
            )),
            Err(err) => {
                witness(&format!("stdin: {err}"));
                return 1;
            }
        }
    }
}

/// A write that failed: a closed pipe is the bar leaving and is not an error.
fn broken(err: &io::Error, what: &str) -> i32 {
    if err.kind() == io::ErrorKind::BrokenPipe {
        witness("stdout closed; exiting");
        return 0;
    }
    witness(&format!("writing {what} failed: {err}"));
    1
}

#[cfg(test)]
mod tests;
