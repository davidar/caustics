use super::*;
use crate::buffer::{ACTION_SET_VALUE, APPLET_AUDIO, BTN_LEFT, CHART_CPU};

fn bytes(frame: &Frame) -> Vec<u8> {
    let mut out = Vec::new();
    write(&mut out, frame).expect("writing a frame");
    out
}

/// The header the other tests corrupt, spelled out once so its offsets are
/// asserted rather than assumed. Three words; the contract's version is `ABI_HASH`, stated once per child in `Hello` (docs/abi.md, *The ABI hash*).
#[test]
fn the_header_is_twelve_bytes_of_magic_kind_length() {
    assert_eq!(size_of::<WireHeader>(), 12);
    assert_eq!(WireHeader::SIZE, 12);
    assert_eq!(WIRE_MAGIC.to_le_bytes(), *b"CAUS");
    let raw = bytes(&Frame::Action(ActionBody::default()));
    assert_eq!(&raw[0..4], b"CAUS");
    assert_eq!(
        u32::from_le_bytes(raw[4..8].try_into().unwrap()),
        WIRE_ACTION
    );
    assert_eq!(u32::from_le_bytes(raw[8..12].try_into().unwrap()), 32);
    assert_eq!(raw.len(), 44);
}

/// *Hello*: the frame a bar reads before it knows whether the child is its own.
#[test]
fn a_hello_round_trips_and_carries_this_build_s_hash() {
    let frame = Frame::Hello(Hello {
        abi_hash: crate::ABI_HASH,
        name: "caustics-sysmon".to_owned(),
        flags: 0,
    });
    let raw = bytes(&frame);
    assert_eq!(
        u32::from_le_bytes(raw[4..8].try_into().unwrap()),
        WIRE_HELLO
    );
    assert_eq!(
        raw.len(),
        WireHeader::SIZE + size_of::<HelloBody>() + "caustics-sysmon".len()
    );
    assert_eq!(read(&mut &raw[..]).unwrap(), Some(frame));

    // And the helper writes exactly that, so a sidecar's `main` and this test
    // are the same frame.
    let mut out = Vec::new();
    hello(&mut out, "caustics-sysmon").expect("writing a hello");
    assert_eq!(out, raw);
}

/// The hash crosses as two words, low half first, and comes back whole.
#[test]
fn a_hello_from_another_tree_decodes_to_that_tree_s_hash() {
    let theirs = 0x0123_4567_89ab_cdefu64;
    let raw = bytes(&Frame::Hello(Hello {
        abi_hash: theirs,
        name: "x".to_owned(),
        flags: 0,
    }));
    assert_eq!(
        u32::from_le_bytes(raw[12..16].try_into().unwrap()),
        0x89ab_cdef
    );
    assert_eq!(
        u32::from_le_bytes(raw[16..20].try_into().unwrap()),
        0x0123_4567
    );
    let Some(Frame::Hello(hello)) = read(&mut &raw[..]).unwrap() else {
        panic!("a hello did not read back");
    };
    assert_eq!(hello.abi_hash, theirs);
    assert_ne!(
        hello.abi_hash,
        crate::ABI_HASH,
        "the fixture must not be this tree's"
    );
}

/// A `Hello` body that is not its own name length, or not UTF-8, is skipped
/// like any other malformed body — it is read before anything is trusted.
#[test]
fn a_malformed_hello_is_skipped() {
    let mut raw = bytes(&Frame::Hello(Hello {
        abi_hash: 1,
        name: "ab".to_owned(),
        flags: 0,
    }));
    raw[20..24].copy_from_slice(&9u32.to_le_bytes());
    assert!(matches!(
        read(&mut &raw[..]).unwrap(),
        Some(Frame::Skipped(Skipped {
            why: Skip::Length,
            ..
        }))
    ));

    let mut raw = bytes(&Frame::Hello(Hello {
        abi_hash: 1,
        name: "ab".to_owned(),
        flags: 0,
    }));
    let last = raw.len() - 1;
    raw[last] = 0xff;
    assert!(matches!(
        read(&mut &raw[..]).unwrap(),
        Some(Frame::Skipped(Skipped {
            why: Skip::Body,
            ..
        }))
    ));
}

#[test]
fn the_bodies_are_the_documented_sizes() {
    assert_eq!(size_of::<Samples>(), SAMPLES_SIZE);
    assert_eq!(SAMPLES_SIZE, 8336);
    assert_eq!(size_of::<ChartRow>(), 16);
    assert_eq!(size_of::<RowsHeader>(), 16);
    assert_eq!(size_of::<Row>(), 48);
    assert_eq!(size_of::<ActionBody>(), 32);
}

#[test]
fn a_samples_frame_round_trips() {
    let mut samples = Box::new(Samples::ZERO);
    samples.chart_count = 2;
    samples.sample_count = 3;
    samples.t = 12.5;
    samples.charts[0] = ChartRow {
        ceiling: 1.0,
        series: 1,
        kind: CHART_CPU,
        _rsvd0: 0,
    };
    samples.samples[CHART_SAMPLES - 1] = 0.5;

    let frame = Frame::Samples(samples.clone());
    let raw = bytes(&frame);
    assert_eq!(raw.len(), WireHeader::SIZE + SAMPLES_SIZE);
    let mut cursor = &raw[..];
    assert_eq!(read(&mut cursor).unwrap(), Some(frame));
    // ...and the stream is now exactly empty: a clean close.
    assert_eq!(read(&mut cursor).unwrap(), None);
}

