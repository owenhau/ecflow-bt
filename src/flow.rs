//! High-level flows shared by the one-shot command and the TUI worker.

use crate::ble::{self, BleDevice, BleTransport};
use crate::config::{Settings, config_path};
use crate::device::DeviceInfo;
use crate::session::{ApplyOutcome, Session, SessionError, Transport};
use crate::setting::{Setting, SettingError, SettingKind};
use crate::status::Status;
use serde::Serialize;
use std::time::Duration;
use tracing::{info, warn};

pub const ATTEMPTS: u32 = 3;
const SETTLE: Duration = Duration::from_millis(1500);

/// Serial and user ID needed for GATT, or the reason they are missing.
pub fn credentials(info: &DeviceInfo, settings: &Settings) -> Result<(String, String), String> {
    let serial =
        info.serial.clone().or_else(|| settings.serial.clone()).ok_or_else(|| {
            "device serial number unknown (not in advertisement; set `serial` or --serial)".to_string()
        })?;
    let user_id = settings.user_id.clone().ok_or_else(|| {
        let path = config_path().map_or_else(|| "config.toml".into(), |p| p.display().to_string());
        format!("no EcoFlow user ID configured (set `user_id` in {path}, EF_USER_ID or --user-id)")
    })?;
    Ok((serial, user_id))
}

/// One connection attempt: connect, authenticate, read a status, disconnect.
pub async fn connect_and_read(
    dev: &BleDevice,
    serial: &str,
    user_id: &str,
    timeout: Duration,
) -> Result<Status, SessionError> {
    let work = async {
        let transport = BleTransport::open(&dev.peripheral, timeout).await?;
        let mut session = Session::establish(transport, dev.info.clone(), serial, user_id, timeout).await?;
        session.read_status(SETTLE).await
    };
    let result = tokio::select! {
        r = work => r,
        _ = tokio::signal::ctrl_c() => Err(SessionError::Transport("interrupted".into())),
    };
    ble::disconnect(&dev.peripheral).await;
    result
}

/// Retry transient failures with backoff; permanent ones (wrong user ID) stop at once.
pub async fn read_with_retries(
    dev: &BleDevice,
    serial: &str,
    user_id: &str,
    timeout: Duration,
) -> Result<Status, SessionError> {
    let mut last = None;
    for attempt in 1..=ATTEMPTS {
        info!(attempt, "connecting");
        match connect_and_read(dev, serial, user_id, timeout).await {
            Ok(s) => return Ok(s),
            Err(e) if e.is_permanent() => return Err(e),
            Err(e) => {
                warn!("attempt {attempt}/{ATTEMPTS} failed: {e}");
                last = Some(e);
                if attempt < ATTEMPTS {
                    tokio::time::sleep(Duration::from_secs(u64::from(attempt) * 2)).await;
                }
            }
        }
    }
    Err(last.expect("at least one attempt"))
}

/// How a `set` ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SetResult {
    /// Written, and the device reported the new value.
    Applied,
    /// The device already had the requested value; nothing was sent.
    Unchanged,
    /// Written, but the device did not report the new value in time.
    Unconfirmed,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SetReport {
    /// Stable key, e.g. `charge_limit`.
    pub setting: &'static str,
    #[serde(skip)]
    pub kind: SettingKind,
    pub requested: String,
    pub before: String,
    /// The value the device reports after the write (`n/a` if it never reported one).
    pub after: String,
    pub result: SetResult,
    pub confirmed: bool,
    pub warnings: Vec<String>,
}

