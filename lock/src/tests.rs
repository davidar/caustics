use super::*;

#[test]
fn an_unnamed_service_is_found_later_and_an_empty_secret_is_no_dry_run() {
    let settings = Settings::from_vars(None, None, None, None);
    assert_eq!(settings.service, None);
    assert!(settings.dry_run.is_none());
    let settings = Settings::from_vars(Some("".into()), Some("".into()), None, None);
    assert_eq!(settings.service, None);
    assert!(
        settings.dry_run.is_none(),
        "an empty secret would unlock on Enter"
    );
    let settings = Settings::from_vars(Some("login".into()), Some("s3cret".into()), None, None);
    assert_eq!(settings.service.as_deref(), Some("login"));
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

/// A directory of this test's own, emptied after it.
struct PamDir(std::path::PathBuf);

impl PamDir {
    fn new(tag: &str) -> PamDir {
        let dir =
            std::env::temp_dir().join(format!("caustics-lock-pam-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        PamDir(dir)
    }
    fn file(&self, name: &str) -> std::path::PathBuf {
        let file = self.0.join(name);
        std::fs::write(&file, "auth required pam_unix.so\n").unwrap();
        file
    }
}

impl Drop for PamDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A named service is taken as named, whether or not a file exists for it:
/// a deliberate name is PAM's to judge.
#[test]
fn a_named_service_is_taken_unchecked() {
    let dir = PamDir::new("named");
    assert_eq!(
        resolve_service(Some("mine"), &[&dir.0]),
        ("mine".to_owned(), ServiceOrigin::Named)
    );
}

/// Unnamed, the first candidate with a file wins, in the candidates' order
/// and not the directories' order: the lock's own service beats a locker's
/// even when only the vendor directory has it, and a locker's beats `login`.
#[test]
fn an_unnamed_service_is_the_first_candidate_with_a_file() {
    let etc = PamDir::new("etc");
    let vendor = PamDir::new("vendor");
    let login = etc.file("login");
    assert_eq!(
        resolve_service(None, &[&etc.0, &vendor.0]),
        ("login".to_owned(), ServiceOrigin::Found(login.clone()))
    );
    let hyprlock = etc.file("hyprlock");
    assert_eq!(
        resolve_service(None, &[&etc.0, &vendor.0]),
        ("hyprlock".to_owned(), ServiceOrigin::Found(hyprlock))
    );
    let swaylock = etc.file("swaylock");
    assert_eq!(
        resolve_service(None, &[&etc.0, &vendor.0]).0,
        "swaylock",
        "{swaylock:?} comes before hyprlock in the order"
    );
    let own = vendor.file("caustics-lock");
    assert_eq!(
        resolve_service(None, &[&etc.0, &vendor.0]),
        ("caustics-lock".to_owned(), ServiceOrigin::Found(own))
    );
    // A directory where the name is a directory is no file.
    std::fs::remove_file(&login).unwrap();
    std::fs::create_dir(&login).unwrap();
    assert_eq!(resolve_service(None, &[&etc.0]).0, "swaylock");
}

/// No file anywhere: `login`, said to be unseen, and the witness names what
/// was looked for and where a stack of the lock's own goes.
#[test]
fn no_file_at_all_is_login_as_a_last_resort() {
    let empty = PamDir::new("empty");
    let (service, origin) = resolve_service(None, &[&empty.0, Path::new("/nonexistent/pam.d")]);
    assert_eq!(service, "login");
    assert_eq!(origin, ServiceOrigin::LastResort);
    let line = describe_service(&service, &origin);
    assert!(line.contains("caustics-lock, swaylock"), "{line}");
    assert!(line.contains("/etc/pam.d/caustics-lock"), "{line}");
    assert_eq!(
        describe_service(
            "swaylock",
            &ServiceOrigin::Found("/etc/pam.d/swaylock".into())
        ),
        "PAM service swaylock (/etc/pam.d/swaylock)"
    );
}

/// The candidates end in the one every system has.
#[test]
fn the_last_candidate_is_login() {
    assert_eq!(SERVICE_CANDIDATES.last(), Some(&"login"));
    assert_eq!(SERVICE_CANDIDATES[0], "caustics-lock");
}
