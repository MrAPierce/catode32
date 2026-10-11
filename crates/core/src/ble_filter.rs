//! Turns raw BLE advertising reports into stable location beacons for the
//! tracker. Pure logic: the H2 scan driver feeds it, tests exercise it on
//! the host.

use heapless::Vec;

use crate::wifi_tracker::ScanAp;

/// Weakest signal still treated as "here" (dBm).
pub const BLE_RSSI_FLOOR: i8 = -85;
/// Strongest N beacons kept per scan, so a crowd doesn't flood the lists.
pub const BLE_MAX_REPORTS: usize = 16;
/// Familiar beacons that must be visible to count as a known place.
pub const BLE_QUORUM: usize = 2;

/// HCI address types, as reported by the controller.
pub const ADDR_PUBLIC: u8 = 0x00;
pub const ADDR_RANDOM: u8 = 0x01;

/// Catode32 advert marker; our own devices must not count as landmarks.
pub const CATODE_MAGIC: [u8; 2] = [0xC3, 0x32];

/// Random addresses: top two bits of the most significant byte (address is
/// little-endian) are 11 = static, 01 = resolvable private, 00 = non-resolvable.
fn is_stable_random(addr: &[u8; 6]) -> bool {
    addr[5] >> 6 == 0b11
}

fn is_catode_advert(data: &[u8]) -> bool {
    let mut i = 0;
    while i + 1 < data.len() {
        let len = data[i] as usize;
        if len == 0 || i + 1 + len > data.len() {
            break;
        }
        // 0xFF = manufacturer specific data
        if data[i + 1] == 0xFF && len >= 3 && data[i + 2..i + 4] == CATODE_MAGIC {
            return true;
        }
        i += 1 + len;
    }
    false
}

fn local_name(data: &[u8]) -> Option<&[u8]> {
    let mut i = 0;
    while i + 1 < data.len() {
        let len = data[i] as usize;
        if len == 0 || i + 1 + len > data.len() {
            break;
        }
        // 0x08 shortened, 0x09 complete local name
        if matches!(data[i + 1], 0x08 | 0x09) {
            return Some(&data[i + 2..i + 1 + len]);
        }
        i += 1 + len;
    }
    None
}

/// Filter one scan's worth of reports. `reports` yields
/// `(addr_kind, addr, rssi, adv_data)`. Duplicates keep the strongest RSSI.
pub fn filter_reports<'a, I>(reports: I) -> Vec<ScanAp, 32>
where
    I: IntoIterator<Item = (u8, [u8; 6], i8, &'a [u8])>,
{
    let mut out: Vec<ScanAp, 32> = Vec::new();
    for (kind, addr, rssi, data) in reports {
        let stable = match kind {
            ADDR_PUBLIC => true,
            ADDR_RANDOM => is_stable_random(&addr),
            _ => false,
        };
        if !stable || rssi < BLE_RSSI_FLOOR || is_catode_advert(data) {
            continue;
        }
        if let Some(existing) = out.iter_mut().find(|a| a.bssid == addr) {
            if rssi > existing.rssi {
                existing.rssi = rssi;
            }
            continue;
        }
        let mut ssid = heapless::String::new();
        if let Some(name) = local_name(data).and_then(|n| core::str::from_utf8(n).ok()) {
            for c in name.chars() {
                if ssid.push(c).is_err() {
                    break;
                }
            }
        }
        if out.is_full() {
            // Replace the weakest if this one is stronger.
            let (idx, weakest) = out
                .iter()
                .enumerate()
                .min_by_key(|(_, a)| a.rssi)
                .map(|(i, a)| (i, a.rssi))
                .unwrap();
            if rssi <= weakest {
                continue;
            }
            out.swap_remove(idx);
        }
        let _ = out.push(ScanAp { bssid: addr, ssid, rssi });
    }
    out.sort_unstable_by(|a, b| b.rssi.cmp(&a.rssi));
    out.truncate(BLE_MAX_REPORTS);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: &[u8] = &[];

    fn addr(last: u8, top: u8) -> [u8; 6] {
        [last, 0, 0, 0, 0, top]
    }

    #[test]
    fn keeps_public_and_static_random_only() {
        let r = [
            (ADDR_PUBLIC, addr(1, 0x10), -50, NONE),
            (ADDR_RANDOM, addr(2, 0xC1), -50, NONE),
            (ADDR_RANDOM, addr(3, 0x41), -50, NONE),
            (ADDR_RANDOM, addr(4, 0x01), -50, NONE),
        ];
        let out = filter_reports(r);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn drops_weak_and_catode_adverts() {
        let ours: &[u8] = &[4, 0xFF, 0xC3, 0x32, 0x01];
        let r = [
            (ADDR_PUBLIC, addr(1, 0x10), -90, NONE),
            (ADDR_PUBLIC, addr(2, 0x10), -40, ours),
            (ADDR_PUBLIC, addr(3, 0x10), -40, NONE),
        ];
        let out = filter_reports(r);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].bssid[0], 3);
    }

    #[test]
    fn dedups_keeping_strongest_and_reads_name() {
        let named: &[u8] = &[4, 0x09, b'T', b'V', b'1'];
        let r = [
            (ADDR_PUBLIC, addr(1, 0x10), -70, named),
            (ADDR_PUBLIC, addr(1, 0x10), -55, NONE),
        ];
        let out = filter_reports(r);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].rssi, -55);
        assert_eq!(out[0].ssid.as_str(), "TV1");
    }

    #[test]
    fn caps_to_strongest_when_crowded() {
        let mut v: std::vec::Vec<(u8, [u8; 6], i8, &[u8])> = std::vec::Vec::new();
        for i in 0..40u8 {
            v.push((ADDR_PUBLIC, addr(i, 0x10), -84 + (i as i8 % 30) - 0, NONE));
        }
        let out = filter_reports(v);
        assert!(out.len() <= BLE_MAX_REPORTS);
        assert!(out.windows(2).all(|w| w[0].rssi >= w[1].rssi));
    }
}
