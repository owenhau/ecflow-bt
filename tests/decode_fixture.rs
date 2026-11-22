use ecflow_bt::device::DeviceInfo;
use ecflow_bt::pb::DisplayPropertyUpload;
use ecflow_bt::protocol::packet::Packet;
use ecflow_bt::status::{ChargeState, Source, Status};
use prost::Message;

fn device() -> DeviceInfo {
    DeviceInfo {
        model: "River 3 Plus".into(),
        serial: None,
        name: None,
        address: "00:00:00:00:00:00".into(),
        rssi: None,
        manufacturer_data: None,
        encrypt_type: Some(7),
    }
}

#[test]
fn decodes_reference_encoded_status_packet() {
    let hex_text = include_str!("fixtures/status_packet.hex");
    let bytes = hex::decode(hex_text.trim()).unwrap();
    let p = Packet::from_bytes(&bytes, true).unwrap();
    assert_eq!((p.src, p.cmd_set, p.cmd_id), (0x02, 0xFE, 0x15));

    let props = DisplayPropertyUpload::decode(p.payload.as_slice()).unwrap();
    let s = Status::from_props(device(), &props);
    assert_eq!(s.source, Source::Gatt);
    assert_eq!(s.battery_percent, Some(64.5));
    assert_eq!(s.charge_state, Some(ChargeState::Discharging));
    assert_eq!(s.battery_watts, Some(-42.25));
    assert_eq!(s.output_watts, Some(42.5));
    assert_eq!(s.ac_output_watts, Some(40.0));
    assert_eq!(s.minutes_to_empty, Some(330));
    assert_eq!(s.cell_temperature_c, Some(27));
    assert_eq!(s.ac_output_on, Some(true));
    assert_eq!(s.ac_plugged_in, Some(false));
}
