//! Minimal ARP-cache lookup for MAC enrichment: SSDP replies and pairing
//! traffic have just primed the kernel's neighbor table, so a discovered
//! TV's MAC — the Wake-on-LAN prerequisite in the LG vendor config — is
//! one `/proc/net/arp` read away.

use std::net::Ipv4Addr;

pub(crate) fn lookup_mac(ip: &str) -> Option<String> {
    if ip.parse::<Ipv4Addr>().is_err() {
        return None;
    }
    let table = std::fs::read_to_string("/proc/net/arp").ok()?;
    parse_mac(&table, ip)
}

/// Columns: IP address, HW type, Flags, HW address, Mask, Device.
/// Flags `0x2` marks a complete entry; incomplete rows carry a zero MAC.
fn parse_mac(table: &str, ip: &str) -> Option<String> {
    for line in table.lines().skip(1) {
        let mut fields = line.split_whitespace();
        let (Some(entry_ip), Some(_hw_type), Some(flags), Some(mac)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if entry_ip == ip && flags != "0x0" && mac != "00:00:00:00:00:00" {
            return Some(mac.to_uppercase());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: &str = "\
IP address       HW type     Flags       HW address            Mask     Device
192.168.1.170    0x1         0x2         84:90:0a:63:a9:94     *        eno2
192.168.1.74     0x1         0x0         00:00:00:00:00:00     *        eno2
192.168.1.239    0x1         0x2         56:cf:bb:83:4b:4e     *        eno2
";

    #[test]
    fn finds_complete_entries_uppercased() {
        assert_eq!(
            parse_mac(TABLE, "192.168.1.170").as_deref(),
            Some("84:90:0A:63:A9:94")
        );
    }

    #[test]
    fn skips_incomplete_and_missing_entries() {
        assert_eq!(parse_mac(TABLE, "192.168.1.74"), None, "flags 0x0");
        assert_eq!(parse_mac(TABLE, "192.168.1.99"), None, "absent");
    }

    #[test]
    fn non_ipv4_addresses_short_circuit() {
        assert_eq!(lookup_mac("mock://den"), None);
    }
}
