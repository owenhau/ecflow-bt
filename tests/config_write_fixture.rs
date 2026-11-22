//! `ConfigWrite` must encode/decode byte-for-byte like the reference `pr705_pb2`.

use ecflow_bt::pb::ConfigWrite;
use ecflow_bt::protocol::packet::Packet;
use prost::Message;

fn fixtures() -> Vec<(String, String, Vec<u8>)> {
    include_str!("fixtures/config_write.txt")
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|l| {
            let parts: Vec<&str> = l.split_whitespace().collect();
            (
                parts[0].to_string(),
                parts[1].to_string(),
                hex::decode(parts[parts.len() - 1]).unwrap(),
            )
        })
        .collect()
}

fn single(name: &str, value: &str) -> ConfigWrite {
    let n: u32 = value.parse().unwrap();
    let mut c = ConfigWrite::default();
    match name {
        "cfg_max_chg_soc" => c.cfg_max_chg_soc = Some(n),
        "cfg_min_dsg_soc" => c.cfg_min_dsg_soc = Some(n),
        "cfg_led_mode" => c.cfg_led_mode = Some(n),
        "cfg_dc_12v_out_open" => c.cfg_dc_12v_out_open = Some(n != 0),
        "cfg_ac_out_open" => c.cfg_ac_out_open = Some(n != 0),
        other => panic!("unknown fixture field {other}"),
    }
    c
}

#[test]
fn encoding_matches_reference_bytes() {
    let cases: Vec<_> = fixtures().into_iter().filter(|(n, _, _)| n != "packet").collect();
    assert_eq!(cases.len(), 5);
    for (name, value, bytes) in cases {
        assert_eq!(single(&name, &value).encode_to_vec(), bytes, "{name}={value}");
    }
}

#[test]
fn reference_bytes_decode() {
    for (name, value, bytes) in fixtures().into_iter().filter(|(n, _, _)| n != "packet") {
        assert_eq!(
            ConfigWrite::decode(bytes.as_slice()).unwrap(),
            single(&name, &value),
            "{name}"
        );
    }
}

#[test]
fn false_values_are_still_encoded() {
    // `optional` presence: turning something off must put the field on the wire.
    let c = ConfigWrite {
        cfg_ac_out_open: Some(false),
        ..Default::default()
    };
    assert_eq!(c.encode_to_vec(), vec![0xE0, 0x04, 0x00]);
}

#[test]
fn full_write_packet_matches_reference_layout() {
    let (_, _, bytes) = fixtures().into_iter().find(|(n, _, _)| n == "packet").unwrap();
    let parsed = Packet::from_bytes(&bytes, true).unwrap();
    assert_eq!(
        (parsed.src, parsed.dst, parsed.cmd_set, parsed.cmd_id),
        (0x20, 0x02, 0xFE, 0x11)
    );
    assert_eq!((parsed.dsrc, parsed.ddst, parsed.version), (1, 1, 0x13));
    let cfg = ConfigWrite::decode(parsed.payload.as_slice()).unwrap();
    assert_eq!(cfg.cfg_min_dsg_soc, Some(15));
    assert_eq!(cfg.summary(), "min_dsg_soc=15");

    // Our own packet builder produces the same bytes as the reference, apart from the CRC16
    // (the reference-encoded fixture keeps a real CRC16 too, so compare everything).
    let ours = Packet::new(0x20, 0x02, 0xFE, 0x11, parsed.payload.clone(), 0x13).to_bytes();
    assert_eq!(ours, bytes);
}
