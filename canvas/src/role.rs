//! What a surface of the canvas is for: its **role**, and everything that
//! differs between one role's surfaces and another's, in one table.
//!
//! Every surface of the canvas is one output's, in one role (docs/canvas.md,
//! *Surfaces*): the **background**, always there, which carries the picture;
//! the **overlay**, there only while a dim is up, which carries the dim over
//! every window and takes the pointer and the keyboard so that any input
//! ends it; and the **top** pane, there only while a prompt is up, which
//! carries the prompt's panel over the darkened wallpaper and takes the
//! keyboard for it. All three are drawn by the one caustic, which is told
//! which it is drawing by `CanvasParams::target`, as the bar's caustic is
//! told whether it draws the bar or its popup.
//!
//! The host's mechanism (a swapchain, a frame pacer, a fractional scale and a
//! viewport per surface) is the same for every role; what is here is what is
//! not: the layer, the anchors and the exclusive zone, the keyboard, the
//! namespace a compositor's rules see, the draw target, and whether input on
//! the surface is a cancel.

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
}

impl Role {
    /// The name the journal uses for this role's surfaces.
    pub fn name(self) -> &'static str {
        match self {
            Role::Background => "background",
            Role::Overlay => "overlay",
            Role::Top => "prompt",
        }
    }

    /// The `zwlr_layer_shell_v1` layer.
    pub fn layer(self) -> Layer {
        match self {
            Role::Background => Layer::Background,
            Role::Overlay => Layer::Overlay,
            Role::Top => Layer::Top,
        }
    }

    pub fn namespace(self) -> &'static str {
        match self {
            Role::Background => NAMESPACE,
            Role::Overlay => NAMESPACE_DIM,
            Role::Top => NAMESPACE_PROMPT,
        }
    }

    /// Every role covers the whole output.
    pub fn anchor(self) -> Anchor {
        Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT
    }

    /// −1 for every role: the canvas never moves another surface, and is
    /// never moved by one's exclusive zone.
    pub fn exclusive_zone(self) -> i32 {
        -1
    }

    /// The background takes no keys. The overlay takes them all, as
    /// chayang's does: a key is how a user says they are back. The prompt
    /// takes them all too: they are what is typed.
    pub fn keyboard(self) -> KeyboardInteractivity {
        match self {
            Role::Background => KeyboardInteractivity::None,
            Role::Overlay | Role::Top => KeyboardInteractivity::Exclusive,
        }
    }

    /// `CanvasParams::target`: what the caustic draws on this role's
    /// surfaces.
    pub fn target(self) -> u32 {
        match self {
            Role::Background => TARGET_BACKGROUND,
            Role::Overlay => TARGET_OVERLAY,
            Role::Top => TARGET_PROMPT,
        }
    }

    /// Whether pointer and touch input on this role's surfaces cancels the
    /// dim (docs/canvas.md, *Surfaces*). The background takes input only
    /// because a compositor gives it some where nothing else is; it means
    /// nothing there; the prompt's is not a cancel either, since Return and
    /// Escape are the whole of a prompt's interaction.
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
