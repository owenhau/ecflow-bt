//! Inner packet codec (V2/V3 and the 0x13 "sentinel" variant).
//!
//! Layout (V3): `AA ver len16 crc8 | product seq[4] 00 00 | src dst dsrc ddst cmd_set cmd_id | payload | crc16`.

use super::crc::{crc8, crc16};

pub const PREFIX: u8 = 0xAA;
pub const CMD_SET_RET_TIME: u8 = 0x52;
pub const CMD_CHECK_RET_TIME: u8 = 0x53;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PacketError {
    #[error("bad packet prefix")]
    BadPrefix,
    #[error("packet too small ({0} bytes)")]
    TooSmall(usize),
    #[error("unsupported packet version 0x{0:02X}")]
    UnsupportedVersion(u8),
    #[error("header CRC8 mismatch")]
    BadCrc8,
    #[error("packet CRC16 mismatch")]
    BadCrc16,
    #[error("payload length {declared} exceeds available {available} bytes")]
    Truncated { declared: usize, available: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    pub src: u8,
    pub dst: u8,
    pub cmd_set: u8,
    pub cmd_id: u8,
    pub payload: Vec<u8>,
    pub dsrc: u8,
    pub ddst: u8,
    /// Raw version byte (0x03 normally, 0x13 for the sentinel variant).
    pub version: u8,
    pub seq: [u8; 4],
}

impl Packet {
    pub fn new(src: u8, dst: u8, cmd_set: u8, cmd_id: u8, payload: Vec<u8>, version: u8) -> Self {
        Self {
            src,
            dst,
            cmd_set,
            cmd_id,
            payload,
            dsrc: 1,
            ddst: 1,
            version,
            seq: [0; 4],
        }
    }

    pub fn from_bytes(data: &[u8], xor_payload: bool) -> Result<Self, PacketError> {
        if data.first() != Some(&PREFIX) {
            return Err(PacketError::BadPrefix);
        }
        if data.len() < 5 {
            return Err(PacketError::TooSmall(data.len()));
        }
        let version_byte = data[1];
        if version_byte == 4 {
            return Err(PacketError::UnsupportedVersion(4));
        }
        let version = version_byte & 0x0F;
        let sentinel = version_byte & 0x10 != 0;
        if version != 2 && version != 3 {
            return Err(PacketError::UnsupportedVersion(version_byte));
        }
        if (version == 2 && data.len() < 18) || (version == 3 && data.len() < 20) {
            return Err(PacketError::TooSmall(data.len()));
        }
        let payload_len = u16::from_le_bytes([data[2], data[3]]) as usize;

        if !sentinel && !data.ends_with(&[0xBB, 0xBB]) {
            let (body, tail) = data.split_at(data.len() - 2);
            if crc16(body) != u16::from_le_bytes([tail[0], tail[1]]) {
                return Err(PacketError::BadCrc16);
            }
        }
        if crc8(&data[..4]) != data[4] {
            return Err(PacketError::BadCrc8);
        }

        let seq = [data[6], data[7], data[8], data[9]];
        let src = data[12];
        let dst = data[13];
        let (dsrc, ddst, cmd_set, cmd_id, payload_start) = if version == 2 {
            (0, 0, data[14], data[15], 16)
        } else {
            (data[14], data[15], data[16], data[17], 18)
        };

        let mut payload = Vec::new();
        if payload_len > 0 {
            let end = payload_start + payload_len;
            if end > data.len() {
                return Err(PacketError::Truncated {
                    declared: payload_len,
                    available: data.len() - payload_start,
                });
            }
            payload = data[payload_start..end].to_vec();
            if xor_payload && seq[0] != 0 {
                for b in &mut payload {
                    *b ^= seq[0];
                }
            }
            if sentinel && payload.ends_with(&[0xBB, 0xBB]) {
                payload.truncate(payload.len() - 2);
            }
        }

        Ok(Self {
            src,
            dst,
            cmd_set,
            cmd_id,
            payload,
            dsrc,
            ddst,
            version: version_byte,
            seq,
        })
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut data = vec![PREFIX, self.version];
        data.extend_from_slice(&(self.payload.len() as u16).to_le_bytes());
        data.push(crc8(&data));
        data.push(0x0D); // product byte
        data.extend_from_slice(&self.seq);
        data.extend_from_slice(&[0, 0]);
        data.push(self.src);
        data.push(self.dst);
        if self.version >= 0x03 {
            data.push(self.dsrc);
            data.push(self.ddst);
        }
        data.push(self.cmd_set);
        data.push(self.cmd_id);
        data.extend_from_slice(&self.payload);
        let crc = crc16(&data);
        data.extend_from_slice(&crc.to_le_bytes());
        data
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Packet {
        Packet::new(0x21, 0x35, 0x35, 0x86, b"ABCDEF".to_vec(), 0x03)
    }

    #[test]
    fn round_trip() {
        let p = sample();
        let bytes = p.to_bytes();
        assert_eq!(Packet::from_bytes(&bytes, false).unwrap(), p);
    }

    #[test]
    fn empty_payload_round_trip() {
        let p = Packet::new(0x21, 0x35, 0x35, 0x89, vec![], 0x03);
        let bytes = p.to_bytes();
        assert_eq!(bytes.len(), 20);
        assert_eq!(Packet::from_bytes(&bytes, false).unwrap(), p);
    }

    #[test]
    fn rejects_bad_prefix_and_short() {
        assert_eq!(Packet::from_bytes(&[0x00; 20], false), Err(PacketError::BadPrefix));
        assert!(matches!(
            Packet::from_bytes(&[0xAA, 3, 0, 0, 0], false),
            Err(PacketError::TooSmall(5))
        ));
    }

    #[test]
    fn rejects_bad_crc16() {
        let mut bytes = sample().to_bytes();
        let n = bytes.len();
        bytes[n - 1] ^= 0xFF;
        assert_eq!(Packet::from_bytes(&bytes, false), Err(PacketError::BadCrc16));
    }

    #[test]
    fn rejects_bad_header_crc8() {
        let mut bytes = sample().to_bytes();
        bytes[4] ^= 0x01;
        // CRC16 covers the header too, so fix it up to isolate the CRC8 check.
        let n = bytes.len();
        let crc = crc16(&bytes[..n - 2]).to_le_bytes();
        bytes[n - 2..].copy_from_slice(&crc);
        assert_eq!(Packet::from_bytes(&bytes, false), Err(PacketError::BadCrc8));
    }

    #[test]
    fn xor_payload_is_undone_with_seq0() {
        let mut p = sample();
        p.seq = [0x5A, 0, 0, 0];
        let plain = p.payload.clone();
        p.payload = plain.iter().map(|b| b ^ 0x5A).collect();
        let bytes = p.to_bytes();
        let parsed = Packet::from_bytes(&bytes, true).unwrap();
        assert_eq!(parsed.payload, plain);
        // Without the flag the payload stays obfuscated.
        assert_ne!(Packet::from_bytes(&bytes, false).unwrap().payload, plain);
    }

    #[test]
    fn sentinel_variant_strips_bb_bb_and_skips_crc16() {
        let mut p = sample();
        p.version = 0x13;
        p.payload = vec![1, 2, 3, 0xBB, 0xBB];
        let mut bytes = p.to_bytes();
        // Sentinel frames carry no valid trailing CRC16.
        let n = bytes.len();
        bytes[n - 2..].copy_from_slice(&[0, 0]);
        let parsed = Packet::from_bytes(&bytes, false).unwrap();
        assert_eq!(parsed.payload, vec![1, 2, 3]);
    }

    #[test]
    fn truncated_payload_is_rejected() {
        let mut bytes = Packet::new(1, 2, 3, 4, vec![9; 10], 0x13).to_bytes();
        bytes.truncate(24);
        assert!(matches!(
            Packet::from_bytes(&bytes, false),
            Err(PacketError::Truncated { .. })
        ));
    }
}
