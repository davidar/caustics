use super::*;
use smithay_client_toolkit::reexports::calloop::channel::channel;

/// The `None` implementation is absent, takes nothing, and says so.
#[test]
fn no_compositor_is_absent_and_performs_nothing() {
    let (tx, rx) = channel::<Update>();
    let mut none = NoCompositor::start(WindowSet::default(), tx);
    assert!(!none.is_bound());
    assert_eq!(none.name(), NoCompositor::ABSENT);
    assert_eq!(
        rx.try_recv().ok(),
        Some(Update::Absent(NoCompositor::ABSENT.to_owned())),
        "the absence is one Update::Absent, like any other feeder's"
    );
    for action in [
        Action::Activate(4),
        Action::Close(4),
        Action::ToggleOverview,
        Action::Launch {
            argv: vec!["/usr/bin/kitty".to_owned()],
        },
    ] {
        assert!(
            !none.perform(&action),
            "nothing is performed and nothing is claimed"
        );
    }
}

/// The fourth request: a launch reaching a bar that has no compositor IPC at
/// all is dropped, and — the point — nothing here forks it as a consolation.
///
/// `NoCompositor` answering `false` is what `wayland::Bar::launch` reads to
/// print `action dropped: Launch (no compositor to spawn it)`; a `true` here
/// would silently swallow every dock click on such a machine.
#[test]
fn a_launch_with_no_compositor_is_dropped_and_not_forked() {
    let (tx, _rx) = channel::<Update>();
    let mut none = NoCompositor::start(WindowSet::default(), tx);
    let argv = vec![
        "/usr/bin/gio".to_owned(),
        "launch".to_owned(),
        "/x/kitty.desktop".to_owned(),
    ];
    assert!(
        !none.perform(&Action::Launch { argv }),
        "unclaimed, so the embassy gets to say why in one line of its own"
    );
}