#[test]
fn an_action_round_trips() {
    let action = ActionBody {
        row: 7,
        kind: ACTION_SET_VALUE,
        button: BTN_LEFT,
        pos: [12.0, 3.0],
        value: 0.4,
        ..ActionBody::default()
    };
    let raw = bytes(&Frame::Action(action));
    let mut cursor = &raw[..];
    assert_eq!(read(&mut cursor).unwrap(), Some(Frame::Action(action)));
}

/// The builder's tree, out and back: labels, a themed icon, a pixmap, a
/// slider, and parents that resolve to the rows they were built from.
#[test]
fn a_rows_frame_round_trips_with_its_labels_and_icons() {
    let pixels = vec![0xffu8; 2 * 2 * 4];
    let key = pixmap_key("caustics-audio", &pixels);
    let mut builder = RowsBuilder::new(APPLET_AUDIO, 3);
    let item = builder.item(
        1,
        Some("42%"),
        Some("audio-volume-high-symbolic"),
        0.42,
        RFLAG_HAS_MENU,
    );
    let volume = builder.slider(item, 2, Some("Volume"), 0.42, 0.0, 1.0, 0);
    let sep = builder.separator(item, 3);
    let sink = builder.menu(
        item,
        4,
        Some("Speakers"),
        Some(&key),
        RFLAG_CHECKABLE | RFLAG_CHECKED | RFLAG_RADIO,
    );
    assert_eq!(
        (item, volume, sep, sink),
        (RowRef(0), RowRef(1), RowRef(2), RowRef(3))
    );

    let built = builder.build();
    let raw = bytes(&Frame::Rows(built.clone()));
    let mut cursor = &raw[..];
    let Some(Frame::Rows(rows)) = read(&mut cursor).unwrap() else {
        panic!("not a rows frame");
    };
    assert_eq!(rows, built);
    assert_eq!(rows.applet, APPLET_AUDIO);
    assert_eq!(rows.revision, 3);
    assert_eq!(rows.rows.len(), 4);
    assert_eq!(rows.label(0).as_deref(), Some("42%"));
    assert_eq!(rows.icon(0), Some("audio-volume-high-symbolic"));
    assert_eq!(rows.label(2), None, "a separator has no label");
    assert_eq!(
        rows.icon(3),
        Some(key.as_str()),
        "a pixmap icon is its key, never its texels"
    );
    assert_eq!(rows.rows[1].role, ROW_SLIDER);
    assert_eq!(rows.rows[1].parent, 0);
    assert_eq!(
        rows.rows[3].flags,
        RFLAG_CHECKABLE | RFLAG_CHECKED | RFLAG_RADIO,
        "the builder adds no flag of its own for an icon"
    );
}

/// A label that is not UTF-8 comes back with U+FFFD rather than losing the
/// applet (docs/abi.md, *Rows*, validation).
#[test]
fn a_mangled_label_is_replaced_and_not_rejected() {
    let mut builder = RowsBuilder::new(APPLET_AUDIO, 1);
    let item = builder.item(1, Some("ok"), None, -1.0, 0);
    let mut rows = builder.build();
    // Overwrite the label's bytes in place with an invalid sequence.
    rows.bytes[0] = 0xff;
    let raw = bytes(&Frame::Rows(rows));
    let mut cursor = &raw[..];
    let Some(Frame::Rows(back)) = read(&mut cursor).unwrap() else {
        panic!("not a rows frame");
    };
    assert_eq!(item, RowRef(0));
    assert!(back.label(0).unwrap().contains('\u{fffd}'));
}

// -- *Framing*'s reader rules, one crafted stream each -----------------------

/// Rule 1: EOF exactly on a boundary is a clean close; EOF inside a header
/// or a body is an error.
#[test]
fn rule_1_eof_on_a_boundary_is_clean_and_anywhere_else_is_not() {
    assert_eq!(read(&mut &[][..]).unwrap(), None);

    let raw = bytes(&Frame::Action(ActionBody::default()));
    let err = read(&mut &raw[..8]).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{err}");
    let err = read(&mut &raw[..30]).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof, "{err}");
}

/// Rule 2: a wrong magic is not our stream at all — the first byte of a
/// sidecar that confused stdout with stderr.
#[test]
fn rule_2_a_bad_magic_is_an_error() {
    let mut raw = bytes(&Frame::Action(ActionBody::default()));
    raw[0] ^= 0xff;
    let err = read(&mut &raw[..]).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("bad magic"), "{err}");
}

/// Rule 3: a length past the cap is a broken writer, not a big frame.
#[test]
fn rule_3_a_body_past_the_cap_is_an_error() {
    let header = WireHeader::new(WIRE_ROWS, WIRE_MAX_BODY as u32 + 1);
    let err = read(&mut as_bytes(&header)).unwrap_err();
    assert!(err.to_string().contains("over the"), "{err}");
}

/// The cap is the largest honest body, computed: an `Image` at
/// `IMAGE_MAX_PX` square behind its head and the longest key. It fits the
/// header's `u32`, and a header stating exactly it passes rule 3.
#[test]
fn rule_3_the_cap_is_the_largest_image_and_fits_the_header() {
    assert_eq!(
        WIRE_MAX_BODY,
        size_of::<ImageHeader>() + MAX_NAME_LEN + IMAGE_MAX_PX as usize * IMAGE_MAX_PX as usize * 4
    );
    assert!(u32::try_from(WIRE_MAX_BODY).is_ok());
    let header = WireHeader::new(WIRE_IMAGE, WIRE_MAX_BODY as u32);
    let err = read(&mut as_bytes(&header)).unwrap_err();
    assert!(!err.to_string().contains("over the"), "{err}");
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
}

/// A `Read` that records how many bytes each call asked for, so a test can
/// say what the reader requested rather than guess what it allocated.
struct Counting<'a> {
    inner: &'a [u8],
    asked: Vec<usize>,
}

