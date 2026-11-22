//! Authenticated GATT session: handshake, packet decoding, time-sync replies.
//!
//! Generic over [`Transport`] so the whole flow runs against a simulated device in tests.

use crate::device::DeviceInfo;
use crate::pb::{
    CONFIG_WRITE_CMD_ID, CONFIG_WRITE_CMD_SET, CONFIG_WRITE_DST, CONFIG_WRITE_PACKET_VERSION, CONFIG_WRITE_SRC,
    ConfigWrite, DisplayPropertyUpload, SysUtcSync,
};
use crate::protocol::crypto::{self, SessionCipher};
use crate::protocol::ecdh::KeyPair;
use crate::protocol::frame::{FRAME_TYPE_COMMAND, FRAME_TYPE_PROTOCOL, FrameSplitter, RawFrame, encode_frame};
use crate::protocol::packet::{CMD_CHECK_RET_TIME, CMD_SET_RET_TIME, Packet};
use crate::setting::Setting;
use crate::status::Status;
use prost::Message;
use std::collections::VecDeque;
use std::future::Future;
use std::time::{Duration, Instant};
use tracing::{debug, trace, warn};

const AUTH_SRC: u8 = 0x21;
const AUTH_DST: u8 = 0x35;
const AUTH_CMD_SET: u8 = 0x35;
const CMD_AUTH_STATUS: u8 = 0x89;
const CMD_AUTH: u8 = 0x86;
const PACKET_VERSION: u8 = 0x03;
const MIN_TIME_SYNC_INTERVAL: Duration = Duration::from_secs(30);
const UNDECRYPTABLE_LIMIT: u32 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AuthError {
    #[error("device asked for a refreshed token")]
    NeedRefreshToken,
    #[error("device reported an internal error")]
    DeviceInternalError,
    #[error("device is already bound to another account")]
    DeviceAlreadyBound,
    #[error("device must be bound in the EcoFlow app first")]
    NeedBindInstallFirst,
    #[error("device rejected the request data")]
    AppSendDataError,
    #[error("wrong user ID or serial number")]
    WrongKey,
    #[error("device has reached its maximum number of paired apps")]
    MaximumDevices,
    #[error("unknown authentication error (code 0x{0:02X})")]
    Unknown(u8),
}

