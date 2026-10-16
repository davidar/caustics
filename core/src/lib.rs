//! caustics-core — the parts of the project that are not policy and not an embassy.
//!
//! A *caustic* is a Slang compute program that makes every decision about
//! what is on screen; the CPU process that runs it (the *embassy*) only owns
//! the Wayland objects and copies bytes (docs/architecture.md, *The model*). The bar process, its sidecars and the shaping and curve crates are built on this crate. What they share is the machinery around a caustic, and that is all this crate is: the slangc driver (compile, reflect, validate), the
//! byte views that upload plain structs to the GPU, the mtime watch under a
//! live reload, the swapchain formats a caustic can be baked for, the
//! declare-once ABI renderer, and the wgpu helpers (pipelines, buffers,
//! acquiring an image). One copy of each.
//!
//! Nothing here decides anything. It owns no Wayland object, sits on no
//! configure round trip, and forwards no event, so it belongs in no embassy: the bar links it and its embassy does not grow by it.
//!
//! # The modules
//!
//! - [`slang`] — the slangc driver: compile, reflect, validate. Used by bar/build.rs, by the caustics-reload sidecar, and by the layout tests.
//! - [`plain`] — the `Plain` trait and its three byte views, the only unsafe
//!   code on the upload path.
//! - [`watch`] — the mtime poller under caustics-reload's loop. Deliberately calloop-free: the timer is three lines in the caller.
//! - [`format`] — the swapchain formats a destination storage image can be
//!   baked for, and the slangc flags that select one. Always compiled, because
//!   build scripts need it and cannot see the `gpu` feature.
//! - [`gpu`] — the wgpu-typed helpers, behind the `gpu` feature.
//! - [`decl`] — **declared once**: the two `macro_rules!` an ABI struct or
//!   constant is written through, and the renderer that turns their tables into
//!   a checked-in Slang module (docs/abi.md, *The two contracts*). It lives here rather than
//!   in `caustics-bar-abi` because `caustics-shape` declares an ABI too and has no
//!   business depending on the bar's.

pub mod decl;
pub mod format;
#[cfg(feature = "gpu")]
pub mod gpu;
pub mod plain;
pub mod slang;
pub mod watch;

pub use format::DstFormat;
