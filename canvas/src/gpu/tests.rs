use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::*;
use caustics_bar_abi::canvas::{STRUCTS, TARGET_BACKGROUND, TARGET_OVERLAY};
use caustics_bar_abi::wire::{CANVAS_SHOW, Canvas};
use punchcutter_slang::Options;

fn caustics_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../caustics")
}

/// Slang's own per-field offsets for every struct of the canvas's contract,
/// against `offset_of!`, **from the declaration table** both ways
/// (docs/abi.md, *Declared once*): a field on one side only fails.
#[test]
fn the_slang_layout_matches_this_one() {
    let caustics = caustics_dir();
    let includes: [&Path; 2] = [&caustics, caustics_bar_abi::rendered_dir()];
    let reflection = punchcutter_slang::reflect(
        &caustics.join("canvas.slang"),
        "canvas_draw",
        &Options {
            defines: &["DST_FORMAT_RGBA8"],
            includes: &includes,
            image_format_unknown: false,
        },
    )
    .unwrap_or_else(|err| panic!("reflection failed:\n{err}"));
    for entry in STRUCTS {
        assert!(entry.bound, "every canvas struct is bound");
        let name = entry.decl.name;
        let actual = reflection
            .structs
            .get(name)
            .unwrap_or_else(|| panic!("Slang reflected no struct named `{name}`"));
        let ours: BTreeMap<&str, usize> = (entry.offsets)().into_iter().collect();
        for (field, offset) in &ours {
            let reported = actual
                .get(*field)
                .copied()
                .unwrap_or_else(|| panic!("`{name}.{field}` is missing from the Slang side"));
            assert_eq!(reported, *offset, "`{name}.{field}`: Slang says {reported}");
        }
        for field in actual.keys() {
            assert!(
                ours.contains_key(field.as_str()),
                "`{name}.{field}` exists in Slang but not in this module"
            );
        }
    }
}

/// Every binding the baked caustic declares is one the bind group has a
/// resource for, and the other way round: a binding added in Slang without
/// an arm here is a panic at the first frame, so it is a failure here first.
#[test]
fn every_baked_binding_is_one_the_host_binds() {
    let names: Vec<&str> = baked_reflection()
        .bindings
        .iter()
        .map(|binding| binding.name.as_str())
        .collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    let mut bound: Vec<&str> = BOUND.iter().chain(ENGINE_BOUND.iter()).copied().collect();
    bound.sort_unstable();
    assert_eq!(sorted, bound);
    for binding in &baked_reflection().bindings {
        let want = if BOUND.contains(&binding.name.as_str()) {
            SET_CANVAS
        } else if binding.name.starts_with("text") {
            SET_TEXT
        } else {
            SET_SHAPE
        };
        assert_eq!(
            binding.set, want,
            "`{}` is in set {}",
            binding.name, binding.set
        );
    }
    assert!(private_stride(&baked_reflection().bindings).expect("a private buffer") >= 16);
}

/// The fade's witnesses are literals of the baked caustic, so the printer can
/// say them from the first frame.
#[test]
fn the_baked_literals_carry_the_fade_s_witnesses() {
    let strings = caustics_host::strings::Strings::baked(BAKED_REFLECTION);
    let all: Vec<&str> = strings.iter().map(|(_, text)| text).collect();
    for want in [
        "midpoint",
        "fading over",
        "no picture yet",
        "done",
        "reached",
    ] {
        assert!(
            all.iter().any(|text| text.contains(want)),
            "no literal with {want:?}: {all:?}"
        );
    }
}

// -- On a device: alpha(t) ----------------------------------------------------
//
// Gated as the bar's oracle is (docs/testing.md, *Three tiers*):
// `CAUSTICS_CANVAS_ORACLE=1 cargo test -p caustics-canvas -- --test-threads=1`
// on any Vulkan adapter, lavapipe included. Without it these return at once,
// so `cargo test --workspace` never creates a Vulkan instance.

fn gated() -> bool {
    if std::env::var_os("CAUSTICS_CANVAS_ORACLE").is_none() {
        eprintln!("skipping: set CAUSTICS_CANVAS_ORACLE=1 to run the canvas on a device");
        return false;
    }
    true
}

