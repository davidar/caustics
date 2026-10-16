//! What the sidecar promises the bar (docs/abi.md, *Bytes*–*Request*): the font
//! arrives as one `Font` and then contiguous `Bytes` runs that reassemble to
//! exactly the buffers it holds, and an icon arrives as a **tail** of each
//! curve buffer and one `Glyph`.
//!
//! Every test is skipped rather than failed on a machine with no font or no
//! icon theme: a build container without fontconfig is not a regression.

use super::*;

use punchcutter_fonts::fallback::Chain;

/// The font, loaded once for the whole binary — a cold load is 200 ms and
/// there is no reason to pay it per test.
fn loaded() -> Option<Loaded> {
    let (family, path) = punchcutter_fonts::pick_family(COMPOSED)?;
    match Loaded::load_path(&family, &path) {
        Ok(loaded) => Some(loaded),
        Err(why) => {
            witness(&format!("tests: {why}"));
            None
        }
    }
}

/// Every frame in a byte stream, decoded.
fn frames(bytes: &[u8]) -> Vec<Frame> {
    let mut input = bytes;
    let mut out = Vec::new();
    while let Some(frame) = wire::read(&mut input).expect("a stream this file wrote") {
        out.push(frame);
    }
    out
}

#[test]
fn every_buffer_the_abi_names_has_bytes() {
    let Some(loaded) = loaded() else { return };
    for buffer in BUFFERS {
        let bytes = crate::buffer(&loaded, buffer)
            .unwrap_or_else(|| panic!("buffer {buffer} is not known"));
        // Every one of the six is non-empty for a real font: the arena always
        // carries at least a `cmap` and an `hmtx` behind its directory.
        if matches!(
            buffer,
            BUF_POINTS | BUF_BANDS | BUF_BAND_CURVES | BUF_GLYPHS | BUF_OT
        ) {
            assert!(!bytes.is_empty(), "buffer {buffer} is empty");
        }
        assert_eq!(
            bytes.len() % 4,
            0,
            "buffer {buffer} is not a whole number of words"
        );
    }
    assert_eq!(
        crate::buffer(&loaded, BUF_SHAPE_FONT)
            .expect("the shape scalars")
            .len(),
        size_of::<punchcutter_shape::abi::ShapeFont>(),
        "BUF_SHAPE_FONT is the `font` half of ShapeParams and nothing else"
    );
    assert!(crate::buffer(&loaded, 0).is_none() && crate::buffer(&loaded, BUF_LAST + 1).is_none());
}

#[test]
fn the_font_goes_out_as_one_font_frame_then_contiguous_runs() {
    let Some(loaded) = loaded() else { return };
    let mut wire_bytes: Vec<u8> = Vec::new();
    let mut sidecar = Sidecar::new(loaded);
    sidecar
        .write_font(&mut wire_bytes)
        .expect("writing to a Vec");

    let decoded = frames(&wire_bytes);
    let Some(Frame::Font(font)) = decoded.first() else {
        panic!("the first frame is the Font, so the bar can size a pixel before any bytes land")
    };
    assert!(font.upem > 0 && font.cap_height > 0.0);
    assert_eq!(
        font.icon_base, font.glyph_count,
        "no icons have been appended yet"
    );
    assert_eq!(font.family, sidecar.loaded().family);

    // Reassemble every run and compare it with what the sidecar holds.
    let mut rebuilt: HashMap<u32, Vec<u8>> = HashMap::new();
    let mut order: Vec<u32> = Vec::new();
    for frame in &decoded[1..] {
        let Frame::Bytes(bytes) = frame else {
            panic!("only Bytes follow the Font")
        };
        let slot = rebuilt.entry(bytes.buffer).or_default();
        if slot.is_empty() {
            order.push(bytes.buffer);
            slot.resize(bytes.total as usize, 0);
        }
        assert_eq!(
            slot.len(),
            bytes.total as usize,
            "every chunk carries the same total"
        );
        let at = bytes.offset as usize;
        assert!(
            bytes.bytes.len() <= CHUNK,
            "a chunk over the wire's cap would not have written"
        );
        slot[at..at + bytes.bytes.len()].copy_from_slice(&bytes.bytes);
    }
    for buffer in BUFFERS {
        let want = crate::buffer(sidecar.loaded(), buffer).expect("a known buffer");
        if want.is_empty() {
            assert!(
                !rebuilt.contains_key(&buffer),
                "an empty buffer says nothing"
            );
            continue;
        }
        assert_eq!(
            rebuilt.get(&buffer).map(Vec::as_slice),
            Some(&*want),
            "buffer {buffer}"
        );
    }
    // The order the ABI fixes: the Font, the four curve buffers, the shape scalars, then the arena — skipping whatever this font has none of.
    let expected: Vec<u32> = BUFFERS
        .into_iter()
        .filter(|b| {
            !crate::buffer(sidecar.loaded(), *b)
                .expect("known")
                .is_empty()
        })
        .collect();
    assert_eq!(order, expected);
}

