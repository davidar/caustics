//! `caustics-glyphs` — the curves sidecar.
//!
//! Everything Slug fills, filled outside the bar: the font's Bézier curves and
//! band tables, the **arena** of verbatim OpenType tables, and every symbolic
//! icon appended to the same glyph-id space. It writes one `Font` frame and
//! then a run of `Bytes` frames — one per chunk, six buffers — and the bar
//! copies each run into the GPU buffer the frame names without reading a byte
//! of it (docs/abi.md, *Bytes*–*Glyph*).
//!
//! The shaping data is one buffer, not a set of flattened tables. What this
//! process sends for `BUF_OT` is `cmap`, `hmtx`, `GDEF`, `GSUB` and `GPOS` sliced out
//! of the font file **unread**, behind a directory saying where each landed;
//! `caustics/ot.slang` walks OpenType's own format on the other side, so a new
//! lookup type is a shader edit and this side never flattens a table.
//!
//! Why a sidecar at all: not a decision that has to be out of process, but
//! *two hundred milliseconds of computation* that would otherwise sit on the
//! bar's startup path. It links ttf-parser, usvg and a cache; none of that
//! belongs in the address space of a process whose job is to hold a Wayland
//! surface and dispatch a caustic. The bar links neither `caustics-text` nor a font
//! parser (docs/architecture.md, *Sidecars*).
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
//! shaper flagged `.notdef` (docs/architecture.md, *Text*, "Fallback faces"; the rest of
//! shaping is on the GPU). The codepoints the merged font cannot draw go down the
//! [`fallback`] chain, and the **first face that draws one is opened into the
//! arena**: it takes its whole glyph range in the merged space, so its own
//! `GSUB` and `GPOS` index the merged ids by one addition and a Hebrew word
//! gets Droid Sans Hebrew's kerning. The records in that range are blanks until
//! a glyph is covered, and a per-face bitmap says which are filled, so the
//! shaper can flag a glyph whose curves have not arrived and ask again.
//!
//! What goes back is the four curve tails, the arena's front block, the new
//! face's table bytes and the fill bitmap, and `BUF_SHAPE_FONT` — deltas, not
//! the arena again.
//!
//! On a `Request` of kind `REQ_GLYPH`: the theme search (`caustics_sidecar::icons`,
//! shared with `caustics-apps` and `caustics-tray`), then, for a
//! symbolic SVG, [`caustics_text::icon::load_file`] into this process's own
//! [`Builder`] and **only the new tail** of each of the four curve buffers back
//! as `Bytes`, followed by a `Glyph` naming the id. A name no theme has, or one
//! that is not curves, is `Glyph { GLYPH_MISSING }` — the bar then takes its
//! pixels path. A repeat is answered from the map without appending, because
//! the bar may have restarted and asked again.
//!
//! # The cache
//!
//! One, for the curves only. The OpenType tables are not cached: slicing five
//! tables out of a directory costs 60 µs and there is nothing worth keeping.
//! The curve cache lives under
//! `$XDG_CACHE_HOME/caustics/glyphs/`, keyed on the FNV-1a of the font file plus
//! its length and the layout version, so a new font and a changed layout both
//! miss. State, not config: a missing or unreadable cache is a rebuild and one
//! witness.
//!
//! # Its own settings, never the bar's
//!
//! `$CAUSTICS_FONT` names a font file (or a fontconfig pattern) to use instead of
//! whatever `fc-match` picks. The bar does not set it, does not read it and has
//! no opinion about it — the same rule `$CAUSTICS_SYSMON_CHARTS` follows
//! (docs/architecture.md, *Sidecars*).

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use caustics_bar_abi::wire::{
    self, BUF_BAND_CURVES, BUF_BANDS, BUF_GLYPHS, BUF_LAST, BUF_OT, BUF_POINTS, BUF_SHAPE_FONT,
    Bytes, BytesHeader, Font, Frame, GLYPH_MISSING, Glyph, REQ_COVER, REQ_GLYPH, WIRE_MAX_BODY,
};
use caustics_bar_abi::{IMAGE_SLOT_PX, slice_as_bytes};
use caustics_shape::{Arena, OT_TABLES_START};
use caustics_sidecar::inbox::{Inbox, Next};
use caustics_text::{Builder, CurveSet};