const SIDE: u32 = 8;

fn solid(key: &str, rgba: [u8; 4]) -> Image {
    Image {
        key: key.into(),
        width: 4,
        height: 4,
        rgba: rgba.repeat(16),
    }
}

/// A canvas fading from red in slot A to blue in slot B, from `start` over
/// `duration` ms.
fn fading(start: u64, duration: u32) -> Gpu {
    let mut gpu = Gpu::headless().expect("a Vulkan adapter (lavapipe will do)");
    gpu.upload(SLOT_A, &solid("red", [255, 0, 0, 255]))
        .expect("uploads");
    gpu.upload(SLOT_B, &solid("blue", [0, 0, 255, 255]))
        .expect("uploads");
    gpu.write_facts(&CanvasFacts {
        show: Canvas::Show {
            key: "blue".into(),
            fade_start_ms: start,
            fade_duration_ms: duration,
        }
        .body(),
        from_slot: SLOT_A,
        to_slot: SLOT_B,
        shows: 2,
        ..CanvasFacts::default()
    });
    gpu
}

/// One frame of the background at `now` on an offscreen image: the stamp it
/// was drawn under, and its centre texel.
fn frame_at(gpu: &mut Gpu, now: u64) -> (u32, [u8; 4]) {
    frame_on(gpu, now, TARGET_BACKGROUND)
}

/// One frame at `now` of the surface `role` names (`TARGET_*`).
fn frame_on(gpu: &mut Gpu, now: u64, role: u32) -> (u32, [u8; 4]) {
    let target = gpu.configure(None, SIDE, SIDE);
    assert!(
        target.ids.is_none(),
        "the canvas's surfaces make no ids texture"
    );
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("canvas-test-dst"),
        size: wgpu::Extent3d {
            width: SIDE,
            height: SIDE,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let (lo, hi) = clock::words(now);
    gpu.write_params(&target.params, SIDE, SIDE, 120, lo, hi, role);
    gpu.shape_if_pending();
    gpu.encode(&target.params, &view, SIDE, SIDE);
    let stamp = gpu.finish().expect("the frame completes");

    let row = 256u32;
    let out = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("canvas-test-readback"),
        size: u64::from(row * SIDE),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &out,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(SIDE),
            },
        },
        wgpu::Extent3d {
            width: SIDE,
            height: SIDE,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit(Some(encoder.finish()));
    let mut words = vec![0u32; (row * SIDE / 4) as usize];
    caustics_host::surface::read_words(&gpu.device, &out, &mut words);
    let centre = words[((SIDE / 2) * row / 4 + SIDE / 2) as usize];
    (stamp, centre.to_le_bytes())
}

/// `CanvasPrivate`'s fields by name, read back.
fn private_f32(gpu: &Gpu, field: &str) -> f32 {
    let offset = *baked_reflection()
        .structs
        .get("CanvasPrivate")
        .and_then(|fields| fields.get(field))
        .unwrap_or_else(|| panic!("no CanvasPrivate.{field}"));
    let size = private_stride(gpu.program.bindings()).expect("a stride");
    let out = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("canvas-test-private"),
        size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.copy_buffer_to_buffer(&gpu.private, 0, &out, 0, size);
    gpu.queue.submit(Some(encoder.finish()));
    let mut words = vec![0u32; (size / 4) as usize];
    caustics_host::surface::read_words(&gpu.device, &out, &mut words);
    f32::from_bits(words[offset / 4])
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-3
}