#[test]
fn an_icon_is_a_tail_of_each_curve_buffer_and_one_glyph() {
    let Some(loaded) = loaded() else { return };
    let name = "audio-volume-high-symbolic";
    if resolve(name, ICON_PX).is_none() {
        witness("tests: no icon theme in this environment; skipping");
        return;
    }
    let font_glyphs = loaded.font_glyphs;
    let mut sidecar = Sidecar::new(loaded);
    let mut start: Vec<u8> = Vec::new();
    sidecar.write_font(&mut start).expect("writing to a Vec");
    let before = sidecar.loaded().curve_lengths();

    let mut answer: Vec<u8> = Vec::new();
    sidecar
        .answer_glyph(&mut answer, name)
        .expect("writing to a Vec");
    let decoded = frames(&answer);
    let Some(Frame::Glyph(glyph)) = decoded.last() else {
        panic!("the Glyph comes last")
    };
    assert_eq!(glyph.flags, 0, "a symbolic icon is curves");
    assert_eq!(glyph.name, name);
    assert!(
        glyph.gid >= font_glyphs,
        "an icon lives above the font's own ids"
    );

    let deltas: Vec<&Bytes> = decoded[..decoded.len() - 1]
        .iter()
        .map(|frame| match frame {
            Frame::Bytes(bytes) => bytes,
            other => panic!("only Bytes precede the Glyph, not kind {}", other.kind()),
        })
        .collect();
    assert!(
        !deltas.is_empty(),
        "an appended icon moves at least the glyph record"
    );
    let after = sidecar.loaded().curve_lengths();
    for delta in &deltas {
        let i = CURVE_BUFFERS
            .iter()
            .position(|b| *b == delta.buffer)
            .expect("a curve buffer");
        assert_eq!(
            delta.offset as usize, before[i],
            "the delta starts where the bar left off"
        );
        assert_eq!(
            delta.total as usize, after[i],
            "and names the buffer's new length"
        );
        assert_eq!(
            delta.offset as usize + delta.bytes.len(),
            after[i],
            "one tail, whole"
        );
    }

    // A repeat is answered from the map: the same id, and not one byte more.
    let mut again: Vec<u8> = Vec::new();
    sidecar
        .answer_glyph(&mut again, name)
        .expect("writing to a Vec");
    let repeat = frames(&again);
    assert_eq!(repeat.len(), 1, "a repeat appends nothing");
    assert!(matches!(&repeat[0], Frame::Glyph(g) if g.gid == glyph.gid));
    assert_eq!(sidecar.loaded().curve_lengths(), after);
    assert_eq!(sidecar.known(name), Some(Some(glyph.gid)));
}

#[test]
fn a_name_no_theme_has_is_glyph_missing() {
    let Some(loaded) = loaded() else { return };
    let name = "caustics-no-such-icon-anywhere-symbolic";
    let mut sidecar = Sidecar::new(loaded);
    let mut out: Vec<u8> = Vec::new();
    sidecar
        .answer_glyph(&mut out, name)
        .expect("writing to a Vec");
    let decoded = frames(&out);
    assert_eq!(
        decoded.len(),
        1,
        "nothing was appended, so there is no delta"
    );
    let Frame::Glyph(glyph) = &decoded[0] else {
        panic!("a Glyph")
    };
    assert_eq!(glyph.flags, GLYPH_MISSING);
    assert_eq!(glyph.name, name);
    assert_eq!(
        sidecar.known(name),
        Some(None),
        "and it is remembered, so the search is once"
    );
}