use crate::fallback::Chain;

pub mod fallback;

/// The sidecar's one knob: a font file, or a fontconfig pattern, to use instead
/// of [`pick_family`]'s search. Read here and nowhere else.
pub const FONT_ENV: &str = "CAUSTICS_FONT";

/// UI families to try, best first, asked of fontconfig. Fedora ships Adwaita Sans and
/// Cantarell; Noto and DejaVu are the safety net, and `sans-serif` below them
/// is whatever the machine has.
pub const UI_FAMILIES: &[&str] = &[
    "Adwaita Sans",
    "Cantarell",
    "Inter",
    "Noto Sans",
    "DejaVu Sans",
    "Liberation Sans",
];

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

/// This sidecar's witness line: `caustics-glyphs: …` on stderr, in **one write**
/// (docs/abi.md, *Writing a sidecar*; the one copy is `caustics_sidecar::Witness`).
pub const WITNESS: caustics_sidecar::Witness = caustics_sidecar::Witness::new("caustics-glyphs");

/// One witness line; see [`WITNESS`].
pub fn witness(line: &str) {
    WITNESS.line(line)
}

// ---------------------------------------------------------------------------
// The font
// ---------------------------------------------------------------------------

/// A font, as the six buffers plus the facts the `Font` frame carries.
pub struct Loaded {
    /// The family fontconfig gave us, for the witness line and the frame.
    pub family: String,
    /// The file it came from.
    pub path: PathBuf,
    /// The curve buffers: the font's glyphs, then any icons appended.
    pub curves: CurveSet,
    /// `head.unitsPerEm`.
    pub units_per_em: u32,
    /// Glyphs in the face. Anything at or above this is an icon.
    pub font_glyphs: u32,
    /// `OS/2.sCapHeight` in em units — `Params.cap_height` on the other side.
    pub cap_height: f32,
    /// The arena: the primary's five tables verbatim, plus every fallback face
    /// a cover has opened.
    pub arena: Arena,
    /// The primary's file, kept so a cover can ask whether it already draws a
    /// codepoint. That is a face probe — the same question
    /// [`caustics_text::font::codepoint_curves`] asks of every face in the chain —
    /// and not a table this side interprets.
    pub data: Vec<u8>,
    /// Whether the curve buffers came out of the cache file.
    pub warm: bool,
    /// How long the load took, ms — cold or warm, whichever this was.
    pub load_ms: u128,
}

impl Loaded {
    /// Find a font and load it: `$CAUSTICS_FONT` if it names one, else the first of
    /// [`UI_FAMILIES`] fontconfig actually has.
    pub fn load() -> Result<Loaded, String> {
        let (family, path) = match std::env::var_os(FONT_ENV) {
            Some(value) if !value.is_empty() => {
                let asked = PathBuf::from(&value);
                if asked.is_file() {
                    let family = asked
                        .file_stem()
                        .map(|stem| stem.to_string_lossy().into_owned())
                        .unwrap_or_else(|| asked.display().to_string());
                    witness(&format!("${FONT_ENV} names {}", asked.display()));
                    (family, asked)
                } else {
                    let pattern = value.to_string_lossy().into_owned();
                    let found = fc_match(&pattern).ok_or_else(|| {
                        format!("${FONT_ENV}={pattern} is neither a file nor a font fontconfig has")
                    })?;
                    witness(&format!("${FONT_ENV}={pattern} matched {}", found.0));
                    found
                }
            }
            _ => pick_family().ok_or_else(|| {
                "fontconfig named no usable sans-serif font; there is nothing to shape with"
                    .to_owned()
            })?,
        };
        Loaded::load_path(&family, &path)
    }

