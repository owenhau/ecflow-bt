//! Settings the tool can change on the device: parsing, validation, wire encoding and the
//! read-back check used to confirm that the device applied a change.

use crate::pb::ConfigWrite;
use crate::status::{LedMode, Status};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    /// Upper battery charge limit, percent.
    ChargeLimit(u32),
    /// Lower battery discharge limit, percent.
    DischargeLimit(u32),
    Light(LedMode),
    Dc12v(bool),
    AcOutput(bool),
}

/// Which setting, independent of its value (used for overlay rows and before/after output).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingKind {
    ChargeLimit,
    DischargeLimit,
    Light,
    Dc12v,
    AcOutput,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SettingError {
    #[error("{what} must be between 0 and 100 (got {value})")]
    OutOfRange { what: &'static str, value: u32 },
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Parse(String),
}

pub fn parse_on_off(s: &str) -> Result<bool, SettingError> {
    match s.trim().to_ascii_lowercase().as_str() {
        "on" | "true" | "1" | "yes" => Ok(true),
        "off" | "false" | "0" | "no" => Ok(false),
        other => Err(SettingError::Parse(format!("invalid value '{other}' (use on or off)"))),
    }
}

impl SettingKind {
    pub const ALL: [SettingKind; 5] = [
        Self::ChargeLimit,
        Self::DischargeLimit,
        Self::Light,
        Self::Dc12v,
        Self::AcOutput,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::ChargeLimit => "charge limit",
            Self::DischargeLimit => "discharge limit",
            Self::Light => "light",
            Self::Dc12v => "12V output",
            Self::AcOutput => "AC output",
        }
    }

    /// Stable identifier used in JSON output.
    pub fn key(self) -> &'static str {
        match self {
            Self::ChargeLimit => "charge_limit",
            Self::DischargeLimit => "discharge_limit",
            Self::Light => "light",
            Self::Dc12v => "dc12v",
            Self::AcOutput => "ac_output",
        }
    }

    /// Current value as the device reports it; `n/a` if it has not reported one.
    pub fn value_text(self, s: &Status) -> String {
        let on_off = |v: Option<bool>| v.map_or("n/a".to_string(), |b| if b { "on" } else { "off" }.to_string());
        match self {
            Self::ChargeLimit => s.charge_limit_max_percent.map_or("n/a".into(), |v| format!("{v}%")),
            Self::DischargeLimit => s.discharge_limit_min_percent.map_or("n/a".into(), |v| format!("{v}%")),
            Self::Light => s.led_mode.map_or("n/a".into(), |m| m.to_string()),
            Self::Dc12v => on_off(s.dc12v_output_on),
            Self::AcOutput => on_off(s.ac_output_on),
        }
    }
}

impl Setting {
    pub fn kind(self) -> SettingKind {
        match self {
            Self::ChargeLimit(_) => SettingKind::ChargeLimit,
            Self::DischargeLimit(_) => SettingKind::DischargeLimit,
            Self::Light(_) => SettingKind::Light,
            Self::Dc12v(_) => SettingKind::Dc12v,
            Self::AcOutput(_) => SettingKind::AcOutput,
        }
    }

    /// The requested value, e.g. `85%`, `dim`, `on`.
    pub fn value_text(self) -> String {
        let on_off = |b: bool| if b { "on" } else { "off" }.to_string();
        match self {
            Self::ChargeLimit(v) | Self::DischargeLimit(v) => format!("{v}%"),
            Self::Light(m) => m.to_string(),
            Self::Dc12v(b) | Self::AcOutput(b) => on_off(b),
        }
    }

    /// `charge limit -> 85%`
    pub fn describe(self) -> String {
        format!("{} -> {}", self.kind().label(), self.value_text())
    }

    /// Check the request against the device's current state before anything is sent.
    pub fn validate(self, s: &Status) -> Result<(), SettingError> {
        match self {
            Self::ChargeLimit(v) => {
                if v > 100 {
                    return Err(SettingError::OutOfRange {
                        what: "charge limit",
                        value: v,
                    });
                }
                match s.discharge_limit_min_percent {
                    Some(min) if v < min => Err(SettingError::Conflict(format!(
                        "charge limit {v}% is below the discharge limit ({min}%); lower the discharge limit first"
                    ))),
                    _ => Ok(()),
                }
            }
            Self::DischargeLimit(v) => {
                if v > 100 {
                    return Err(SettingError::OutOfRange {
                        what: "discharge limit",
                        value: v,
                    });
                }
                match s.charge_limit_max_percent {
                    Some(max) if v > max => Err(SettingError::Conflict(format!(
                        "discharge limit {v}% is above the charge limit ({max}%); raise the charge limit first"
                    ))),
                    _ => Ok(()),
                }
            }
            Self::Light(_) | Self::Dc12v(_) | Self::AcOutput(_) => Ok(()),
        }
    }

