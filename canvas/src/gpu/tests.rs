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

/// `canvas_draw` declares the surface constant in both formats' modules, and
/// set to each surface it is a module of its own that `spirv-val` takes.
#[test]
fn every_surface_s_draw_is_a_module_of_its_own() {
    let [draw] = SPECIALISED;
    assert_eq!(draw.entry, DRAW, "the draw is what is specialised");
    for format in [
        wgpu::TextureFormat::Rgba8Unorm,
        wgpu::TextureFormat::Bgra8Unorm,
    ] {
        let spirv = baked_spv(format).expect("a baked format");
        let mut modules = Vec::new();
        for surface in draw.values {
            let module = caustics_host::spirv::specialise(spirv[DRAW], draw.id, surface.value)
                .unwrap_or_else(|err| panic!("{}: {err:#}", surface.name))
                .unwrap_or_else(|| panic!("canvas_draw declares no SpecId {}", draw.id));
            if let Some(Err(err)) = caustics_host::spirv::validate(&module) {
                panic!("canvas_draw[{}] ({format:?}): {err}", surface.name);
            }
            modules.push(module);
        }
        modules.sort();
        modules.dedup();
        assert_eq!(modules.len(), draw.values.len(), "one module per surface");
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
    gpu.write_params(&target.params, SIDE, SIDE, 120, lo, hi, role, 1);
    gpu.shape_if_pending();
    gpu.encode(&target.params, &view, SIDE, SIDE, role);
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
    image_at(gpu, width, height, role, 1, 1_000)
}

/// A whole frame of `role` on pane `pane`, drawn at `now` ms, at `width` x
/// `height`, scale 1, as RGBA8.
fn image_at(
    gpu: &mut Gpu,
    width: u32,
    height: u32,
    role: u32,
    pane: u32,
    now: u64,
) -> Vec<[u8; 4]> {
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
    let (lo, hi) = clock::words(now);
    gpu.write_params(&target.params, width, height, 120, lo, hi, role, pane);
    gpu.shape_if_pending();
    gpu.encode(&target.params, &view, width, height, role);
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

// -- On a device: the wake ------------------------------------------------------

/// A canvas showing `image` in slot A, cut in, with the pointer nowhere.
fn showing(image: &Image) -> (Gpu, CanvasFacts) {
    let mut gpu = Gpu::headless().expect("a Vulkan adapter (lavapipe will do)");
    gpu.upload(SLOT_A, image).expect("uploads");
    let facts = CanvasFacts {
        show: Canvas::Show {
            key: image.key.clone(),
            fade_start_ms: 0,
            fade_duration_ms: 0,
        }
        .body(),
        from_slot: NO_INDEX,
        to_slot: SLOT_A,
        shows: 1,
        ..CanvasFacts::default()
    };
    gpu.write_facts(&facts);
    (gpu, facts)
}

/// How many pixels of `a` and `b` differ, inside columns `x0..x1`.
fn differing(a: &[[u8; 4]], b: &[[u8; 4]], width: u32, x0: u32, x1: u32) -> usize {
    a.iter()
        .zip(b)
        .enumerate()
        .filter(|(i, (p, q))| {
            let x = *i as u32 % width;
            x >= x0 && x < x1 && p[..3] != q[..3]
        })
        .count()
}

/// The wake on a lock surface: the pointer moving on its pane stirs the
/// water around its path and nowhere far from it, the pane asks for frames
/// while it does, another pane drawn at the same moment is the plain lock,
/// a press drops a wave of its own, and `WAKE_SETTLE_S` after the last
/// source the lock is the resting picture pixel for pixel and asks for
/// nothing. The prompt's pane, under the same pointer, is no pool.
#[test]
fn the_wake_stirs_the_lock_where_the_pointer_moves_and_settles_to_the_picture() {
    use caustics_bar_abi::canvas::{TARGET_LOCK, TARGET_PROMPT};
    if !gated() {
        return;
    }
    let (mut gpu, mut facts) = showing(&solid("blue", [0, 0, 255, 255]));
    let (width, height) = (640u32, 200u32);
    let settle_ms = (tunable("WAKE_SETTLE_S")[0] * 1000.0) as u64;
    let rest = image_at(&mut gpu, width, height, TARGET_LOCK, 1, 10_000);
    assert_eq!(
        gpu.mirror().wants_redraw,
        0,
        "a still lock asks for nothing"
    );
    assert_ne!(gpu.mirror().wants_pointer, 0, "the lock wants the pointer");

    // A sweep along y = 100 from x = 40, 16 px a frame at 60 frames a
    // second (960 px/s), half a second long.
    let mut now = 10_000u64;
    let mut moving = Vec::new();
    for k in 0..30u32 {
        now = 10_000 + u64::from(k) * 16;
        facts.pointer_pane = 1;
        facts.pointer_x = 40.0 + 6.0 * k as f32;
        facts.pointer_y = 100.0;
        gpu.write_facts(&facts);
        moving = image_at(&mut gpu, width, height, TARGET_LOCK, 1, now);
    }
    let end_x = 40 + 6 * 29;
    assert_ne!(
        gpu.mirror().wants_redraw,
        0,
        "a stirred lock asks for its next frame"
    );
    let near = differing(&moving, &rest, width, 0, end_x + 60);
    assert!(
        near > 2_000,
        "the water moved around the path: {near} px differ"
    );
    let far = differing(&moving, &rest, width, width - 40, width);
    assert_eq!(far, 0, "nothing reaches the far end yet: {far} px differ");

    // Another pane, at the same moment: its own water is flat.
    let other = image_at(&mut gpu, width, height, TARGET_LOCK, 2, now);
    assert_eq!(
        differing(&other, &rest, width, 0, width),
        0,
        "pane 2 shows pane 1's wake"
    );
    assert_eq!(
        gpu.mirror().wants_redraw,
        0,
        "pane 2 asks for frames for pane 1's wake"
    );

    // The prompt's pane is no pool, whoever's pointer is on it.
    let prompt_rest = image_at(&mut gpu, width, height, TARGET_PROMPT, 3, now);
    facts.pointer_pane = 3;
    for k in 0..20u32 {
        facts.pointer_x = 40.0 + 12.0 * k as f32;
        gpu.write_facts(&facts);
        let prompt = image_at(
            &mut gpu,
            width,
            height,
            TARGET_PROMPT,
            3,
            now + u64::from(k) * 16,
        );
        assert_eq!(differing(&prompt, &prompt_rest, width, 0, width), 0);
        assert_eq!(
            gpu.mirror().wants_pointer,
            0,
            "the prompt's pane wants no pointer"
        );
    }

    // A press, back on the lock, after the sweep's sources have died: a
    // wave at the pointer, at once.
    facts.pointer_pane = 1;
    facts.pointer_x = 500.0;
    gpu.write_facts(&facts);
    now += settle_ms + 100;
    let arrived = image_at(&mut gpu, width, height, TARGET_LOCK, 1, now);
    assert_eq!(
        differing(&arrived, &rest, width, 0, width),
        0,
        "arriving is not moving"
    );
    facts.presses += 1;
    gpu.write_facts(&facts);
    let _ = image_at(&mut gpu, width, height, TARGET_LOCK, 1, now + 16);
    let pressed = image_at(&mut gpu, width, height, TARGET_LOCK, 1, now + 300);
    let ring = differing(&pressed, &rest, width, 400, width);
    assert!(ring > 2_000, "a press drops a wave: {ring} px differ");
    assert_ne!(gpu.mirror().wants_redraw, 0);

    // And WAKE_SETTLE_S after it, the resting lock, which asks for nothing.
    let settled = image_at(
        &mut gpu,
        width,
        height,
        TARGET_LOCK,
        1,
        now + 16 + settle_ms + 20,
    );
    assert_eq!(
        differing(&settled, &rest, width, 0, width),
        0,
        "the water is flat again"
    );
    assert_eq!(
        gpu.mirror().wants_redraw,
        0,
        "a settled lock asks for nothing"
    );
}

/// Not a check: with `CAUSTICS_CANVAS_WAKE_DUMP=<dir>`, a pointer's sweep
/// across a 1920x1080 background and a lock, drawn frame by frame, the
/// frames timed (submission to readback, the host's own wait) and a few
/// written as PPMs for looking at. `CAUSTICS_CANVAS_WAKE_PICTURE` names a
/// binary PPM to draw the water over instead of a made-up picture.
#[test]
fn the_wake_drawn_for_a_look() {
    use caustics_bar_abi::canvas::TARGET_LOCK;
    if !gated() {
        return;
    }
    let Some(dir) = std::env::var_os("CAUSTICS_CANVAS_WAKE_DUMP").map(PathBuf::from) else {
        eprintln!("skipping: set CAUSTICS_CANVAS_WAKE_DUMP=<dir> to look at the wake");
        return;
    };
    std::fs::create_dir_all(&dir).expect("the dump directory");
    let picture = match std::env::var_os("CAUSTICS_CANVAS_WAKE_PICTURE") {
        Some(path) => read_ppm(Path::new(&path)),
        None => made_up_picture(480, 270),
    };
    let (width, height) = (1920u32, 1080u32);
    let settle_ms = (tunable("WAKE_SETTLE_S")[0] * 1000.0) as u64;
    for (name, role) in [("background", TARGET_BACKGROUND), ("lock", TARGET_LOCK)] {
        let (mut gpu, mut facts) = showing(&picture);
        let target = gpu.configure(None, width, height);
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("canvas-test-wake-dst"),
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
        // A sweep in an arc, 1400 px/s, for 1.2 s; then a click; then rest.
        let frames = 72u32;
        let mut times = Vec::new();
        let draw = |gpu: &mut Gpu, now: u64, times: &mut Vec<f64>| {
            let (lo, hi) = clock::words(now);
            gpu.write_params(&target.params, width, height, 120, lo, hi, role, 1);
            let t0 = std::time::Instant::now();
            gpu.encode(&target.params, &view, width, height, role);
            gpu.finish().expect("the frame completes");
            times.push(t0.elapsed().as_secs_f64() * 1000.0);
        };
        draw(&mut gpu, 100_000, &mut Vec::new());
        for k in 0..frames {
            let t = k as f32 / 60.0;
            facts.pointer_pane = 1;
            facts.pointer_x = 300.0 + 1400.0 * t * 0.8;
            facts.pointer_y = 540.0 + 260.0 * (t * 3.0).sin();
            if k == frames - 1 {
                facts.presses += 1;
            }
            gpu.write_facts(&facts);
            let now = 100_000 + u64::from(k) * 16;
            draw(&mut gpu, now, &mut times);
            if k == 36 || k == frames - 1 {
                dump(
                    &gpu,
                    &texture,
                    width,
                    height,
                    &dir.join(format!("{name}-{k}.ppm")),
                );
            }
        }
        let end = 100_000 + u64::from(frames) * 16;
        for (label, after) in [("after-400ms", 400), ("after-900ms", 900)] {
            draw(&mut gpu, end + after, &mut times);
            dump(
                &gpu,
                &texture,
                width,
                height,
                &dir.join(format!("{name}-{label}.ppm")),
            );
        }
        draw(&mut gpu, end + settle_ms + 50, &mut Vec::new());
        let settled = gpu.mirror().wants_redraw;
        times.sort_by(f64::total_cmp);
        let mean = times.iter().sum::<f64>() / times.len() as f64;
        eprintln!(
            "wake on the {name}, {width}x{height}: {} stirred frames, mean {mean:.2} ms, median {:.2} ms, \
             worst {:.2} ms; settled: wants_redraw {settled}",
            times.len(),
            times[times.len() / 2],
            times[times.len() - 1]
        );
    }
}

/// A frame's texture, written as a binary PPM.
fn dump(gpu: &Gpu, texture: &wgpu::Texture, width: u32, height: u32, path: &Path) {
    let row = (width * 4).next_multiple_of(256);
    let out = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("canvas-test-wake-readback"),
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
    let mut ppm = format!("P6 {width} {height} 255\n").into_bytes();
    for y in 0..height {
        for x in 0..width {
            ppm.extend_from_slice(&words[(y * row / 4 + x) as usize].to_le_bytes()[..3]);
        }
    }
    std::fs::write(path, ppm).expect("the dump is written");
}

/// A binary PPM, as an opaque picture.
fn read_ppm(path: &Path) -> Image {
    let bytes = std::fs::read(path).expect("the picture reads");
    let mut fields = Vec::new();
    let mut at = 0;
    while fields.len() < 4 {
        while bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        let start = at;
        while !bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        fields.push(String::from_utf8_lossy(&bytes[start..at]).into_owned());
    }
    assert_eq!(fields[0], "P6", "a binary PPM");
    let (width, height): (u32, u32) = (fields[1].parse().unwrap(), fields[2].parse().unwrap());
    let rgb = &bytes[at + 1..];
    Image {
        key: "picture".into(),
        width,
        height,
        rgba: rgb
            .chunks(3)
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
    }
}

/// A picture with something in it to see bent: a dusk gradient, soft hills
/// and a grid of pale dots.
fn made_up_picture(width: u32, height: u32) -> Image {
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let (u, v) = (x as f32 / width as f32, y as f32 / height as f32);
            let hill = 0.62 + 0.08 * (u * 9.0).sin() + 0.05 * (u * 23.0 + 1.0).sin();
            let mut c = if v > hill {
                [0.10 + 0.10 * u, 0.22 + 0.12 * v, 0.20]
            } else {
                [0.30 + 0.45 * v, 0.25 + 0.25 * v, 0.55 - 0.25 * v]
            };
            if x % 24 < 3 && y % 24 < 3 {
                c = [0.9, 0.9, 0.85];
            }
            rgba.extend(c.iter().map(|c| (c * 255.0) as u8));
            rgba.push(255);
        }
    }
    Image {
        key: "made-up".into(),
        width,
        height,
        rgba,
    }
}