/// alpha(t) is a function of the time alone: before the start it is the old
/// picture, at the midpoint of the time it is half of each, at the end the
/// new one, and every frame drawn at the same time is the same frame.
#[test]
fn the_fade_is_at_its_midpoint_at_the_midpoint() {
    if !gated() {
        return;
    }
    let mut gpu = fading(10_000, 4_000);
    let (_, before) = frame_at(&mut gpu, 9_000);
    assert_eq!(
        before,
        [255, 0, 0, 255],
        "before the start: the old picture"
    );
    assert!(close(private_f32(&gpu, "alpha"), 0.0));
    assert_eq!(
        gpu.mirror().wants_redraw,
        1,
        "a fade ahead is a fade in progress"
    );

    let (stamp, mid) = frame_at(&mut gpu, 12_000);
    assert!(close(private_f32(&gpu, "progress"), 0.5));
    assert!(
        close(private_f32(&gpu, "alpha"), 0.5),
        "smoothstep(0.5) is 0.5"
    );
    for (got, want) in mid.iter().zip([128u8, 0, 128, 255]) {
        assert!(got.abs_diff(want) <= 1, "midpoint texel {mid:?}");
    }
    let said: Vec<u32> = gpu
        .mirror()
        .witnesses_of(stamp)
        .iter()
        .map(|note| note.head)
        .collect();
    let strings = caustics_host::strings::Strings::baked(BAKED_REFLECTION);
    assert!(
        said.iter().any(|head| strings
            .get(*head)
            .is_some_and(|text| text.contains("midpoint"))),
        "the midpoint is witnessed on the frame that reaches it"
    );
    // Many frames at one time are one frame: nothing compounds.
    for _ in 0..5 {
        let (_, again) = frame_at(&mut gpu, 12_000);
        assert_eq!(again, mid, "a frame drawn again at the same time moved");
    }

    let (_, after) = frame_at(&mut gpu, 14_000);
    assert_eq!(after, [0, 0, 255, 255], "at the end: the new picture");
    assert_eq!(
        gpu.mirror().wants_redraw,
        0,
        "a finished fade asks for nothing"
    );
}

/// A now that jumps across a suspend lands where the clock says: an hour on,
/// the four-second fade is over; and a fade whose start sits just under a
/// 2^32 ms boundary is at its midpoint just past it.
#[test]
fn a_fade_across_a_suspend_lands_where_the_clock_says() {
    if !gated() {
        return;
    }
    let mut gpu = fading(10_000, 4_000);
    frame_at(&mut gpu, 11_000);
    assert!(close(private_f32(&gpu, "progress"), 0.25));
    frame_at(&mut gpu, 11_000 + 3_600_000);
    assert!(close(private_f32(&gpu, "progress"), 1.0));
    assert_eq!(gpu.mirror().wants_redraw, 0);

    let start = (1u64 << 32) - 2_000;
    let mut gpu = fading(start, 4_000);
    frame_at(&mut gpu, start + 2_000);
    assert!(
        close(private_f32(&gpu, "progress"), 0.5),
        "the two words' borrow"
    );
    frame_at(&mut gpu, start + 5 * 3_600_000);
    assert!(close(private_f32(&gpu, "progress"), 1.0));
}

/// Before any picture the canvas is the dim colour, and a first show fades
/// in from it.
#[test]
fn before_a_picture_the_canvas_is_the_dim_colour() {
    if !gated() {
        return;
    }
    let mut gpu = Gpu::headless().expect("a Vulkan adapter");
    let (_, blank) = frame_at(&mut gpu, 1_000);
    assert_eq!(blank[3], 255, "opaque");
    gpu.upload(SLOT_A, &solid("red", [255, 0, 0, 255]))
        .expect("uploads");
    gpu.write_facts(&CanvasFacts {
        show: Canvas::Show {
            key: "red".into(),
            fade_start_ms: 1_000,
            fade_duration_ms: 1_000,
        }
        .body(),
        from_slot: NO_INDEX,
        to_slot: SLOT_A,
        shows: 1,
        ..CanvasFacts::default()
    });
    assert_eq!(gpu.mirror().wants_redraw, 0);
    let (_, start) = frame_at(&mut gpu, 1_000);
    assert_eq!(start, blank, "the first show starts from the dim colour");
    let (_, end) = frame_at(&mut gpu, 2_000);
    assert_eq!(end, [255, 0, 0, 255]);
    assert_eq!(CANVAS_SHOW, 1);
}

// -- On a device: the dim, on the overlay ---------------------------------------

