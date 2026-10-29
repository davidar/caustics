use super::*;

#[test]
fn the_service_defaults_and_an_empty_secret_is_no_dry_run() {
    let settings = Settings::from_vars(None, None, None, None);
    assert_eq!(settings.service, DEFAULT_SERVICE);
    assert!(settings.dry_run.is_none());
    let settings = Settings::from_vars(Some("".into()), Some("".into()), None, None);
    assert_eq!(settings.service, DEFAULT_SERVICE);
    assert!(
        settings.dry_run.is_none(),
        "an empty secret would unlock on Enter"
    );
    let settings = Settings::from_vars(Some("login".into()), Some("s3cret".into()), None, None);
    assert_eq!(settings.service, "login");
    assert_eq!(
        settings.dry_run.map(|s| s.as_bytes().to_vec()),
        Some(b"s3cret".to_vec())
    );
}

/// The settings' `Debug` holds no secret: a witness of the settings could
/// never carry the dry run's bytes.
#[test]
fn the_settings_show_no_secret() {
    let settings = Settings::from_vars(None, Some("s3cret".into()), None, None);
    assert!(!format!("{settings:?}").contains("s3cret"));
}

/// The reader is on by default and off only for `0`.
#[test]
fn the_fingerprint_is_on_unless_it_is_zero() {
    assert!(Settings::from_vars(None, None, None, None).fingerprint);
    assert!(Settings::from_vars(None, None, Some("1".into()), None).fingerprint);
    assert!(!Settings::from_vars(None, None, Some("0".into()), None).fingerprint);
}

/// The reader's windows: the default unset or empty, two whole numbers of
/// seconds when given, and a value that is not that kept for the witness.
#[test]
fn the_fingerprint_window_is_idle_cap_in_seconds() {
    use std::time::Duration;
    let windows =
        |value: Option<&str>| Settings::from_vars(None, None, None, value.map(Into::into)).windows;
    assert_eq!(windows(None), Ok(Windows::default()));
    assert_eq!(windows(Some("")), Ok(Windows::default()));
    assert_eq!(
        windows(Some("8, 60")),
        Ok(Windows {
            idle: Duration::from_secs(8),
            cap: Duration::from_secs(60),
        })
    );
    for bad in ["8", "8,60,1", "0,60", "8,0", "8,sixty", "-1,60", "1.5,60"] {
        assert_eq!(windows(Some(bad)), Err(bad.to_owned()), "{bad}");
    }
    assert_eq!(
        Windows::default(),
        Windows {
            idle: Duration::from_secs(30),
            cap: Duration::from_secs(90),
        }
    );
}