impl<'a> Counting<'a> {
    fn new(inner: &'a [u8]) -> Counting<'a> {
        Counting {
            inner,
            asked: Vec::new(),
        }
    }
}

impl Read for Counting<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.asked.push(buf.len());
        self.inner.read(buf)
    }
}

/// Rule 4: a header that lies about its length costs what the writer sent
/// and a step. The reader never asks for more than `WIRE_READ_STEP` in one
/// call, and an EOF inside the body is an error, not a clean close.
#[test]
fn rule_4_a_lying_header_costs_one_step_and_its_eof_is_an_error() {
    let mut raw = as_bytes(&WireHeader::new(WIRE_IMAGE, WIRE_MAX_BODY as u32)).to_vec();
    raw.extend_from_slice(&[7u8; 100]);
    let mut input = Counting::new(&raw);
    let err = read(&mut input).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof, "{err}");
    let most = input.asked.iter().copied().max().unwrap_or(0);
    assert!(
        most <= WIRE_READ_STEP,
        "a request of {most} bytes, past the {WIRE_READ_STEP} step"
    );
    // The header, the first step that returned the hundred bytes, and the rest
    // of that step that met the EOF: the reader stopped at the first one.
    assert!(input.asked.len() <= 4, "{:?}", input.asked);

    // And EOF inside a header is the same error, while EOF exactly on a
    // boundary is the clean close.
    assert!(read(&mut &raw[..5]).is_err());
    assert!(matches!(read(&mut &raw[..0]), Ok(None)));
}

/// Rule 4 on an honest body of several steps: every request is at most one
/// step, and the frame reads back whole.
#[test]
fn rule_4_an_honest_body_is_read_one_step_at_a_time() {
    let picture = image("steps", 640, 480);
    let raw = bytes(&Frame::Image(picture.clone()));
    assert!(raw.len() > 10 * WIRE_READ_STEP);
    let mut input = Counting::new(&raw);
    let back = read(&mut input).expect("reads").expect("a frame");
    assert_eq!(back, Frame::Image(picture));
    assert!(
        input.asked.iter().all(|&asked| asked <= WIRE_READ_STEP),
        "{:?}",
        input.asked.iter().max()
    );
    assert!(input.asked.len() >= raw.len() / WIRE_READ_STEP);
}

/// Rules 4 and 5: an unknown kind is consumed whole and skipped, and the
/// *next* frame still reads — which is what length-prefixing is for.
#[test]
fn rule_5_an_unknown_kind_is_skipped_and_the_stream_survives_it() {
    let mut raw = Vec::new();
    raw.extend_from_slice(as_bytes(&WireHeader::new(99, 4)));
    raw.extend_from_slice(&[1u8, 2, 3, 4]);
    raw.extend_from_slice(&bytes(&Frame::Action(ActionBody {
        row: 5,
        ..Default::default()
    })));

    let mut cursor = &raw[..];
    assert_eq!(
        read(&mut cursor).unwrap(),
        Some(Frame::Skipped(Skipped {
            kind: 99,
            why: Skip::Kind
        }))
    );
    let Some(Frame::Action(action)) = read(&mut cursor).unwrap() else {
        panic!("the frame after a skip was lost");
    };
    assert_eq!(action.row, 5);
    assert_eq!(read(&mut cursor).unwrap(), None);
}

/// Rule 6: the right kind with a length that cannot be it.
#[test]
fn rule_6_a_wrong_length_is_skipped() {
    // A samples frame one byte short.
    let mut raw = bytes(&Frame::Samples(Box::new(Samples::ZERO)));
    raw.pop();
    raw[8..12].copy_from_slice(&((SAMPLES_SIZE - 1) as u32).to_le_bytes());
    assert!(matches!(
        read(&mut &raw[..]).unwrap(),
        Some(Frame::Skipped(Skipped {
            why: Skip::Length,
            ..
        }))
    ));

    // A rows frame whose row_count does not match its bytes.
    let mut body = Vec::new();
    body.extend_from_slice(as_bytes(&RowsHeader {
        applet: 0,
        row_count: 4,
        bytes_len: 0,
        revision: 1,
    }));
    body.extend_from_slice(&[0u8; size_of::<Row>()]);
    let mut raw = Vec::new();
    raw.extend_from_slice(as_bytes(&WireHeader::new(WIRE_ROWS, body.len() as u32)));
    raw.extend_from_slice(&body);
    assert!(matches!(
        read(&mut &raw[..]).unwrap(),
        Some(Frame::Skipped(Skipped {
            why: Skip::Length,
            ..
        }))
    ));
}