/// A reload is queued on the compile workers, never built on the caller:
/// the canvas that runs keeps drawing while the next one builds, a
/// generation superseded before it was whole is never swapped in, and the
/// one that superseded it is.
#[test]
fn a_reload_builds_beside_the_running_canvas() {
    if !gated() {
        return;
    }
    let mut gpu = fading(0, 1000);
    let spirv = baked_spv(gpu.presentation.format).expect("a baked format");
    let older = gpu
        .reload(baked_reflection(), spirv)
        .expect("the baked bindings are the baked bindings");
    let newer = gpu
        .reload(baked_reflection(), spirv)
        .expect("the baked bindings are the baked bindings");
    assert!(newer > older, "each reload is a generation of its own");
    assert!(
        gpu.program.reloading(),
        "nothing is swapped until it is polled"
    );
    assert!(
        gpu.can_draw(TARGET_BACKGROUND),
        "the running canvas still draws"
    );
    let (before, _) = frame_at(&mut gpu, 500);
    assert_eq!(
        gpu.program.generation(),
        1,
        "the baked canvas drew that frame"
    );

    gpu.program
        .wait_staged()
        .expect("the newer generation builds and swaps in");
    assert!(!gpu.program.reloading());
    assert_eq!(
        gpu.program.generation(),
        newer,
        "the superseded generation never ran"
    );
    let (after, _) = frame_at(&mut gpu, 500);
    assert!(after > before, "and the swapped one draws");
}
