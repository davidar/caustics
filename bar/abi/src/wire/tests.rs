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
    assert_eq!(size_of::<ModuleHeader>(), 40);
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

/// *Module*. The payload carries three variable runs in a fixed order, so the one
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
    let chunks = module.chunks(WIRE_MAX_BODY);
    assert_eq!(chunks.len(), 1, "a module this size is one frame");
    let frame = Frame::Module(chunks[0].clone());
    assert_eq!(round_trip(frame.clone()), frame);
    assert_eq!(ModuleAssembly::new().push(chunks[0].clone()), Some(module));
}

/// Every way a `Module` body can be wrong, and none of them a panic or a hung
/// bar: a writer's error where the writer could have known, a reader's skip
/// where only the bytes say.
#[test]
fn a_malformed_module_is_a_writers_error_or_a_readers_skip() {
    let whole = Module {
        generation: 1,
        index: 0,
        count: 6,
        format: 0,
        name: "bar_compose".into(),
        spirv: vec![1, 2, 3, 4],
        reflection: String::new(),
    };
    let good = whole.chunks(WIRE_MAX_BODY).remove(0);
    // SPIR-V that is not a whole number of words, an index outside its own
    // generation, a piece that lands outside its payload: all writer errors.
    let ragged = ModuleChunk {
        spirv_len: 3,
        total: good.total - 1,
        ..good.clone()
    };
    assert!(write(&mut Vec::new(), &Frame::Module(ragged)).is_err());
    let outside = ModuleChunk {
        index: 6,
        ..good.clone()
    };
    assert!(write(&mut Vec::new(), &Frame::Module(outside)).is_err());
    let too_many = ModuleChunk {
        index: 0,
        count: MAX_MODULES as u32 + 1,
        ..good.clone()
    };
    assert!(write(&mut Vec::new(), &Frame::Module(too_many)).is_err());
    let ragged_total = ModuleChunk {
        total: good.total + 1,
        ..good.clone()
    };
    assert!(write(&mut Vec::new(), &Frame::Module(ragged_total)).is_err());
    let past_end = ModuleChunk {
        offset: good.total,
        ..good.clone()
    };
    assert!(write(&mut Vec::new(), &Frame::Module(past_end)).is_err());

    // A body whose `spirv_len` no longer adds up to `total` is a skip, not an
    // error: the frame was read whole and the child keeps running.
    let mut raw = bytes(&Frame::Module(good));
    let at = WireHeader::SIZE + std::mem::offset_of!(ModuleHeader, spirv_len);
    raw[at..at + 4].copy_from_slice(&1024u32.to_le_bytes());
    let mut cursor = std::io::Cursor::new(raw);
    assert!(matches!(
        read(&mut cursor).expect("reads"),
        Some(Frame::Skipped(_))
    ));
}

/// The assembly's own rule (docs/abi.md, *Module*): pieces arrive in order, and one that does
/// not continue where the last left off starts the payload over rather than
/// splicing two compiles together.
#[test]
fn a_module_assembly_takes_its_pieces_in_order_and_nothing_else() {
    let module = Module {
        generation: 4,
        index: 1,
        count: 3,
        format: 9,
        name: "bar_shape".into(),
        spirv: (0u32..256).flat_map(u32::to_le_bytes).collect(),
        reflection: "{\"parameters\":[1]}".into(),
    };
    let chunks = module.chunks(size_of::<ModuleHeader>() + 64);
    assert!(chunks.len() > 4, "{} pieces", chunks.len());

    // The tail of a module whose head this reader never saw is dropped.
    let mut assembly = ModuleAssembly::new();
    assert_eq!(assembly.push(chunks[2].clone()), None);
    assert_eq!(assembly.progress(), (0, 0));

    // In order, it completes exactly once, on the last piece.
    for chunk in &chunks[..chunks.len() - 1] {
        assert_eq!(assembly.push(chunk.clone()), None);
    }
    assert_eq!(assembly.generation(), Some(4));
    assert_eq!(
        assembly.push(chunks[chunks.len() - 1].clone()),
        Some(module.clone())
    );

    // A piece out of order forgets what was held; the writer starting again at
    // offset 0 is what recovers it.
    let mut assembly = ModuleAssembly::new();
    assert_eq!(assembly.push(chunks[0].clone()), None);
    assert_eq!(assembly.push(chunks[2].clone()), None);
    assert_eq!(assembly.progress(), (0, 0));
    for chunk in &chunks[..chunks.len() - 1] {
        assert_eq!(assembly.push(chunk.clone()), None);
    }
    assert_eq!(
        assembly.push(chunks[chunks.len() - 1].clone()),
        Some(module)
    );
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

/// `REQ_LAST` (`REQ_ACTIVATION`) is the highest kind a `Request` may name, and 4 is `REQ_COVER`: both must pass the bound, and one past it must not.
#[test]
fn the_request_bound_admits_every_kind_the_wire_has_numbered() {
    for kind in [REQ_GLYPH, REQ_APP, REQ_ICON, 4, REQ_PROGRAM] {
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
/// 1.73 MiB with its reflection — **larger than the cap**. Sent as one frame,
/// `caustics-reload` would refuse the whole generation and the bar would keep running the modules built into its binary, with no hot reload; so `Module` is chunked the way
/// *Bytes*'s `Bytes` is.
///
/// That is what this tests: a module over the cap splits, and the pieces
/// reassemble **byte-exact**. The cap stays at one mebibyte,
/// and a shader may grow past it as far as it likes.
#[test]
fn a_module_larger_than_the_cap_splits_and_reassembles() {
    const BAR_SHAPE_SPIRV: usize = 1_534_000;
    const WITH_REFLECTION: usize = 1_734_140;
    assert!(
        WITH_REFLECTION > WIRE_MAX_BODY,
        "the shaper at {WITH_REFLECTION} bytes is what the cap of {WIRE_MAX_BODY} cannot hold"
    );
    let name = "bar_shape";
    let module = Module {
        generation: 7,
        index: 2,
        count: 7,
        format: format_code("DST_FORMAT_RGBA8"),
        name: name.to_owned(),
        spirv: vec![0x07u8; BAR_SHAPE_SPIRV],
        reflection: "{".repeat(WITH_REFLECTION - BAR_SHAPE_SPIRV - name.len()),
    };
    let chunks = module.chunks(WIRE_MAX_BODY);
    assert_eq!(chunks.len(), 2, "1.73 MiB under a 1 MiB cap is two frames");

    let mut assembly = ModuleAssembly::new();
    let mut back = None;
    for chunk in chunks {
        let frame = Frame::Module(chunk);
        // Each piece is a legal frame on its own, and survives the wire.
        let Frame::Module(read_back) = round_trip(frame) else {
            panic!("not a module")
        };
        back = assembly.push(read_back);
    }
    assert_eq!(back, Some(module), "the pieces reassemble byte for byte");
}