/// *Rows*'s tree rules, each as its own crafted frame: every one of them
/// costs the whole frame and nothing else.
#[test]
fn a_malformed_tree_is_skipped_row_rule_by_row_rule() {
    let framed = |rows: Vec<Row>, blob: &[u8]| -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(as_bytes(&RowsHeader {
            applet: 0,
            row_count: rows.len() as u32,
            bytes_len: blob.len() as u32,
            revision: 1,
        }));
        for row in &rows {
            body.extend_from_slice(as_bytes(row));
        }
        body.extend_from_slice(blob);
        let mut raw = Vec::new();
        raw.extend_from_slice(as_bytes(&WireHeader::new(WIRE_ROWS, body.len() as u32)));
        raw.extend_from_slice(&body);
        raw
    };
    let item = Row {
        id: 1,
        parent: NO_INDEX,
        role: ROW_ITEM,
        ..Row::default()
    };
    let skipped = |rows: Vec<Row>, blob: &[u8], why: &str| {
        let raw = framed(rows, blob);
        match read(&mut &raw[..]).unwrap() {
            Some(Frame::Skipped(Skipped {
                why: Skip::Body, ..
            })) => {}
            other => panic!("{why}: expected a skip, got {other:?}"),
        }
    };

    // A bar item with a parent.
    skipped(
        vec![Row { parent: 0, ..item }],
        &[],
        "a ROW_ITEM with a parent",
    );
    // A child whose parent is itself or later.
    skipped(
        vec![
            item,
            Row {
                id: 2,
                parent: 1,
                role: ROW_MENU,
                ..Row::default()
            },
        ],
        &[],
        "a parent at or past its own index",
    );
    // A child under a plain menu row that never said it had children.
    skipped(
        vec![
            item,
            Row {
                id: 2,
                parent: 0,
                role: ROW_MENU,
                ..Row::default()
            },
            Row {
                id: 3,
                parent: 1,
                role: ROW_MENU,
                ..Row::default()
            },
        ],
        &[],
        "a parent that is neither an item nor a submenu",
    );
    // A slider with an empty range.
    skipped(
        vec![
            item,
            Row {
                id: 2,
                parent: 0,
                role: ROW_SLIDER,
                min: 1.0,
                max: 1.0,
                ..Row::default()
            },
        ],
        &[],
        "a slider with min >= max",
    );
    // A label reaching past the blob.
    skipped(
        vec![Row {
            label_off: 2,
            label_len: 8,
            ..item
        }],
        b"abcd",
        "a label past the blob",
    );
    // A body reaching past the blob: offset 2, length 8, packed 16/16, against
    // four bytes.
    skipped(
        vec![Row {
            body: 2 | (8 << 16),
            ..item
        }],
        b"abcd",
        "a body past the blob",
    );
    // A role nobody declared.
    skipped(vec![Row { role: 99, ..item }], &[], "an unknown role");

    // ...and the well-formed version of the same tree decodes.
    let raw = framed(
        vec![
            item,
            Row {
                id: 2,
                parent: 0,
                role: ROW_MENU,
                flags: RFLAG_SUBMENU,
                ..Row::default()
            },
            Row {
                id: 3,
                parent: 1,
                role: ROW_MENU,
                ..Row::default()
            },
        ],
        &[],
    );
    let Some(Frame::Rows(rows)) = read(&mut &raw[..]).unwrap() else {
        panic!("the well-formed tree did not decode");
    };
    assert_eq!(rows.rows.len(), 3);
}

/// A row's body is its second string (docs/abi.md, *Rows*): the builder packs its range into `Row::body`, the reader validates it like a
/// label's and hands the bytes back, and a body past what 16 bits can name is
/// not attached at all.
#[test]
fn a_body_is_a_second_range_packed_in_one_word() {
    let mut builder = RowsBuilder::new(0, 1);
    let chip = builder.item(1, Some("chip"), None, -1.0, 0);
    let row = builder.menu(chip, 2, Some("Summary"), None, 0);
    assert!(builder.body(row, "first line\nsecond line"));
    builder.menu(chip, 3, Some("Plain"), None, 0);
    let rows = builder.build();
    assert_eq!(
        rows.rows[1].body_range(),
        (11, 22),
        "after `chip` and `Summary`"
    );
    assert_eq!(rows.rows[1].body, 11 | (22 << 16));
    assert_eq!(rows.rows[2].body, 0, "no body is the zero word");

    let mut raw = Vec::new();
    write(&mut raw, &Frame::Rows(rows.clone())).unwrap();
    let Some(Frame::Rows(back)) = read(&mut &raw[..]).unwrap() else {
        panic!("a frame with a body did not decode");
    };
    assert_eq!(back.body(1).as_deref(), Some("first line\nsecond line"));
    assert_eq!(back.label(1).as_deref(), Some("Summary"));
    assert_eq!(back.body(2), None);

    // Past 2^16 bytes of blob, the packing cannot say where: not attached.
    let mut builder = RowsBuilder::new(0, 1);
    let chip = builder.item(1, Some(&"x".repeat(70_000)), None, -1.0, 0);
    let row = builder.menu(chip, 2, None, None, 0);
    assert!(!builder.body(row, "too far"));
    assert_eq!(builder.build().rows[1].body, 0);
}

/// A row count past `MAX_ROWS` is refused by the writer and skipped by the
/// reader; nothing in between allocates 256 MB because a `u32` said so.
#[test]
fn a_row_count_past_the_cap_is_refused_at_both_ends() {
    let mut header = Vec::new();
    header.extend_from_slice(as_bytes(&RowsHeader {
        applet: 0,
        row_count: MAX_ROWS as u32 + 1,
        bytes_len: 0,
        revision: 1,
    }));
    let mut raw = Vec::new();
    raw.extend_from_slice(as_bytes(&WireHeader::new(WIRE_ROWS, header.len() as u32)));
    raw.extend_from_slice(&header);
    assert!(matches!(
        read(&mut &raw[..]).unwrap(),
        Some(Frame::Skipped(Skipped {
            why: Skip::Length,
            ..
        }))
    ));

    let rows = Rows {
        applet: 0,
        revision: 1,
        rows: vec![Row::default(); MAX_ROWS + 1],
        bytes: Vec::new(),
    };
    assert!(write(&mut Vec::new(), &Frame::Rows(rows)).is_err());
}

