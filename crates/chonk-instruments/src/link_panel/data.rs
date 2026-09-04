//! The link panel's NetworkManager data layer: pure parsers over
//! `nmcli -t` terse output, fixture-tested against canned text. No
//! entry point here can reach a syscall — the dock samples the
//! commands (see [`crate::link_panel`]'s source declarations) and these
//! functions reduce the stdout it collected to the plain values the
//! panel folds.
//!
//! Terse mode (`-t`) is the stable parse surface nmcli(1) documents:
//! colon-separated fields with `\:` and `\\` escaping inside values.
//! `split_terse` is the one splitter everything shares — including
//! the LNK tile's own parser in `crate::wifi` — because an SSID or a
//! connection name may legally contain both `:` and `\`, and a naive
//! `split(':')` shears such a value apart and misreads its tail.
//!
//! Everything tolerates garbage: a line that does not parse is
//! skipped, never fatal. nmcli's output is input, not a contract.

/// Splits one nmcli terse-mode line on unescaped `:`. Terse mode
/// backslash-escapes both `:` and `\` inside values, so the split has
/// to walk the escapes rather than the bytes.
pub(crate) fn split_terse(line: &str) -> Vec<String> {
    let mut fields = vec![String::new()];
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(escaped) = chars.next() {
                    fields.last_mut().expect("fields never empty").push(escaped);
                }
            }
            ':' => fields.push(String::new()),
            _ => fields.last_mut().expect("fields never empty").push(c),
        }
    }
    fields
}

/// What kind of NIC a device row is. Only the two kinds the panel
/// reasons about get names; a busy machine's zoo of bridges, veth
/// pairs and tunnels is filtered out before this is ever constructed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceKind {
    Ethernet,
    Wifi,
}

/// A device's activation state, reduced from nmcli's STATE column.
/// nmcli suffixes detail in parentheses — `connected (externally)`,
/// `connecting (getting IP configuration)` — so parsing is by prefix,
/// with "externally" kept distinct: an externally-activated device is
/// live, but NetworkManager did not bring it up and a `connection
/// down` may not keep it down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceState {
    Connected,
    ConnectedExternally,
    Connecting,
    Disconnecting,
    Disconnected,
    Unavailable,
    Other,
}

/// One row of `nmcli -t -f DEVICE,TYPE,STATE,CONNECTION device status`
/// that the panel cares about: a managed wifi or ethernet NIC.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NmDevice {
    pub name: String,
    pub kind: DeviceKind,
    pub state: DeviceState,
    /// The active connection's name, empty when none.
    pub connection: String,
}

fn parse_device_state(raw: &str) -> DeviceState {
    let raw = raw.trim();
    if raw == "connected (externally)" {
        return DeviceState::ConnectedExternally;
    }
    let head = raw.split(" (").next().unwrap_or(raw);
    match head {
        "connected" => DeviceState::Connected,
        "connecting" => DeviceState::Connecting,
        "disconnecting" => DeviceState::Disconnecting,
        "disconnected" => DeviceState::Disconnected,
        "unavailable" => DeviceState::Unavailable,
        _ => DeviceState::Other,
    }
}

/// `nmcli -t -f DEVICE,TYPE,STATE,CONNECTION device status`, reduced
/// to the managed real NICs. Bridges, tuns, veths, loopback and
/// unmanaged devices are noise here — the tailscale tunnel gets its
/// own row from its own tool, and docker's bridge zoo gets nothing.
pub fn parse_devices(text: &str) -> Vec<NmDevice> {
    let mut out = Vec::new();
    for line in text.lines() {
        let fields = split_terse(line);
        if fields.len() < 4 {
            continue;
        }
        let kind = match fields[1].as_str() {
            "ethernet" => DeviceKind::Ethernet,
            "wifi" => DeviceKind::Wifi,
            _ => continue,
        };
        let state_raw = fields[2].trim();
        if state_raw == "unmanaged" {
            continue;
        }
        // CONNECTION is the last declared field; a raw colon that
        // survived escaping would split it, so rejoin the tail.
        let connection = fields[3..].join(":");
        out.push(NmDevice { name: fields[0].clone(), kind, state: parse_device_state(state_raw), connection });
    }
    out
}

