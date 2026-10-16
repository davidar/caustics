use super::*;

/// The shipped `caustics/bar_tunables.slang`, as text.
fn shipped_tunables() -> String {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../caustics/bar_tunables.slang");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("could not read {}: {err}", path.display()))
}

/// The value of one `public static const <type> NAME = …;` in that file.
fn tunable(source: &str, name: &str) -> String {
    let needle = format!(" {name} = ");
    source
        .lines()
        .find(|line| line.starts_with("public static const") && line.contains(&needle))
        .and_then(|line| line.split_once(&needle))
        .map(|(_, tail)| tail.trim().trim_end_matches(';').trim().to_owned())
        .unwrap_or_else(|| panic!("no `{name}` in the shipped tunables"))
}

/// The bar asks the compositor for an anchor, a height and an icon size
/// before any frame exists — before the Wayland connection even, in the
/// edge lock's case — so those three are compiled in here. They have to be
/// the shipped tunables' values, or a machine running the shipped tunables visibly re-anchors on its first frame.
///
/// The documented
/// default and the running default are one thing by construction, not by
/// somebody remembering to update both.
#[test]
fn the_compiled_defaults_are_the_shipped_tunables() {
    let source = shipped_tunables();

    let expected_edge = match BAR_EDGE {
        Edge::Top => "EDGE_TOP",
        Edge::Bottom => "EDGE_BOTTOM",
    };
    assert_eq!(
        tunable(&source, "EDGE"),
        expected_edge,
        "EDGE and `BAR_EDGE` disagree"
    );

    let height: f32 = tunable(&source, "BAR_HEIGHT")
        .parse()
        .expect("BAR_HEIGHT is a number");
    assert_eq!(
        height, BAR_HEIGHT as f32,
        "BAR_HEIGHT and `wayland::BAR_HEIGHT` disagree"
    );

    let icon: f32 = tunable(&source, "ICON_LOGICAL")
        .parse()
        .expect("ICON_LOGICAL is a number");
    assert_eq!(
        icon, ICON_LOGICAL as f32,
        "ICON_LOGICAL and `wayland::ICON_LOGICAL` disagree"
    );
}

/// `EDGE_*` round-trips, and a word the caustic has not written yet is not
/// silently read as an edge.
#[test]
fn an_edge_survives_the_abi_and_a_bad_one_does_not() {
    for edge in [Edge::Top, Edge::Bottom] {
        assert_eq!(Edge::from_abi(edge.abi()), Some(edge));
    }
    assert_eq!(Edge::from_abi(9), None);
    assert_eq!(BAR_EDGE.name(), "top");
}

/// Every action has an owner, and the compositor's own are named by the
/// seam rather than by a literal: the witness says `niri-ipc` on niri and
/// whatever the next implementation calls itself on that.
///
/// `Spawn` and `Launch` are in that family: starting a client is a request
/// over the socket the bar already holds, and the bar forks nothing itself.
///
/// The power actions are `caustics-power`'s rows, so every one of them arrives as
/// `Action::Sidecar` and this table has one word for the whole family.
#[test]
fn every_action_names_the_feeder_that_performs_it() {
    use crate::abi::wire::ActionBody;
    use crate::feeders::compositor::NoCompositor;
    use crate::feeders::niri_ipc::NiriIpc;

    for name in [NiriIpc::ABSENT, NoCompositor::ABSENT] {
        for action in [
            Action::Activate(4),
            Action::Close(4),
            Action::ToggleOverview,
            // Both halves of a dock launch: the app id the caustic named and
            // the argv it resolved to. The compositor owns both, and no arm
            // of this table says `embassy`.
            Action::Spawn {
                app_id: "kitty".into(),
            },
            Action::Launch {
                argv: vec!["/usr/bin/gio".into(), "launch".into()],
            },
        ] {
            assert_eq!(action_owner(&action, name), name);
        }
        assert_eq!(action_owner(&Action::PreviewWanted(None), name), "capture");
        assert_eq!(action_owner(&Action::ToggleCaffeine, name), "caffeine");
        let row = Action::Sidecar {
            name: "caustics-power".into(),
            body: ActionBody {
                row: 8,
                ..ActionBody::default()
            },
        };
        assert_eq!(action_owner(&row, name), "sidecar");
    }
}

/// The output's size reaches the caustic in the surface's px, so it can clamp
/// a long menu to the screen: the logical size `xdg_output` reported, times the buffer scale — and
/// an output not yet known stays (0, 0), which the caustic reads as no bound.
#[test]
fn the_output_size_is_logical_times_the_scale() {
    assert_eq!(output_px((1920, 1200), 2), [3840, 2400]);
    assert_eq!(output_px((1920, 1200), 1), [1920, 1200]);
    assert_eq!(
        output_px((0, 0), 2),
        [0, 0],
        "unknown is no bound, at any scale"
    );
    assert_eq!(
        output_px((800, 600), 0),
        [800, 600],
        "a scale of 0 is read as 1"
    );
}

/// A wheel frame is one `CMD_AXIS` with the px in `args[0]`: logical px times
/// the scale, positive down, and the discrete steps
/// beside them as they came. A horizontal-only frame and a touchpad's stop
/// carry nothing vertical and queue nothing.
#[test]
fn a_wheel_frame_is_one_axis_command() {
    let wheel = AxisScroll {
        absolute: 15.0,
        discrete: 1,
        stop: false,
    };
    let command = axis_command(abi::TARGET_POPUP, [10.0, 20.0], &wheel, 2.0)
        .expect("a vertical scroll is a command");
    assert_eq!(command.tag, abi::CMD_AXIS);
    assert_eq!(command.target, abi::TARGET_POPUP);
    assert_eq!(command.pos, [10.0, 20.0]);
    assert_eq!(command.args[0] as i32, 30, "15 logical px at scale 2");
    assert_eq!(command.args[1] as i32, 1, "one step, as it came");

    let up = AxisScroll {
        absolute: -7.5,
        discrete: 0,
        stop: false,
    };
    let command = axis_command(abi::TARGET_POPUP, [0.0, 0.0], &up, 2.0).expect("up is a command");
    assert_eq!(command.args[0] as i32, -15);
    assert_eq!(command.args[1], 0);

    let steps_only = AxisScroll {
        absolute: 0.0,
        discrete: -2,
        stop: false,
    };
    let command =
        axis_command(abi::TARGET_BAR, [0.0, 0.0], &steps_only, 1.0).expect("steps alone count");
    assert_eq!((command.args[0], command.args[1] as i32), (0, -2));

    let stop = AxisScroll {
        absolute: 0.0,
        discrete: 0,
        stop: true,
    };
    assert!(axis_command(abi::TARGET_POPUP, [0.0, 0.0], &stop, 2.0).is_none());
}

/// Where the logical size comes from: `xdg_output` when the compositor sent
/// it, else the current mode over the integer scale, turned for a rotated
/// output; nothing known is (0, 0).
#[test]
fn the_logical_size_is_xdg_outputs_else_the_modes() {
    assert_eq!(
        logical_from(Some((1440, 900)), Some((2880, 1800)), 2, false),
        (1440, 900)
    );
    assert_eq!(
        logical_from(None, Some((2880, 1800)), 2, false),
        (1440, 900)
    );
    assert_eq!(logical_from(None, Some((2880, 1800)), 2, true), (900, 1440));
    assert_eq!(logical_from(None, None, 2, false), (0, 0));
    assert_eq!(logical_from(None, Some((1280, 800)), 0, false), (1280, 800));
}
