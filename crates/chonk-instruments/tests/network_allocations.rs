//! Allocation budgets over synthetic nmcli text. No commands or network I/O.

use chonk_instruments::link_panel::data::{parse_connections, parse_devices};
use chonk_test_support::{measure, AllocationCounter, AllocationStats};

#[global_allocator]
static ALLOCATOR: AllocationCounter = AllocationCounter;

#[test]
fn ignored_device_rows_do_not_allocate_owned_fields() {
    let fixture: String = (0..256)
        .map(|index| format!("veth{index}:veth:unmanaged:--\n"))
        .collect();
    let (devices, stats) = measure(|| parse_devices(&fixture));
    assert!(devices.is_empty());
    eprintln!("256 ignored device rows: {stats:?}");
    assert_eq!(stats, AllocationStats::default());
}

#[test]
fn ignored_connection_rows_do_not_allocate_owned_fields() {
    let fixture: String = (0..256)
        .map(|index| format!("docker{index}:bridge:yes:uuid-{index}\n"))
        .collect();
    let (connections, stats) = measure(|| parse_connections(&fixture));
    assert!(connections.is_empty());
    eprintln!("256 ignored connection rows: {stats:?}");
    assert_eq!(stats, AllocationStats::default());
}