    /// [`Self::load`] for a font already chosen — what the tests use.
    pub fn load_path(family: &str, path: &Path) -> Result<Loaded, String> {
        let started = std::time::Instant::now();
        let data = std::fs::read(path)
            .map_err(|err| format!("could not read the font at {}: {err}", path.display()))?;
        let dir = cache_dir();
        let mut arena = Arena::of(&data, 0)
            .map_err(|err| format!("{} has no usable tables: {err}", path.display()))?;

        let key = cache_key(&dir, &data);
        let (cached, warm) = match read_cache(&key) {
            Some(cached) => (cached, true),
            None => {
                let font = caustics_text::Font::parse(&data, 0).map_err(|err| {
                    format!("could not build curves for {}: {err:#}", path.display())
                })?;
                let cached = caustics_text::Cached {
                    curves: font.curves,
                    units_per_em: font.units_per_em,
                    glyph_count: font.glyph_count,
                    cap_height: font.cap_height,
                };
                write_cache(&key, &cached);
                (cached, false)
            }
        };

        // The merged glyph space starts as the curve records: the primary's
        // glyphs, and after them whatever icons and faces arrive. The arena's
        // watermark follows it, so a face opened later takes a base past them.
        arena.set_glyph_count(cached.curves.glyphs.len() as u32);
        Ok(Loaded {
            family: family.to_owned(),
            path: path.to_path_buf(),
            units_per_em: cached.units_per_em as u32,
            font_glyphs: cached.glyph_count,
            cap_height: cached.cap_height,
            curves: cached.curves,
            arena,
            data,
            warm,
            load_ms: started.elapsed().as_millis(),
        })
    }

    /// `"warm"` or `"cold"`: whether the curve buffers came out of the cache.
    pub fn start(&self) -> &'static str {
        if self.warm { "warm" } else { "cold" }
    }

    /// Icons appended after the font's own glyphs.
    pub fn icon_count(&self) -> u32 {
        self.curves.glyphs.len() as u32 - self.font_glyphs
    }

    /// The `Font` frame: what the bar needs in order to size a pixel and label
    /// its health line, and nothing about what is in the buffers.
    pub fn font_frame(&self) -> Frame {
        Frame::Font(Font {
            upem: self.units_per_em,
            cap_height: self.cap_height,
            glyph_count: self.font_glyphs,
            icon_base: self.font_glyphs,
            family: self.family.clone(),
        })
    }

    /// One buffer's whole contents, as the bytes the GPU holds.
    ///
    /// Borrowed for the curve buffers and the arena — which are already the
    /// bytes — and owned only for the sixteen-byte `ShapeFont`.
    pub fn buffer(&self, buffer: u32) -> Option<Cow<'_, [u8]>> {
        let set = &self.curves;
        Some(match buffer {
            BUF_POINTS => Cow::Borrowed(slice_as_bytes(&set.points)),
            BUF_BANDS => Cow::Borrowed(slice_as_bytes(&set.bands)),
            BUF_BAND_CURVES => Cow::Borrowed(slice_as_bytes(&set.band_curves)),
            BUF_GLYPHS => Cow::Borrowed(slice_as_bytes(&set.glyphs)),
            BUF_SHAPE_FONT => Cow::Owned(caustics_bar_abi::as_bytes(&self.arena.font()).to_vec()),
            BUF_OT => Cow::Borrowed(self.arena.bytes()),
            _ => return None,
        })
    }

    /// Whether the **primary** face draws a codepoint — the question a cover
    /// asks before it walks the chain.
    pub fn primary_draws(&self, ch: char) -> bool {
        matches!(
            caustics_text::font::codepoint_curves(&self.data, 0, ch),
            Ok(Some(found)) if found.gid != 0
        )
    }

    /// The four curve buffers' live lengths in bytes, in ABI order — what an
    /// icon append is a delta against.
    pub fn curve_lengths(&self) -> [usize; 4] {
        [
            size_of_val(self.curves.points.as_slice()),
            size_of_val(self.curves.bands.as_slice()),
            size_of_val(self.curves.band_curves.as_slice()),
            size_of_val(self.curves.glyphs.as_slice()),
        ]
    }
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
    pub loaded: Loaded,
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
    /// The fallback chain and its per-face caches (see [`fallback`]).
    fallback: Chain,
    /// Codepoints this process has already decided about: in the primary, newly
    /// appended, or nothing in the chain can draw them. A repeated title costs
    /// one pass over its characters and no font is opened twice.
    seen: HashSet<u32>,
    /// Frames and bytes written since the start, for the witness line.
    pub frames: u64,
    pub bytes: u64,
}

