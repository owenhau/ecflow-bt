//! Status model shared by the one-shot output, JSON output and the TUI.

use crate::device::DeviceInfo;
use crate::pb::DisplayPropertyUpload;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// Decoded from the encrypted GATT session.
    Gatt,
    /// Only the passive advertisement was available.
    Advertisement,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ChargeState {
    Charging,
    Discharging,
    Idle,
}

impl std::fmt::Display for ChargeState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Charging => "charging",
            Self::Discharging => "discharging",
            Self::Idle => "idle",
        })
    }
}

/// Time estimates above this (72 h) are device placeholders, not real predictions.
pub const MAX_PLAUSIBLE_MINUTES: u32 = 72 * 60;

/// Minutes until the battery reaches the configured discharge limit.
///
/// The device only reports time to empty (0 %). Assuming the present load stays constant,
/// time scales with the energy left: `T_empty * (soc - limit) / soc`. Returns `Some(0)` if the
/// limit is already reached, and `None` if the inputs do not allow an estimate.
pub fn minutes_to_limit(soc: Option<f64>, limit: Option<u32>, minutes_to_empty: Option<u32>) -> Option<u32> {
    let (soc, limit) = (soc?, f64::from(limit?));
    let empty = minutes_to_empty.filter(|&m| m > 0 && m <= MAX_PLAUSIBLE_MINUTES)?;
    if limit <= 0.0 || soc <= 0.0 {
        return None;
    }
    if soc <= limit {
        return Some(0);
    }
    Some((f64::from(empty) * (soc - limit) / soc).round() as u32)
}

/// Minutes until the battery reaches the configured charge limit.
///
/// The device only reports time to full (100 %). Assuming the present charging power stays
/// constant, the time to the limit is `T_full * (limit - soc) / (100 - soc)`. Returns `Some(0)`
/// once the limit is reached; `None` if there is no limit below 100 % or no usable estimate.
pub fn minutes_to_charge_limit(soc: Option<f64>, limit: Option<u32>, minutes_to_full: Option<u32>) -> Option<u32> {
    let (soc, limit) = (soc?, f64::from(limit.filter(|l| (1..100).contains(l))?));
    if soc >= limit {
        return Some(0);
    }
    let full = minutes_to_full.filter(|&m| m > 0 && m <= MAX_PLAUSIBLE_MINUTES)?;
    Some((f64::from(full) * (limit - soc) / (100.0 - soc)).round() as u32)
}

/// Battery power below this magnitude (watts) counts as idle, to avoid flicker from sensor noise.
const IDLE_DEADBAND_W: f32 = 0.5;

/// Charging state from the BMS power (positive = charging, negative = discharging).
/// If the BMS value is missing, falls back to comparing total input and output power.
pub fn charge_state(bms_w: Option<f32>, in_w: Option<f32>, out_w: Option<f32>) -> Option<ChargeState> {
    if let Some(w) = bms_w {
        return Some(if w > IDLE_DEADBAND_W {
            ChargeState::Charging
        } else if w < -IDLE_DEADBAND_W {
            ChargeState::Discharging
        } else {
            ChargeState::Idle
        });
    }
    match (in_w, out_w) {
        (None, None) => None,
        (i, o) => {
            let (i, o) = (i.unwrap_or(0.0), o.unwrap_or(0.0).abs());
            Some(if i - o > IDLE_DEADBAND_W {
                ChargeState::Charging
            } else if o - i > IDLE_DEADBAND_W {
                ChargeState::Discharging
            } else {
                ChargeState::Idle
            })
        }
    }
}

/// Front light mode (River 3 Plus). Unrecognised values are kept rather than dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedMode {
    Off,
    Dim,
    Bright,
    Sos,
    Unknown(u32),
}

impl LedMode {
    pub fn from_code(code: u32) -> Self {
        match code {
            0 => Self::Off,
            1 => Self::Dim,
            2 => Self::Bright,
            3 => Self::Sos,
            n => Self::Unknown(n),
        }
    }

