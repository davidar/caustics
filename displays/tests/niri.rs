//! niri's side: the replies read, and the requests written as `niri msg
//! output` writes them (read off a socket standing in for niri's).

mod common;

use caustics_displays::niri::{self, Change, Mode};

#[test]
fn the_outputs_reply_is_read_sorted_by_name() {
    let list = common::outputs();
    assert_eq!(
        list.iter().map(|o| o.name.as_str()).collect::<Vec<_>>(),
        ["DP-1", "eDP-1"]
    );
    let edp = &list[1];
    assert!(edp.on);
    assert_eq!(edp.scale, Some(1.75));
    assert_eq!(edp.transform.as_deref(), Some("Normal"));
    assert_eq!(
        edp.current_mode(),
        Some(Mode {
            width: 2880,
            height: 1800,
            refresh: 120001,
            preferred: true
        })
    );
    assert_eq!(edp.modes.len(), 3);
    assert_eq!(edp.label(), "eDP-1 · Example Panel Co. 0x1234");
}

#[test]
fn an_output_that_is_off_has_no_current_anything() {
    let dp = &common::outputs()[0];
    assert!(!dp.on);
    assert_eq!(dp.current, None);
    assert_eq!(dp.scale, None);
    assert_eq!(dp.transform, None);
    assert_eq!(dp.modes.len(), 1, "its modes are still listed");
    assert_eq!(dp.label(), "DP-1 · Example Inc. EX2720");
}

#[test]
fn a_missing_edid_is_the_connector_alone() {
    let reply = r#"{"Ok":{"Outputs":{"winit":{"name":"winit","make":"Smithay","model":"Winit",
        "modes":[{"width":1280,"height":800,"refresh_rate":60000,"is_preferred":true}],
        "current_mode":0,"logical":{"scale":2.0,"transform":"Normal"}},
        "HDMI-A-1":{"name":"HDMI-A-1","make":"Unknown","model":"Unknown","modes":[],
        "current_mode":null,"logical":null}}}}"#;
    let list = niri::parse_outputs(reply).unwrap();
    assert_eq!(list[0].label(), "HDMI-A-1");
    assert_eq!(list[1].label(), "winit · Smithay Winit");
}

#[test]
fn a_reply_that_is_not_outputs_is_said_as_niri_said_it() {
    assert_eq!(
        niri::parse_outputs(r#"{"Err":"no such thing"}"#),
        Err("niri answered no such thing".to_owned())
    );
    assert!(niri::parse_outputs("").unwrap_err().contains("not JSON"));
    assert!(
        niri::parse_outputs(r#"{"Ok":"Handled"}"#)
            .unwrap_err()
            .starts_with("niri answered ")
    );
}

/// What `niri msg output FAKE-1 <action>` wrote to a socket standing in for
/// niri's, verbatim. Compared as JSON values: the key order is serde_json's.
const NIRI_MSG: [(&str, &str); 6] = [
    ("on", r#"{"Output":{"output":"FAKE-1","action":"On"}}"#),
    ("off", r#"{"Output":{"output":"FAKE-1","action":"Off"}}"#),
    (
        "scale 1.5",
        r#"{"Output":{"output":"FAKE-1","action":{"Scale":{"scale":{"Specific":1.5}}}}}"#,
    ),
    (
        "scale 2",
        r#"{"Output":{"output":"FAKE-1","action":{"Scale":{"scale":{"Specific":2.0}}}}}"#,
    ),
    (
        "mode 1920x1080@60.000",
        r#"{"Output":{"output":"FAKE-1","action":{"Mode":{"mode":{"Specific":{"width":1920,"height":1080,"refresh":60.0}}}}}}"#,
    ),
    (
        "transform 90",
        r#"{"Output":{"output":"FAKE-1","action":{"Transform":{"transform":"90"}}}}"#,
    ),
];

#[test]
fn the_requests_are_what_niri_msg_sends() {
    let changes = [
        Change::On,
        Change::Off,
        Change::Scale(1.5),
        Change::Scale(2.0),
        Change::Mode(Mode {
            width: 1920,
            height: 1080,
            refresh: 60000,
            preferred: false,
        }),
        Change::Transform("90"),
    ];
    for ((what, theirs), change) in NIRI_MSG.iter().zip(changes) {
        let ours: serde_json::Value =
            serde_json::from_str(&niri::request("FAKE-1", &change)).unwrap();
        let theirs: serde_json::Value = serde_json::from_str(theirs).unwrap();
        assert_eq!(ours, theirs, "niri msg output FAKE-1 {what}");
    }
}

#[test]
fn a_mode_asks_for_its_own_refresh_in_hertz() {
    let req = niri::request(
        "eDP-1",
        &Change::Mode(Mode {
            width: 2880,
            height: 1800,
            refresh: 120001,
            preferred: true,
        }),
    );
    assert!(req.contains(r#""refresh":120.001"#), "{req}");
}

#[test]
fn an_output_name_is_a_json_string_however_it_is_spelt() {
    let req = niri::request("a\"b\\c", &Change::On);
    let v: serde_json::Value = serde_json::from_str(&req).unwrap();
    assert_eq!(v["Output"]["output"], "a\"b\\c");
}

#[test]
fn only_applied_is_ok() {
    assert_eq!(
        niri::outcome(r#"{"Ok":{"OutputConfigChanged":"Applied"}}"#),
        Ok(())
    );
    assert!(
        niri::outcome(r#"{"Ok":{"OutputConfigChanged":"OutputWasMissing"}}"#)
            .unwrap_err()
            .contains("no such output")
    );
    assert_eq!(
        niri::outcome(r#"{"Err":"invalid scale"}"#),
        Err("niri answered invalid scale".to_owned())
    );
    assert_eq!(
        niri::outcome(""),
        Err("niri closed the socket without a reply".to_owned())
    );
    assert!(
        niri::outcome("garbage")
            .unwrap_err()
            .starts_with("niri answered")
    );
}