impl Sidecar {
    pub fn new(loaded: Loaded) -> Sidecar {
        let chain = Chain::discover(&loaded.path);
        Sidecar::with_chain(loaded, chain)
    }

    /// [`Self::new`] with the chain handed in — what the tests use, so a test
    /// names the faces it means instead of whatever the machine has installed.
    pub fn with_chain(loaded: Loaded, fallback: Chain) -> Sidecar {
        Sidecar {
            loaded,
            sent: [0; 4],
            icons: HashMap::new(),
            sent_ot: 0,
            fallback,
            seen: HashSet::new(),
            frames: 0,
            bytes: 0,
        }
    }

    /// The whole font: one `Font`, then every buffer from offset 0.
    pub fn write_font(&mut self, out: &mut impl Write) -> io::Result<()> {
        self.write_frame(out, &self.loaded.font_frame())?;
        for buffer in BUFFERS {
            let bytes = match self.loaded.buffer(buffer) {
                Some(bytes) => bytes.into_owned(),
                None => continue,
            };
            self.write_run(out, buffer, 0, &bytes, bytes.len())?;
        }
        self.sent = self.loaded.curve_lengths();
        self.sent_ot = self.loaded.arena.bytes().len();
        // Everything just went out whole, rewritten records included.
        self.loaded.curves.take_rewritten();
        Ok(())
    }

    /// Answer one `REQ_COVER`: the bytes of a string the bar's shaper could not
    /// draw (docs/abi.md, *Request*).
    ///
    /// Every codepoint the merged font cannot draw goes down the chain. The
    /// first face that draws one is **opened into the arena** — taking its
    /// whole glyph range in the merged space — and the glyph is filled into the
    /// blank record its face id names. What goes out is the four curve tails,
    /// the arena's front block and its new tail, the fill bitmaps, and
    /// `BUF_SHAPE_FONT`.
    ///
    /// A string with nothing missing writes **nothing**: silence is the answer
    /// to "there was nothing to do", and the bar has already stopped asking.
    pub fn answer_cover(&mut self, out: &mut impl Write, text: &str) -> io::Result<()> {
        // Family → (codepoints, glyphs appended, bytes, all of them cached).
        let mut tally: Vec<(String, u32, u32, usize, bool)> = Vec::new();
        let mut appended = 0u32;

        for ch in text.chars() {
            let codepoint = ch as u32;
            if !self.seen.insert(codepoint) {
                continue;
            }
            // A codepoint the primary already draws is not this sidecar's
            // business, and neither is one an earlier cover already filled.
            if self.loaded.primary_draws(ch) {
                continue;
            }
            let Some(covered) =
                self.fallback
                    .cover(&mut self.loaded.curves, &mut self.loaded.arena, ch)
            else {
                continue;
            };
            appended += 1;
            match tally
                .iter_mut()
                .find(|(family, ..)| *family == covered.family)
            {
                Some(row) => {
                    row.1 += 1;
                    row.2 += 1;
                    row.3 += covered.bytes;
                    row.4 &= covered.warm;
                }
                None => tally.push((covered.family, 1, 1, covered.bytes, covered.warm)),
            }
        }
        // The glyphs the covered faces' own substitutions reach: a ligature, a
        // joining form, a `ccmp` output (see `fallback`, *The closure*). Filled in the same
        // answer, so the shaper that flagged the string never meets a reserved
        // record nothing filled.
        for closed in self
            .fallback
            .close(&mut self.loaded.curves, &mut self.loaded.arena)
        {
            witness(&format!(
                "cover {:?}: closure of {} +{} glyphs, {} KiB{} ({}/{} lookups rooted, {} rounds, {} steps, {:.1} ms{})",
                text,
                closed.family,
                closed.glyphs,
                closed.bytes.div_ceil(1024),
                if closed.warm && closed.glyphs > 0 {
                    ", cached"
                } else {
                    ""
                },
                closed.roots,
                closed.lookups,
                closed.rounds,
                closed.steps,
                closed.micros as f64 / 1000.0,
                match closed.bound {
                    Some(bound) => format!(", stopped at the {} bound", bound.name()),
                    None => String::new(),
                },
            ));
            appended += closed.glyphs;
        }
        self.fallback.settle();

        for (family, codepoints, glyphs, bytes, warm) in &tally {
            witness(&format!(
                "cover {:?}: {codepoints} codepoints from {family} (+{glyphs} glyphs, {} KiB{})",
                text,
                bytes.div_ceil(1024),
                if *warm { ", cached" } else { "" }
            ));
        }
        if appended == 0 {
            return Ok(());
        }

        // The curve tails first, then everything that *names* a glyph: a frame
        // between the two would leave the caustic's directory pointing at a
        // record the bar does not have yet, and the wire has no transaction.
        self.write_curve_tails(out)?;
        self.write_arena(out)?;
        let font = self
            .loaded
            .buffer(BUF_SHAPE_FONT)
            .expect("a known buffer")
            .into_owned();
        self.write_run(out, BUF_SHAPE_FONT, 0, &font, font.len())?;
        Ok(())
    }