    pub fn is_on(self) -> bool {
        !matches!(self, Self::Off)
    }
}

impl std::fmt::Display for LedMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Off => f.write_str("off"),
            Self::Dim => f.write_str("dim"),
            Self::Bright => f.write_str("bright"),
            Self::Sos => f.write_str("sos"),
            Self::Unknown(n) => write!(f, "unknown({n})"),
        }
    }
}

impl Serialize for LedMode {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.collect_str(self)
    }
}

fn round2(v: f32) -> f64 {
    (f64::from(v) * 100.0).round() / 100.0
}

/// Port flow-info bitmask: values 2 and 3 mean "on" (same check as the EcoFlow app).
fn flow_is_on(v: u32) -> bool {
    matches!(v & 0b11, 0b10 | 0b11)
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Status {
    pub source: Source,
    pub device: DeviceInfo,
    /// Why battery data is missing (advertisement-only results).
    pub note: Option<String>,

    pub battery_percent: Option<f64>,
    /// Main (internal) battery only; differs from `battery_percent` with an extra battery.
    pub main_battery_percent: Option<f64>,
    pub charge_state: Option<ChargeState>,
    pub ac_plugged_in: Option<bool>,
    pub extra_battery_attached: Option<bool>,

    pub input_watts: Option<f64>,
    pub output_watts: Option<f64>,
    pub battery_watts: Option<f64>,
    pub ac_input_watts: Option<f64>,
    pub solar_input_watts: Option<f64>,
    pub ac_output_watts: Option<f64>,
    pub dc12v_output_watts: Option<f64>,
    pub usbc_output_watts: Option<f64>,
    pub usba_output_watts: Option<f64>,

    pub minutes_to_full: Option<u32>,
    pub minutes_to_empty: Option<u32>,
    /// Estimated minutes until the discharge limit is reached (only while discharging).
    pub minutes_to_discharge_limit: Option<u32>,
    /// Estimated minutes until the charge limit is reached (only while charging).
    pub minutes_to_charge_limit: Option<u32>,
    pub cell_temperature_c: Option<i32>,
    pub charge_limit_max_percent: Option<u32>,
    pub discharge_limit_min_percent: Option<u32>,
    pub ac_output_on: Option<bool>,
    pub dc12v_output_on: Option<bool>,
    pub error_code: Option<u32>,
    /// Front light mode; `None` if the device does not report one.
    pub led_mode: Option<LedMode>,
}

/// The limit the battery is currently heading toward, and the estimated time to reach it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LimitTimer {
    /// The configured limit in percent: charge limit while charging, discharge limit while discharging.
    pub limit_percent: u32,
    /// Minutes until reached; 0 means already reached.
    pub minutes: u32,
    pub charging: bool,
}

impl Status {
    /// Timer for the relevant limit: the upper (charge) limit while charging, the lower
    /// (discharge) limit while discharging. `None` when idle or without a usable estimate.
    pub fn limit_timer(&self) -> Option<LimitTimer> {
        match self.charge_state? {
            ChargeState::Charging => Some(LimitTimer {
                limit_percent: self.charge_limit_max_percent.filter(|l| (1..100).contains(l))?,
                minutes: self.minutes_to_charge_limit?,
                charging: true,
            }),
            ChargeState::Discharging => Some(LimitTimer {
                limit_percent: self.discharge_limit_min_percent.filter(|&l| l > 0)?,
                minutes: self.minutes_to_discharge_limit?,
                charging: false,
            }),
            ChargeState::Idle => None,
        }
    }