/// The *Pixmap* key convention: `<sidecar>:<16 hex digits of FNV-1a 64>`, the
/// same texels the same key and different texels a different one, and a
/// minted key far inside `MAX_NAME_LEN`.
#[test]
fn a_minted_pixmap_key_is_the_sidecar_and_the_texels_hash() {
    let slot = (IMAGE_SLOT_PX * IMAGE_SLOT_PX * 4) as usize;
    let a = vec![0x40u8; slot];
    let mut b = a.clone();
    b[slot - 1] ^= 1;
    let key = pixmap_key("caustics-tray", &a);
    assert_eq!(key, format!("caustics-tray:{:016x}", fnv1a64(&a)));
    assert_eq!(key.len(), "caustics-tray:".len() + 16);
    assert!(key.len() <= MAX_NAME_LEN);
    assert_eq!(
        key,
        pixmap_key("caustics-tray", &a.clone()),
        "content-addressed"
    );
    assert_ne!(
        key,
        pixmap_key("caustics-tray", &b),
        "one texel changed is a new key"
    );
    assert_ne!(
        key,
        pixmap_key("caustics-apps", &a),
        "the sidecar's name keeps keys apart"
    );
    // The FNV-1a 64 reference vectors, so the digits cannot drift.
    assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
    assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);

    // And the frame that carries the texels round-trips under that key.
    let frame = Frame::Pixmap(Pixmap {
        key: key.clone(),
        width: IMAGE_SLOT_PX,
        height: IMAGE_SLOT_PX,
        rgba: a.clone(),
    });
    let raw = bytes(&frame);
    let mut cursor = &raw[..];
    assert_eq!(read(&mut cursor).unwrap(), Some(frame));
    assert!(cursor.is_empty());
}

/// A skipped frame cannot be written back: there are no bytes left of it.
#[test]
fn a_skipped_frame_is_not_writable() {
    let frame = Frame::Skipped(Skipped {
        kind: 9,
        why: Skip::Kind,
    });
    assert!(write(&mut Vec::new(), &frame).is_err());
    assert_eq!(frame.kind(), 9);
}

// -- Bytes, Font, Glyph, Request, App, Pixmap (docs/abi.md, *Bytes*–*Pixmap*) ---

fn round_trip(frame: Frame) -> Frame {
    let mut buf = Vec::new();
    write(&mut buf, &frame).expect("writes");
    let mut cursor = std::io::Cursor::new(buf);
    read(&mut cursor).expect("reads").expect("a frame")
}

#[test]
fn the_m_heads_are_the_documented_sizes() {
    assert_eq!(size_of::<BytesHeader>(), 16);
    assert_eq!(size_of::<FontBody>(), 32);
    assert_eq!(size_of::<GlyphBody>(), 16);
    assert_eq!(size_of::<RequestBody>(), 16);
    assert_eq!(size_of::<AppBody>(), 32);
    assert_eq!(size_of::<PixmapBody>(), 16);
    assert_eq!(size_of::<ModuleHeader>(), 28);
    assert_eq!(size_of::<ImageHeader>(), 12);
    assert_eq!(size_of::<CanvasBody>(), 32);
}

#[test]
fn a_bytes_frame_round_trips_and_names_a_buffer() {
    let frame = Frame::Bytes(Bytes {
        buffer: BUF_OT,
        offset: 64,
        total: 128,
        bytes: vec![7; 40],
    });
    assert_eq!(round_trip(frame.clone()), frame);
    // `total` is raised to cover the write if the writer understated it.
    let short = Frame::Bytes(Bytes {
        buffer: BUF_POINTS,
        offset: 16,
        total: 0,
        bytes: vec![1; 8],
    });
    let Frame::Bytes(back) = round_trip(short) else {
        panic!("not bytes")
    };
    assert_eq!(back.total, 24);
    // A buffer nobody has is a writer's error and a reader's skip.
    let none = Frame::Bytes(Bytes {
        buffer: BUF_LAST + 1,
        offset: 0,
        total: 1,
        bytes: vec![1],
    });
    assert!(write(&mut Vec::new(), &none).is_err());
}

#[test]
fn a_bytes_body_past_its_total_is_skipped() {
    let head = BytesHeader {
        buffer: BUF_BANDS,
        offset: 100,
        len: 8,
        total: 104,
    };
    let mut body = as_bytes(&head).to_vec();
    body.extend_from_slice(&[0; 8]);
    let mut buf = as_bytes(&WireHeader::new(WIRE_BYTES, body.len() as u32)).to_vec();
    buf.extend_from_slice(&body);
    let mut cursor = std::io::Cursor::new(buf);
    match read(&mut cursor).expect("reads").expect("a frame") {
        Frame::Skipped(skipped) => assert_eq!(skipped.why, Skip::Body),
        other => panic!("{other:?} was not skipped"),
    }
}

#[test]
fn a_font_frame_round_trips() {
    let frame = Frame::Font(Font {
        upem: 2048,
        cap_height: 0.7275,
        glyph_count: 2937,
        icon_base: 2937,
        family: "Adwaita Sans".into(),
    });
    assert_eq!(round_trip(frame.clone()), frame);
}

#[test]
fn a_glyph_and_a_request_round_trip() {
    let glyph = Frame::Glyph(Glyph {
        gid: 2940,
        flags: 0,
        name: "audio-volume-high-symbolic".into(),
    });
    assert_eq!(round_trip(glyph.clone()), glyph);
    let missing = Frame::Glyph(Glyph {
        gid: 0,
        flags: GLYPH_MISSING,
        name: "no-such-icon".into(),
    });
    assert_eq!(round_trip(missing.clone()), missing);
    let request = Frame::Request(Request {
        kind: REQ_APP,
        arg: 0,
        name: "org.mozilla.firefox".into(),
    });
    assert_eq!(round_trip(request.clone()), request);
    // An empty name asks for nothing: a writer's success, a reader's skip.
    let mut buf = Vec::new();
    write(
        &mut buf,
        &Frame::Request(Request {
            kind: REQ_GLYPH,
            arg: 0,
            name: String::new(),
        }),
    )
    .expect("writes");
    let mut cursor = std::io::Cursor::new(buf);
    assert!(matches!(
        read(&mut cursor).expect("reads"),
        Some(Frame::Skipped(_))
    ));
}