/// What kind of connection profile a row is — only the kinds worth a
/// toggle row on the panel. NetworkManager's own WireGuard support
/// (`type wireguard`, NM >= 1.16) is the one WireGuard this panel
/// speaks: an NM profile activates without root from an active
/// session, where `wg-quick` needs privileges the dock does not have
/// and should not want.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnKind {
    Ethernet,
    Wifi,
    WireGuard,
    Vpn,
}

/// One row of `nmcli -t -f NAME,TYPE,ACTIVE,UUID connection show`.
/// Inactive rows are the connect candidates; the UUID is what every
/// action argv names, because a NAME may contain anything.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NmConnection {
    pub name: String,
    pub kind: ConnKind,
    pub active: bool,
    pub uuid: String,
}

/// `nmcli -t -f NAME,TYPE,ACTIVE,UUID connection show`, filtered to
/// the togglable kinds. Input order is preserved — nmcli already
/// sorts actives first — so the panel's rows do not jump around
/// between samples.
pub fn parse_connections(text: &str) -> Vec<NmConnection> {
    let mut out = Vec::new();
    for line in text.lines() {
        let fields = split_terse(line);
        if fields.len() < 4 {
            continue;
        }
        // NAME leads and UUID trails; TYPE and ACTIVE sit just before
        // the UUID, so a stray raw colon in the name (escaping should
        // prevent one, but output is input) widens the middle.
        let n = fields.len();
        let name = fields[..n - 3].join(":");
        let kind = match fields[n - 3].as_str() {
            "802-3-ethernet" => ConnKind::Ethernet,
            "802-11-wireless" => ConnKind::Wifi,
            "wireguard" => ConnKind::WireGuard,
            "vpn" => ConnKind::Vpn,
            _ => continue,
        };
        let active = match fields[n - 2].as_str() {
            "yes" => true,
            "no" => false,
            _ => continue,
        };
        let uuid = fields[n - 1].trim().to_string();
        if uuid.is_empty() {
            continue;
        }
        out.push(NmConnection { name, kind, active, uuid });
    }
    out
}

/// One network of the scan list, deduplicated to its best BSS.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WifiNetwork {
    pub ssid: String,
    /// 0-100, clamped.
    pub signal: u8,
    /// Whether associating needs a secret — the gate between
    /// one-click connect and the join dialog.
    pub secured: bool,
    pub in_use: bool,
}

/// `nmcli -t -f IN-USE,SSID,SIGNAL,SECURITY dev wifi list --rescan no`
/// — the cache read, never the radio (see `crate::wifi` for the
/// measured difference). Deduplicates by SSID keeping the strongest
/// BSS (an in-use BSS always wins its SSID), sorts in-use first then
/// strongest first, and drops hidden (empty-SSID) entries — a row the
/// user cannot name is not a row they can join.
pub fn parse_wifi_networks(text: &str) -> Vec<WifiNetwork> {
    let mut best: Vec<WifiNetwork> = Vec::new();
    for line in text.lines() {
        let fields = split_terse(line);
        if fields.len() < 4 {
            continue;
        }
        let n = fields.len();
        let in_use = fields[0].trim() == "*";
        // SSID is everything between IN-USE and the SIGNAL,SECURITY
        // tail, rejoined on the off chance a raw colon survives.
        let ssid = fields[1..n - 2].join(":");
        if ssid.is_empty() {
            continue;
        }
        let Ok(signal) = fields[n - 2].trim().parse::<u8>() else { continue };
        let security = fields[n - 1].trim();
        let secured = !(security.is_empty() || security == "--" || security == "none");
        let net = WifiNetwork { ssid, signal: signal.min(100), secured, in_use };
        match best.iter_mut().find(|b| b.ssid == net.ssid) {
            Some(prev) => {
                if (net.in_use && !prev.in_use) || (net.in_use == prev.in_use && net.signal > prev.signal) {
                    *prev = net;
                }
            }
            None => best.push(net),
        }
    }
    best.sort_by(|a, b| {
        b.in_use.cmp(&a.in_use).then(b.signal.cmp(&a.signal)).then(a.ssid.cmp(&b.ssid))
    });
    best
}

