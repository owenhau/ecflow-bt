//! Device identification: serial prefixes, advertisement parsing, pure matcher.

use serde::Serialize;
use std::collections::HashMap;

/// EcoFlow's BLE manufacturer id.
pub const MANUFACTURER_ID: u16 = 0xB5B5;
pub const NAME_PREFIX: &str = "EF-R3";

const RIVER3_PREFIXES: &[(&str, &str)] = &[
    ("R651", "River 3 (245Wh)"),
    ("R653", "River 3 (230Wh)"),
    ("R654", "River 3 UPS (230Wh)"),
    ("R655", "River 3 UPS (245Wh)"),
];
const RIVER3_PLUS_PREFIXES: &[(&str, &str)] = &[
    ("R631", "River 3 Plus"),
    ("R634", "River 3 Plus (270)"),
    ("R635", "River 3 Plus Wireless"),
];

/// Model name for a serial number, if it is a supported River 3 / River 3 Plus.
pub fn model_for_serial(serial: &str) -> Option<&'static str> {
    let prefix = serial.get(..4)?;
    RIVER3_PREFIXES
        .iter()
        .chain(RIVER3_PLUS_PREFIXES)
        .find(|(p, _)| *p == prefix)
        .map(|(_, name)| *name)
}

/// Fields of the EcoFlow manufacturer-data record (V2 scan record).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanRecord {
    pub proto_version: u8,
    pub serial: String,
    pub status: u8,
    pub product_type: u8,
    pub capability_flags: u8,
}

impl ScanRecord {
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < 17 {
            return None;
        }
        let serial_bytes = &data[1..17];
        if !serial_bytes.iter().all(|b| b.is_ascii_graphic()) {
            return None;
        }
        Some(Self {
            proto_version: data[0],
            serial: String::from_utf8_lossy(serial_bytes).into_owned(),
            status: data.get(17).copied().unwrap_or(0),
            product_type: data.get(18).copied().unwrap_or(0),
            capability_flags: data.get(22).copied().unwrap_or(0b0111000),
        })
    }

    /// 0 = plain, 1 = md5(serial) AES, 7 = ECDH + session key.
    pub fn encrypt_type(&self) -> u8 {
        (self.capability_flags & 0b0111000) >> 3
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DeviceInfo {
    pub model: String,
    pub serial: Option<String>,
    pub name: Option<String>,
    pub address: String,
    pub rssi: Option<i16>,
    /// Raw EcoFlow manufacturer data, hex encoded.
    pub manufacturer_data: Option<String>,
    #[serde(skip)]
    pub encrypt_type: Option<u8>,
}

/// Decide whether an advertisement belongs to a supported device.
pub fn match_advertisement(
    address: &str,
    name: Option<&str>,
    rssi: Option<i16>,
    manufacturer_data: &HashMap<u16, Vec<u8>>,
) -> Option<DeviceInfo> {
    let mfr = manufacturer_data.get(&MANUFACTURER_ID);
    let record = mfr.and_then(|d| ScanRecord::parse(d));
    let by_serial = record.as_ref().and_then(|r| model_for_serial(&r.serial));
    let by_name = name.is_some_and(|n| n.starts_with(NAME_PREFIX));

    let model = match (by_serial, by_name) {
        (Some(m), _) => m.to_string(),
        (None, true) => "River 3 (unknown variant)".to_string(),
        (None, false) => return None,
    };
    Some(DeviceInfo {
        model,
        serial: record.as_ref().map(|r| r.serial.clone()),
        name: name.map(str::to_owned),
        address: address.to_owned(),
        rssi,
        manufacturer_data: mfr.map(hex::encode),
        encrypt_type: record.map(|r| r.encrypt_type()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mfr(serial: &str) -> HashMap<u16, Vec<u8>> {
        let mut d = vec![0x01];
        d.extend_from_slice(serial.as_bytes());
        d.extend_from_slice(&[0x00, 0x00, 0, 0, 0, 0b0111001, 0]);
        HashMap::from([(MANUFACTURER_ID, d)])
    }

    #[test]
    fn model_lookup() {
        assert_eq!(model_for_serial("R631ZZZZ12345678"), Some("River 3 Plus"));
        assert_eq!(model_for_serial("R635XXXXXXXXXXXX"), Some("River 3 Plus Wireless"));
        assert_eq!(model_for_serial("R654XXXXXXXXXXXX"), Some("River 3 UPS (230Wh)"));
        assert_eq!(model_for_serial("R331XXXXXXXXXXXX"), None);
        assert_eq!(model_for_serial("R6"), None);
    }

    #[test]
    fn matches_by_serial_in_manufacturer_data() {
        let d = match_advertisement("AA:BB", Some("whatever"), Some(-60), &mfr("R631ABCD12345678")).unwrap();
        assert_eq!(d.model, "River 3 Plus");
        assert_eq!(d.serial.as_deref(), Some("R631ABCD12345678"));
        assert_eq!(d.encrypt_type, Some(7));
        assert_eq!(d.rssi, Some(-60));
    }

    #[test]
    fn matches_by_name_prefix_only() {
        let d = match_advertisement("AA:BB", Some("EF-R3-1234"), None, &HashMap::new()).unwrap();
        assert_eq!(d.model, "River 3 (unknown variant)");
        assert!(d.serial.is_none());
    }

    #[test]
    fn ignores_other_devices() {
        assert!(match_advertisement("AA", Some("Headphones"), None, &HashMap::new()).is_none());
        // EcoFlow device of another family (e.g. Delta) is not matched.
        assert!(match_advertisement("AA", None, None, &mfr("P351ABCD12345678")).is_none());
        assert!(match_advertisement("AA", None, None, &HashMap::new()).is_none());
    }

    #[test]
    fn scan_record_rejects_garbage() {
        assert!(ScanRecord::parse(&[1, 2, 3]).is_none());
        let mut d = vec![1];
        d.extend_from_slice(&[0x00; 16]);
        assert!(ScanRecord::parse(&d).is_none());
    }

    #[test]
    fn default_encrypt_type_is_7_when_flags_absent() {
        let mut d = vec![1];
        d.extend_from_slice(b"R631ABCD12345678");
        let r = ScanRecord::parse(&d).unwrap();
        assert_eq!(r.encrypt_type(), 7);
    }
}