/// A miss expires: a symbolic icon installed after the
/// name was answered `GLYPH_MISSING` is found by the sweep and sent unasked —
/// its curve tails, then a `Glyph` with an id — and one changed under the
/// same name later is appended again under a new id. A sweep with nothing
/// changed writes nothing. The name is an absolute path because a fixture
/// cannot add a theme to the process's list (`freedesktop-icons` reads it
/// once); the expiry is the same either way.
#[test]
fn a_symbolic_icon_installed_after_the_miss_is_sent_unasked() {
    let Some(loaded) = loaded() else { return };
    let dir = std::env::temp_dir().join(format!("caustics-glyphs-later-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("later-symbolic.svg");
    let name = path.to_str().unwrap().to_owned();
    let square = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><path d="M2 2h12v12H2z"/></svg>"#;
    let disc = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><path d="M8 1a7 7 0 1 0 0 14A7 7 0 0 0 8 1z"/></svg>"#;
    let install = |svg: &str, age: u64| {
        std::fs::write(&path, svg).unwrap();
        let file = std::fs::File::options().write(true).open(&path).unwrap();
        file.set_modified(
            std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000 + age),
        )
        .unwrap();
    };

    let mut sidecar = Sidecar::new(loaded);
    let mut start: Vec<u8> = Vec::new();
    sidecar.write_font(&mut start).expect("writing to a Vec");
    let mut miss: Vec<u8> = Vec::new();
    sidecar
        .answer_glyph(&mut miss, &name)
        .expect("writing to a Vec");
    assert!(matches!(frames(&miss).as_slice(), [Frame::Glyph(g)] if g.flags == GLYPH_MISSING));
    let mut quiet: Vec<u8> = Vec::new();
    assert_eq!(sidecar.sweep(&mut quiet).unwrap(), 0);
    assert!(quiet.is_empty(), "nothing changed, nothing is said");

    install(square, 0);
    let mut found: Vec<u8> = Vec::new();
    assert_eq!(sidecar.sweep(&mut found).unwrap(), 1);
    let decoded = frames(&found);
    let Some(Frame::Glyph(first)) = decoded.last() else {
        panic!("a Glyph last: {decoded:?}")
    };
    assert_eq!(first.flags, 0, "curves now");
    assert_eq!(first.name, name);
    assert!(
        decoded[..decoded.len() - 1]
            .iter()
            .all(|f| matches!(f, Frame::Bytes(_)))
    );
    assert!(
        decoded.len() > 1,
        "the curves go before the id that names them"
    );
    let mut again: Vec<u8> = Vec::new();
    assert_eq!(sidecar.sweep(&mut again).unwrap(), 0, "sent once");

    install(disc, 60);
    let mut moved: Vec<u8> = Vec::new();
    assert_eq!(sidecar.sweep(&mut moved).unwrap(), 1);
    let Some(Frame::Glyph(second)) = frames(&moved).last().cloned() else {
        panic!("a Glyph")
    };
    assert_eq!(second.flags, 0);
    assert!(
        second.gid > first.gid,
        "a changed file is a new glyph, appended"
    );
    assert_eq!(sidecar.known(&name), Some(Some(second.gid)));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn only_a_symbolic_svg_is_curves() {
    assert!(is_symbolic(
        "x-symbolic",
        Path::new("/usr/share/icons/A/16/x-symbolic.svg")
    ));
    assert!(is_symbolic(
        "x",
        Path::new("/usr/share/icons/A/symbolic/apps/x.svg")
    ));
    assert!(!is_symbolic(
        "firefox",
        Path::new("/usr/share/icons/A/apps/firefox.svg")
    ));
    assert!(!is_symbolic(
        "x-symbolic",
        Path::new("/usr/share/icons/A/16/x-symbolic.png")
    ));
}

#[test]
fn the_theme_order_ends_at_hicolor_with_no_duplicates() {
    let order = theme_order();
    assert_eq!(order.last().map(String::as_str), Some("hicolor"));
    for theme in FALLBACK_THEMES {
        assert_eq!(
            order.iter().filter(|have| *have == theme).count(),
            1,
            "{theme} appears once"
        );
    }
}

/// The cover path end to end: Latin the primary already
/// has, one CJK codepoint and one emoji it does not. What comes back is a tail of each curve buffer, the arena's front block, fill bitmaps and new tables, and the whole `BUF_SHAPE_FONT` — and the covered codepoints land in a fallback face's range, resolving to
/// glyph ids **past the icons**, which is what "one merged glyph space" means.
#[test]
fn a_cover_opens_a_face_and_fills_what_the_primary_lacks() {
    let Some(loaded) = loaded() else { return };
    let Some(chain) = test_chain() else {
        witness("tests: no CJK or emoji face on this machine; skipping");
        return;
    };
    let icon_base = loaded.font_glyphs;
    let mut sidecar = Sidecar::with_chain(loaded, chain);
    let mut start: Vec<u8> = Vec::new();
    sidecar.write_font(&mut start).expect("writing to a Vec");
    let before_curves = sidecar.loaded().curve_lengths();

    let mut out: Vec<u8> = Vec::new();
    sidecar
        .answer_cover(&mut out, "Hi 日 \u{1f600}")
        .expect("writing to a Vec");
    let decoded = frames(&out);
    assert!(
        !decoded.is_empty(),
        "two codepoints were missing and nothing went out"
    );

    let mut buffers: Vec<u32> = Vec::new();
    for frame in &decoded {
        let Frame::Bytes(bytes) = frame else {
            panic!(
                "a cover answers in Bytes and nothing else, not kind {}",
                frame.kind()
            )
        };
        if !buffers.contains(&bytes.buffer) {
            buffers.push(bytes.buffer);
        }
    }
    // The curves before anything that names a glyph: a frame in between would
    // leave the caustic's directory pointing at a record the bar does not have.
    let named = buffers
        .iter()
        .position(|b| *b == BUF_OT)
        .expect("the arena's front block goes back");
    for buffer in CURVE_BUFFERS {
        if let Some(at) = buffers.iter().position(|b| *b == buffer) {
            assert!(at < named, "buffer {buffer} came after the arena");
        }
    }
    assert!(
        buffers.contains(&BUF_SHAPE_FONT),
        "the merged glyph count moved"
    );

    // Every curve delta starts where the bar left off, and `BUF_SHAPE_FONT` goes
    // back whole.
    for frame in &decoded {
        let Frame::Bytes(bytes) = frame else {
            unreachable!()
        };
        match CURVE_BUFFERS.iter().position(|b| *b == bytes.buffer) {
            Some(i) if bytes.offset == before_curves[i] as u32 => {}
            Some(_) if bytes.offset > 0 => {} // a later chunk of the same run
            Some(i) => panic!(
                "buffer {} restarted at {} not {}",
                bytes.buffer, bytes.offset, before_curves[i]
            ),
            // The arena goes back in three pieces: the front block at zero, a
            // fill bitmap in the middle, and the new face's tables as a tail.
            None if bytes.buffer == BUF_OT => {}
            None => assert_eq!(bytes.offset, 0, "buffer {} goes back whole", bytes.buffer),
        }
    }

    let arena = &sidecar.loaded().arena;
    assert!(
        arena.faces().len() >= 2,
        "a cover opens at least one fallback face"
    );
    for face in 1..arena.faces().len() {
        let info = arena.faces()[face];
        assert!(
            info.glyph_base >= icon_base,
            "face {face} overlaps the primary's ids"
        );
        assert!(
            !info.all_filled,
            "a face opened by a cover fills glyph by glyph"
        );
        assert_eq!(
            sidecar.loaded().curves.glyphs.len() as u32 >= info.glyph_base + info.glyph_count,
            true,
            "face {face}'s reserved range has no records"
        );
    }
    // Every codepoint that was covered has a filled record inside some face's
    // reserved range.
    let filled: usize = (1..arena.faces().len())
        .map(|face| {
            let info = arena.faces()[face];
            (0..info.glyph_count)
                .filter(|gid| arena.is_filled(face, *gid))
                .count()
        })
        .sum();
    assert!(filled >= 1, "nothing was filled");
}

/// A string the primary draws entirely is answered with silence — the bar asked
/// because *its* font is the one that flagged, and a sidecar that appends
/// nothing has nothing to say.
#[test]
fn a_cover_with_nothing_missing_writes_nothing() {
    let Some(loaded) = loaded() else { return };
    let mut sidecar = Sidecar::with_chain(loaded, Chain::of([]));
    let mut start: Vec<u8> = Vec::new();
    sidecar.write_font(&mut start).expect("writing to a Vec");
    let mut out: Vec<u8> = Vec::new();
    sidecar
        .answer_cover(&mut out, "Terminal — bash")
        .expect("writing to a Vec");
    assert!(out.is_empty());
}

/// A chain of exactly the two faces the cover test needs, or `None` on a
/// machine that has neither.
fn test_chain() -> Option<Chain> {
    let mut faces = Vec::new();
    for (pattern, ch) in [(":lang=ja", '日'), ("emoji", '\u{1f600}')] {
        let output = std::process::Command::new("fc-match")
            .args(["--format=%{family}\t%{file}\t%{index}", pattern])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let answer = String::from_utf8(output.stdout).ok()?;
        let mut fields = answer.split('\t');
        let family = fields.next()?.split(',').next()?.trim().to_owned();
        let path = PathBuf::from(fields.next()?.trim());
        let index: u32 = fields
            .next()
            .and_then(|word| word.trim().parse().ok())
            .unwrap_or(0);
        let data = std::fs::read(&path).ok()?;
        // A face that claims the codepoint and draws it with a colour table is
        // no use to this test — and is exactly what the chain walks past.
        let drawn = punchcutter_text::font::codepoint_curves(&data, index, ch)
            .ok()
            .flatten()
            .is_some_and(|glyph| glyph.has_ink());
        if !drawn {
            return None;
        }
        faces.push((family, path, index));
    }
    Some(Chain::of(faces))
}

// ---------------------------------------------------------------------------
// A cover fills the GSUB closure
// ---------------------------------------------------------------------------

/// The face fontconfig lists under exactly this family, as a one-face chain
/// entry. `fc-list`, because `fc-match "Noto Emoji"` answers Noto Color Emoji.
pub(crate) fn family_face(family: &str) -> Option<(String, PathBuf, u32)> {
    let output = std::process::Command::new("fc-list")
        .args([family, "--format=%{family}\t%{file}\t%{index}\n"])
        .output()
        .ok()?;
    let text = String::from_utf8(output.stdout).ok()?;
    let mut lines: Vec<&str> = text.lines().collect();
    lines.sort_unstable();
    for line in lines {
        let mut fields = line.split('\t');
        if !fields.next()?.split(',').any(|name| name.trim() == family) {
            continue;
        }
        let path = PathBuf::from(fields.next()?.trim());
        let index = fields
            .next()
            .and_then(|word| word.trim().parse().ok())
            .unwrap_or(0);
        if path.is_file() {
            return Some((family.to_owned(), path, index));
        }
    }
    None
}

// ---------------------------------------------------------------------------
// A record filled in place is sent again
// ---------------------------------------------------------------------------

/// What the bar holds of one buffer after these frames: each `Bytes` written
/// at its offset into a buffer sized by its `total`, as `write_bytes` does.
fn replay(copy: &mut Vec<u8>, buffer: u32, frames: &[Frame]) {
    for frame in frames {
        if let Frame::Bytes(bytes) = frame
            && bytes.buffer == buffer
        {
            copy.resize(copy.len().max(bytes.total as usize), 0);
            let at = bytes.offset as usize;
            copy[at..at + bytes.bytes.len()].copy_from_slice(&bytes.bytes);
        }
    }
}

/// The first flag cover opens Noto Emoji — reserving its whole
/// range of blank records — and fills them inside the new tail. A second flag
/// fills records **below** what was sent, which no tail carries: unless they
/// are sent again, the arena's bitmap says filled, the bar's record is the
/// blank, and 🇯🇵 draws as nothing while 🇦🇺 draws.
#[test]
fn a_second_cover_sends_the_records_it_filled_in_place() {
    let Some(loaded) = loaded() else { return };
    let Some(face) = family_face("Noto Emoji") else {
        witness("tests: Noto Emoji is not installed; skipping");
        return;
    };
    let mut sidecar = Sidecar::with_chain(loaded, Chain::of([face]));
    let mut start: Vec<u8> = Vec::new();
    sidecar.write_font(&mut start).expect("writing to a Vec");
    let mut bar: Vec<u8> = Vec::new();
    replay(&mut bar, BUF_GLYPHS, &frames(&start));

    let mut first: Vec<u8> = Vec::new();
    sidecar
        .answer_cover(&mut first, "🇦🇺 Australia")
        .expect("writing to a Vec");
    let first = frames(&first);
    replay(&mut bar, BUF_GLYPHS, &first);
    // The first answer's record tail ends where the second answer's begins.
    let watermark = bar.len();
    assert_eq!(watermark, sidecar.loaded().curve_lengths()[3]);

    let mut second: Vec<u8> = Vec::new();
    sidecar
        .answer_cover(&mut second, "🇯🇵 Japan")
        .expect("writing to a Vec");
    let second = frames(&second);
    replay(&mut bar, BUF_GLYPHS, &second);

    let stride = size_of::<punchcutter_text::Glyph>();
    let below: Vec<(u32, usize)> = second
        .iter()
        .filter_map(|frame| match frame {
            Frame::Bytes(bytes)
                if bytes.buffer == BUF_GLYPHS && (bytes.offset as usize) < watermark =>
            {
                Some((bytes.offset, bytes.bytes.len()))
            }
            _ => None,
        })
        .collect();
    eprintln!(
        "🇯🇵 Japan after 🇦🇺 Australia: BUF_GLYPHS writes below the tail at {watermark}: {below:?}"
    );
    assert!(
        !below.is_empty(),
        "no record below the first answer's tail was sent"
    );
    assert!(
        below
            .iter()
            .all(|(at, len)| *at as usize % stride == 0 && len % stride == 0)
    );

    // The bar's copy is the sidecar's, record for record — the whole promise.
    let whole = crate::buffer(sidecar.loaded(), BUF_GLYPHS).expect("a known buffer");
    assert_eq!(bar.len(), whole.len());
    assert!(
        bar == *whole,
        "the bar's record table differs from the sidecar's"
    );

    let arena = &sidecar.loaded().arena;
    let emoji = arena.faces().len() - 1;
    let info = arena.faces()[emoji];
    if info.glyph_count == 1895 {
        // flagJapan, gid 1734 in the 1,895-glyph build of Noto Emoji.
        let record = (info.glyph_base + 1734) as usize;
        assert!(
            arena.is_filled(emoji, 1734),
            "flagJapan (1734) is not filled"
        );
        assert!(
            below.iter().any(|(at, len)| {
                let at = *at as usize;
                (at..at + len).contains(&(record * stride))
            }),
            "flagJapan's record was not in a write below the tail"
        );
        let glyph = sidecar.loaded().curves.glyphs[record];
        assert!(
            glyph.hband_count > 0 && glyph.vband_count > 0,
            "flagJapan's record is the blank"
        );
    } else {
        witness(&format!(
            "tests: Noto Emoji has {} glyphs, not the 1895 of the build probed; flagJapan's gid is unchecked",
            info.glyph_count
        ));
    }

    // Nothing is sent twice: a third cover of the same flags writes nothing.
    let mut third: Vec<u8> = Vec::new();
    sidecar
        .answer_cover(&mut third, "🇯🇵 🇦🇺")
        .expect("writing to a Vec");
    assert!(third.is_empty());
}

/// Every non-ASCII character the tunables put into a `getStringHash` literal
/// is either one the caustic composes into a run, listed in
/// [`COMPOSED`] so the font choice requires it, or the overview item's
/// glyph, a label the bar sends whole and the fallback chain can cover. A new
/// composed glyph must join the list, or a font that lacks it is chosen and
/// draws a box.
#[test]
fn every_composed_literal_is_in_the_font_requirement() {
    let tunables =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../caustics/bar_tunables.slang");
    let source = std::fs::read_to_string(&tunables).expect("the tunables module is in the tree");
    let covered_whole = ['\u{2637}']; // OVERVIEW_GLYPH, a label on its own
    let mut seen = Vec::new();
    for line in source.lines().filter(|l| !l.trim_start().starts_with("//")) {
        let mut rest = line;
        while let Some(at) = rest.find("getStringHash(\"") {
            let lit = &rest[at + "getStringHash(\"".len()..];
            let Some(end) = lit.find('"') else { break };
            for c in lit[..end].chars().filter(|c| !c.is_ascii()) {
                assert!(
                    COMPOSED.contains(&c) || covered_whole.contains(&c),
                    "{c:?} (U+{:04X}) in {line:?} is neither a composed glyph the font must carry nor a whole label",
                    c as u32
                );
                seen.push(c);
            }
            rest = &lit[end..];
        }
    }
    for c in COMPOSED {
        assert!(
            seen.contains(c),
            "{c:?} is required of the font but no literal composes it"
        );
    }
}