/// Whether a scanned network already has a saved NetworkManager
/// profile to activate — the difference between a one-click
/// `connection up` and the join dialog. Matched by profile name,
/// which is what nmcli itself names a saved wifi network after its
/// SSID; a profile the user renamed will look unknown and offer
/// "Join…", which still works (NetworkManager reuses the stored
/// secret for a network it already knows) — a wrong answer here costs
/// one extra dialog, never a broken connect.
pub fn known_profile<'a>(connections: &'a [NmConnection], ssid: &str) -> Option<&'a NmConnection> {
    connections.iter().find(|c| c.kind == ConnKind::Wifi && c.name == ssid)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEVICES: &str = "\
eno1:ethernet:connected:Wired connection 1
tailscale0:tun:connected (externally):tailscale0
cni0:bridge:connected (externally):cni0
lo:loopback:connected (externally):lo
wlan0:wifi:disconnected:
veth0a2a6d7d:ethernet:unmanaged:
docker0:bridge:connected (externally):docker0
";

    #[test]
    fn devices_keep_only_managed_real_nics() {
        let devices = parse_devices(DEVICES);
        assert_eq!(
            devices,
            vec![
                NmDevice {
                    name: "eno1".into(),
                    kind: DeviceKind::Ethernet,
                    state: DeviceState::Connected,
                    connection: "Wired connection 1".into()
                },
                NmDevice { name: "wlan0".into(), kind: DeviceKind::Wifi, state: DeviceState::Disconnected, connection: String::new() },
            ]
        );
    }

    #[test]
    fn device_states_parse_by_prefix_with_externally_kept_distinct() {
        assert_eq!(parse_device_state("connected"), DeviceState::Connected);
        assert_eq!(parse_device_state("connected (externally)"), DeviceState::ConnectedExternally);
        assert_eq!(parse_device_state("connecting (getting IP configuration)"), DeviceState::Connecting);
        assert_eq!(parse_device_state("connecting (prepare)"), DeviceState::Connecting);
        assert_eq!(parse_device_state("disconnecting"), DeviceState::Disconnecting);
        assert_eq!(parse_device_state("disconnected"), DeviceState::Disconnected);
        assert_eq!(parse_device_state("unavailable"), DeviceState::Unavailable);
        assert_eq!(parse_device_state("deactivating"), DeviceState::Other);
    }

    #[test]
    fn device_garbage_lines_are_skipped_not_fatal() {
        assert!(parse_devices("").is_empty());
        assert!(parse_devices("eno1:ethernet\nhalf:a:line\n").is_empty());
        assert_eq!(parse_devices("eno1:ethernet:connected:Name\\: with colon\n")[0].connection, "Name: with colon");
    }

    const CONNECTIONS: &str = "\
Wired connection 1:802-3-ethernet:yes:f78e907a-0990-383e-a6b8-d194d65b5790
tailscale0:tun:yes:e5ac2866-bf5d-4004-a575-cfd351c70a8f
docker0:bridge:yes:91faa32f-1cbc-4911-b3f3-ba3893196760
HomeBase:802-11-wireless:no:11111111-2222-3333-4444-555555555555
wg-home:wireguard:no:aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee
office-vpn:vpn:no:99999999-8888-7777-6666-555555555555
lo:loopback:yes:9c25c879-aab5-40d0-a50d-ddd0f17922a7
";

    #[test]
    fn connections_keep_the_togglable_kinds_in_order() {
        let conns = parse_connections(CONNECTIONS);
        assert_eq!(
            conns.iter().map(|c| (c.name.as_str(), c.kind, c.active)).collect::<Vec<_>>(),
            vec![
                ("Wired connection 1", ConnKind::Ethernet, true),
                ("HomeBase", ConnKind::Wifi, false),
                ("wg-home", ConnKind::WireGuard, false),
                ("office-vpn", ConnKind::Vpn, false),
            ]
        );
        assert_eq!(conns[0].uuid, "f78e907a-0990-383e-a6b8-d194d65b5790");
    }

    #[test]
    fn connection_names_with_escaped_colons_stay_whole() {
        let conns = parse_connections("Cafe\\: Guest:802-11-wireless:no:12345678-1234-1234-1234-123456789012\n");
        assert_eq!(conns[0].name, "Cafe: Guest");
        assert_eq!(conns[0].uuid, "12345678-1234-1234-1234-123456789012");
    }

    #[test]
    fn connection_rows_without_a_uuid_or_a_kind_are_dropped() {
        assert!(parse_connections("ghost:802-3-ethernet:yes:\n").is_empty());
        assert!(parse_connections("br0:bridge:yes:11111111-2222-3333-4444-555555555555\n").is_empty());
        assert!(parse_connections("weird:802-3-ethernet:maybe:11111111-2222-3333-4444-555555555555\n").is_empty());
    }

    const WIFI_LIST: &str = "\
*:HomeBase:87:WPA2
 :HomeBase:44:WPA2
 :Cafe:61:WPA2 WPA3
 :OpenMesh:52:
 :OldRouter:31:--
 :Lab\\:5G:64:WPA2
 ::71:WPA2
 :Broken:notanumber:WPA2
