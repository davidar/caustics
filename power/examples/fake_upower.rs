//! A battery for a machine that has none: UPower's `DisplayDevice` on the
//! **session** bus, for `bar/bar-test.sh`.
//!
//! A test machine's real UPower (a virtual machine's, a desktop's) reports
//! `IsPresent = false`, so `caustics-power` publishes no battery chip there and
//! the battery's menu has nothing to hang from. The test runs this beside the
//! bar and points the bar at the session bus as its system bus
//! (`DBUS_SYSTEM_BUS_ADDRESS`, which zbus honours and every sidecar
//! inherits): the sidecar then sees an 87% battery discharging with 2 h 13 m
//! left, no power-profiles daemon and no logind — and, with no backlight, its
//! menu is the state row alone.
//!
//! Not a test of UPower: a fixture with the six properties the sidecar reads.
//! It serves until it is stopped.
//!
//! ```text
//! cargo build -p caustics-power --example fake_upower
//! systemd-run --user --unit=caustics-fake-upower target/debug/examples/fake_upower
//! ```

const SERVICE: &str = "org.freedesktop.UPower";
const DISPLAY_DEVICE: &str = "/org/freedesktop/UPower/devices/DisplayDevice";

struct Device;

#[zbus::interface(name = "org.freedesktop.UPower.Device")]
impl Device {
    #[zbus(property)]
    fn is_present(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn percentage(&self) -> f64 {
        87.0
    }
    #[zbus(property)]
    fn icon_name(&self) -> String {
        "battery-level-90-symbolic".to_owned()
    }
    /// Discharging.
    #[zbus(property)]
    fn state(&self) -> u32 {
        2
    }
    /// 2 h 13 m.
    #[zbus(property)]
    fn time_to_empty(&self) -> i64 {
        7980
    }
    #[zbus(property)]
    fn time_to_full(&self) -> i64 {
        0
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> zbus::Result<()> {
    let _conn = zbus::connection::Builder::session()?
        .serve_at(DISPLAY_DEVICE, Device)?
        .name(SERVICE)?
        .build()
        .await?;
    eprintln!("fake-upower: ready, {SERVICE}{DISPLAY_DEVICE} on the session bus");
    std::future::pending::<()>().await;
    Ok(())
}
