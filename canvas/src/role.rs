//! What a surface of the canvas is for: its **role**, and everything that
//! differs between one role's surfaces and another's, in one table.
//!
//! Every surface of the canvas is one output's, in one role (docs/canvas.md,
//! *Surfaces*): the **background**, always there, which carries the picture;
//! the **overlay**, there only while a dim is up, which carries the dim over
//! every window and takes the pointer and the keyboard so that any input
//! ends it; and the **top** pane, there only while a prompt is up, which
//! carries the prompt's panel over the darkened wallpaper and takes the
//! keyboard for it; and the **lock** pane, there only while the session is
//! locked, an `ext_session_lock_v1` lock surface rather than a layer surface,
//! which carries the same darkened wallpaper and the prompt's panel when the
//! lock asks for a secret. All four are drawn by the one caustic, which is
//! told which it is drawing by `CanvasParams::target`, as the bar's caustic
//! is told whether it draws the bar or its popup.
//!
//! The host's mechanism (a swapchain, a frame pacer, a fractional scale and a
//! viewport per surface) is the same for every role; what is here is what is
//! not: for a layer surface its layer, anchors, exclusive zone, keyboard and
//! the namespace a compositor's rules see ([`LayerSpec`]); and for every role
//! the draw target and whether input on the surface is a cancel.
//!
//! **Why the layer-only fields are one `Option`.** A lock surface has none of
//! them: the compositor sizes it to its output, gives it the keyboard as it
//! sees fit, and shows it over everything. Rather than five answers that
//! mean nothing for one role, [`Role::layer`] is `None` for the lock, and
//! `wayland.rs` makes a layer surface from a [`LayerSpec`] or a lock surface
//! from the lock, one branch, in one place.

use caustics_bar_abi::canvas::{TARGET_BACKGROUND, TARGET_OVERLAY, TARGET_PROMPT};
use smithay_client_toolkit::shell::wlr_layer::{Anchor, KeyboardInteractivity, Layer};

/// The layer surfaces' namespace for the background: what a compositor's
/// rules and `niri msg layers` call the canvas.
pub const NAMESPACE: &str = "caustics-canvas";

/// The overlay's namespace, apart from the background's so that a rule can
/// tell the dim from the wallpaper (a compositor that blurs or animates
/// layers by namespace, say).
pub const NAMESPACE_DIM: &str = "caustics-canvas-dim";

/// The prompt's namespace: a rule for the panel can tell it from the dim.
pub const NAMESPACE_PROMPT: &str = "caustics-canvas-prompt";

/// One role a surface of the canvas plays on an output.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Role {
    /// The picture, on the bottom layer, opaque, never in the way.
    Background,
    /// The dim, on the overlay layer, from a `CANVAS_DIM` until an undim or
    /// any input.
    Overlay,
    /// The prompt, on the top layer, from a conversation's first ask until
    /// it ends.
    Top,
    /// The lock surface, from the lock request until the unlock: the
    /// darkened wallpaper, and the lock's prompt on it.
    Lock,
}

/// What a layer surface of a role is, to `zwlr_layer_shell_v1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayerSpec {
    pub layer: Layer,
    /// What a compositor's rules and `niri msg layers` call the surface.
    pub namespace: &'static str,
    /// Every layer role covers the whole output.
    pub anchor: Anchor,
    /// −1 for every layer role: the canvas never moves another surface, and
    /// is never moved by one's exclusive zone.
    pub exclusive_zone: i32,
    /// The background takes no keys. The overlay takes them all, as
    /// chayang's does: a key is how a user says they are back. The prompt
    /// takes them all too: they are what is typed.
    pub keyboard: KeyboardInteractivity,
}

impl Role {
    /// The name the journal uses for this role's surfaces.
    pub fn name(self) -> &'static str {
        match self {
            Role::Background => "background",
            Role::Overlay => "overlay",
            Role::Top => "prompt",
            Role::Lock => "lock",
        }
    }

    /// The layer surface this role is, or `None` for the lock's, which is
    /// a lock surface (the module's note says why one `Option`).
    pub fn layer(self) -> Option<LayerSpec> {
        let (layer, namespace, keyboard) = match self {
            Role::Background => (Layer::Background, NAMESPACE, KeyboardInteractivity::None),
            Role::Overlay => (
                Layer::Overlay,
                NAMESPACE_DIM,
                KeyboardInteractivity::Exclusive,
            ),
            Role::Top => (
                Layer::Top,
                NAMESPACE_PROMPT,
                KeyboardInteractivity::Exclusive,
            ),
            Role::Lock => return None,
        };
        Some(LayerSpec {
            layer,
            namespace,
            anchor: Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
            exclusive_zone: -1,
            keyboard,
        })
    }

    /// Whether the surface is committed with nothing attached as it is made,
    /// so that the compositor configures it: a layer surface's rule. A lock
    /// surface is configured as it is made, and a commit without a buffer on
    /// one is a protocol error (`ext_session_lock_surface_v1.null_buffer`),
    /// so its first commit is its first frame.
    pub fn commits_before_configure(self) -> bool {
        self.layer().is_some()
    }

    /// Whether the prompt's panel is drawn on this role's surfaces: the top
    /// pane's, and the lock's while locked.
    pub fn carries_prompt(self) -> bool {
        matches!(self, Role::Top | Role::Lock)
    }

    /// `CanvasParams::target`: what the caustic draws on this role's
    /// surfaces.
    pub fn target(self) -> u32 {
        match self {
            Role::Background => TARGET_BACKGROUND,
            Role::Overlay => TARGET_OVERLAY,
            // The lock is the prompt's look: the wallpaper darkened by
            // `PROMPT_DARKEN`, opaque, and the panel while a conversation
            // asks.
            Role::Top | Role::Lock => TARGET_PROMPT,
        }
    }

    /// Whether pointer and touch input on this role's surfaces cancels the
    /// dim (docs/canvas.md, *Surfaces*). The background takes input only
    /// because a compositor gives it some where nothing else is; it means
    /// nothing there; the prompt's and the lock's are not a cancel either,
    /// since Return and Escape are the whole of a prompt's interaction.
    pub fn input_cancels(self) -> bool {
        matches!(self, Role::Overlay)
    }

    /// Whether the size the wallpaper sidecar fits its pictures to counts
    /// this role's surfaces: only the surface the picture is drawn on.
    pub fn sizes_the_picture(self) -> bool {
        matches!(self, Role::Background)
    }
}

#[cfg(test)]
mod tests;