/// A canvas showing red in slot A, with a dim towards `alpha_milli` from
/// `start` over `duration` ms under `id`.
fn dimming(start: u64, duration: u32, alpha_milli: u32, id: u32) -> Gpu {
    let mut gpu = Gpu::headless().expect("a Vulkan adapter (lavapipe will do)");
    gpu.upload(SLOT_A, &solid("red", [255, 0, 0, 255]))
        .expect("uploads");
    gpu.write_facts(&CanvasFacts {
        show: Canvas::Show {
            key: "red".into(),
            fade_start_ms: 0,
            fade_duration_ms: 0,
        }
        .body(),
        dim: Canvas::Dim {
            id,
            alpha_milli,
            start_ms: start,
            duration_ms: duration,
        }
        .body(),
        from_slot: NO_INDEX,
        to_slot: SLOT_A,
        shows: 1,
        ..CanvasFacts::default()
    });
    gpu
}

/// A tunable's value as `canvas_tunables.slang` spells it: the floats inside
/// a `float3(…)`, or the one float.
fn tunable(name: &str) -> Vec<f32> {
    let text = std::fs::read_to_string(caustics_dir().join("canvas_tunables.slang"))
        .expect("the tunables are readable");
    let line = text
        .lines()
        .find(|line| line.contains(&format!(" {name} = ")))
        .unwrap_or_else(|| panic!("no tunable {name}"));
    let value = line
        .split_once(" = ")
        .expect("an assignment")
        .1
        .trim_end_matches(';')
        .trim_start_matches("float3(")
        .trim_end_matches(')');
    value
        .split(',')
        .map(|word| word.trim().parse().expect("a float"))
        .collect()
}

/// `dim_now` on the overlay: nothing at the start, half the depth at the
/// midpoint of the time, the depth after it, and premultiplied, so its
/// colour is never more than its alpha. At its depth the overlay is the
/// background's own picture, darkened towards `DIM_COLOUR` by `DIM_DARKEN`. The overlay asks for frames while
/// its alpha moves and stops when it is there, and the state says when.
#[test]
fn the_overlay_dims_to_its_depth_over_its_time() {
    if !gated() {
        return;
    }
    let mut gpu = dimming(10_000, 4_000, 1000, 7);
    let (_, start) = frame_on(&mut gpu, 10_000, TARGET_OVERLAY);
    assert_eq!(start, [0, 0, 0, 0], "at the start: transparent");
    assert!(close(private_f32(&gpu, "dim"), 0.0));
    assert_eq!(gpu.mirror().wants_redraw, 1, "a dim on its way");
    assert_eq!(gpu.mirror().dim_reached, 0);

    let (_, mid) = frame_on(&mut gpu, 12_000, TARGET_OVERLAY);
    assert!(close(private_f32(&gpu, "dim"), 0.5));
    assert!(
        mid[3].abs_diff(128) <= 1,
        "half the depth at the midpoint: {mid:?}"
    );
    assert!(
        mid[..3].iter().all(|c| *c <= mid[3]),
        "premultiplied: {mid:?}"
    );
    assert_eq!(gpu.mirror().dim_reached, 0);

    let (stamp, end) = frame_on(&mut gpu, 15_000, TARGET_OVERLAY);
    assert_eq!(end[3], 255, "at its depth, opaque: {end:?}");
    let (dim, darken) = (tunable("DIM_COLOUR"), tunable("DIM_DARKEN")[0]);
    for (channel, picture) in [1.0f32, 0.0, 0.0].into_iter().enumerate() {
        let want = (picture + (dim[channel] - picture) * darken) * 255.0;
        assert!(
            (f32::from(end[channel]) - want).abs() <= 1.5,
            "channel {channel}: {end:?}, want {want:.1} (the red picture, darkened by {darken})"
        );
    }
    assert!(close(private_f32(&gpu, "dim"), 1.0));
    assert_eq!(
        gpu.mirror().wants_redraw,
        0,
        "a dim at its depth asks for nothing"
    );
    assert_eq!((gpu.mirror().dim_reached, gpu.mirror().dim_id), (1, 7));
    let strings = caustics_host::strings::Strings::baked(BAKED_REFLECTION);
    assert!(
        gpu.mirror().witnesses_of(stamp).iter().any(|note| strings
            .get(note.head)
            .is_some_and(|text| text.contains("reached"))),
        "the depth is witnessed on the frame that reaches it"
    );
    // A completed dim stays at its depth.
    let (_, later) = frame_on(&mut gpu, 3_600_000, TARGET_OVERLAY);
    assert_eq!(later, end);
    assert_eq!(gpu.mirror().dim_reached, 1);
}