";

    #[test]
    fn wifi_list_dedups_sorts_and_reads_security() {
        let nets = parse_wifi_networks(WIFI_LIST);
        assert_eq!(
            nets.iter().map(|n| (n.ssid.as_str(), n.signal, n.secured, n.in_use)).collect::<Vec<_>>(),
            vec![
                ("HomeBase", 87, true, true),
                ("Lab:5G", 64, true, false),
                ("Cafe", 61, true, false),
                ("OpenMesh", 52, false, false),
                ("OldRouter", 31, false, false),
            ]
        );
    }

    #[test]
    fn an_in_use_bss_wins_its_ssid_even_when_weaker() {
        let nets = parse_wifi_networks(" :Net:90:WPA2\n*:Net:40:WPA2\n");
        assert_eq!(nets.len(), 1);
        assert!(nets[0].in_use);
        assert_eq!(nets[0].signal, 40, "the associated BSS is the honest reading");
    }

    #[test]
    fn wifi_signal_clamps_and_hidden_ssids_drop() {
        let nets = parse_wifi_networks(" :Hot:250:WPA2\n ::99:WPA2\n");
        assert_eq!(nets.len(), 1);
        assert_eq!(nets[0].signal, 100);
    }

    #[test]
    fn known_profile_matches_wifi_profiles_by_ssid_name() {
        let conns = parse_connections(CONNECTIONS);
        assert_eq!(known_profile(&conns, "HomeBase").map(|c| c.uuid.as_str()), Some("11111111-2222-3333-4444-555555555555"));
        assert!(known_profile(&conns, "Cafe").is_none());
        assert!(known_profile(&conns, "Wired connection 1").is_none(), "an ethernet profile is not a wifi credential");
    }

    #[test]
    fn split_terse_handles_the_escapes() {
        assert_eq!(split_terse("a\\:b:c"), vec!["a:b", "c"]);
        assert_eq!(split_terse("back\\\\slash:x"), vec!["back\\slash", "x"]);
        assert_eq!(split_terse(""), vec![""]);
        assert_eq!(split_terse("trailing:"), vec!["trailing", ""]);
    }
}