/// A notification's activation token crosses with the row it is for in the
/// request's one word, and is under `PIPE_BUF` for any token a compositor
/// mints (smithay's are 32 bytes) — the bar writes it in one non-blocking write.
#[test]
fn an_activation_request_carries_its_row_in_the_arg_word() {
    let token = "0123456789abcdef0123456789abcdef";
    let request = Frame::Request(Request {
        kind: REQ_ACTIVATION,
        arg: 0x0000_0102,
        name: token.into(),
    });
    let raw = bytes(&request);
    assert_eq!(
        u32::from_le_bytes(raw[12..16].try_into().unwrap()),
        REQ_ACTIVATION
    );
    assert_eq!(
        u32::from_le_bytes(raw[20..24].try_into().unwrap()),
        0x0000_0102
    );
    assert!(raw.len() < 512, "one write under PIPE_BUF");
    assert_eq!(round_trip(request.clone()), request);
}

/// *Hello*'s flags word is reserved: every greeting
/// writes it zero, and a greeting
/// that reads back carries the name it was written with.
#[test]
fn a_hello_writes_its_reserved_word_zero() {
    let mut out = Vec::new();
    hello(&mut out, "caustics-notify").expect("writing a hello");
    assert_eq!(
        u32::from_le_bytes(out[24..28].try_into().unwrap()),
        0,
        "the reserved word"
    );
    let Some(Frame::Hello(greeting)) = read(&mut &out[..]).unwrap() else {
        panic!("a hello did not read back");
    };
    assert_eq!(greeting.flags, 0);
    assert_eq!(greeting.name, "caustics-notify");
    assert_eq!(greeting.abi_hash, crate::ABI_HASH);
}

/// A toast's end crosses as one `Action` per
/// row it showed, the row in the body's first word and the kind in its second.
#[test]
fn a_toast_shown_action_names_its_row() {
    let frame = Frame::Action(ActionBody {
        row: 0x0000_0207,
        kind: ACTION_TOAST_SHOWN,
        ..ActionBody::default()
    });
    let raw = bytes(&frame);
    assert_eq!(
        u32::from_le_bytes(raw[12..16].try_into().unwrap()),
        0x0000_0207
    );
    assert_eq!(
        u32::from_le_bytes(raw[16..20].try_into().unwrap()),
        ACTION_TOAST_SHOWN
    );
    assert_eq!(round_trip(frame.clone()), frame);
}

#[test]
fn an_app_frame_round_trips_with_its_argv() {
    let frame = Frame::App(App {
        flags: APP_ICON_PIXMAP,
        app_id: "com.system76.CosmicTerm".into(),
        name: "COSMIC Terminal".into(),
        icon: "com.system76.CosmicTerm".into(),
        path: "/usr/share/applications/com.system76.CosmicTerm.desktop".into(),
        exec: vec!["cosmic-term".into(), "--new-window".into()],
    });
    assert_eq!(round_trip(frame.clone()), frame);
    let missing = Frame::App(App {
        flags: APP_MISSING,
        app_id: "nobody".into(),
        name: "nobody".into(),
        icon: String::new(),
        path: String::new(),
        exec: Vec::new(),
    });
    assert_eq!(round_trip(missing.clone()), missing);
}

#[test]
fn a_pixmap_frame_is_one_slot_exactly() {
    let px = IMAGE_SLOT_PX as usize;
    let frame = Frame::Pixmap(Pixmap {
        key: "firefox".into(),
        width: IMAGE_SLOT_PX,
        height: IMAGE_SLOT_PX,
        rgba: vec![200; px * px * 4],
    });
    assert_eq!(round_trip(frame.clone()), frame);
    let wrong = Frame::Pixmap(Pixmap {
        key: "x".into(),
        width: 32,
        height: 32,
        rgba: vec![0; 32 * 32 * 4],
    });
    assert!(write(&mut Vec::new(), &wrong).is_err());
}

/// *Module*. The body carries three variable runs in a fixed order, so the one
/// thing worth pinning is that a reader trusting `spirv_len` lands on the
/// reflection and not in the middle of a word.
#[test]
fn a_module_frame_round_trips_with_its_spirv_and_reflection() {
    let module = Module {
        generation: 7,
        index: 2,
        count: 6,
        format: format_code("DST_FORMAT_BGRA8"),
        name: "bar_apply".into(),
        spirv: (0u32..64).flat_map(u32::to_le_bytes).collect(),
        reflection: "{\"parameters\":[]}".into(),
    };
    let frame = Frame::Module(module);
    assert_eq!(round_trip(frame.clone()), frame);
}

