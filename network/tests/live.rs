//! The tests that need a machine, and the two that write
//! `network/tests/data/`.
//!
//! The committed fixtures are **synthesised**, not recorded:
//! `synthesise_fixtures` writes them from the invented data in `synth/`, so
//! nothing in the repository names a real network, address or machine. The
//! recorder beside it, `record_fixtures`, is for diagnosis: it captures the
//! live system bus into the same files so a developer can see what their own
//! NetworkManager answers and run it through the parsers. **A recording is
//! never committed**: it carries the machine's hostname, SSIDs, BSSIDs,
//! addresses and connection ids. Run `synthesise_fixtures` again (or restore
//! the files from version control) before committing.
//!
//! The binary test spawns the built sidecar against the real system bus and
//! reads its frames back with the ABI's own reader. All three are `#[ignore]`;
//! the recorder and the binary test need NetworkManager on the system bus.
//!
//! None of them **changes anything** on the bus. The recorder calls only
//! `GetManagedObjects`, `ListConnections` and `GetSettings`.
//! `ActivateConnection`, `DeactivateConnection` and `WirelessEnabled` are never
//! called by a test: the machine running it is probably online through the
//! connection they would change.
//!
//! The one thing a test does put on the child's stdin is an `ACTION_MENU_OPEN`
//! notice, which makes the sidecar call `RequestScan`. **A scan is a read of
//! the air**: it joins nothing, leaves nothing and changes no setting. It does
//! take the radio off-channel for a moment, which is the only reason it is
//! worth saying out loud, and it is what the menu does every time it opens.
//!
//! ```text
//! cargo test -p caustics-network --test live -- --ignored --nocapture
//! cargo test -p caustics-network --test live synthesise_fixtures -- --ignored
//! CAUSTICS_NETWORK_RECORD=1 cargo test -p caustics-network --test live record_fixtures -- --ignored
//! ```

use std::io::Write as _;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use caustics_bar_abi::buffer::{ACTION_MENU_OPEN, APPLET_NETWORK};
use caustics_bar_abi::wire::{
    self, ActionBody, Frame, RFLAG_CHECKABLE, RFLAG_CHECKED, RFLAG_HAS_MENU, RFLAG_RADIO, ROW_ITEM,
    ROW_MENU, Rows,
};
use caustics_network::model::VISIBLE;
use caustics_network::nm;
use caustics_network::rows::{ITEM, SEP_VISIBLE, SETTINGS, WIFI};

mod synth;

/// How long to wait for the frame that answers the scan. NM takes a second or
/// two to sweep the channels and then emits its `AccessPointAdded` burst, which
/// the sidecar's 250 ms debounce rolls into one re-read.
const SCAN_PATIENCE: Duration = Duration::from_secs(10);

/// Print one frame the way a human reads it.
fn dump(what: &str, rows: &Rows) {
    println!(
        "{what}: applet={} revision={} rows={}",
        rows.applet,
        rows.revision,
        rows.rows.len()
    );
    for (i, row) in rows.rows.iter().enumerate() {
        println!(
            "  [{i}] id={} parent={} role={} flags={} label={:?} icon={:?}",
            row.id,
            row.parent,
            row.role,
            row.flags,
            rows.label(i).as_deref(),
            rows.icon(i)
        );
    }
}

// ---------------------------------------------------------------------------
// The binary, against the real bus
// ---------------------------------------------------------------------------

