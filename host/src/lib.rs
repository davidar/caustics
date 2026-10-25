//! caustics-host: what a program needs to host a caustic, lifted out of the
//! bar process so a second program can host one without copying `bar/src`.
//!
//! A host owns four things a caustic cannot do without: a wgpu device on
//! Vulkan; the caustic itself, a Slang program compiled to SPIR-V and loaded
//! through passthrough, with bind groups built from the compiler's
//! reflection; the hot-reload swap that replaces that program whole or not at
//! all; and a set of sidecar child processes whose stdout is the wire
//! (docs/abi.md, *The wire*). This crate is the mechanism of those four, and
//! the small CPU-side tables that go with them: the image slots, the literal
//! table, the witness printer and the `/proc` reader.
//!
//! **The rule for what belongs here: mechanism, never policy.** Nothing in
//! this crate names an entry point, a tunable, a sidecar binary or one of a
//! host's own buffers. Each of those is a parameter, passed by the program
//! that knows it: the entry list comes from the ABI crate's `ENTRIES`, a
//! binding's resource from a closure the host matches on the reflected name,
//! the default sidecar set from the host's own constant. A function that would
//! have to know which buffer is which stays in the host.
//!
//! Two kinds of name are historical and stay as they were, because installed
//! drop-ins and journal filters depend on them. The environment variables are
//! still `CAUSTICS_BAR_ALLOW_DGPU` ([`device`]) and `CAUSTICS_SIDECARS`
//! ([`pipe`]). The witness printer still logs under the `caustics_bar`,
//! `caustics_bar::layout` and `caustics_bar::shape` targets ([`witness`]).
//!
//! docs/architecture.md, *The bar process*, is the design this follows. The
//! bar (`bar/`) is this crate's first client, and the code here is the code
//! the bar ran before it was a client, moved rather than rewritten.

pub mod images;
pub mod modules;
pub mod proc;
pub mod strings;
pub mod witness;