    /// The arena's changed parts: the front block (rewritten whenever a face is
    /// opened), every open face's fill bitmap, and the tail.
    fn write_arena(&mut self, out: &mut impl Write) -> io::Result<()> {
        let bytes = self.loaded.arena.bytes().to_vec();
        let total = bytes.len();
        self.write_run(
            out,
            BUF_OT,
            0,
            &bytes[..(OT_TABLES_START as usize).min(total)],
            total,
        )?;
        for face in 0..self.loaded.arena.faces().len() {
            let Some((at, len)) = self.loaded.arena.bitmap_range(face) else {
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
        let now = self.loaded.curve_lengths();
        let before = self.sent;
        for (i, buffer) in CURVE_BUFFERS.into_iter().enumerate() {
            if now[i] <= before[i] {
                continue;
            }
            let whole = match self.loaded.buffer(buffer) {
                Some(bytes) => bytes.into_owned(),
                None => continue,
            };
            let offset = before[i];
            self.write_run(out, buffer, offset, &whole[offset..now[i]], now[i])?;
        }
        self.sent = now;

        // Indices at or past the old watermark went out in the tail.
        const STRIDE: usize = size_of::<caustics_text::Glyph>();
        let below = before[3] / STRIDE;
        let rewritten = self.loaded.curves.take_rewritten();
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
            let bytes = slice_as_bytes(&self.loaded.curves.glyphs)[at..end].to_vec();
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
        let mut builder = Builder::resume(std::mem::take(&mut self.loaded.curves));
        let loaded = caustics_text::icon::load_file(&mut builder, &path);
        self.loaded.curves = builder.finish();
        // An icon is one more merged glyph id, so the watermark moves with it
        // and the next face a cover opens takes a base past it.
        let glyphs = self.loaded.curves.glyphs.len() as u32;
        self.loaded.arena.set_glyph_count(glyphs);
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
// fontconfig, and the curve cache
// ---------------------------------------------------------------------------

/// The characters the caustic composes into runs of its own: the chart
/// arrows, the degree sign, the dash that stands for no value, and the
/// ellipsis a cut body ends in (`caustics/bar_tunables.slang`'s literals). A
/// label the bar sends whole can be covered by a fallback face when the
/// primary lacks a glyph (docs/architecture.md, *Text*); a run the caustic
/// composes cannot, so the primary face has to carry these itself. Noto Sans
/// has no arrows, and a bar shaped with it would show two boxes in the
/// network line.
pub const COMPOSED_CHARS: &[char] = &['\u{2193}', '\u{2191}', '\u{00B0}', '\u{2013}', '\u{2026}'];

/// A fontconfig pattern for `family` that also asks for [`COMPOSED_CHARS`]:
/// `fc-match` then answers a *different* family when this one lacks them.
fn pattern_with_composed(family: &str) -> String {
    let charset: Vec<String> = COMPOSED_CHARS
        .iter()
        .map(|c| format!("{:x}", *c as u32))
        .collect();
    format!("{family}:charset={}", charset.join(" "))
}

/// The first of [`UI_FAMILIES`] fontconfig actually has that carries
/// [`COMPOSED_CHARS`], else whatever it calls `sans-serif` with them.
///
/// `fc-match` always answers *something*, so a requested family counts as
/// installed only when the family it names back contains what was asked for;
/// an installed family that fails the charset comes back as another family,
/// and one witness says which was passed over and why.
pub fn pick_family() -> Option<(String, PathBuf)> {
    for candidate in UI_FAMILIES {
        let wanted = candidate.to_ascii_lowercase();
        let Some((installed, _)) = fc_match(candidate) else {
            continue;
        };
        if !installed.to_ascii_lowercase().contains(&wanted) {
            continue;
        }
        if let Some((family, path)) = fc_match(&pattern_with_composed(candidate))
            && family.to_ascii_lowercase().contains(&wanted)
        {
            return Some((family, path));
        }
        witness(&format!(
            "{installed} is installed but lacks a glyph the caustic composes ({}); passed over",
            COMPOSED_CHARS.iter().collect::<String>()
        ));
    }
    let (family, path) = fc_match(&pattern_with_composed("sans-serif"))?;
    witness(&format!(
        "none of {UI_FAMILIES:?} is installed with every glyph the caustic composes; falling back to {family} ({})",
        path.display()
    ));
    Some((family, path))
}

/// One `fc-match`: the family fontconfig chose and the file it lives in.
fn fc_match(pattern: &str) -> Option<(String, PathBuf)> {
    let output = std::process::Command::new("fc-match")
        .args(["--format=%{family}\t%{file}", pattern])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let answer = String::from_utf8(output.stdout).ok()?;
    let (family, file) = answer.split_once('\t')?;
    let file = file.trim();
    if file.is_empty() {
        return None;
    }
    // A family list comes back comma-separated ("Noto Sans,Noto Sans Regular").
    let family = family.split(',').next().unwrap_or(family).trim();
    Some((family.to_owned(), PathBuf::from(file)))
}

/// `$XDG_CACHE_HOME/caustics/glyphs`, or `~/.cache/caustics/glyphs`.
///
/// Owned by this process, with the code that fills it: the bar caches nothing
/// about a font.
pub fn cache_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("caustics").join("glyphs")
}

/// The cache file name for a font's bytes: a content hash and the layout
/// version, so a font replaced in place and a layout change both miss.
///
/// FNV-1a over the whole file. Not a cryptographic digest and not meant to be:
/// the file it names is derived data this process wrote, and the only question
/// asked of the name is whether these are the same bytes as last time.
pub fn cache_key(dir: &Path, font: &[u8]) -> PathBuf {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in font {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    dir.join(format!(
        "{hash:016x}-{}-{}.curves",
        font.len(),
        caustics_text::CACHE_VERSION
    ))
}

/// The cached buffers, or `None` for every way that can fail.
fn read_cache(path: &Path) -> Option<caustics_text::Cached> {
    let bytes = std::fs::read(path).ok()?;
    let cached = caustics_text::Cached::from_bytes(&bytes);
    if cached.is_none() {
        witness(&format!(
            "{} is not a cache this build reads; rebuilding",
            path.display()
        ));
    }
    cached
}

/// Write the cache through a temporary file in the same directory, so a killed
/// sidecar cannot leave a half-written one behind. Failure is a witness.
fn write_cache(path: &Path, cached: &caustics_text::Cached) {
    let write = || -> io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let temp = path.with_extension("curves.new");
        std::fs::write(&temp, cached.to_bytes())?;
        std::fs::rename(&temp, path)
    };
    match write() {
        Ok(()) => witness(&format!("cached the curves at {}", path.display())),
        Err(err) => witness(&format!(
            "could not cache the curves at {}: {err}",
            path.display()
        )),
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
    let loaded = match Loaded::load() {
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
        loaded.arena.bytes().len().div_ceil(1024),
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