#[test]
#[ignore = "needs NetworkManager on the system bus"]
fn the_binary_publishes_the_item_the_switch_and_the_known_networks() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_caustics-network"))
        // The bar spawns a sidecar with no arguments (docs/abi.md, *Lifecycle*).
        // A named settings program, so the settings row is drawn whatever is
        // installed; no click reaches it, so it never runs.
        .env("CAUSTICS_SETTINGS", "true")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // Inherited, so the witnesses land in this test's own output.
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawning caustics-network");

    // The frames come back on a thread, so every read below can have a deadline:
    // the scan's answer arrives seconds later and a blocking read would hang the
    // test rather than fail it.
    let mut out = child.stdout.take().expect("piped stdout");
    let (tx, frames) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        while let Ok(Some(frame)) = wire::read(&mut out) {
            if tx.send(frame).is_err() {
                return;
            }
        }
    });
    let next = |why: &str| -> Rows {
        match frames.recv_timeout(SCAN_PATIENCE) {
            Ok(Frame::Rows(rows)) => rows,
            Ok(other) => panic!("expected a rows frame, got {other:?}"),
            Err(err) => panic!("{why}: {err}"),
        }
    };

    // The first frame is the empty one, written before the bus has answered;
    // the picture arrives after the `GetManagedObjects` round trip. Read until
    // a frame has rows, or run out of patience.
    let mut rows = next("the first frame");
    for _ in 0..3 {
        if !rows.rows.is_empty() {
            break;
        }
        rows = next("the first frame with rows");
    }
    dump("closed", &rows);

    assert_eq!(rows.applet, APPLET_NETWORK);
    assert!(
        !rows.rows.is_empty(),
        "NetworkManager is running, so there are rows"
    );

    let item = &rows.rows[0];
    assert_eq!(item.role, ROW_ITEM);
    assert_eq!(
        item.flags & RFLAG_HAS_MENU,
        RFLAG_HAS_MENU,
        "the chip opens its menu"
    );
    assert_eq!(
        rows.rows.iter().filter(|row| row.role == ROW_ITEM).count(),
        1,
        "one chip, and the menu under it"
    );

    let switch = rows
        .rows
        .iter()
        .find(|row| row.id == WIFI)
        .expect("the Wi-Fi row");
    assert_eq!(switch.flags & RFLAG_CHECKABLE, RFLAG_CHECKABLE);
    assert_eq!(
        switch.flags & RFLAG_CHECKED,
        RFLAG_CHECKED,
        "the radio is on while this test runs"
    );
    assert!(
        rows.rows.iter().any(|row| row.id == SETTINGS),
        "the settings row is last"
    );

    // The known networks: radio rows, and at most one of them checked — exactly
    // one while the machine is on Wi-Fi, none while it is on a wire.
    let radios: Vec<&caustics_bar_abi::wire::Row> = rows
        .rows
        .iter()
        .filter(|row| row.flags & RFLAG_RADIO != 0)
        .collect();
    assert!(radios.iter().all(|row| row.role == ROW_MENU));
    let checked = radios
        .iter()
        .filter(|row| row.flags & RFLAG_CHECKED != 0)
        .count();
    println!("known networks: {} ({checked} checked)", radios.len());
    assert!(checked <= 1, "radio semantics: {checked} rows checked");

    // What NM happens to list right now. **Not asserted to be empty**: the
    // aging takes about twenty minutes, and anything else on the
    // machine that scanned in the meantime leaves the list warm — this sidecar
    // reads NM's list, it does not clear it.
    let before = in_range(&rows).len();
    println!("in range before the menu opens: {before} (whatever NM still lists)");

    // ---- the scan --------------------------------------------------------
    //
    // The bar's own `ACTION_MENU_OPEN`, byte for byte. The sidecar answers it with
    // `RequestScan` on every Wi-Fi device — a read of the air — and NM's results
    // arrive as signals a second or two later. Its own witness lines are on this
    // test's stderr.
    let open = ActionBody {
        row: ITEM,
        kind: ACTION_MENU_OPEN,
        ..ActionBody::default()
    };
    let mut stdin = child.stdin.take().expect("piped stdin");
    wire::write(&mut stdin, &Frame::Action(open)).expect("writing the menu notice");
    stdin.flush().expect("flushing the menu notice");
    println!("sent ACTION_MENU_OPEN (row {ITEM})");

    // Read for the frame the scan produced: a newer revision carrying the
    // section. The wait is bounded and the *absence* of a newer frame is a note
    // rather than a failure — the sidecar publishes on change only, and a scan
    // that finds exactly the picture NM already had is no frame at all. What is
    // asserted below is the in-range section.
    let deadline = Instant::now() + SCAN_PATIENCE;
    let mut scanned = rows.clone();
    let mut fresh = false;
    while !(fresh && !in_range(&scanned).is_empty()) {
        match frames.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(Frame::Rows(frame)) => {
                fresh = frame.revision > rows.revision;
                scanned = frame;
            }
            Ok(other) => panic!("expected a rows frame, got {other:?}"),
            Err(_) => break,
        }
    }
    dump("open", &scanned);
    if fresh {
        println!(
            "the scan's own frame: rev {} -> {}",
            rows.revision, scanned.revision
        );
    } else {
        println!(
            "no newer frame within {SCAN_PATIENCE:?}: the scan found the picture NM already had"
        );
    }

    let found = in_range(&scanned);
    println!("in range after the scan: {} (was {before})", found.len());
    println!("known: {:?}", radio_labels(&scanned));
    println!("in range: {:?}", in_range_labels(&scanned));
    assert!(
        !found.is_empty(),
        "the scan found nothing within {SCAN_PATIENCE:?}; a machine in range of \
         any network sees some when scanned"
    );
    assert!(found.len() <= 10, "capped at ten: {}", found.len());
    assert!(
        scanned.rows.iter().any(|row| row.id == SEP_VISIBLE),
        "the section has a separator"
    );
    for row in &found {
        assert_eq!(row.role, ROW_MENU);
        assert_eq!(row.flags, 0, "an unknown network carries no mark");
        assert_eq!(row.parent, 0, "it hangs off the chip");
    }
    // One row per name, and no hidden network: a mesh puts the same SSID on
    // several access points and an empty SSID is not a label.
    let mut labels = in_range_labels(&scanned);
    assert!(
        labels.iter().all(|label| !label.is_empty()),
        "a hidden network is not a row"
    );
    let total = labels.len();
    labels.sort();
    labels.dedup();
    assert_eq!(labels.len(), total, "one row per SSID");
    assert_eq!(
        scanned.rows.iter().position(|row| row.id == SETTINGS),
        Some(scanned.rows.len() - 1),
        "the settings row is still last"
    );

    // The child's stdin closing is its notice to leave (docs/abi.md, *Lifecycle*).
    drop(stdin);
    let status = child.wait().expect("waiting for caustics-network");
    let _ = reader.join();
    println!("exit: {status}");
    assert!(status.success(), "a sidecar exits 0 when its stdin closes");
}

