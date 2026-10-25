use super::*;

use zbus::zvariant::Value;

fn user(uid: u32) -> Identity {
    (
        "unix-user".to_owned(),
        HashMap::from([(
            "uid".to_owned(),
            OwnedValue::try_from(Value::U32(uid)).unwrap(),
        )]),
    )
}

fn group(gid: u32) -> Identity {
    (
        "unix-group".to_owned(),
        HashMap::from([(
            "gid".to_owned(),
            OwnedValue::try_from(Value::U32(gid)).unwrap(),
        )]),
    )
}

/// Root has a name on every system the tests run on; this process's uid has
/// one too.
#[test]
fn the_own_uid_is_chosen_wherever_it_is_offered() {
    let own = own_uid();
    let name = user_name(own).expect("this process's user has a name");
    let other = if own == 0 { 1 } else { 0 };
    assert_eq!(
        choose(&[user(other), group(10), user(own)], own),
        Some((own, name.clone()))
    );
    assert_eq!(choose(&[user(own)], own), Some((own, name)));
}

/// Without it, the first user offered; a group is passed over.
#[test]
fn otherwise_the_first_user_offered() {
    let own = own_uid();
    let root = user_name(0).expect("root has a name");
    if own != 0 {
        assert_eq!(choose(&[group(10), user(0)], own), Some((0, root)));
    }
    assert_eq!(choose(&[group(10)], own), None);
    assert_eq!(choose(&[], own), None);
    // A user with no name in the database cannot be the helper's argument.
    assert_eq!(choose(&[user(u32::MAX - 7)], own), None);
}
