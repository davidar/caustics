//! The question table: what a `GetSecrets` call's arguments ask for, with no
//! bus.

use std::collections::HashMap;

use caustics_secrets::ask::{
    ALLOW_INTERACTION, ConnectionDict, IEEE_8021X, REQUEST_NEW, Refusal, USER_REQUESTED,
    WIRELESS_SECURITY, ask, psk_valid, title,
};
use zbus::zvariant::{OwnedValue, Value};

fn owned(value: Value<'_>) -> OwnedValue {
    value.try_to_owned().expect("an owned value")
}

/// A Wi-Fi connection as the bar's join makes it, with `key_mgmt`.
fn wifi(id: &str, key_mgmt: Option<&str>) -> ConnectionDict {
    let mut connection = HashMap::from([
        (
            "connection".to_owned(),
            HashMap::from([
                ("id".to_owned(), owned(Value::from(id))),
                ("type".to_owned(), owned(Value::from("802-11-wireless"))),
            ]),
        ),
        (
            "802-11-wireless".to_owned(),
            HashMap::from([(
                "ssid".to_owned(),
                owned(Value::from(b"CoffeeShop".to_vec())),
            )]),
        ),
    ]);
    if let Some(key_mgmt) = key_mgmt {
        connection.insert(
            WIRELESS_SECURITY.to_owned(),
            HashMap::from([("key-mgmt".to_owned(), owned(Value::from(key_mgmt)))]),
        );
    }
    connection
}

const ASK: u32 = ALLOW_INTERACTION | USER_REQUESTED;

#[test]
fn a_wpa_network_asks_for_its_psk_under_its_name() {
    let asked = ask(
        &wifi("CoffeeShop", Some("wpa-psk")),
        WIRELESS_SECURITY,
        &[],
        ASK,
    )
    .expect("a question");
    assert_eq!(asked.title, "CoffeeShop");
    assert_eq!(asked.setting, WIRELESS_SECURITY);
    assert_eq!(asked.key, "psk");
    assert_eq!(asked.words, "Password");
    assert!(asked.psk_rule);
    assert!(!asked.request_new);

    // WPA3's SAE password is `psk` too, and has no length rule.
    let sae = ask(&wifi("Home", Some("sae")), WIRELESS_SECURITY, &[], ASK).expect("a question");
    assert_eq!((sae.key.as_str(), sae.psk_rule), ("psk", false));

    // A hint naming the key changes nothing for a PSK network.
    let hinted = ask(
        &wifi("Home", Some("wpa-psk")),
        WIRELESS_SECURITY,
        &["psk".to_owned()],
        ASK,
    )
    .expect("a question");
    assert_eq!(hinted.key, "psk");
}

#[test]
fn no_interaction_is_no_question() {
    for flags in [0, REQUEST_NEW, USER_REQUESTED] {
        assert_eq!(
            ask(
                &wifi("Home", Some("wpa-psk")),
                WIRELESS_SECURITY,
                &[],
                flags
            ),
            Err(Refusal::NoInteraction),
            "flags {flags:#x}"
        );
    }
}

#[test]
fn request_new_says_the_last_answer_was_refused() {
    let asked = ask(
        &wifi("Home", Some("wpa-psk")),
        WIRELESS_SECURITY,
        &[],
        ASK | REQUEST_NEW,
    )
    .expect("a question");
    assert!(asked.request_new);
}

#[test]
fn eight_oh_two_one_x_asks_for_the_hinted_key() {
    let connection = wifi("Work", Some("wpa-eap"));
    let password = ask(&connection, IEEE_8021X, &["password".to_owned()], ASK).expect("a question");
    assert_eq!(
        (password.key.as_str(), password.words.as_str()),
        ("password", "Password")
    );
    let key = ask(
        &connection,
        IEEE_8021X,
        &["private-key-password".to_owned()],
        ASK,
    )
    .expect("a question");
    assert_eq!(
        (key.key.as_str(), key.words.as_str()),
        ("private-key-password", "Private key password")
    );
    // No hint: the password.
    assert_eq!(
        ask(&connection, IEEE_8021X, &[], ASK)
            .expect("a question")
            .key,
        "password"
    );
}

#[test]
fn a_setting_with_nothing_to_ask_is_refused_with_its_name() {
    assert_eq!(
        ask(&wifi("Home", None), "vpn", &[], ASK),
        Err(Refusal::Unknown("vpn".to_owned()))
    );
    // A VPN plugin's message hint is not a key.
    assert_eq!(
        ask(
            &wifi("Home", None),
            "vpn",
            &["x-vpn-message:Enter the code".to_owned()],
            ASK
        ),
        Err(Refusal::Unknown("vpn".to_owned()))
    );
    // WEP has no `psk`, and with no hint there is no key to ask for.
    assert_eq!(
        ask(&wifi("Old", Some("none")), WIRELESS_SECURITY, &[], ASK),
        Err(Refusal::Unknown(
            "802-11-wireless-security key-mgmt none".to_owned()
        ))
    );
    // Another setting with a hint is asked for by that key.
    let gsm = ask(&wifi("Modem", None), "gsm", &["pin".to_owned()], ASK).expect("a question");
    assert_eq!((gsm.key.as_str(), gsm.words.as_str()), ("pin", "PIN"));
}

#[test]
fn the_title_falls_back_to_the_ssid() {
    let mut connection = wifi("", Some("wpa-psk"));
    assert_eq!(title(&connection), "CoffeeShop");
    connection.remove("802-11-wireless");
    assert_eq!(title(&connection), "A network");
}

#[test]
fn a_psk_is_eight_to_sixty_three_characters_or_sixty_four_hex_digits() {
    assert!(!psk_valid("short"));
    assert!(psk_valid("eight ch"));
    assert!(psk_valid(&"x".repeat(63)));
    assert!(!psk_valid(&"x".repeat(64)));
    assert!(psk_valid(&"0a".repeat(32)));
    assert!(!psk_valid(&"0g".repeat(32)));
}
