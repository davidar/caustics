//! The `/proc` and sysfs parsers, against snapshots recorded on a machine a
//! second apart (`tests/data/`), with the column each pair should
//! produce written out. The expected numbers were worked by hand from the two
//! files, so a parser that starts reading the wrong field fails here rather
//! than drawing a wrong chart.

use caustics_sysmon::{GAP, cpu, disk, memory, net, temp};

const STAT_0: &str = include_str!("data/stat.0");
const STAT_1: &str = include_str!("data/stat.1");
const MEMINFO: &str = include_str!("data/meminfo.0");
const NET_0: &str = include_str!("data/net-dev.0");
const NET_1: &str = include_str!("data/net-dev.1");
const DISK_0: &str = include_str!("data/diskstats.0");
const DISK_1: &str = include_str!("data/diskstats.1");

fn near(got: f32, want: f32, tolerance: f32) {
    assert!(
        (got - want).abs() <= tolerance,
        "got {got}, wanted {want} ± {tolerance}"
    );
}

/// Δtotal = 1631 jiffies, Δ(idle+iowait) = 1555, so busy = 76/1631.
#[test]
fn cpu_busy_is_the_non_idle_share_of_the_tick() {
    let mut cpu = cpu::Cpu::default();
    assert_eq!(cpu.sample_text(STAT_0), GAP, "a rate needs two readings");
    near(cpu.sample_text(STAT_1), 76.0 / 1631.0, 1e-6);
    // A file that is not /proc/stat resets rather than reporting nonsense.
    assert_eq!(cpu.sample_text("nothing like a stat file"), GAP);
    assert_eq!(
        cpu.sample_text(STAT_1),
        GAP,
        "and the next tick is a gap again"
    );
}

/// MemTotal 32009584 kB, MemAvailable 17576848 kB.
#[test]
fn memory_is_the_unavailable_share() {
    near(memory::parse(MEMINFO), 14_432_736.0 / 32_009_584.0, 1e-6);
    assert_eq!(memory::parse("Bogus: 1 kB\n"), GAP);
}

/// Summed over wlo1 and tailscale0, never `lo`: Δrx 159437 B, Δtx 14382 B.
#[test]
fn network_sums_every_interface_but_the_loopback() {
    let mut all = net::Net::new(None);
    assert_eq!(all.sample_text(NET_0, 1.0), [GAP, GAP]);
    let column = all.sample_text(NET_1, 1.0);
    near(column[0], 159_437.0, 1.0);
    near(column[1], 14_382.0, 1.0);

    // `lo` moved not at all between the two snapshots, but it is excluded from
    // the totals on principle: it is loopback traffic, not a link.
    let counters = net::parse(NET_0);
    let lo = counters
        .iter()
        .find(|c| c.name == "lo")
        .expect("lo is in the file");
    let (rx, _) = net::total(&counters, None).unwrap();
    assert_eq!(
        rx,
        23_955_330_717 + 24_614,
        "wlo1 + tailscale0, and nothing else"
    );
    assert_ne!(rx, rx + lo.rx, "the loopback's 9 GB is not in the sum");

    // One named interface is just that interface.
    let mut one = net::Net::new(Some("wlo1".into()));
    assert_eq!(one.sample_text(NET_0, 1.0), [GAP, GAP]);
    near(one.sample_text(NET_1, 1.0)[0], 159_437.0, 1.0);

    // An interface that is not there is all gaps, not a panic.
    let mut missing = net::Net::new(Some("eth42".into()));
    assert_eq!(missing.sample_text(NET_0, 1.0), [GAP, GAP]);
    assert_eq!(missing.sample_text(NET_1, 1.0), [GAP, GAP]);
}

/// nvme0n1 alone: Δ144 sectors read, Δ64 written, × 512 B.
#[test]
fn disk_counts_whole_disks_and_not_their_partitions() {
    let mut all = disk::Disk::new(None);
    assert_eq!(all.sample_text(DISK_0, 1.0), [GAP, GAP]);
    let column = all.sample_text(DISK_1, 1.0);
    near(column[0], 144.0 * 512.0, 1.0);
    near(column[1], 64.0 * 512.0, 1.0);

    // The snapshot has seven partitions of nvme0n1, one of which moved by the
    // same 144 sectors the disk did: counting it would double the read rate.
    let counters = disk::parse(DISK_0);
    assert!(counters.iter().any(|c| c.name == "nvme0n1p6"));
    assert_eq!(disk::total(&counters, None).unwrap().0, 610_868_171);
}

/// zram and loop devices are block devices but not disks.
#[test]
fn the_virtual_block_devices_are_skipped() {
    // The recorded file has zram0 and loop0 with large counters; a sum that
    // included them would be nowhere near nvme0n1's alone.
    let counters = disk::parse(DISK_0);
    assert!(counters.iter().any(|c| c.name == "zram0"));
    assert!(counters.iter().any(|c| c.name == "loop0"));
    let (read, _) = disk::total(&counters, None).unwrap();
    let nvme = counters.iter().find(|c| c.name == "nvme0n1").unwrap();
    assert_eq!(read, nvme.read, "only the one whole disk is in the sum");

    // Named explicitly, a virtual device is still sampled: the filter is for
    // the bare `disk` word's arithmetic, not a ban.
    assert_eq!(disk::total(&counters, Some("zram0")).unwrap().0, 22_872_168);
}

/// A synthetic hwmon tree: `k10temp`/`Tctl` beats `acpitz`, whichever order
/// the directories are read in.
#[test]
fn the_temperature_sensor_is_chosen_by_driver_then_label() {
    let root = scratch("hwmon-pick");
    write(&root, "hwmon0/name", "acpitz");
    write(&root, "hwmon0/temp1_input", "56000");
    write(&root, "hwmon1/name", "nvme");
    write(&root, "hwmon1/temp1_input", "41850");
    write(&root, "hwmon2/name", "k10temp");
    write(&root, "hwmon2/temp1_label", "Tctl");
    write(&root, "hwmon2/temp1_input", "45375");

    let source = temp::discover(&root).expect("a sensor");
    assert_eq!(source.witness, "hwmon2/k10temp/Tctl");
    assert_eq!(temp::read(&source.input), 45.375);

    // Without k10temp, the ACPI thermal zone is the last resort.
    std::fs::remove_dir_all(root.join("hwmon2")).unwrap();
    let source = temp::discover(&root).expect("a fallback sensor");
    assert_eq!(source.witness, "hwmon0/acpitz/temp1");

    // With neither, discovery says nothing rather than guessing.
    std::fs::remove_dir_all(root.join("hwmon0")).unwrap();
    assert_eq!(temp::discover(&root), None);
}

/// Intel's package sensor is found by its label, not by its index.
#[test]
fn coretemp_is_found_by_its_package_label() {
    let root = scratch("hwmon-coretemp");
    write(&root, "hwmon3/name", "coretemp");
    write(&root, "hwmon3/temp1_label", "Core 0");
    write(&root, "hwmon3/temp1_input", "40000");
    write(&root, "hwmon3/temp5_label", "Package id 0");
    write(&root, "hwmon3/temp5_input", "52000");

    let source = temp::discover(&root).expect("a sensor");
    assert_eq!(source.witness, "hwmon3/coretemp/Package id 0");
    assert_eq!(temp::read(&source.input), 52.0);
}

// ---------------------------------------------------------------------------

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "caustics-sysmon-test-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(root: &std::path::Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, format!("{contents}\n")).unwrap();
}
