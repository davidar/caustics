use super::*;

/// The embassy's wiring, compiled in isolation.
///
/// Without this the `delegate_feeders!` expansion and the `bind` bounds would
/// be checked only through `Bar`, where a broken bound reads as an error in
/// the embassy. `Probe` is exactly what `Bar` is for these protocols: a state type with one `AsMut<Feeders>` and one macro
/// call. The function is never run — a `QueueHandle` needs a connection —
/// but it is type-checked, which is the whole point.
mod wiring {
    use super::*;
    use wayland_client::QueueHandle;
    use wayland_client::globals::GlobalList;

    struct Probe {
        feeders: Feeders,
    }

    impl AsMut<Feeders> for Probe {
        fn as_mut(&mut self) -> &mut Feeders {
            &mut self.feeders
        }
    }

    crate::delegate_feeders!(Probe);

    #[allow(dead_code)]
    fn bind(
        globals: &GlobalList,
        qh: &QueueHandle<Probe>,
        surface: &wayland_client::protocol::wl_surface::WlSurface,
        updates: Updates,
    ) -> Probe {
        Probe {
            feeders: Feeders::bind(globals, qh, surface, updates),
        }
    }

    #[allow(dead_code)]
    fn act(probe: &mut Probe, action: &crate::model::Action) -> bool {
        probe.feeders.perform(action)
    }
}

#[test]
fn the_two_halves_of_a_window_join_without_clobbering() {
    let set = WindowSet::default();
    // The compositor speaks first: focus is its to give.
    let w = set
        .merge_state(3, "firefox".into(), "Home".into(), true, false)
        .unwrap();
    assert!(w.focused && w.focus_stamp == 1);
    // Then the ext handle's `done` arrives with a title. Focus survives.
    let w = set.merge_ext(3, "firefox".into(), "Mail".into()).unwrap();
    assert!(w.focused, "the ext half must not stamp focus off");
    assert_eq!(w.title, "Mail");
    // An identical merge is a no-op, so no frame is owed for it.
    assert!(set.merge_ext(3, "firefox".into(), "Mail".into()).is_none());
}

#[test]
fn focus_stamps_increase_and_only_one_window_is_focused() {
    let set = WindowSet::default();
    set.merge_ext(1, "kitty".into(), "a".into());
    set.merge_ext(2, "kitty".into(), "b".into());
    let changed = set.set_focus(Some(1));
    assert_eq!(changed.len(), 1);
    let changed = set.set_focus(Some(2));
    // 1 lost focus, 2 gained it.
    assert_eq!(changed.len(), 2);
    assert!(set.get(2).unwrap().focus_stamp > set.get(1).unwrap().focus_stamp);
    // Idempotent: saying it again changes nothing.
    assert!(set.set_focus(Some(2)).is_empty());
    assert!(set.remove(1));
    assert!(!set.remove(1));
}
