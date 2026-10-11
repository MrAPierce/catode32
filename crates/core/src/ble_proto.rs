//! Wire-level constants and pure helpers for H2 BLE messaging.
//!
//! Design: every H2 advertises a small connectable catode32 advert while
//! idle. The inviter scans, connects as central, and exchanges the existing
//! message payloads through a two-characteristic GATT service. The invitee
//! is the peripheral. Payloads are the same bytes ESP-NOW carries, so the
//! game protocol is unchanged; GATT gives ordering and delivery.

use crate::ble_filter::CATODE_MAGIC;
use crate::transport::PeerId;

/// Advert/protocol version.
pub const PROTO_VER: u8 = 1;

/// Bytes in an advert built by [`encode_advert`].
pub const ADVERT_LEN: usize = 10;

/// Largest payload that fits one write/notify at the default ATT MTU (23 - 3).
pub const DEFAULT_ATT_PAYLOAD: usize = 20;

/// 128-bit service UUID (little-endian bytes as sent on air).
pub const SERVICE_UUID: [u8; 16] = [
    0x32, 0xC3, 0x5E, 0x1A, 0x7D, 0x4B, 0x4C, 0x91, 0xA2, 0x0F, 0x6B, 0x83, 0x00, 0x1C, 0x32, 0xC3,
];
/// Central -> peripheral: write-without-response.
pub const RX_CHAR_UUID: [u8; 16] = {
    let mut u = SERVICE_UUID;
    u[12] = 0x01;
    u
};
/// Peripheral -> central: notify.
pub const TX_CHAR_UUID: [u8; 16] = {
    let mut u = SERVICE_UUID;
    u[12] = 0x02;
    u
};

/// Flags (LE general discoverable, no BR/EDR) + manufacturer data
/// `[C3 32][ver][id 2B]`. Short enough to leave room for a scan response.
pub fn encode_advert(id: PeerId) -> [u8; ADVERT_LEN] {
    let b = id.as_bytes();
    [
        2, 0x01, 0x06, //
        6, 0xFF, CATODE_MAGIC[0], CATODE_MAGIC[1], PROTO_VER, b[0], b[1],
    ]
}

/// Extract the short peer id from a catode32 advert, if it is one.
pub fn parse_advert(data: &[u8]) -> Option<PeerId> {
    let mut i = 0;
    while i + 1 < data.len() {
        let len = data[i] as usize;
        if len == 0 || i + 1 + len > data.len() {
            return None;
        }
        let body = &data[i + 1..i + 1 + len];
        if body.len() >= 6 && body[0] == 0xFF && body[1..3] == CATODE_MAGIC && body[3] == PROTO_VER {
            return Some(PeerId::from_short(u16::from_be_bytes([body[4], body[5]])));
        }
        i += 1 + len;
    }
    None
}

/// Split a payload into chunks that fit one ATT write/notify.
/// Single-frame messages (the common case) yield exactly one chunk.
pub fn chunks(payload: &[u8], att_payload: usize) -> impl Iterator<Item = &[u8]> {
    payload.chunks(att_payload.max(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ble_filter::filter_reports;

    #[test]
    fn advert_round_trips() {
        let id = PeerId::from_short(0xABCD);
        let adv = encode_advert(id);
        assert_eq!(parse_advert(&adv), Some(id));
    }

    #[test]
    fn foreign_adverts_are_not_ours() {
        assert_eq!(parse_advert(&[2, 0x01, 0x06]), None);
        assert_eq!(parse_advert(&[4, 0xFF, 0x4C, 0x00, 0x01]), None);
        assert_eq!(parse_advert(&[]), None);
    }

    #[test]
    fn location_filter_ignores_our_adverts() {
        let adv = encode_advert(PeerId::from_short(0x0102));
        let r = [(0u8, [1, 2, 3, 4, 5, 0x10], -40i8, &adv[..])];
        assert!(filter_reports(r).is_empty());
    }

    #[test]
    fn chunking_respects_mtu() {
        let data = [0u8; 45];
        let sizes: std::vec::Vec<usize> = chunks(&data, DEFAULT_ATT_PAYLOAD).map(|c| c.len()).collect();
        assert_eq!(sizes, [20, 20, 5]);
    }
}