    /// Advertisement-only status: battery fields stay empty and `note` says why.
    pub fn from_advertisement(device: DeviceInfo, reason: impl Into<String>) -> Self {
        Self {
            source: Source::Advertisement,
            device,
            note: Some(format!(
                "Battery data unavailable (advertisement only): {}",
                reason.into()
            )),
            battery_percent: None,
            main_battery_percent: None,
            charge_state: None,
            ac_plugged_in: None,
            extra_battery_attached: None,
            input_watts: None,
            output_watts: None,
            battery_watts: None,
            ac_input_watts: None,
            solar_input_watts: None,
            ac_output_watts: None,
            dc12v_output_watts: None,
            usbc_output_watts: None,
            usba_output_watts: None,
            minutes_to_full: None,
            minutes_to_empty: None,
            minutes_to_discharge_limit: None,
            minutes_to_charge_limit: None,
            cell_temperature_c: None,
            charge_limit_max_percent: None,
            discharge_limit_min_percent: None,
            ac_output_on: None,
            dc12v_output_on: None,
            error_code: None,
            led_mode: None,
        }
    }

    /// Build a GATT status from the accumulated device properties.
    pub fn from_props(device: DeviceInfo, p: &DisplayPropertyUpload) -> Self {
        let f = |v: Option<f32>| v.map(round2);
        // Output ports report negative power; show them as positive watts.
        let out = |v: Option<f32>| v.map(|x| if x == 0.0 { 0.0 } else { round2(-x) });
        let mut s = Self::from_advertisement(device, "");
        s.source = Source::Gatt;
        s.note = None;
        s.battery_percent = f(p.cms_batt_soc);
        s.main_battery_percent = f(p.bms_batt_soc);
        s.charge_state = charge_state(p.pow_get_bms, p.pow_in_sum_w, p.pow_out_sum_w);
        s.ac_plugged_in = p.plug_in_info_ac_charger_flag;
        s.extra_battery_attached = p.plug_in_info_dcp_in_flag;
        s.input_watts = f(p.pow_in_sum_w);
        s.output_watts = f(p.pow_out_sum_w);
        s.battery_watts = f(p.pow_get_bms);
        s.ac_input_watts = f(p.pow_get_ac_in);
        s.solar_input_watts = f(p.pow_get_pv);
        s.ac_output_watts = out(p.pow_get_ac_out);
        s.dc12v_output_watts = out(p.pow_get_12v);
        s.usbc_output_watts = out(p.pow_get_typec1);
        s.usba_output_watts = out(p.pow_get_qcusb1);
        s.minutes_to_full = p.cms_chg_rem_time;
        s.minutes_to_empty = p.cms_dsg_rem_time;
        s.cell_temperature_c = p.bms_max_cell_temp;
        s.charge_limit_max_percent = p.cms_max_chg_soc;
        s.discharge_limit_min_percent = p.cms_min_dsg_soc;
        s.ac_output_on = p.flow_info_ac_out.map(flow_is_on);
        s.dc12v_output_on = p.flow_info_12v.map(flow_is_on);
        s.minutes_to_discharge_limit = if s.charge_state == Some(ChargeState::Discharging) {
            minutes_to_limit(s.battery_percent, s.discharge_limit_min_percent, s.minutes_to_empty)
        } else {
            None
        };
        s.minutes_to_charge_limit = if s.charge_state == Some(ChargeState::Charging) {
            minutes_to_charge_limit(s.battery_percent, s.charge_limit_max_percent, s.minutes_to_full)
        } else {
            None
        };
        s.error_code = p.errcode;
        s.led_mode = p.led_mode.map(LedMode::from_code);
        s
    }
}

