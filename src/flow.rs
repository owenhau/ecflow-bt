//! High-level flows shared by the one-shot command and the TUI worker.

use crate::ble::{self, BleDevice, BleTransport};
use crate::config::{Settings, config_path};
use crate::device::DeviceInfo;
use crate::session::{Session, SessionError};
use crate::status::Status;
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