/// The rows of the in-range section: the ones whose id carries
/// [`VISIBLE`], which is the half of the derived id space they live in.
fn in_range(rows: &Rows) -> Vec<&caustics_bar_abi::wire::Row> {
    rows.rows
        .iter()
        .filter(|row| row.id & VISIBLE != 0)
        .collect()
}

fn in_range_labels(rows: &Rows) -> Vec<String> {
    labels_where(rows, |row| row.id & VISIBLE != 0)
}

/// The labels of the known networks: the radio rows.
fn radio_labels(rows: &Rows) -> Vec<String> {
    labels_where(rows, |row| row.flags & RFLAG_RADIO != 0)
}

fn labels_where(rows: &Rows, which: impl Fn(&caustics_bar_abi::wire::Row) -> bool) -> Vec<String> {
    rows.rows
        .iter()
        .enumerate()
        .filter(|(_, row)| which(row))
        .filter_map(|(i, _)| rows.label(i).map(|l| l.into_owned()))
        .collect()
}

// ---------------------------------------------------------------------------
// The synthesiser
// ---------------------------------------------------------------------------

/// Rewrite `network/tests/data/` from `synth/`. Run it after editing the
/// invented data; `frames.rs` fails until the files and the module agree.
#[test]
#[ignore = "rewrites network/tests/data from synth/"]
fn synthesise_fixtures() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data");
    for (name, body) in synth::fixtures() {
        std::fs::write(format!("{dir}/{name}"), &body).expect("the fixture writes");
        println!("synthesised {name} ({} bytes)", body.len());
    }
}

// ---------------------------------------------------------------------------
// The recorder
// ---------------------------------------------------------------------------