#[cfg(test)]
pub(crate) fn test_device() -> DeviceInfo {
    DeviceInfo {
        model: "River 3 Plus".into(),
        serial: Some("R631XXXXXXXXXXXX".into()),
        name: Some("EF-R3-TEST".into()),
        address: "AA:BB:CC:DD:EE:FF".into(),
        rssi: Some(-55),
        manufacturer_data: None,
        encrypt_type: Some(7),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charge_state_from_bms_power() {
        assert_eq!(charge_state(Some(120.0), None, None), Some(ChargeState::Charging));
        assert_eq!(charge_state(Some(-45.5), None, None), Some(ChargeState::Discharging));
        assert_eq!(charge_state(Some(0.0), Some(300.0), None), Some(ChargeState::Idle));
        assert_eq!(charge_state(Some(0.3), None, None), Some(ChargeState::Idle));
        assert_eq!(charge_state(Some(-0.3), None, None), Some(ChargeState::Idle));
    }

    #[test]
    fn charge_state_falls_back_to_in_out_power() {
        assert_eq!(
            charge_state(None, Some(200.0), Some(-50.0)),
            Some(ChargeState::Charging)
        );
        assert_eq!(
            charge_state(None, Some(0.0), Some(-60.0)),
            Some(ChargeState::Discharging)
        );
        assert_eq!(charge_state(None, Some(30.0), Some(-30.0)), Some(ChargeState::Idle));
        assert_eq!(charge_state(None, None, None), None);
    }

    #[test]
    fn from_props_maps_fields() {
        let p = DisplayPropertyUpload {
            cms_batt_soc: Some(87.456),
            bms_batt_soc: Some(80.0),
            pow_get_bms: Some(-61.234),
            pow_in_sum_w: Some(0.0),
            pow_out_sum_w: Some(61.5),
            pow_get_ac_out: Some(-55.0),
            pow_get_12v: Some(0.0),
            plug_in_info_ac_charger_flag: Some(false),
            cms_dsg_rem_time: Some(300),
            bms_max_cell_temp: Some(27),
            flow_info_ac_out: Some(2),
            flow_info_12v: Some(0),
            errcode: Some(0),
            ..Default::default()
        };
        let s = Status::from_props(test_device(), &p);
        assert_eq!(s.source, Source::Gatt);
        assert_eq!(s.battery_percent, Some(87.46));
        assert_eq!(s.main_battery_percent, Some(80.0));
        assert_eq!(s.charge_state, Some(ChargeState::Discharging));
        assert_eq!(s.battery_watts, Some(-61.23));
        assert_eq!(s.ac_output_watts, Some(55.0));
        assert_eq!(s.dc12v_output_watts, Some(0.0));
        assert_eq!(s.ac_output_on, Some(true));
        assert_eq!(s.dc12v_output_on, Some(false));
        assert_eq!(s.ac_plugged_in, Some(false));
        assert_eq!(s.minutes_to_empty, Some(300));
        assert_eq!(s.minutes_to_full, None);
        assert!(s.note.is_none());
    }

    #[test]
    fn time_to_discharge_limit_scales_empty_estimate() {
        // 42 % now, limit 15 %, 477 min to empty -> 477 * 27/42 = 306.6 min.
        assert_eq!(minutes_to_limit(Some(42.0), Some(15), Some(477)), Some(307));
        assert_eq!(minutes_to_limit(Some(15.0), Some(15), Some(100)), Some(0));
        assert_eq!(minutes_to_limit(Some(10.0), Some(15), Some(100)), Some(0));
        assert_eq!(minutes_to_limit(Some(50.0), Some(0), Some(100)), None); // no limit set
        assert_eq!(minutes_to_limit(Some(50.0), Some(15), Some(0)), None);
        assert_eq!(minutes_to_limit(Some(50.0), Some(15), Some(5939)), None); // placeholder
        assert_eq!(minutes_to_limit(None, Some(15), Some(100)), None);
        assert_eq!(minutes_to_limit(Some(50.0), None, Some(100)), None);
        assert_eq!(minutes_to_limit(Some(0.0), Some(15), Some(100)), None);
    }

    #[test]
    fn time_to_charge_limit_scales_full_estimate() {
        // 60 % now, limit 85 %, 90 min to full (100 %) -> 90 * 25/40 = 56.25 min.
        assert_eq!(minutes_to_charge_limit(Some(60.0), Some(85), Some(90)), Some(56));
        assert_eq!(minutes_to_charge_limit(Some(85.0), Some(85), Some(90)), Some(0));
        assert_eq!(minutes_to_charge_limit(Some(90.0), Some(85), None), Some(0)); // reached needs no estimate
        assert_eq!(minutes_to_charge_limit(Some(60.0), Some(100), Some(90)), None); // no limit below 100 %
        assert_eq!(minutes_to_charge_limit(Some(60.0), Some(0), Some(90)), None);
        assert_eq!(minutes_to_charge_limit(Some(60.0), Some(85), Some(0)), None);
        assert_eq!(minutes_to_charge_limit(Some(60.0), Some(85), Some(5939)), None); // placeholder
        assert_eq!(minutes_to_charge_limit(None, Some(85), Some(90)), None);
    }

    #[test]
    fn limit_timer_follows_charge_state() {
        let base = DisplayPropertyUpload {
            cms_batt_soc: Some(60.0),
            cms_max_chg_soc: Some(85),
            cms_min_dsg_soc: Some(15),
            cms_chg_rem_time: Some(90),
            cms_dsg_rem_time: Some(400),
            ..Default::default()
        };
        let charging = Status::from_props(
            test_device(),
            &DisplayPropertyUpload {
                pow_get_bms: Some(100.0),
                ..base.clone()
            },
        );
        assert_eq!(charging.minutes_to_discharge_limit, None);
        assert_eq!(
            charging.limit_timer(),
            Some(LimitTimer {
                limit_percent: 85,
                minutes: 56,
                charging: true
            })
        );

        let discharging = Status::from_props(
            test_device(),
            &DisplayPropertyUpload {
                pow_get_bms: Some(-20.0),
                ..base.clone()
            },
        );
        assert_eq!(discharging.minutes_to_charge_limit, None);
        let t = discharging.limit_timer().unwrap();
        assert_eq!((t.limit_percent, t.charging), (15, false));

        let idle = Status::from_props(
            test_device(),
            &DisplayPropertyUpload {
                pow_get_bms: Some(0.0),
                ..base
            },
        );
        assert_eq!(idle.limit_timer(), None);
    }

    #[test]
    fn limit_timer_only_while_discharging() {
        let base = DisplayPropertyUpload {
            cms_batt_soc: Some(42.0),
            cms_min_dsg_soc: Some(15),
            cms_dsg_rem_time: Some(477),
            ..Default::default()
        };
        let discharging = DisplayPropertyUpload {
            pow_get_bms: Some(-14.0),
            ..base.clone()
        };
        let s = Status::from_props(test_device(), &discharging);
        assert_eq!(s.minutes_to_discharge_limit, Some(307));

        let charging = DisplayPropertyUpload {
            pow_get_bms: Some(50.0),
            ..base
        };
        assert_eq!(
            Status::from_props(test_device(), &charging).minutes_to_discharge_limit,
            None
        );
    }

    #[test]
    fn led_mode_mapping_and_json() {
        assert_eq!(LedMode::from_code(0), LedMode::Off);
        assert_eq!(LedMode::from_code(1), LedMode::Dim);
        assert_eq!(LedMode::from_code(2), LedMode::Bright);
        assert_eq!(LedMode::from_code(3), LedMode::Sos);
        assert_eq!(LedMode::from_code(9), LedMode::Unknown(9));
        assert!(!LedMode::Off.is_on() && LedMode::Dim.is_on() && LedMode::Unknown(9).is_on());

        let p = DisplayPropertyUpload {
            led_mode: Some(2),
            ..Default::default()
        };
        let s = Status::from_props(test_device(), &p);
        assert_eq!(s.led_mode, Some(LedMode::Bright));
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["led_mode"], "bright");
        let none = serde_json::to_value(Status::from_props(test_device(), &Default::default())).unwrap();
        assert!(none["led_mode"].is_null());
    }

    #[test]
    fn missing_fields_stay_none() {
        let s = Status::from_props(test_device(), &DisplayPropertyUpload::default());
        assert_eq!(s.battery_percent, None);
        assert_eq!(s.charge_state, None);
        assert_eq!(s.error_code, None);
    }

    #[test]
    fn advertisement_status_has_note_and_no_battery() {
        let s = Status::from_advertisement(test_device(), "no user id configured");
        assert_eq!(s.source, Source::Advertisement);
        assert!(s.battery_percent.is_none());
        assert!(s.note.unwrap().contains("no user id configured"));
    }
}
