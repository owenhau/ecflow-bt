//! CRC8 (poly 0x07, init 0) and CRC16/ARC (poly 0x8005 reflected, init 0).

const fn build_crc8() -> [u8; 256] {
    let mut table = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u8;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 0x80 != 0 { (crc << 1) ^ 0x07 } else { crc << 1 };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

const fn build_crc16() -> [u16; 256] {
    let mut table = [0u16; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u16;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xA001 } else { crc >> 1 };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

static CRC8_TABLE: [u8; 256] = build_crc8();
static CRC16_TABLE: [u16; 256] = build_crc16();

pub fn crc8(data: &[u8]) -> u8 {
    data.iter().fold(0u8, |crc, &b| CRC8_TABLE[(crc ^ b) as usize])
}

pub fn crc16(data: &[u8]) -> u16 {
    data.iter().fold(0u16, |crc, &b| {
        CRC16_TABLE[((crc ^ b as u16) & 0xFF) as usize] ^ (crc >> 8)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc8_check_value() {
        // CRC-8 (poly 0x07) check value for "123456789" is 0xF4.
        assert_eq!(crc8(b"123456789"), 0xF4);
    }

    #[test]
    fn crc16_arc_check_value() {
        // CRC-16/ARC check value for "123456789" is 0xBB3D.
        assert_eq!(crc16(b"123456789"), 0xBB3D);
    }
}
