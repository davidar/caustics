use super::*;
use crate::abi::wire::{self, RowsBuilder, Samples};

/// `$CAUSTICS_SIDECARS` replaces the compiled-in set; nothing else does, and
/// no command ever carries arguments (a sidecar's settings are the
/// sidecar's).
#[test]
fn the_set_comes_from_the_environment_or_the_compiled_in_list() {
    // The variable is process-wide, so this test reads rather than sets
    // it: whichever way the harness runs, the names that come out are the
    // ones the environment or the constant asked for.
    let commands = commands();
    match std::env::var_os(SIDECARS_ENV) {
        Some(value) if !value.to_string_lossy().split_whitespace().count() == 0 => {
            assert_eq!(
                commands.len(),
                value.to_string_lossy().split_whitespace().count()
            );
        }
        _ => {
            assert_eq!(commands.len(), SIDECARS.len());
            for (command, name) in commands.iter().zip(SIDECARS) {
                assert!(command.ends_with(name), "{command} is not a {name}");
            }
        }
    }
    assert!(
        commands.iter().all(|c| !c.contains(' ')),
        "a command carries no arguments"
    );
}

/// The bar's half of the pipe: an absence, a gone sidecar and the three
/// shapes of frame become the updates the model already knew before the
/// supervisor was a crate.
#[test]
fn a_pipes_events_become_the_models_updates() {
    let name = "caustics-sysmon".to_owned();
    assert_eq!(
        update_of(Event::Absent(name.clone())),
        Update::Absent(name.clone())
    );
    assert_eq!(
        update_of(Event::Gone(name.clone())),
        Update::SidecarGone(name.clone())
    );

    let mut body = Box::new(Samples::ZERO);
    body.chart_count = 2;
    assert_eq!(
        update_of(Event::Frame {
            sidecar: name.clone(),
            frame: Frame::Samples(body.clone()),
        }),
        Update::Samples {
            sidecar: name.clone(),
            body,
        }
    );

    let rows = RowsBuilder::new(abi::APPLET_AUDIO, 7);
    let Frame::Rows(rows) = rows.frame() else {
        panic!("a rows builder builds a rows frame");
    };
    assert_eq!(
        update_of(Event::Frame {
            sidecar: name.clone(),
            frame: Frame::Rows(rows.clone()),
        }),
        Update::Rows {
            sidecar: name.clone(),
            rows,
        }
    );

    let hello = Frame::Hello(wire::Hello {
        abi_hash: abi::ABI_HASH,
        name: name.clone(),
        flags: 0,
    });
    assert_eq!(
        update_of(Event::Frame {
            sidecar: name.clone(),
            frame: hello.clone(),
        }),
        Update::Frame {
            sidecar: name,
            frame: hello,
        }
    );
}