/// The background no longer carries the dim: mid-dim and at its depth it is
/// the picture, and it asks for no frames for the dim's sake.
#[test]
fn the_background_never_darkens() {
    if !gated() {
        return;
    }
    let mut gpu = dimming(10_000, 4_000, 1000, 1);
    for now in [12_000, 20_000] {
        let (_, texel) = frame_at(&mut gpu, now);
        assert_eq!(texel, [255, 0, 0, 255], "the background at {now} ms");
        assert_eq!(gpu.mirror().wants_redraw, 0, "the background's own word");
    }
    // An undim, and the overlay is nothing at all.
    gpu.write_facts(&CanvasFacts {
        dim: Canvas::Undim.body(),
        to_slot: SLOT_A,
        from_slot: NO_INDEX,
        shows: 1,
        ..CanvasFacts::default()
    });
    let (_, overlay) = frame_on(&mut gpu, 20_000, TARGET_OVERLAY);
    assert_eq!(overlay, [0, 0, 0, 0]);
    assert_eq!(gpu.mirror().dim_reached, 0);
}

// -- On a device: text ----------------------------------------------------------

/// A canvas with a real font in its engine buffers, as its `caustics-glyphs`
/// would send it, and the baked caustic's literal pool.
fn with_font() -> Gpu {
    use caustics_bar_abi::wire::{
        BUF_BAND_CURVES, BUF_BANDS, BUF_GLYPHS, BUF_OT, BUF_POINTS, BUF_SHAPE_FONT,
    };
    let mut gpu = Gpu::headless().expect("a Vulkan adapter (lavapipe will do)");
    let loaded = punchcutter_fonts::Loaded::load_requiring(caustics_glyphs::COMPOSED)
        .expect("a sans-serif font through fontconfig");
    for buffer in [
        BUF_POINTS,
        BUF_BANDS,
        BUF_BAND_CURVES,
        BUF_GLYPHS,
        BUF_OT,
        BUF_SHAPE_FONT,
    ] {
        let bytes = caustics_glyphs::buffer(&loaded, buffer).expect("every buffer");
        gpu.write_bytes(buffer, 0, bytes.len() as u32, &bytes);
    }
    gpu
}

