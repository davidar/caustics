use super::*;

#[test]
fn the_service_defaults_and_an_empty_secret_is_no_dry_run() {
    let settings = Settings::from_vars(None, None);
    assert_eq!(settings.service, DEFAULT_SERVICE);
    assert!(settings.dry_run.is_none());
    let settings = Settings::from_vars(Some("".into()), Some("".into()));
    assert_eq!(settings.service, DEFAULT_SERVICE);
    assert!(
        settings.dry_run.is_none(),
        "an empty secret would unlock on Enter"
    );
    let settings = Settings::from_vars(Some("login".into()), Some("s3cret".into()));
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
    let settings = Settings::from_vars(None, Some("s3cret".into()));
    assert!(!format!("{settings:?}").contains("s3cret"));
}