/// Set this to `1` to overwrite `network/tests/data/` with a recording of the
/// live system bus. Recording is a deliberate act, for diagnosis on the
/// developer's own machine; `--ignored` alone must not do it, and the result
/// is never committed (see the module note).
const RECORD_ENV: &str = "CAUSTICS_NETWORK_RECORD";

#[test]
#[ignore = "records fixtures from the live system bus; needs CAUSTICS_NETWORK_RECORD=1"]
fn record_fixtures() {
    if std::env::var(RECORD_ENV).as_deref() != Ok("1") {
        println!("{RECORD_ENV} is not 1; recording nothing");
        return;
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .expect("a tokio runtime");
    runtime.block_on(record());
}

async fn record() {
    use std::fs;

    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data");
    let conn = zbus::Connection::system().await.expect("the system bus");

    // The whole picture in one call.
    let body = call(
        &conn,
        nm::OBJECT_MANAGER_PATH,
        OBJECT_MANAGER,
        "GetManagedObjects",
    )
    .await;
    fs::write(format!("{dir}/managed-objects.bin"), &body).expect("the fixture writes");
    println!("recorded managed-objects.bin ({} bytes)", body.len());
    let managed = nm::parse_managed_objects(&body).expect("the recorded body parses");
    println!(
        "  wireless_enabled={} primary={} connections={} devices={} aps={} active={}",
        managed.wireless_enabled,
        managed.primary,
        managed.connections.len(),
        managed.devices.len(),
        managed.access_points.len(),
        managed.active.len()
    );

    // `ListConnections` answers the same paths the walk already carried; called
    // here so the recording says so rather than assuming it.
    let listed = call(&conn, nm::SETTINGS_PATH, IF_SETTINGS, "ListConnections").await;
    println!("  ListConnections: {} bytes of object paths", listed.len());

    // One Wi-Fi profile that is active, one that is not.
    let mut wrote_active = false;
    let mut wrote_other = false;
    for path in &managed.connections {
        let body = call(&conn, path, CONNECTION, "GetSettings").await;

        // The contract, checked rather than trusted: `GetSettings` omits
        // secrets, so nothing with a secret key is ever written to a file,
        // even one that stays on this machine.
        let secrets = nm::secret_keys(&body).expect("the recorded body parses");
        assert!(
            secrets.is_empty(),
            "{path}: GetSettings carried {secrets:?}"
        );

        let settings = nm::parse_settings(&body).expect("the recorded body parses");
        if settings.kind != nm::TYPE_WIFI {
            println!(
                "  {path}: {} ({}), not recorded",
                settings.id, settings.kind
            );
            continue;
        }
        let active = managed.active.iter().any(|a| a.uuid == settings.uuid);
        let name = match (active, wrote_active, wrote_other) {
            (true, false, _) => {
                wrote_active = true;
                "settings-wifi-active"
            }
            (false, _, false) => {
                wrote_other = true;
                "settings-wifi-other"
            }
            _ => {
                println!("  {path}: {} already covered, not recorded", settings.id);
                continue;
            }
        };
        fs::write(format!("{dir}/{name}.bin"), &body).expect("the fixture writes");
        println!(
            "recorded {name}.bin ({} bytes): id={:?} uuid={} ssid={} bytes active={active}",
            body.len(),
            settings.id,
            settings.uuid,
            settings.ssid.as_ref().map(|s| s.len()).unwrap_or(0)
        );
    }
    assert!(wrote_active, "no active Wi-Fi profile to record");
    assert!(wrote_other, "no second Wi-Fi profile to record");
}

const OBJECT_MANAGER: &str = "org.freedesktop.DBus.ObjectManager";
const IF_SETTINGS: &str = "org.freedesktop.NetworkManager.Settings";
const CONNECTION: &str = "org.freedesktop.NetworkManager.Settings.Connection";

async fn call(conn: &zbus::Connection, path: &str, interface: &str, member: &str) -> Vec<u8> {
    let reply = conn
        .call_method(Some(nm::SERVICE), path, Some(interface), member, &())
        .await
        .unwrap_or_else(|e| panic!("{member} on {path}: {e}"));
    reply.body().data().bytes().to_vec()
}
