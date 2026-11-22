//! Outer frame ("EncPacket") codec carried over the BLE characteristics.
//!
//! Wire format: `5A 5A | frame_type<<4 | 01 | len16 (payload+2) | payload | crc16`.
//! BLE notifications can split or merge frames, so [`FrameSplitter`] buffers input.

use super::crc::crc16;

pub const PREFIX: [u8; 2] = [0x5A, 0x5A];
pub const FRAME_TYPE_COMMAND: u8 = 0x00;
pub const FRAME_TYPE_PROTOCOL: u8 = 0x01;
const MAX_PAYLOAD: usize = 10_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawFrame {
    pub frame_type: u8,
    pub payload: Vec<u8>,
}

pub fn encode_frame(frame_type: u8, payload: &[u8]) -> Vec<u8> {
    let mut data = Vec::with_capacity(payload.len() + 8);
    data.extend_from_slice(&PREFIX);
    data.push(frame_type << 4);
    data.push(0x01);
    data.extend_from_slice(&((payload.len() + 2) as u16).to_le_bytes());
    data.extend_from_slice(payload);
    let crc = crc16(&data);
    data.extend_from_slice(&crc.to_le_bytes());
    data
}

fn find_prefix(data: &[u8]) -> Option<usize> {
    data.windows(2).position(|w| w == PREFIX)
}

#[derive(Debug, Default)]
pub struct FrameSplitter {
    buffer: Vec<u8>,
}

impl FrameSplitter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one notification; returns every complete, CRC-valid frame it finished.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<RawFrame> {
        let mut data = std::mem::take(&mut self.buffer);
        data.extend_from_slice(chunk);
        let mut frames = Vec::new();

        while !data.is_empty() {
            let Some(start) = find_prefix(&data) else {
                data.clear();
                break;
            };
            if start > 0 {
                data.drain(..start);
            }
            if data.len() < 8 {
                break;
            }
            let payload_len = u16::from_le_bytes([data[4], data[5]]) as usize;
            if !(2..=MAX_PAYLOAD).contains(&payload_len) {
                data.drain(..2);
                continue;
            }
            let end = 6 + payload_len;
            if end > data.len() {
                // Incomplete frame: wait for more data. A bogus length is bounded by
                // MAX_PAYLOAD and resyncs through the CRC check once enough arrives.
                break;
            }
            let body = &data[6..end - 2];
            let crc = u16::from_le_bytes([data[end - 2], data[end - 1]]);
            if crc16(&data[..end - 2]) != crc {
                data.drain(..2);
                continue;
            }
            frames.push(RawFrame {
                frame_type: data[2] >> 4,
                payload: body.to_vec(),
            });
            data.drain(..end);
        }

        self.buffer = data;
        frames
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_then_split() {
        let bytes = encode_frame(FRAME_TYPE_PROTOCOL, b"hello");
        let mut s = FrameSplitter::new();
        let frames = s.push(&bytes);
        assert_eq!(
            frames,
            vec![RawFrame {
                frame_type: 1,
                payload: b"hello".to_vec()
            }]
        );
    }

    #[test]
    fn handles_fragmented_and_merged_notifications() {
        let a = encode_frame(FRAME_TYPE_COMMAND, &[1, 2, 3]);
        let b = encode_frame(FRAME_TYPE_PROTOCOL, &[9; 40]);
        let mut all = a.clone();
        all.extend_from_slice(&b);
        let mut s = FrameSplitter::new();
        let mut got = Vec::new();
        for chunk in all.chunks(7) {
            got.extend(s.push(chunk));
        }
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].frame_type, 0);
        assert_eq!(got[1].payload, vec![9; 40]);

        let mut s = FrameSplitter::new();
        assert_eq!(s.push(&all).len(), 2);
    }

    #[test]
    fn skips_garbage_and_bad_crc() {
        let good = encode_frame(FRAME_TYPE_PROTOCOL, &[7; 10]);
        let mut bad = encode_frame(FRAME_TYPE_PROTOCOL, &[8; 10]);
        let n = bad.len();
        bad[n - 1] ^= 0xFF;
        let mut data = vec![0x00, 0x11, 0x22];
        data.extend_from_slice(&bad);
        data.extend_from_slice(&good);
        let mut s = FrameSplitter::new();
        let frames = s.push(&data);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].payload, vec![7; 10]);
    }

    #[test]
    fn payload_containing_prefix_bytes() {
        let payload = [0x5A, 0x5A, 1, 2, 3, 0x5A, 0x5A, 4, 5, 6];
        let bytes = encode_frame(FRAME_TYPE_PROTOCOL, &payload);
        let mut s = FrameSplitter::new();
        let frames: Vec<_> = bytes.chunks(5).flat_map(|c| s.push(c)).collect();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].payload, payload);
    }
}