    /// Non-fatal remarks about a request (the write is still sent).
    pub fn warnings(self, s: &Status) -> Vec<String> {
        let mut w = Vec::new();
        if matches!(self, Self::Light(_)) && s.led_mode.is_none() {
            w.push("the device did not report a light mode; the change may not be confirmed".to_string());
        }
        w
    }

    /// The protobuf message that carries this change; exactly one field is set.
    pub fn to_config(self) -> ConfigWrite {
        let mut c = ConfigWrite::default();
        match self {
            Self::ChargeLimit(v) => c.cfg_max_chg_soc = Some(v),
            Self::DischargeLimit(v) => c.cfg_min_dsg_soc = Some(v),
            Self::Light(m) => c.cfg_led_mode = Some(m.code()),
            Self::Dc12v(b) => c.cfg_dc_12v_out_open = Some(b),
            Self::AcOutput(b) => c.cfg_ac_out_open = Some(b),
        }
        c
    }

    /// Read-back check: does the device now report the requested value?
    pub fn matches(self, s: &Status) -> bool {
        match self {
            Self::ChargeLimit(v) => s.charge_limit_max_percent == Some(v),
            Self::DischargeLimit(v) => s.discharge_limit_min_percent == Some(v),
            Self::Light(m) => s.led_mode == Some(m),
            Self::Dc12v(b) => s.dc12v_output_on == Some(b),
            Self::AcOutput(b) => s.ac_output_on == Some(b),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pb::DisplayPropertyUpload;
    use crate::status::test_device;

    fn status(max: Option<u32>, min: Option<u32>) -> Status {
        let p = DisplayPropertyUpload {
            cms_batt_soc: Some(50.0),
            cms_max_chg_soc: max,
            cms_min_dsg_soc: min,
            ..Default::default()
        };
        Status::from_props(test_device(), &p)
    }

    #[test]
    fn parses_light_modes_case_insensitively() {
        assert_eq!("Dim".parse::<LedMode>(), Ok(LedMode::Dim));
        assert_eq!(" SOS ".parse::<LedMode>(), Ok(LedMode::Sos));
        assert_eq!("off".parse::<LedMode>(), Ok(LedMode::Off));
        assert_eq!("bright".parse::<LedMode>(), Ok(LedMode::Bright));
        let err = "blue".parse::<LedMode>().unwrap_err();
        assert!(err.contains("blue") && err.contains("sos"), "{err}");
    }

    #[test]
    fn light_mode_codes_and_cycle() {
        for (m, c) in [
            (LedMode::Off, 0),
            (LedMode::Dim, 1),
            (LedMode::Bright, 2),
            (LedMode::Sos, 3),
            (LedMode::Unknown(9), 9),
        ] {
            assert_eq!(m.code(), c);
            assert_eq!(LedMode::from_code(c), m);
        }
        assert_eq!(LedMode::Off.next(), LedMode::Dim);
        assert_eq!(LedMode::Sos.next(), LedMode::Off);
        assert_eq!(LedMode::Unknown(7).next(), LedMode::Off);
    }

    #[test]
    fn parses_on_off() {
        for t in ["on", "ON", "true", "1", "yes"] {
            assert_eq!(parse_on_off(t), Ok(true), "{t}");
        }
        for f in ["off", "Off", "false", "0", "no"] {
            assert_eq!(parse_on_off(f), Ok(false), "{f}");
        }
        assert!(matches!(parse_on_off("maybe"), Err(SettingError::Parse(_))));
    }

    #[test]
    fn limit_boundaries() {
        let s = status(Some(85), Some(15));
        assert!(Setting::ChargeLimit(100).validate(&s).is_ok());
        assert!(Setting::ChargeLimit(15).validate(&s).is_ok()); // equal to the discharge limit
        assert!(Setting::DischargeLimit(0).validate(&s).is_ok());
        assert!(Setting::DischargeLimit(85).validate(&s).is_ok()); // equal to the charge limit
        assert_eq!(
            Setting::ChargeLimit(101).validate(&s),
            Err(SettingError::OutOfRange {
                what: "charge limit",
                value: 101
            })
        );
        assert_eq!(
            Setting::DischargeLimit(150).validate(&s),
            Err(SettingError::OutOfRange {
                what: "discharge limit",
                value: 150
            })
        );
    }

    #[test]
    fn cross_limit_conflicts_say_what_to_change_first() {
        let s = status(Some(85), Some(15));
        let e = Setting::ChargeLimit(10).validate(&s).unwrap_err().to_string();
        assert!(
            e.contains("below the discharge limit (15%)") && e.contains("lower the discharge limit first"),
            "{e}"
        );
        let e = Setting::DischargeLimit(90).validate(&s).unwrap_err().to_string();
        assert!(
            e.contains("above the charge limit (85%)") && e.contains("raise the charge limit first"),
            "{e}"
        );
    }

    #[test]
    fn unknown_current_limits_do_not_block() {
        let s = status(None, None);
        assert!(Setting::ChargeLimit(5).validate(&s).is_ok());
        assert!(Setting::DischargeLimit(95).validate(&s).is_ok());
        let only_min = status(None, Some(30));
        assert!(Setting::ChargeLimit(20).validate(&only_min).is_err());
        assert!(Setting::ChargeLimit(40).validate(&only_min).is_ok());
    }

    #[test]
    fn to_config_sets_exactly_one_field() {
        let cases = [
            (
                Setting::ChargeLimit(80),
                ConfigWrite {
                    cfg_max_chg_soc: Some(80),
                    ..Default::default()
                },
            ),
            (
                Setting::DischargeLimit(20),
                ConfigWrite {
                    cfg_min_dsg_soc: Some(20),
                    ..Default::default()
                },
            ),
            (
                Setting::Light(LedMode::Bright),
                ConfigWrite {
                    cfg_led_mode: Some(2),
                    ..Default::default()
                },
            ),
            (
                Setting::Dc12v(true),
                ConfigWrite {
                    cfg_dc_12v_out_open: Some(true),
                    ..Default::default()
                },
            ),
            (
                Setting::AcOutput(false),
                ConfigWrite {
                    cfg_ac_out_open: Some(false),
                    ..Default::default()
                },
            ),
        ];
        for (setting, want) in cases {
            assert_eq!(setting.to_config(), want, "{setting:?}");
        }
    }

    #[test]
    fn matches_checks_the_reported_value() {
        let p = DisplayPropertyUpload {
            cms_batt_soc: Some(50.0),
            cms_max_chg_soc: Some(85),
            cms_min_dsg_soc: Some(15),
            led_mode: Some(1),
            flow_info_12v: Some(2),
            flow_info_ac_out: Some(0),
            ..Default::default()
        };
        let s = Status::from_props(test_device(), &p);
        assert!(Setting::ChargeLimit(85).matches(&s) && !Setting::ChargeLimit(80).matches(&s));
        assert!(Setting::DischargeLimit(15).matches(&s) && !Setting::DischargeLimit(20).matches(&s));
        assert!(Setting::Light(LedMode::Dim).matches(&s) && !Setting::Light(LedMode::Off).matches(&s));
        assert!(Setting::Dc12v(true).matches(&s) && !Setting::Dc12v(false).matches(&s));
        assert!(Setting::AcOutput(false).matches(&s) && !Setting::AcOutput(true).matches(&s));
        // Nothing reported -> nothing can be confirmed.
        let empty = Status::from_props(test_device(), &DisplayPropertyUpload::default());
        for set in [
            Setting::ChargeLimit(85),
            Setting::Light(LedMode::Off),
            Setting::Dc12v(false),
            Setting::AcOutput(false),
        ] {
            assert!(!set.matches(&empty), "{set:?}");
        }
    }

    #[test]
    fn text_helpers() {
        let s = status(Some(85), Some(15));
        assert_eq!(SettingKind::ChargeLimit.value_text(&s), "85%");
        assert_eq!(SettingKind::DischargeLimit.value_text(&s), "15%");
        assert_eq!(SettingKind::Light.value_text(&s), "n/a");
        assert_eq!(Setting::ChargeLimit(80).describe(), "charge limit -> 80%");
        assert_eq!(Setting::Light(LedMode::Sos).describe(), "light -> sos");
        assert_eq!(Setting::AcOutput(true).describe(), "AC output -> on");
        assert_eq!(Setting::Dc12v(false).kind(), SettingKind::Dc12v);
        assert_eq!(SettingKind::ALL.len(), 5);
    }

    #[test]
    fn warns_when_light_is_not_reported() {
        let none = status(Some(85), Some(15));
        assert_eq!(Setting::Light(LedMode::Dim).warnings(&none).len(), 1);
        assert!(Setting::Dc12v(true).warnings(&none).is_empty());
        let p = DisplayPropertyUpload {
            cms_batt_soc: Some(1.0),
            led_mode: Some(0),
            ..Default::default()
        };
        assert!(
            Setting::Light(LedMode::Dim)
                .warnings(&Status::from_props(test_device(), &p))
                .is_empty()
        );
    }
}