impl AuthError {
    /// `None` means success (payload `00`).
    pub fn from_payload(payload: &[u8]) -> Option<Self> {
        match payload {
            [0x00] => None,
            [0x01] => Some(Self::NeedRefreshToken),
            [0x02] => Some(Self::DeviceInternalError),
            [0x03] => Some(Self::DeviceAlreadyBound),
            [0x04] => Some(Self::NeedBindInstallFirst),
            [0x05] => Some(Self::AppSendDataError),
            [0x06] => Some(Self::WrongKey),
            [0x07] => Some(Self::MaximumDevices),
            [code, ..] => Some(Self::Unknown(*code)),
            [] => Some(Self::Unknown(0xFF)),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("timed out during {0}")]
    Timeout(&'static str),
    #[error("Bluetooth link closed")]
    LinkClosed,
    #[error("Bluetooth transport error: {0}")]
    Transport(String),
    #[error("device uses unsupported encryption type {0} (only type 7 is implemented)")]
    UnsupportedEncryption(u8),
    #[error("handshake failed: {0}")]
    Handshake(String),
    #[error("authentication rejected: {0}")]
    Auth(#[from] AuthError),
    #[error("session lost: {UNDECRYPTABLE_LIMIT} consecutive frames could not be decoded")]
    SessionLost,
}

impl SessionError {
    /// Errors that retrying cannot fix (bad credentials, unsupported device).
    pub fn is_permanent(&self) -> bool {
        matches!(
            self,
            Self::Auth(AuthError::WrongKey | AuthError::DeviceAlreadyBound | AuthError::NeedBindInstallFirst)
                | Self::UnsupportedEncryption(_)
        )
    }
}

/// Byte pipe to the device's write and notify characteristics.
pub trait Transport: Send {
    fn write(&mut self, data: &[u8]) -> impl Future<Output = Result<(), SessionError>> + Send;
    /// Next notification; `Err(LinkClosed)` when the link is gone.
    fn recv(&mut self) -> impl Future<Output = Result<Vec<u8>, SessionError>> + Send;
}

/// Result of [`Session::apply`].
#[derive(Debug, Clone, PartialEq)]
pub enum ApplyOutcome {
    /// The device already reported the requested value; nothing was sent.
    Unchanged(Box<Status>),
    /// The write was sent and the device reported the new value.
    Applied(Box<Status>),
    /// The write was sent but the device did not report the new value in time.
    Unconfirmed(Box<Status>),
}

impl ApplyOutcome {
    pub fn status(&self) -> &Status {
        match self {
            Self::Unchanged(s) | Self::Applied(s) | Self::Unconfirmed(s) => s,
        }
    }
}

pub struct Session<T: Transport> {
    transport: T,
    device: DeviceInfo,
    splitter: FrameSplitter,
    frames: VecDeque<RawFrame>,
    pending: VecDeque<Packet>,
    cipher: Option<SessionCipher>,
    props: DisplayPropertyUpload,
    timeout: Duration,
    last_time_sync: Option<Instant>,
    undecryptable: u32,
}

/// UTC offset as the device expects it: whole hours and "hundredths of an hour" minutes
/// (e.g. +5:30 -> (5, 50)), matching the reference implementation.
pub fn tz_bytes(offset_secs: i32) -> (i8, i8) {
    let hours = f64::from(offset_secs) / 3600.0;
    let maj = hours.trunc();
    (maj as i8, ((hours - maj) * 100.0).trunc() as i8)
}

fn auth_token(user_id: &str, serial: &str) -> Vec<u8> {
    let digest = crypto::md5(format!("{user_id}{serial}").as_bytes());
    hex::encode_upper(digest).into_bytes()
}

impl<T: Transport> Session<T> {
    /// Run the full handshake. Each stage is bounded by `timeout`.
    pub async fn establish(
        transport: T,
        device: DeviceInfo,
        serial: &str,
        user_id: &str,
        timeout: Duration,
    ) -> Result<Self, SessionError> {
        if let Some(t) = device.encrypt_type
            && t != 7
        {
            return Err(SessionError::UnsupportedEncryption(t));
        }
        let mut s = Self {
            transport,
            device,
            splitter: FrameSplitter::new(),
            frames: VecDeque::new(),
            pending: VecDeque::new(),
            cipher: None,
            props: DisplayPropertyUpload::default(),
            timeout,
            last_time_sync: None,
            undecryptable: 0,
        };
        let t = timeout;
        let stage = |name| move |_| SessionError::Timeout(name);
        let initial = tokio::time::timeout(t, s.exchange_keys())
            .await
            .map_err(stage("key exchange"))??;
        tokio::time::timeout(t, s.request_session_key(initial))
            .await
            .map_err(stage("session key request"))??;
        tokio::time::timeout(t, s.request_auth_status())
            .await
            .map_err(stage("auth status"))??;
        tokio::time::timeout(t, s.authenticate(serial, user_id))
            .await
            .map_err(stage("authentication"))??;
        debug!("session authenticated");
        Ok(s)
    }

    pub fn device(&self) -> &DeviceInfo {
        &self.device
    }

    async fn next_frame(&mut self) -> Result<RawFrame, SessionError> {
        loop {
            if let Some(f) = self.frames.pop_front() {
                return Ok(f);
            }
            let chunk = self.transport.recv().await?;
            trace!(len = chunk.len(), "notification");
            self.frames.extend(self.splitter.push(&chunk));
        }
    }

    /// Wait for a plaintext command frame of the given type; other frames are dropped
    /// (encrypted device data cannot be decoded before the session key exists).
    async fn read_command(&mut self, expected: u8, min_len: usize) -> Result<Vec<u8>, SessionError> {
        loop {
            let f = self.next_frame().await?;
            if f.frame_type == FRAME_TYPE_COMMAND && f.payload.first() == Some(&expected) && f.payload.len() >= min_len
            {
                return Ok(f.payload);
            }
            trace!(
                frame_type = f.frame_type,
                len = f.payload.len(),
                "ignoring frame during handshake"
            );
        }
    }

    async fn exchange_keys(&mut self) -> Result<SessionCipher, SessionError> {
        let keys = KeyPair::generate();
        let mut req = vec![0x01, 0x00];
        req.extend_from_slice(&keys.public_bytes());
        self.transport.write(&encode_frame(FRAME_TYPE_COMMAND, &req)).await?;

        let reply = self.read_command(0x01, 43).await?;
        // reply[1] = status, reply[2] = curve id. Only the 40-byte secp160r1 key is supported.
        if matches!(reply[2], 1..=4) {
            return Err(SessionError::Handshake(format!("unsupported curve id {}", reply[2])));
        }
        let shared = keys
            .shared_secret(&reply[3..43])
            .map_err(|e| SessionError::Handshake(format!("bad device public key: {e}")))?;
        Ok(SessionCipher::from_shared_secret(&shared))
    }

    async fn request_session_key(&mut self, initial: SessionCipher) -> Result<(), SessionError> {
        self.transport.write(&encode_frame(FRAME_TYPE_COMMAND, &[0x02])).await?;
        let reply = self.read_command(0x02, 33).await?;
        let key_info = initial.decrypt(&reply[1..]);
        let key = crypto::derive_session_key(&key_info).map_err(|e| SessionError::Handshake(e.to_string()))?;
        self.cipher = Some(initial.with_key(key));
        Ok(())
    }

    async fn request_auth_status(&mut self) -> Result<(), SessionError> {
        let pkt = Packet::new(
            AUTH_SRC,
            AUTH_DST,
            AUTH_CMD_SET,
            CMD_AUTH_STATUS,
            vec![],
            PACKET_VERSION,
        );
        self.send_packet(&pkt).await?;
        loop {
            let p = self.next_packet().await?;
            if p.src == AUTH_DST && p.cmd_set == AUTH_CMD_SET {
                return Ok(());
            }
            self.pending.push_back(p);
        }
    }

    async fn authenticate(&mut self, serial: &str, user_id: &str) -> Result<(), SessionError> {
        let pkt = Packet::new(
            AUTH_SRC,
            AUTH_DST,
            AUTH_CMD_SET,
            CMD_AUTH,
            auth_token(user_id, serial),
            PACKET_VERSION,
        );
        self.send_packet(&pkt).await?;
        loop {
            let p = self.next_packet().await?;
            let is_auth_exchange = p.src == AUTH_DST && p.cmd_set == AUTH_CMD_SET;
            if is_auth_exchange && p.cmd_id == CMD_AUTH {
                return match AuthError::from_payload(&p.payload) {
                    None => Ok(()),
                    Some(e) => Err(e.into()),
                };
            }
            if !is_auth_exchange {
                // The first regular data packet also confirms the link is authenticated.
                self.pending.push_back(p);
                return Ok(());
            }
        }
    }

    async fn send_packet(&mut self, p: &Packet) -> Result<(), SessionError> {
        let cipher = self
            .cipher
            .as_ref()
            .ok_or_else(|| SessionError::Handshake("no session key".into()))?;
        let frame = encode_frame(FRAME_TYPE_PROTOCOL, &cipher.encrypt(&p.to_bytes()));
        self.transport.write(&frame).await
    }

    /// Next decodable packet. Undecodable frames are skipped, but a run of them means
    /// the session key no longer matches the device and the link must be re-established.
    async fn next_packet(&mut self) -> Result<Packet, SessionError> {
        loop {
            if let Some(p) = self.pending.pop_front() {
                return Ok(p);
            }
            let f = self.next_frame().await?;
            if f.frame_type == FRAME_TYPE_COMMAND {
                continue; // repeated plaintext handshake replies
            }
            let cipher = self.cipher.as_ref().expect("cipher set during handshake");
            let plain = cipher.decrypt(&f.payload);
            match Packet::from_bytes(&plain, true) {
                Ok(p) => {
                    self.undecryptable = 0;
                    return Ok(p);
                }
                Err(e) => {
                    debug!("dropping undecodable frame: {e}");
                    self.undecryptable += 1;
                    if self.undecryptable >= UNDECRYPTABLE_LIMIT {
                        return Err(SessionError::SessionLost);
                    }
                }
            }
        }
    }

    async fn send_time_sync(&mut self) -> Result<(), SessionError> {
        let now = Instant::now();
        if self
            .last_time_sync
            .is_some_and(|t| now.duration_since(t) < MIN_TIME_SYNC_INTERVAL)
        {
            debug!("throttling repeated time-sync request");
            return Ok(());
        }

        let secs = chrono::Utc::now().timestamp() as u32;
        let offset = chrono::Local::now().offset().local_minus_utc();
        let (tz_maj, tz_min) = tz_bytes(offset);
        let mut rtc = secs.to_le_bytes().to_vec();
        rtc.push(tz_maj as u8);
        rtc.push(tz_min as u8);

        let utc = SysUtcSync {
            sys_utc_time: Some(secs),
        }
        .encode_to_vec();
        self.send_packet(&Packet::new(0x21, 0x0B, 0x01, 0x55, utc, 0x13))
            .await?;
        self.send_packet(&Packet::new(0x21, 0x35, 0x01, CMD_SET_RET_TIME, rtc.clone(), 0x03))
            .await?;
        self.send_packet(&Packet::new(0x21, 0x35, 0x01, CMD_CHECK_RET_TIME, rtc, 0x03))
            .await?;
        // Only now: if this future is dropped mid-way (the TUI worker selects between status
        // reads and commands), the device simply asks again and the sync is retried.
        self.last_time_sync = Some(now);
        Ok(())
    }

    /// Everything the device has reported so far in this session, merged.
    pub fn current_status(&self) -> Status {
        Status::from_props(self.device.clone(), &self.props)
    }

    /// Send a configuration write. Does not wait for the device to apply it; see [`Self::apply`].
    pub async fn write(&mut self, cfg: &ConfigWrite) -> Result<(), SessionError> {
        let pkt = Packet::new(
            CONFIG_WRITE_SRC,
            CONFIG_WRITE_DST,
            CONFIG_WRITE_CMD_SET,
            CONFIG_WRITE_CMD_ID,
            cfg.encode_to_vec(),
            CONFIG_WRITE_PACKET_VERSION,
        );
        debug!(change = %cfg.summary(), "sending config write");
        self.send_packet(&pkt).await
    }

    /// Write one setting and wait (up to the session timeout) until the device reports the new
    /// value. Validation against the current state is the caller's job (`Setting::validate`).
    pub async fn apply(&mut self, setting: Setting) -> Result<ApplyOutcome, SessionError> {
        let now = self.current_status();
        if setting.matches(&now) {
            return Ok(ApplyOutcome::Unchanged(Box::new(now)));
        }
        self.write(&setting.to_config()).await?;

        let deadline = tokio::time::Instant::now() + self.timeout;
        loop {
            match tokio::time::timeout_at(deadline, self.next_status()).await {
                Ok(Ok(status)) if setting.matches(&status) => return Ok(ApplyOutcome::Applied(Box::new(status))),
                Ok(Ok(_)) => {}
                Ok(Err(e)) => return Err(e),
                Err(_) => return Ok(ApplyOutcome::Unconfirmed(Box::new(self.current_status()))),
            }
        }
    }

    /// Process packets until the next status push; returns the merged status.
    pub async fn next_status(&mut self) -> Result<Status, SessionError> {
        loop {
            let p = self.next_packet().await?;
            match (p.src, p.cmd_set, p.cmd_id) {
                (0x02, 0xFE, 0x15) => {
                    // Pushes are partial: merge so earlier fields persist.
                    match self.props.merge(p.payload.as_slice()) {
                        Ok(()) => return Ok(Status::from_props(self.device.clone(), &self.props)),
                        Err(e) => warn!("undecodable status payload: {e}"),
                    }
                }
                (0x35, 0x01, CMD_SET_RET_TIME) if p.payload.is_empty() => self.send_time_sync().await?,
                _ => trace!(src = p.src, cmd_set = p.cmd_set, cmd_id = p.cmd_id, "unhandled packet"),
            }
        }
    }

    /// One-shot read: wait until battery data has arrived, then keep merging pushes for
    /// `settle` so the first print carries power and port fields too.
    pub async fn read_status(&mut self, settle: Duration) -> Result<Status, SessionError> {
        let have_battery = |s: &Status| s.battery_percent.is_some() || s.main_battery_percent.is_some();
        let first = tokio::time::timeout(self.timeout, async {
            loop {
                let s = self.next_status().await?;
                if have_battery(&s) {
                    return Ok::<_, SessionError>(s);
                }
            }
        })
        .await
        .map_err(|_| SessionError::Timeout("waiting for battery data"))??;

        let mut latest = first;
        let deadline = tokio::time::Instant::now() + settle;
        while let Ok(res) = tokio::time::timeout_at(deadline, self.next_status()).await {
            match res {
                Ok(s) => latest = s,
                Err(e) => {
                    // Keep what we have; a persistent error resurfaces on the next read.
                    debug!("stopped settling early: {e}");
                    break;
                }
            }
        }
        Ok(latest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::{ChargeState, Source, test_device};
    use crate::testutil::{FakeDevice, FakeTransport, WriteBehavior};

    const SERIAL: &str = "R631TESTSERIAL01";
    const USER: &str = "1234567890";
    const T: Duration = Duration::from_secs(2);

    fn status_props() -> DisplayPropertyUpload {
        DisplayPropertyUpload {
            cms_batt_soc: Some(64.5),
            pow_get_bms: Some(150.0),
            pow_in_sum_w: Some(160.0),
            pow_out_sum_w: Some(10.0),
            plug_in_info_ac_charger_flag: Some(true),
            ..Default::default()
        }
    }

    #[test]
    fn tz_encoding_matches_reference() {
        assert_eq!(tz_bytes(0), (0, 0));
        assert_eq!(tz_bytes(3600), (1, 0));
        assert_eq!(tz_bytes(19800), (5, 50));
        assert_eq!(tz_bytes(-18000), (-5, 0));
        assert_eq!(tz_bytes(-12600), (-3, -50));
    }

    #[test]
    fn auth_token_is_uppercase_md5_hex() {
        let t = String::from_utf8(auth_token("u", "s")).unwrap();
        assert_eq!(t.len(), 32);
        assert!(t.chars().all(|c| c.is_ascii_digit() || ('A'..='F').contains(&c)));
        assert_eq!(t, hex::encode_upper(crypto::md5(b"us")));
    }

    #[test]
    fn auth_error_mapping() {
        assert_eq!(AuthError::from_payload(&[0]), None);
        assert_eq!(AuthError::from_payload(&[6]), Some(AuthError::WrongKey));
        assert_eq!(AuthError::from_payload(&[0x42]), Some(AuthError::Unknown(0x42)));
        assert_eq!(AuthError::from_payload(&[]), Some(AuthError::Unknown(0xFF)));
    }

    #[tokio::test]
    async fn full_handshake_and_status_read() {
        let (transport, log) = FakeTransport::new(FakeDevice::new(SERIAL, USER, vec![status_props()]));
        let mut s = Session::establish(transport, test_device(), SERIAL, USER, T)
            .await
            .unwrap();
        let st = s.read_status(Duration::from_millis(50)).await.unwrap();
        assert_eq!(st.source, Source::Gatt);
        assert_eq!(st.battery_percent, Some(64.5));
        assert_eq!(st.charge_state, Some(ChargeState::Charging));
        assert_eq!(st.ac_plugged_in, Some(true));

        // The device's time request must have been answered (UTC + RTC + check).
        let seen = log.lock().unwrap().time_packets.clone();
        assert!(seen.contains(&(0x01, 0x55)), "{seen:?}");
        assert!(seen.contains(&(0x01, CMD_SET_RET_TIME)));
        assert!(seen.contains(&(0x01, CMD_CHECK_RET_TIME)));
    }

    #[tokio::test]
    async fn partial_pushes_are_merged() {
        let a = DisplayPropertyUpload {
            cms_batt_soc: Some(50.0),
            ..Default::default()
        };
        let b = DisplayPropertyUpload {
            pow_get_bms: Some(-20.0),
            bms_max_cell_temp: Some(31),
            ..Default::default()
        };
        let (transport, _) = FakeTransport::new(FakeDevice::new(SERIAL, USER, vec![a, b]));
        let mut s = Session::establish(transport, test_device(), SERIAL, USER, T)
            .await
            .unwrap();
        let st = s.read_status(Duration::from_millis(100)).await.unwrap();
        assert_eq!(st.battery_percent, Some(50.0));
        assert_eq!(st.charge_state, Some(ChargeState::Discharging));
        assert_eq!(st.cell_temperature_c, Some(31));
    }

    #[tokio::test]
    async fn wrong_user_id_is_rejected_as_permanent() {
        let (transport, _) = FakeTransport::new(FakeDevice::new(SERIAL, USER, vec![status_props()]));
        let err = Session::establish(transport, test_device(), SERIAL, "9999", T)
            .await
            .err()
            .unwrap();
        assert!(matches!(err, SessionError::Auth(AuthError::WrongKey)), "{err:?}");
        assert!(err.is_permanent());
    }

    #[tokio::test]
    async fn unsupported_encryption_type_is_refused() {
        let (transport, _) = FakeTransport::new(FakeDevice::new(SERIAL, USER, vec![]));
        let mut dev = test_device();
        dev.encrypt_type = Some(1);
        let err = Session::establish(transport, dev, SERIAL, USER, T).await.err().unwrap();
        assert!(matches!(err, SessionError::UnsupportedEncryption(1)));
    }

    #[tokio::test]
    async fn silent_device_times_out() {
        let (transport, _) = FakeTransport::new(FakeDevice::silent());
        let err = Session::establish(transport, test_device(), SERIAL, USER, Duration::from_millis(100))
            .await
            .err()
            .unwrap();
        assert!(matches!(err, SessionError::Timeout("key exchange")), "{err:?}");
    }

    fn base_props() -> DisplayPropertyUpload {
        DisplayPropertyUpload {
            cms_batt_soc: Some(64.5),
            pow_get_bms: Some(-20.0),
            cms_max_chg_soc: Some(85),
            cms_min_dsg_soc: Some(15),
            led_mode: Some(0),
            flow_info_12v: Some(0),
            flow_info_ac_out: Some(0),
            ..Default::default()
        }
    }

    async fn connected(
        behavior: WriteBehavior,
        timeout: Duration,
    ) -> (
        Session<FakeTransport>,
        std::sync::Arc<std::sync::Mutex<crate::testutil::DeviceLog>>,
    ) {
        let device = FakeDevice::new(SERIAL, USER, vec![base_props()]).with_write_behavior(behavior);
        let (transport, log) = FakeTransport::new(device);
        let mut s = Session::establish(transport, test_device(), SERIAL, USER, timeout)
            .await
            .unwrap();
        s.read_status(Duration::from_millis(50)).await.unwrap();
        (s, log)
    }

    #[tokio::test]
    async fn every_setting_is_written_and_read_back() {
        use crate::status::LedMode;
        let (mut s, log) = connected(WriteBehavior::Apply, T).await;
        let changes = [
            Setting::ChargeLimit(90),
            Setting::DischargeLimit(20),
            Setting::Light(LedMode::Bright),
            Setting::Dc12v(true),
            Setting::AcOutput(true),
        ];
        for change in changes {
            let out = s.apply(change).await.unwrap();
            assert!(matches!(out, ApplyOutcome::Applied(_)), "{change:?}: {out:?}");
            assert!(change.matches(out.status()), "{change:?}");
        }
        let st = s.current_status();
        assert_eq!(st.charge_limit_max_percent, Some(90));
        assert_eq!(st.discharge_limit_min_percent, Some(20));
        assert_eq!(st.led_mode, Some(LedMode::Bright));
        assert_eq!((st.dc12v_output_on, st.ac_output_on), (Some(true), Some(true)));
        // Unrelated fields from the earlier statuses survive the partial updates.
        assert_eq!(st.battery_percent, Some(64.5));

        let log = log.lock().unwrap();
        assert_eq!(log.writes.len(), 5);
        assert_eq!(log.writes[0].cfg_max_chg_soc, Some(90));
        // Exactly the addressing the reference implementation uses.
        for a in &log.write_addressing {
            assert_eq!(*a, (0x20, 0x02, 1, 1, 0x13));
        }
    }

    #[tokio::test]
    async fn turning_things_off_is_written_too() {
        let (mut s, log) = connected(WriteBehavior::Apply, T).await;
        s.apply(Setting::Dc12v(true)).await.unwrap();
        let out = s.apply(Setting::Dc12v(false)).await.unwrap();
        assert!(matches!(out, ApplyOutcome::Applied(_)));
        assert_eq!(log.lock().unwrap().writes[1].cfg_dc_12v_out_open, Some(false));
    }

    #[tokio::test]
    async fn setting_the_current_value_sends_nothing() {
        let (mut s, log) = connected(WriteBehavior::Apply, T).await;
        let out = s.apply(Setting::ChargeLimit(85)).await.unwrap();
        assert!(matches!(out, ApplyOutcome::Unchanged(_)));
        assert!(log.lock().unwrap().writes.is_empty());
    }

    #[tokio::test]
    async fn ignored_write_is_reported_unconfirmed() {
        let (mut s, log) = connected(WriteBehavior::IgnoreAndStall, Duration::from_millis(150)).await;
        let out = s.apply(Setting::DischargeLimit(25)).await.unwrap();
        match out {
            ApplyOutcome::Unconfirmed(st) => assert_eq!(st.discharge_limit_min_percent, Some(15)),
            other => panic!("expected Unconfirmed, got {other:?}"),
        }
        assert_eq!(log.lock().unwrap().writes.len(), 1);
    }

    #[tokio::test]
    async fn link_closing_during_a_write_is_an_error() {
        let (mut s, _) = connected(WriteBehavior::DropLink, T).await;
        let err = s.apply(Setting::Dc12v(true)).await.unwrap_err();
        assert!(matches!(err, SessionError::LinkClosed), "{err:?}");
    }

    #[tokio::test]
    async fn unrelated_status_pushes_do_not_confirm_a_write() {
        // The simulated device pushes an unrelated update (cell temperature) before the change.
        let (mut s, _) = connected(WriteBehavior::Apply, T).await;
        let out = s.apply(Setting::ChargeLimit(70)).await.unwrap();
        let ApplyOutcome::Applied(st) = out else {
            panic!("{out:?}")
        };
        assert_eq!(st.charge_limit_max_percent, Some(70));
        assert_eq!(st.cell_temperature_c, Some(33)); // the unrelated push was merged on the way
    }

    #[tokio::test]
    async fn closed_link_is_reported() {
        let (transport, _) = FakeTransport::new(FakeDevice::new(SERIAL, USER, vec![]));
        let mut s = Session::establish(transport, test_device(), SERIAL, USER, T)
            .await
            .unwrap();
        // Device has no status to send and then the link drops.
        let err = s.next_status().await.unwrap_err();
        assert!(matches!(err, SessionError::LinkClosed), "{err:?}");
    }
}
