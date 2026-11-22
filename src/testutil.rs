//! Simulated EcoFlow device for tests: speaks the real handshake and packet format.

use crate::pb::{CONFIG_WRITE_CMD_ID, CONFIG_WRITE_CMD_SET, ConfigWrite, DisplayPropertyUpload};
use crate::protocol::crypto::{self, SessionCipher};
use crate::protocol::ecdh::KeyPair;
use crate::protocol::frame::{FRAME_TYPE_COMMAND, FRAME_TYPE_PROTOCOL, FrameSplitter, encode_frame};
use crate::protocol::packet::Packet;
use crate::session::{SessionError, Transport};
use prost::Message;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

const SEQ0: u8 = 0x37;

#[derive(Default, Debug)]
pub struct DeviceLog {
    /// `(cmd_set, cmd_id)` of time-related packets received from the client.
    pub time_packets: Vec<(u8, u8)>,
    /// Config writes received, with the packet addressing the client used.
    pub writes: Vec<ConfigWrite>,
    pub write_addressing: Vec<(u8, u8, u8, u8, u8)>,
}

pub type LogHandle = Arc<Mutex<DeviceLog>>;

/// How the simulated device reacts to a `ConfigWrite`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WriteBehavior {
    /// Push an unrelated status first, then the changed value (like a real, chatty device).
    #[default]
    Apply,
    /// Accept the packet but never change anything or answer (the link stays up silently).
    IgnoreAndStall,
    /// Drop the link when a write arrives.
    DropLink,
}

pub struct FakeDevice {
    serial: String,
    user_id: String,
    statuses: Vec<DisplayPropertyUpload>,
    /// Merged device state; config writes modify it.
    props: DisplayPropertyUpload,
    write_behavior: WriteBehavior,
    stalled: bool,
    closed: bool,
    silent: bool,
    keys: Option<KeyPair>,
    cipher0: Option<SessionCipher>,
    session: Option<SessionCipher>,
    splitter: FrameSplitter,
    log: Arc<Mutex<DeviceLog>>,
}

/// Build a device->client packet the way the firmware does (payload XOR seq[0]).
pub fn device_packet(src: u8, cmd_set: u8, cmd_id: u8, payload: &[u8]) -> Packet {
    let mut p = Packet::new(
        src,
        0x21,
        cmd_set,
        cmd_id,
        payload.iter().map(|b| b ^ SEQ0).collect(),
        0x03,
    );
    p.seq = [SEQ0, 0, 0, 0];
    p
}

impl FakeDevice {
    pub fn new(serial: &str, user_id: &str, statuses: Vec<DisplayPropertyUpload>) -> Self {
        let mut props = DisplayPropertyUpload::default();
        for st in &statuses {
            props.merge(st.encode_to_vec().as_slice()).unwrap();
        }
        Self {
            serial: serial.into(),
            user_id: user_id.into(),
            statuses,
            props,
            write_behavior: WriteBehavior::Apply,
            stalled: false,
            closed: false,
            silent: false,
            keys: None,
            cipher0: None,
            session: None,
            splitter: FrameSplitter::new(),
            log: Arc::default(),
        }
    }

    /// A device that never answers.
    pub fn silent() -> Self {
        let mut d = Self::new("", "", vec![]);
        d.silent = true;
        d
    }

    /// Keep the link open (reads pend) once the queued notifications are consumed, instead of closing it.
    pub fn stay_open(mut self) -> Self {
        self.stalled = true;
        self
    }

    pub fn with_write_behavior(mut self, b: WriteBehavior) -> Self {
        self.write_behavior = b;
        self
    }

    /// Apply a config write to the device state; returns the partial status to push back.
    fn apply_config(&mut self, cfg: &ConfigWrite) -> DisplayPropertyUpload {
        let mut changed = DisplayPropertyUpload::default();
        if let Some(v) = cfg.cfg_max_chg_soc {
            self.props.cms_max_chg_soc = Some(v);
            changed.cms_max_chg_soc = Some(v);
        }
        if let Some(v) = cfg.cfg_min_dsg_soc {
            self.props.cms_min_dsg_soc = Some(v);
            changed.cms_min_dsg_soc = Some(v);
        }
        if let Some(v) = cfg.cfg_led_mode {
            self.props.led_mode = Some(v);
            changed.led_mode = Some(v);
        }
        if let Some(on) = cfg.cfg_dc_12v_out_open {
            let flow = if on { 2 } else { 0 };
            self.props.flow_info_12v = Some(flow);
            changed.flow_info_12v = Some(flow);
        }
        if let Some(on) = cfg.cfg_ac_out_open {
            let flow = if on { 2 } else { 0 };
            self.props.flow_info_ac_out = Some(flow);
            changed.flow_info_ac_out = Some(flow);
        }
        changed
    }

