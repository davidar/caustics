/// The entry points of the bar caustic, in the order `Gpu::reload_all` (bar/src/gpu.rs) takes them.
///
/// One list, four readers: `bar/build.rs` bakes it, `caustics-reload` recompiles
/// it, `bar/src/gpu.rs` names the pipelines from it, and the compile test in `bar/src/abi/tests.rs` walks it. Written out four times, the same strings could
/// drift apart; written once, they cannot.
///
/// The list is in **dispatch order**, which is why the three model passes lead
/// it. `bar_build` turns the facts into the item and block tables,
/// `bar_compose` writes the clock's bytes into the strings buffer and
/// `bar_shape` shapes them and every other block into the text metrics
/// `bar_apply`'s layout measures — a frame that ran any of the four the other way round
/// would lay out last rebuild's table, shape last minute's clock, or size
/// every chip around last frame's string.
///
/// It lives in the **ABI crate** because the compiler sidecar compiles every
/// entry in it and names each `Module` frame after one, and a sidecar that
/// carried its own copy of the list would compile a program the bar cannot
/// assemble. `bar/build.rs` still reaches it by `include!` rather than `use`,
/// because a build script cannot depend on the crate it builds — one list,
/// one `pub use`, one inclusion, no copy.
pub const BAR_ENTRIES: [&str; 7] = [
    "bar_build",
    "bar_compose",
    "bar_shape",
    "bar_apply",
    "bar_visibility",
    "bar_sample",
    "bar_draw",
];

/// The entry points of the canvas caustic (`caustics/canvas.slang`), in
/// dispatch order: `canvas_shape`, in a submission of its own whenever the
/// text or the font changed, then per frame `canvas_apply` once and
/// `canvas_draw` over the surface being drawn (docs/canvas.md, *Time*).
///
/// Here beside the bar's for the same reason: `canvas/build.rs` bakes it,
/// `caustics-reload` compiles it when the canvas host asks for its program
/// (`PROGRAM_CANVAS`), and the canvas names its pipelines from it.
pub const CANVAS_ENTRIES: [&str; 3] = ["canvas_shape", "canvas_apply", "canvas_draw"];
