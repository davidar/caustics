use super::*;

#[test]
fn show_takes_a_path_and_an_optional_fade() {
    assert_eq!(
        parse("show /usr/share/backgrounds/a.png"),
        Ok(Command::Show {
            path: "/usr/share/backgrounds/a.png".into(),
            fade_ms: 0
        })
    );
    assert_eq!(
        parse("show /tmp/a.jpg fade 4\n"),
        Ok(Command::Show {
            path: "/tmp/a.jpg".into(),
            fade_ms: 4000
        })
    );
    assert_eq!(
        parse("show /tmp/a.jpg fade 0.25"),
        Ok(Command::Show {
            path: "/tmp/a.jpg".into(),
            fade_ms: 250
        })
    );
}

/// A path is everything up to a trailing `fade <seconds>`, so a space in it
/// is the path's.
#[test]
fn a_path_may_hold_spaces() {
    assert_eq!(
        parse("show /home/me/My Pictures/sea at dawn.png fade 7200"),
        Ok(Command::Show {
            path: "/home/me/My Pictures/sea at dawn.png".into(),
            fade_ms: 7_200_000
        })
    );
    assert_eq!(
        parse("show /a b.png"),
        Ok(Command::Show {
            path: "/a b.png".into(),
            fade_ms: 0
        })
    );
}

#[test]
fn a_relative_path_or_a_bad_fade_is_an_error() {
    assert!(parse("show a.png").is_err(), "relative");
    assert!(parse("show").is_err());
    assert!(parse("show /a.png fade -1").is_err());
    assert!(parse("show /a.png fade lots").is_err());
    assert!(parse("show /a.png fade 86401").is_err(), "past a day");
    assert!(parse("show /a.png fade NaN").is_err());
}

#[test]
fn dim_undim_and_status() {
    assert_eq!(
        parse("dim 0.6"),
        Ok(Command::Dim {
            alpha_milli: 600,
            over_ms: 0,
            wait: false
        })
    );
    assert_eq!(
        parse("dim 1 over 30"),
        Ok(Command::Dim {
            alpha_milli: 1000,
            over_ms: 30_000,
            wait: false
        })
    );
    assert_eq!(
        parse("dim 1 over 10 wait"),
        Ok(Command::Dim {
            alpha_milli: 1000,
            over_ms: 10_000,
            wait: true
        })
    );
    assert_eq!(
        parse("dim 0.5 wait"),
        Ok(Command::Dim {
            alpha_milli: 500,
            over_ms: 0,
            wait: true
        })
    );
    assert!(parse("dim wait").is_err(), "wait is not an alpha");
    assert!(parse("dim 1 wait over 3").is_err(), "wait comes last");
    assert!(parse("dim 1 over 3 wait wait").is_err());
    assert!(parse("dim 1.5").is_err());
    assert!(parse("dim").is_err());
    assert!(parse("dim 0.5 for 3").is_err());
    assert_eq!(parse("undim"), Ok(Command::Undim));
    assert_eq!(parse("status"), Ok(Command::Status));
    assert!(parse("status now").is_err());
    assert!(parse("").is_err());
    assert!(parse("lock").is_err());
}
