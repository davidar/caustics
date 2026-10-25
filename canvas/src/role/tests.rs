use super::*;

const LAYERED: [Role; 3] = [Role::Background, Role::Overlay, Role::Top];

fn spec(role: Role) -> LayerSpec {
    role.layer()
        .unwrap_or_else(|| panic!("{role:?} is a layer surface"))
}

/// The table, cell by cell: the background is the bottom layer and takes no
/// keys; the overlay is the overlay layer, takes every key, and its input is
/// a cancel; the prompt is the top layer and takes every key. Every layer
/// role covers the output and moves nothing.
#[test]
fn each_role_is_its_layer_its_keyboard_and_its_target() {
    assert_eq!(spec(Role::Background).layer, Layer::Background);
    assert_eq!(spec(Role::Overlay).layer, Layer::Overlay);
    assert_eq!(
        spec(Role::Background).keyboard,
        KeyboardInteractivity::None,
        "a wallpaper never takes the keyboard"
    );
    assert_eq!(
        spec(Role::Overlay).keyboard,
        KeyboardInteractivity::Exclusive
    );
    assert_eq!(Role::Background.target(), TARGET_BACKGROUND);
    assert_eq!(Role::Overlay.target(), TARGET_OVERLAY);
    assert!(!Role::Background.input_cancels());
    assert!(Role::Overlay.input_cancels());
    assert!(Role::Background.sizes_the_picture());
    assert!(!Role::Overlay.sizes_the_picture());
    for role in LAYERED {
        assert_eq!(
            spec(role).anchor,
            Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT
        );
        assert_eq!(spec(role).exclusive_zone, -1);
        assert!(role.commits_before_configure());
    }
    assert_ne!(
        spec(Role::Background).namespace,
        spec(Role::Overlay).namespace
    );
    assert_ne!(Role::Background.target(), Role::Overlay.target());
    // The prompt: the top layer, every key, its own target, and no cancel.
    assert_eq!(spec(Role::Top).layer, Layer::Top);
    assert_eq!(spec(Role::Top).keyboard, KeyboardInteractivity::Exclusive);
    assert_eq!(Role::Top.target(), TARGET_PROMPT);
    assert_eq!(spec(Role::Top).namespace, "caustics-canvas-prompt");
    assert!(!Role::Top.input_cancels());
    assert!(!Role::Top.sizes_the_picture());
    assert!(Role::Top.carries_prompt());
    assert!(!Role::Background.carries_prompt() && !Role::Overlay.carries_prompt());
}

/// The lock is no layer surface: it has no layer, no namespace, no keyboard
/// mode of its own, and is not committed before its configure (a bufferless
/// commit on a lock surface is a protocol error). It draws the prompt's
/// target, carries the prompt, cancels nothing and sizes no picture.
#[test]
fn the_lock_is_a_lock_surface_with_the_prompts_look() {
    assert_eq!(Role::Lock.layer(), None);
    assert!(!Role::Lock.commits_before_configure());
    assert_eq!(Role::Lock.target(), TARGET_PROMPT);
    assert!(Role::Lock.carries_prompt());
    assert!(!Role::Lock.input_cancels());
    assert!(!Role::Lock.sizes_the_picture());
    assert_eq!(Role::Lock.name(), "lock");
    let names: std::collections::HashSet<_> =
        [Role::Background, Role::Overlay, Role::Top, Role::Lock]
            .iter()
            .map(|role| role.name())
            .collect();
    assert_eq!(names.len(), 4, "the journal tells every role apart");
}
