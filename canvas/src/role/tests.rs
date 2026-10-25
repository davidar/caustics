use super::*;

/// The table, cell by cell: the background is the bottom layer and takes no
/// keys; the overlay is the top layer, takes every key, and its input is a
/// cancel. Both cover the output and move nothing.
#[test]
fn each_role_is_its_layer_its_keyboard_and_its_target() {
    assert_eq!(Role::Background.layer(), Layer::Background);
    assert_eq!(Role::Overlay.layer(), Layer::Overlay);
    assert_eq!(
        Role::Background.keyboard(),
        KeyboardInteractivity::None,
        "a wallpaper never takes the keyboard"
    );
    assert_eq!(Role::Overlay.keyboard(), KeyboardInteractivity::Exclusive);
    assert_eq!(Role::Background.target(), TARGET_BACKGROUND);
    assert_eq!(Role::Overlay.target(), TARGET_OVERLAY);
    assert!(!Role::Background.input_cancels());
    assert!(Role::Overlay.input_cancels());
    assert!(Role::Background.sizes_the_picture());
    assert!(!Role::Overlay.sizes_the_picture());
    for role in [Role::Background, Role::Overlay] {
        assert_eq!(
            role.anchor(),
            Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT
        );
        assert_eq!(role.exclusive_zone(), -1);
    }
    assert_ne!(Role::Background.namespace(), Role::Overlay.namespace());
    assert_ne!(Role::Background.target(), Role::Overlay.target());
}