    fn protocol_frame(&self, p: &Packet) -> Vec<u8> {
        let c = self.session.as_ref().unwrap();
        encode_frame(FRAME_TYPE_PROTOCOL, &c.encrypt(&p.to_bytes()))
    }

    /// Handle bytes written by the client; returns bytes to notify back.
    fn on_write(&mut self, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for f in self.splitter.push(data) {
            if f.frame_type == FRAME_TYPE_COMMAND {
                match f.payload.as_slice() {
                    [0x01, 0x00, peer @ ..] => {
                        let keys = KeyPair::generate();
                        let shared = keys.shared_secret(peer).expect("client key valid");
                        self.cipher0 = Some(SessionCipher::from_shared_secret(&shared));
                        let mut reply = vec![0x01, 0x00, 0x00];
                        reply.extend_from_slice(&keys.public_bytes());
                        out.extend(encode_frame(FRAME_TYPE_COMMAND, &reply));
                        self.keys = Some(keys);
                    }
                    [0x02] => {
                        let mut info: Vec<u8> = (0..16u8).map(|i| i.wrapping_mul(7).wrapping_add(3)).collect();
                        info.extend_from_slice(&[0x03, 0x05]);
                        let c0 = self.cipher0.as_ref().expect("key exchange first");
                        let mut reply = vec![0x02];
                        reply.extend(c0.encrypt(&info));
                        out.extend(encode_frame(FRAME_TYPE_COMMAND, &reply));
                        self.session = Some(c0.with_key(crypto::derive_session_key(&info).unwrap()));
                    }
                    other => panic!("unexpected command frame {other:02x?}"),
                }
                continue;
            }
            let plain = self.session.as_ref().expect("session key").decrypt(&f.payload);
            let p = Packet::from_bytes(&plain, false).expect("client packet valid");
            match (p.cmd_set, p.cmd_id) {
                (0x35, 0x89) => out.extend(self.protocol_frame(&device_packet(0x35, 0x35, 0x89, &[0x00]))),
                (0x35, 0x86) => {
                    let expect = hex::encode_upper(crypto::md5(format!("{}{}", self.user_id, self.serial).as_bytes()));
                    let ok = p.payload == expect.as_bytes();
                    out.extend(self.protocol_frame(&device_packet(0x35, 0x35, 0x86, &[if ok { 0x00 } else { 0x06 }])));
                    if ok {
                        out.extend(self.protocol_frame(&device_packet(0x35, 0x01, 0x52, &[])));
                        for st in &self.statuses {
                            out.extend(self.protocol_frame(&device_packet(0x02, 0xFE, 0x15, &st.encode_to_vec())));
                        }
                    }
                }
                (0x01, id) => self.log.lock().unwrap().time_packets.push((0x01, id)),
                (CONFIG_WRITE_CMD_SET, CONFIG_WRITE_CMD_ID) => {
                    let cfg = ConfigWrite::decode(p.payload.as_slice()).expect("valid ConfigWrite");
                    {
                        let mut log = self.log.lock().unwrap();
                        log.writes.push(cfg.clone());
                        log.write_addressing.push((p.src, p.dst, p.dsrc, p.ddst, p.version));
                    }
                    match self.write_behavior {
                        WriteBehavior::IgnoreAndStall => self.stalled = true,
                        WriteBehavior::DropLink => self.closed = true,
                        WriteBehavior::Apply => {
                            let changed = self.apply_config(&cfg);
                            let unrelated = DisplayPropertyUpload {
                                bms_max_cell_temp: Some(33),
                                ..Default::default()
                            };
                            for st in [unrelated, changed] {
                                out.extend(self.protocol_frame(&device_packet(0x02, 0xFE, 0x15, &st.encode_to_vec())));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        out
    }
}

pub struct FakeTransport {
    device: FakeDevice,
    queue: VecDeque<Vec<u8>>,
}

impl FakeTransport {
    pub fn new(device: FakeDevice) -> (Self, LogHandle) {
        let log = device.log.clone();
        (
            Self {
                device,
                queue: VecDeque::new(),
            },
            log,
        )
    }
}

impl Transport for FakeTransport {
    async fn write(&mut self, data: &[u8]) -> Result<(), SessionError> {
        if self.device.silent {
            return Ok(());
        }
        let out = self.device.on_write(data);
        // Deliver in 20-byte notifications to exercise reassembly.
        self.queue.extend(out.chunks(20).map(<[u8]>::to_vec));
        Ok(())
    }

    async fn recv(&mut self) -> Result<Vec<u8>, SessionError> {
        if let Some(c) = self.queue.pop_front() {
            return Ok(c);
        }
        if !self.device.closed && (self.device.silent || self.device.stalled) {
            std::future::pending().await
        }
        Err(SessionError::LinkClosed)
    }
}
