//! Two outputs in niri's `Outputs` reply shape: a laptop panel that is on at
//! 1.75, and an external monitor that is off.

#![allow(dead_code)]

use caustics_displays::niri::{self, Output};

pub const REPLY: &str = r#"{"Ok":{"Outputs":{
"eDP-1":{"name":"eDP-1","make":"Example Panel Co.","model":"0x1234","serial":null,
  "physical_size":[310,200],
  "modes":[
    {"width":2880,"height":1800,"refresh_rate":120001,"is_preferred":true},
    {"width":2880,"height":1800,"refresh_rate":60001,"is_preferred":false},
    {"width":1920,"height":1200,"refresh_rate":59940,"is_preferred":false}],
  "current_mode":0,"is_custom_mode":false,"vrr_supported":false,"vrr_enabled":false,
  "logical":{"x":0,"y":0,"width":1645,"height":1028,"scale":1.75,"transform":"Normal"}},
"DP-1":{"name":"DP-1","make":"Example Inc.","model":"EX2720","serial":"ABC",
  "physical_size":[600,340],
  "modes":[{"width":3840,"height":2160,"refresh_rate":60000,"is_preferred":true}],
  "current_mode":null,"is_custom_mode":false,"vrr_supported":false,"vrr_enabled":false,
  "logical":null}
}}}"#;

pub fn outputs() -> Vec<Output> {
    niri::parse_outputs(REPLY).expect("the fixture parses")
}

/// The same two, with the external monitor turned on at scale 1 and 90°.
pub fn both_on() -> Vec<Output> {
    let mut list = outputs();
    let dp = list.iter_mut().find(|o| o.name == "DP-1").unwrap();
    dp.on = true;
    dp.current = Some(0);
    dp.scale = Some(1.0);
    dp.transform = Some("90".to_owned());
    list
}
