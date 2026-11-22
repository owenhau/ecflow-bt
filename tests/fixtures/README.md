# Fixtures

`status_packet.hex` is a **synthetic** decrypted inner packet (src 0x02, cmd_set 0xFE,
cmd_id 0x15, payload XOR-obfuscated with seq[0]) whose protobuf body was encoded with the
reference `pr705_pb2` from rabits/ha-ef-ble, independent of this crate's hand-written
message. Expected values: 64.5 %, discharging at 42.25 W, AC out 40 W, 330 min to empty.

To add a real capture, run with `-vvv` is not needed: save the hex of a decrypted packet
(printed by `ecflow-bt decode-packet`'s input) into `tests/fixtures/` after redacting
anything private. Never commit user IDs or full serial numbers.