/// Every way a `Module` body can be wrong, and none of them a panic or a hung
/// bar: a writer's error where the writer could have known, a reader's skip
/// where only the bytes say.
#[test]
fn a_malformed_module_is_a_writers_error_or_a_readers_skip() {
    let good = Module {
        generation: 1,
        index: 0,
        count: 6,
        format: 0,
        name: "bar_compose".into(),
        spirv: vec![1, 2, 3, 4],
        reflection: String::new(),
    };
    // SPIR-V that is not a whole number of words, none at all, no name, an
    // index outside its own generation, a generation past the bound: all
    // writer errors.
    for bad in [
        Module {
            spirv: vec![1, 2, 3],
            ..good.clone()
        },
        Module {
            spirv: Vec::new(),
            ..good.clone()
        },
        Module {
            name: String::new(),
            ..good.clone()
        },
        Module {
            index: 6,
            ..good.clone()
        },
        Module {
            count: MAX_MODULES as u32 + 1,
            ..good.clone()
        },
    ] {
        assert!(
            write(&mut Vec::new(), &Frame::Module(bad.clone())).is_err(),
            "{bad:?}"
        );
    }

    // A body whose `spirv_len` no longer adds up to the bytes present is a
    // skip, not an error: the frame was read whole and the child keeps
    // running.
    let mut raw = bytes(&Frame::Module(good));
    let at = WireHeader::SIZE + std::mem::offset_of!(ModuleHeader, spirv_len);
    raw[at..at + 4].copy_from_slice(&1024u32.to_le_bytes());
    let mut cursor = std::io::Cursor::new(raw);
    assert!(matches!(
        read(&mut cursor).expect("reads"),
        Some(Frame::Skipped(Skipped {
            kind: WIRE_MODULE,
            why: Skip::Length
        }))
    ));
}

/// The two ends never share a table: each hashes the define it holds. So the
/// property that matters is that the same define gives the same word and a
/// different one does not.
#[test]
fn the_format_code_separates_the_two_variants() {
    assert_eq!(
        format_code("DST_FORMAT_RGBA8"),
        format_code("DST_FORMAT_RGBA8")
    );
    assert_ne!(
        format_code("DST_FORMAT_RGBA8"),
        format_code("DST_FORMAT_BGRA8")
    );
}

/// `REQ_LAST` (`REQ_OUTPUT_SIZE`) is the highest kind a `Request` may name, and 4 is `REQ_COVER`: both must pass the bound, and one past it must not.
#[test]
fn the_request_bound_admits_every_kind_the_wire_has_numbered() {
    for kind in [
        REQ_GLYPH,
        REQ_APP,
        REQ_ICON,
        4,
        REQ_PROGRAM,
        REQ_ACTIVATION,
        REQ_OUTPUT_SIZE,
    ] {
        let frame = Frame::Request(Request {
            kind,
            arg: 0,
            name: "x".into(),
        });
        assert_eq!(round_trip(frame.clone()), frame, "kind {kind}");
    }
    let mut buf = Vec::new();
    write(
        &mut buf,
        &Frame::Request(Request {
            kind: REQ_LAST + 1,
            arg: 0,
            name: "x".into(),
        }),
    )
    .expect("writes");
    let mut cursor = std::io::Cursor::new(buf);
    assert!(matches!(
        read(&mut cursor).expect("reads"),
        Some(Frame::Skipped(_))
    ));
}

/// `bar_shape` with an OpenType shaper in it is 1.53 MiB of SPIR-V and
/// 1.73 MiB with its reflection, and a shader only grows. A module of several
/// mebibytes is **one frame**, and it comes back byte-exact.
#[test]
fn a_module_of_several_mebibytes_is_one_frame() {
    const SPIRV: usize = 6 << 20;
    const REFLECTION: usize = 200 << 10;
    let name = "bar_shape";
    let module = Module {
        generation: 7,
        index: 2,
        count: 7,
        format: format_code("DST_FORMAT_RGBA8"),
        name: name.to_owned(),
        spirv: (0..SPIRV).map(|i| (i % 249) as u8).collect(),
        reflection: "{".repeat(REFLECTION),
    };
    let raw = bytes(&Frame::Module(module.clone()));
    assert_eq!(
        raw.len(),
        WireHeader::SIZE + size_of::<ModuleHeader>() + name.len() + SPIRV + REFLECTION
    );
    let mut cursor = std::io::Cursor::new(raw);
    assert_eq!(
        read(&mut cursor).expect("reads"),
        Some(Frame::Module(module)),
        "the module comes back byte for byte"
    );
    assert_eq!(
        read(&mut cursor).expect("reads"),
        None,
        "and it was one frame"
    );
}

// -- Image and Canvas (docs/abi.md, *Image*, *Canvas*) -------------------------

/// The bytes of one frame with its header's `len` and the head's words
/// rewritten, for the reader-rule tests: a writer refuses what these build.
fn with_words(frame: &Frame, words: &[(usize, u32)], trailing: &[u8]) -> Vec<u8> {
    let mut raw = bytes(frame);
    for (at, word) in words {
        let at = WireHeader::SIZE + at;
        raw[at..at + 4].copy_from_slice(&word.to_le_bytes());
    }
    raw.extend_from_slice(trailing);
    let len = (raw.len() - WireHeader::SIZE) as u32;
    raw[8..12].copy_from_slice(&len.to_le_bytes());
    raw
}

fn read_one(raw: Vec<u8>) -> Frame {
    read(&mut std::io::Cursor::new(raw))
        .expect("reads")
        .expect("a frame")
}

fn image(key: &str, width: u32, height: u32) -> Image {
    Image {
        key: key.into(),
        width,
        height,
        rgba: (0..width * height * 4).map(|i| (i % 251) as u8).collect(),
    }
}

/// *Image*: a small image is one frame, and it reads back whole.
#[test]
fn a_small_image_round_trips() {
    let frame = Frame::Image(image("caustics-wallpaper:1-1", 3, 2));
    assert_eq!(round_trip(frame.clone()), frame);
}

