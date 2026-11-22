//! BlueZ access through btleplug: scanning and the GATT transport.

use crate::device::{DeviceInfo, match_advertisement};
use crate::session::{SessionError, Transport};
use btleplug::api::{Central, CharPropFlags, Characteristic, Manager as _, Peripheral as _, ScanFilter, WriteType};
use btleplug::platform::{Adapter, Manager, Peripheral};
use futures::StreamExt;
use futures::stream::BoxStream;
use std::time::Duration;
use tracing::{debug, info};
use uuid::Uuid;

/// (write, notify) characteristic pairs used by EcoFlow devices.
const PROTOCOL_UUIDS: [(Uuid, Uuid); 2] = [
    (
        Uuid::from_u128(0x00000002_0000_1000_8000_00805f9b34fb),
        Uuid::from_u128(0x00000003_0000_1000_8000_00805f9b34fb),
    ),
    (
        Uuid::from_u128(0x6e400002_b5a3_f393_e0a9_e50e24dcca9e),
        Uuid::from_u128(0x6e400003_b5a3_f393_e0a9_e50e24dcca9e),
    ),
];

#[derive(Debug, thiserror::Error)]
pub enum FindError {
    #[error("no Bluetooth adapter found (is BlueZ running and the adapter powered on?)")]
    NoAdapter,
    #[error("Bluetooth error: {0}")]
    Bluetooth(#[from] btleplug::Error),
    #[error("no River 3 / River 3 Plus found within {0:?}{1}")]
    NotFound(Duration, String),
}

#[derive(Clone)]
pub struct BleDevice {
    pub info: DeviceInfo,
    pub peripheral: Peripheral,
}

async fn first_adapter() -> Result<Adapter, FindError> {
    let manager = Manager::new().await?;
    manager.adapters().await?.into_iter().next().ok_or(FindError::NoAdapter)
}

fn serial_matches(info: &DeviceInfo, filter: &str) -> bool {
    info.serial.as_deref().is_some_and(|s| s.eq_ignore_ascii_case(filter))
}

/// Scan until a supported device shows up (optionally restricted to one serial number).
pub async fn find_device(timeout: Duration, serial_filter: Option<&str>) -> Result<BleDevice, FindError> {
    let adapter = first_adapter().await?;
    adapter.start_scan(ScanFilter::default()).await?;
    let deadline = tokio::time::Instant::now() + timeout;

    let result = loop {
        let mut other = 0usize;
        for peripheral in adapter.peripherals().await? {
            let Ok(Some(props)) = peripheral.properties().await else {
                continue;
            };
            let Some(info) = match_advertisement(
                &props.address.to_string(),
                props.local_name.as_deref(),
                props.rssi,
                &props.manufacturer_data,
            ) else {
                debug!(
                    address = %props.address,
                    name = ?props.local_name,
                    manufacturer_ids = ?props.manufacturer_data.keys().map(|k| format!("0x{k:04X}")).collect::<Vec<_>>(),
                    "not a supported device"
                );
                other += 1;
                continue;
            };
            if serial_filter.is_some_and(|f| !serial_matches(&info, f)) {
                debug!(address = %info.address, "device skipped: serial filter does not match");
                continue;
            }
            let _ = adapter.stop_scan().await;
            return Ok(BleDevice { info, peripheral });
        }
        if tokio::time::Instant::now() >= deadline {
            break Err(FindError::NotFound(
                timeout,
                format!(" ({other} other Bluetooth devices seen)"),
            ));
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    };
    let _ = adapter.stop_scan().await;
    result
}

pub struct BleTransport {
    peripheral: Peripheral,
    write_char: Characteristic,
    notifications: BoxStream<'static, btleplug::api::ValueNotification>,
}

fn t(e: impl std::fmt::Display) -> SessionError {
    SessionError::Transport(e.to_string())
}

impl BleTransport {
    /// Connect, discover services and subscribe to the notify characteristic.
    pub async fn open(peripheral: &Peripheral, timeout: Duration) -> Result<Self, SessionError> {
        let setup = async {
            if !peripheral.is_connected().await.map_err(t)? {
                peripheral.connect().await.map_err(t)?;
            }
            peripheral.discover_services().await.map_err(t)?;
            let chars = peripheral.characteristics();
            let (write_char, notify_char) = PROTOCOL_UUIDS
                .iter()
                .find_map(|(w, n)| {
                    let w = chars.iter().find(|c| c.uuid == *w)?;
                    let n = chars.iter().find(|c| c.uuid == *n)?;
                    Some((w.clone(), n.clone()))
                })
                .ok_or_else(|| {
                    let list: Vec<_> = chars.iter().map(|c| c.uuid.to_string()).collect();
                    SessionError::Transport(format!(
                        "device does not expose a known EcoFlow GATT protocol; characteristics: {}",
                        list.join(", ")
                    ))
                })?;
            if !notify_char.properties.contains(CharPropFlags::NOTIFY)
                && !notify_char.properties.contains(CharPropFlags::INDICATE)
            {
                return Err(t("notify characteristic does not support notifications"));
            }
            // Subscribe to the stream before enabling notifications so none are missed.
            let stream = peripheral.notifications().await.map_err(t)?;
            peripheral.subscribe(&notify_char).await.map_err(t)?;
            let notify_uuid = notify_char.uuid;
            let notifications = stream
                .filter(move |n| futures::future::ready(n.uuid == notify_uuid))
                .boxed();
            Ok(Self {
                peripheral: peripheral.clone(),
                write_char,
                notifications,
            })
        };
        tokio::time::timeout(timeout, setup)
            .await
            .map_err(|_| SessionError::Timeout("BLE connect"))?
    }
}

impl Transport for BleTransport {
    async fn write(&mut self, data: &[u8]) -> Result<(), SessionError> {
        let mut last = None;
        for attempt in 0..3u64 {
            match self
                .peripheral
                .write(&self.write_char, data, WriteType::WithResponse)
                .await
            {
                Ok(()) => return Ok(()),
                Err(e) => {
                    debug!("write attempt {} failed: {e}", attempt + 1);
                    last = Some(e);
                    tokio::time::sleep(Duration::from_millis(200 * (attempt + 1))).await;
                }
            }
        }
        Err(t(last.expect("loop ran")))
    }

    async fn recv(&mut self) -> Result<Vec<u8>, SessionError> {
        self.notifications
            .next()
            .await
            .map(|n| n.value)
            .ok_or(SessionError::LinkClosed)
    }
}

pub async fn disconnect(peripheral: &Peripheral) {
    match peripheral.disconnect().await {
        Ok(()) => info!("disconnected"),
        Err(e) => debug!("disconnect failed: {e}"),
    }
}