impl SetReport {
    /// 0 when the change is in effect, 3 when it was sent but not confirmed.
    pub fn exit_code(&self) -> u8 {
        if self.confirmed { 0 } else { 3 }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SetError {
    #[error("{0}")]
    Invalid(#[from] SettingError),
    /// Failed before anything was written; safe to retry.
    #[error("{0}")]
    Connect(SessionError),
    /// Failed while writing or waiting for the read-back; the write may have been sent.
    #[error("{0}")]
    Write(SessionError),
}

/// Read the current state, validate `setting` against it, write it, and report the read-back.
pub async fn apply_setting<T: Transport>(
    session: &mut Session<T>,
    setting: Setting,
    settle: Duration,
) -> Result<SetReport, SetError> {
    let status = session.read_status(settle).await.map_err(SetError::Connect)?;
    setting.validate(&status)?;
    let warnings = setting.warnings(&status);
    let kind = setting.kind();
    let before = kind.value_text(&status);

    let outcome = session.apply(setting).await.map_err(SetError::Write)?;
    let after = kind.value_text(outcome.status());
    let (result, confirmed) = match outcome {
        ApplyOutcome::Applied(_) => (SetResult::Applied, true),
        ApplyOutcome::Unchanged(_) => (SetResult::Unchanged, true),
        ApplyOutcome::Unconfirmed(_) => (SetResult::Unconfirmed, false),
    };
    Ok(SetReport {
        setting: kind.key(),
        kind,
        requested: setting.value_text(),
        before,
        after,
        result,
        confirmed,
        warnings,
    })
}

async fn connect_and_set(
    dev: &BleDevice,
    serial: &str,
    user_id: &str,
    timeout: Duration,
    setting: Setting,
) -> Result<SetReport, SetError> {
    let work = async {
        let transport = BleTransport::open(&dev.peripheral, timeout)
            .await
            .map_err(SetError::Connect)?;
        let mut session = Session::establish(transport, dev.info.clone(), serial, user_id, timeout)
            .await
            .map_err(SetError::Connect)?;
        apply_setting(&mut session, setting, SETTLE).await
    };
    let result = tokio::select! {
        r = work => r,
        _ = tokio::signal::ctrl_c() => Err(SetError::Write(SessionError::Transport("interrupted".into()))),
    };
    ble::disconnect(&dev.peripheral).await;
    result
}

/// Connect (retrying transient failures, never after a write may have been sent) and apply `setting`.
pub async fn set_with_retries(
    dev: &BleDevice,
    serial: &str,
    user_id: &str,
    timeout: Duration,
    setting: Setting,
) -> Result<SetReport, SetError> {
    let mut attempt = 1;
    loop {
        info!(attempt, "connecting");
        match connect_and_set(dev, serial, user_id, timeout, setting).await {
            Err(SetError::Connect(e)) if !e.is_permanent() && attempt < ATTEMPTS => {
                warn!("attempt {attempt}/{ATTEMPTS} failed: {e}");
                tokio::time::sleep(Duration::from_secs(u64::from(attempt) * 2)).await;
                attempt += 1;
            }
            other => return other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pb::DisplayPropertyUpload;
    use crate::status::{LedMode, test_device};
    use crate::testutil::{FakeDevice, FakeTransport, WriteBehavior};

    const SERIAL: &str = "R631TESTSERIAL01";
    const USER: &str = "1234567890";

    fn props() -> DisplayPropertyUpload {
        DisplayPropertyUpload {
            cms_batt_soc: Some(42.0),
            cms_max_chg_soc: Some(85),
            cms_min_dsg_soc: Some(15),
            led_mode: Some(0),
            flow_info_12v: Some(0),
            flow_info_ac_out: Some(0),
            ..Default::default()
        }
    }

    async fn run(
        setting: Setting,
        behavior: WriteBehavior,
    ) -> (Result<SetReport, SetError>, crate::testutil::LogHandle) {
        let device = FakeDevice::new(SERIAL, USER, vec![props()]).with_write_behavior(behavior);
        let (transport, log) = FakeTransport::new(device);
        let t = Duration::from_millis(300);
        let mut session = Session::establish(transport, test_device(), SERIAL, USER, t)
            .await
            .unwrap();
        (
            apply_setting(&mut session, setting, Duration::from_millis(30)).await,
            log,
        )
    }

    #[tokio::test]
    async fn applied_report_has_before_and_after() {
        let (r, log) = run(Setting::ChargeLimit(80), WriteBehavior::Apply).await;
        let r = r.unwrap();
        assert_eq!(
            (r.setting, r.before.as_str(), r.after.as_str(), r.result, r.confirmed),
            ("charge_limit", "85%", "80%", SetResult::Applied, true)
        );
        assert_eq!(r.requested, "80%");
        assert_eq!(r.exit_code(), 0);
        assert_eq!(log.lock().unwrap().writes.len(), 1);
    }

    #[tokio::test]
    async fn light_and_ports_report_readable_values() {
        let (r, _) = run(Setting::Light(LedMode::Dim), WriteBehavior::Apply).await;
        let r = r.unwrap();
        assert_eq!((r.before.as_str(), r.after.as_str()), ("off", "dim"));
        let (r, _) = run(Setting::Dc12v(true), WriteBehavior::Apply).await;
        assert_eq!(r.unwrap().after, "on");
    }

    #[tokio::test]
    async fn unchanged_value_sends_nothing_and_succeeds() {
        let (r, log) = run(Setting::DischargeLimit(15), WriteBehavior::Apply).await;
        let r = r.unwrap();
        assert_eq!(r.result, SetResult::Unchanged);
        assert_eq!(r.exit_code(), 0);
        assert!(log.lock().unwrap().writes.is_empty());
    }

    #[tokio::test]
    async fn unconfirmed_write_exits_with_code_3() {
        let (r, log) = run(Setting::Light(LedMode::Bright), WriteBehavior::IgnoreAndStall).await;
        let r = r.unwrap();
        assert_eq!(r.result, SetResult::Unconfirmed);
        assert!(!r.confirmed);
        assert_eq!((r.before.as_str(), r.after.as_str()), ("off", "off"));
        assert_eq!(r.exit_code(), 3);
        assert_eq!(log.lock().unwrap().writes.len(), 1);
    }

    #[tokio::test]
    async fn invalid_requests_are_rejected_before_anything_is_sent() {
        let (r, log) = run(Setting::DischargeLimit(95), WriteBehavior::Apply).await;
        assert!(matches!(r, Err(SetError::Invalid(SettingError::Conflict(_)))), "{r:?}");
        let (r, _) = run(Setting::ChargeLimit(120), WriteBehavior::Apply).await;
        assert!(matches!(r, Err(SetError::Invalid(SettingError::OutOfRange { .. }))));
        assert!(log.lock().unwrap().writes.is_empty());
    }

    #[tokio::test]
    async fn dropped_link_during_write_is_a_write_error_and_not_retryable() {
        let (r, _) = run(Setting::Dc12v(true), WriteBehavior::DropLink).await;
        assert!(matches!(r, Err(SetError::Write(SessionError::LinkClosed))), "{r:?}");
    }

    fn settings(user_id: Option<&str>, serial: Option<&str>) -> Settings {
        Settings {
            user_id: user_id.map(Into::into),
            serial: serial.map(Into::into),
            timeout: Duration::from_secs(5),
        }
    }

    #[test]
    fn writes_need_credentials_so_missing_ones_are_reported() {
        let mut info = test_device();
        // Everything present: serial comes from the advertisement.
        assert_eq!(
            credentials(&info, &settings(Some("1234567890"), None)),
            Ok((info.serial.clone().unwrap(), "1234567890".to_string()))
        );
        // No user ID: an error that says where to configure it (set refuses to write).
        let e = credentials(&info, &settings(None, None)).unwrap_err();
        assert!(e.contains("user ID") && e.contains("--user-id"), "{e}");
        // No serial anywhere: error too; a configured serial fills the gap.
        info.serial = None;
        let e = credentials(&info, &settings(Some("1"), None)).unwrap_err();
        assert!(e.contains("serial"), "{e}");
        assert_eq!(
            credentials(&info, &settings(Some("1"), Some("R631X"))),
            Ok(("R631X".to_string(), "1".to_string()))
        );
    }

    #[test]
    fn report_serializes_with_stable_keys() {
        let r = SetReport {
            setting: "light",
            kind: SettingKind::Light,
            requested: "dim".into(),
            before: "off".into(),
            after: "dim".into(),
            result: SetResult::Applied,
            confirmed: true,
            warnings: vec![],
        };
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["setting"], "light");
        assert_eq!(v["result"], "applied");
        assert_eq!(v["confirmed"], true);
        assert!(v.get("kind").is_none());
    }
}