/// An image the size of an output is tens of mebibytes and still one frame:
/// 2880 by 1800 is 20.7 MB of RGBA8.
#[test]
fn an_output_sized_image_is_one_frame() {
    let picture = Image {
        key: "caustics-wallpaper:1-2".into(),
        width: 2880,
        height: 1800,
        rgba: (0..2880 * 1800 * 4).map(|i| (i % 251) as u8).collect(),
    };
    let raw = bytes(&Frame::Image(picture.clone()));
    assert_eq!(
        raw.len(),
        WireHeader::SIZE + size_of::<ImageHeader>() + picture.key.len() + 2880 * 1800 * 4
    );
    let mut cursor = std::io::Cursor::new(raw);
    assert_eq!(
        read(&mut cursor).expect("reads"),
        Some(Frame::Image(picture))
    );
    assert_eq!(
        read(&mut cursor).expect("reads"),
        None,
        "and it was one frame"
    );
}

/// *Image*'s reader rules: a side of zero or past the cap and a key past the
/// name cap are each a skip (body); pixels that are not the size's are a skip
/// (length). Never a frame.
#[test]
fn a_malformed_image_is_skipped_rule_by_rule() {
    let frame = Frame::Image(image("k", 2, 2));
    // ImageHeader: key_len 0, width 4, height 8.
    let cases: [(&str, Vec<u8>); 6] = [
        ("width 0", with_words(&frame, &[(4, 0)], &[])),
        (
            "width past the cap",
            with_words(&frame, &[(4, IMAGE_MAX_PX + 1)], &[]),
        ),
        (
            "height past the cap",
            with_words(&frame, &[(8, IMAGE_MAX_PX + 1)], &[]),
        ),
        (
            "fewer pixels than the size",
            with_words(&frame, &[(8, 3)], &[]),
        ),
        (
            "more pixels than the size",
            with_words(&frame, &[], &[0; 4]),
        ),
        (
            "a key past the name cap",
            with_words(&frame, &[(0, MAX_NAME_LEN as u32 + 1)], &[]),
        ),
    ];
    for (what, raw) in cases {
        assert!(
            matches!(
                read_one(raw),
                Frame::Skipped(Skipped {
                    kind: WIRE_IMAGE,
                    ..
                })
            ),
            "{what} was not skipped"
        );
    }
    // And the writer refuses what the reader would skip.
    for bad in [
        Image {
            key: String::new(),
            ..image("k", 1, 1)
        },
        Image {
            width: 0,
            ..image("k", 1, 1)
        },
        Image {
            width: IMAGE_MAX_PX + 1,
            ..image("k", 1, 1)
        },
        Image {
            rgba: vec![0; 8],
            ..image("k", 1, 1)
        },
    ] {
        assert!(
            write(&mut Vec::new(), &Frame::Image(bad.clone())).is_err(),
            "{bad:?}"
        );
    }
}

/// *Canvas*: the three commands round-trip, and a show's head carries its
/// start as two words, low first, so a start past 2^32 ms survives.
#[test]
fn the_canvas_commands_round_trip() {
    let start = (1u64 << 32) + 1234;
    for command in [
        Canvas::Show {
            key: "caustics-wallpaper:7-2".into(),
            fade_start_ms: start,
            fade_duration_ms: 4000,
        },
        Canvas::Dim {
            alpha_milli: 600,
            start_ms: start,
            duration_ms: 30_000,
        },
        Canvas::Undim,
    ] {
        assert_eq!(
            round_trip(Frame::Canvas(command.clone())),
            Frame::Canvas(command)
        );
    }
    let body = Canvas::Show {
        key: "k".into(),
        fade_start_ms: start,
        fade_duration_ms: 1,
    }
    .body();
    assert_eq!((body.start_lo, body.start_hi), (1234, 1));
    assert_eq!(body.start_ms(), start);
    assert_eq!(body.key_len, 1);
}

/// *Canvas*'s reader rules: an unknown command, a show with no key, a key on
/// a dim or an undim, and a dim past all of it are skips.
#[test]
fn a_malformed_canvas_command_is_skipped_rule_by_rule() {
    let show = Frame::Canvas(Canvas::Show {
        key: "k".into(),
        fade_start_ms: 0,
        fade_duration_ms: 0,
    });
    let dim = Frame::Canvas(Canvas::Dim {
        alpha_milli: 500,
        start_ms: 0,
        duration_ms: 0,
    });
    let undim = Frame::Canvas(Canvas::Undim);
    // CanvasBody: command 0, key_len 4, alpha_milli 20.
    let cases: [(&str, Vec<u8>); 6] = [
        ("command 0", with_words(&undim, &[(0, 0)], &[])),
        (
            "a command past the last",
            with_words(&undim, &[(0, CANVAS_LAST + 1)], &[]),
        ),
        ("a show with no key", with_words(&show, &[(4, 0)], &[])),
        ("a key on a dim", with_words(&dim, &[(4, 1)], b"k")),
        ("a key on an undim", with_words(&undim, &[(4, 1)], b"k")),
        (
            "a dim past all of it",
            with_words(&dim, &[(20, DIM_MAX_MILLI + 1)], &[]),
        ),
    ];
    for (what, raw) in cases {
        let read = read_one(raw);
        assert!(
            matches!(
                read,
                Frame::Skipped(Skipped {
                    kind: WIRE_CANVAS,
                    ..
                })
            ),
            "{what} was not skipped: {read:?}"
        );
    }
    // A key the head does not count is a wrong length.
    assert!(matches!(
        read_one(with_words(&undim, &[], b"x")),
        Frame::Skipped(Skipped {
            kind: WIRE_CANVAS,
            why: Skip::Length
        })
    ));
    assert!(
        write(
            &mut Vec::new(),
            &Frame::Canvas(Canvas::Dim {
                alpha_milli: DIM_MAX_MILLI + 1,
                start_ms: 0,
                duration_ms: 0,
            })
        )
        .is_err()
    );
}