/// A whole frame of `role` at `width` x `height`, scale 1, as RGBA8.
fn image_on(gpu: &mut Gpu, width: u32, height: u32, role: u32) -> Vec<[u8; 4]> {
    let target = gpu.configure(None, width, height);
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("canvas-test-text-dst"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let (lo, hi) = clock::words(1_000);
    gpu.write_params(&target.params, width, height, 120, lo, hi, role);
    gpu.shape_if_pending();
    gpu.encode(&target.params, &view, width, height);
    gpu.finish().expect("the frame completes");
    let row = (width * 4).next_multiple_of(256);
    let out = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("canvas-test-text-readback"),
        size: u64::from(row * height),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &out,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit(Some(encoder.finish()));
    let mut words = vec![0u32; (row * height / 4) as usize];
    caustics_host::surface::read_words(&gpu.device, &out, &mut words);
    let mut pixels = Vec::with_capacity((width * height) as usize);
    for y in 0..height {
        for x in 0..width {
            pixels.push(words[(y * row / 4 + x) as usize].to_le_bytes());
        }
    }
    pixels
}

/// One `float4` of the prompt's layout, `PromptLayout.<field>` inside
/// `CanvasPrivate.prompt`, read back by the reflection's offsets.
fn layout_box(gpu: &Gpu, field: &str) -> [f32; 4] {
    let structs = &baked_reflection().structs;
    let base = structs["CanvasPrivate"]["prompt"];
    let at = base + structs["PromptLayout"][field];
    let size = private_stride(gpu.program.bindings()).expect("a stride");
    let out = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("canvas-test-layout"),
        size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.copy_buffer_to_buffer(&gpu.private, 0, &out, 0, size);
    gpu.queue.submit(Some(encoder.finish()));
    let mut words = vec![0u32; (size / 4) as usize];
    caustics_host::surface::read_words(&gpu.device, &out, &mut words);
    std::array::from_fn(|i| f32::from_bits(words[at / 4 + i]))
}

/// The text pipeline end to end on a device: a title from the prompt's text
/// buffer and the caustic's own literal for an ask with no words of its own
/// are shaped by `canvas_shape` against a real font and drawn by
/// `canvas_draw` on the prompt's pane. Inside each run's box the panel has
/// ink on it; in the gap between the two rows, and beside the title past
/// its end, it is the bare panel colour. `CANVAS_TEST_TITLE` replaces the
/// title (a long one shows the wrap) and `CANVAS_TEST_DUMP` names a PPM to
/// write the pane to, for looking at.
#[test]
fn the_prompt_draws_its_text_on_the_device() {
    use caustics_bar_abi::canvas::{CanvasPrompt, PROMPT_ASKING, TARGET_PROMPT};
    if !gated() {
        return;
    }
    let mut gpu = with_font();
    let strings = caustics_host::strings::Strings::baked(BAKED_REFLECTION);
    let literal_count = gpu.write_literals(&strings);
    assert!(literal_count > 0, "the baked caustic names literals");
    let title = std::env::var("CANVAS_TEST_TITLE").unwrap_or_else(|_| "Canvas text".to_owned());
    let title = title.as_bytes();
    gpu.write_prompt_text(title);
    gpu.write_facts(&CanvasFacts {
        from_slot: NO_INDEX,
        to_slot: NO_INDEX,
        literal_count,
        prompt: CanvasPrompt {
            id: 1,
            state: PROMPT_ASKING,
            title_off: 0,
            title_len: title.len() as u32,
            ..CanvasPrompt::default()
        },
        ..CanvasFacts::default()
    });
    let (width, height) = (480u32, 320u32);
    let pixels = image_on(&mut gpu, width, height, TARGET_PROMPT);
    let at = |x: u32, y: u32| pixels[(y * width + x) as usize];
    if let Some(path) = std::env::var_os("CANVAS_TEST_DUMP") {
        let mut ppm = format!("P6 {width} {height} 255\n").into_bytes();
        for px in &pixels {
            ppm.extend_from_slice(&px[..3]);
        }
        std::fs::write(path, ppm).unwrap();
    }
    let panel: Vec<u8> = tunable("PROMPT_PANEL_COLOUR")
        .iter()
        .map(|c| (c * 255.0).round() as u8)
        .collect();
    let is_panel = |px: [u8; 4]| {
        px[..3]
            .iter()
            .zip(&panel)
            .all(|(got, want)| got.abs_diff(*want) <= 1)
    };

    let title_box = layout_box(&gpu, "title_box");
    let ask_box = layout_box(&gpu, "ask_box");
    for (name, b) in [("title", title_box), ("ask", ask_box)] {
        assert!(
            b[2] - b[0] > 20.0 && b[3] - b[1] > 8.0,
            "the {name}'s run has a width and a line: {b:?}"
        );
        let (mut inked, mut all) = (0u32, 0u32);
        for y in b[1] as u32..b[3] as u32 {
            for x in b[0] as u32..b[2] as u32 {
                all += 1;
                inked += u32::from(!is_panel(at(x, y)));
            }
        }
        assert!(
            inked * 20 > all,
            "the {name}'s box {b:?} has ink on {inked} of {all} pixels"
        );
    }
    for y in (title_box[3] as u32 + 2)..(ask_box[1] as u32).saturating_sub(1) {
        for x in title_box[0] as u32..(title_box[0] as u32 + 200) {
            assert!(
                is_panel(at(x, y)),
                "the gap at ({x}, {y}) is {:?}",
                at(x, y)
            );
        }
    }
    // Beside the title, as far as the panel goes: a title as wide as the
    // panel leaves no room, and the range is empty.
    let panel_box = layout_box(&gpu, "panel");
    let beside_end = (title_box[2] + 60.0).min(panel_box[2] - 2.0) as u32;
    for x in (title_box[2] as u32 + 3)..beside_end {
        let y = (0.5 * (title_box[1] + title_box[3])) as u32;
        assert!(
            is_panel(at(x, y)),
            "past the title at ({x}, {y}): {:?}",
            at(x, y)
        );
    }
    assert_eq!(gpu.mirror().cover_count, 0, "the font draws both runs");
    assert!(!gpu.shape_blocked());
}
