//! One-shot output: human text and JSON.

use crate::flow::{SetReport, SetResult};
use crate::status::{Source, Status};
use std::fmt::Write;

fn opt<T: std::fmt::Display>(v: Option<T>, unit: &str) -> String {
    v.map_or_else(|| "n/a".to_string(), |v| format!("{v}{unit}"))
}

pub fn format_minutes(m: u32) -> String {
    format!("{}h {:02}m", m / 60, m % 60)
}

/// `charge limit: 85% -> 80% (confirmed)`
pub fn render_set_text(r: &SetReport) -> String {
    let label = r.kind.label();
    match r.result {
        SetResult::Applied => format!("{label}: {} -> {} (confirmed)\n", r.before, r.after),
        SetResult::Unchanged => format!("{label}: already {}\n", r.after),
        SetResult::Unconfirmed => format!(
            "{label}: requested {} (was {}); sent, but the device did not report the change (still {})\n",
            r.requested, r.before, r.after
        ),
    }
}

pub fn render_set_json(r: &SetReport) -> String {
    serde_json::to_string_pretty(r).expect("report serializes")
}

pub fn render_json(s: &Status) -> String {
    serde_json::to_string_pretty(s).expect("status serializes")
}

pub fn render_text(s: &Status) -> String {
    let mut out = String::new();
    let d = &s.device;
    let _ = writeln!(out, "{} ({})", d.model, d.serial.as_deref().unwrap_or("serial unknown"));
    let _ = writeln!(
        out,
        "  {}  RSSI {}",
        d.address,
        d.rssi.map_or_else(|| "n/a".into(), |r| format!("{r} dBm"))
    );
    match s.source {
        Source::Advertisement => {
            let _ = writeln!(out, "  Source:  advertisement only");
            if let Some(n) = &s.note {
                let _ = writeln!(out, "  Note:    {n}");
            }
            return out;
        }
        Source::Gatt => {}
    }
    let _ = write!(out, "  Battery: {}", opt(s.battery_percent, " %"));
    if let (Some(main), Some(total)) = (s.main_battery_percent, s.battery_percent)
        && (main - total).abs() > 0.05
    {
        let _ = write!(out, " (main {main} %)");
    }
    out.push('\n');
    let state = s.charge_state.map_or_else(|| "unknown".to_string(), |c| c.to_string());
    let plug = match s.ac_plugged_in {
        Some(true) => ", AC plugged in",
        Some(false) => ", AC unplugged",
        None => "",
    };
    let _ = writeln!(out, "  State:   {state}{plug}");
    let _ = writeln!(
        out,
        "  Input:   {}  (AC {}, solar/DC {})",
        opt(s.input_watts, " W"),
        opt(s.ac_input_watts, " W"),
        opt(s.solar_input_watts, " W")
    );
    let _ = writeln!(
        out,
        "  Output:  {}  (AC {}, 12V {}, USB-C {}, USB-A {})",
        opt(s.output_watts, " W"),
        opt(s.ac_output_watts, " W"),
        opt(s.dc12v_output_watts, " W"),
        opt(s.usbc_output_watts, " W"),
        opt(s.usba_output_watts, " W")
    );
    let _ = writeln!(out, "  Battery power: {}", opt(s.battery_watts, " W"));
    if let Some(m) = s
        .minutes_to_full
        .filter(|&m| m > 0 && s.charge_state == Some(crate::status::ChargeState::Charging))
    {
        let _ = writeln!(out, "  Full in: {}", format_minutes(m));
    }
    if let Some(m) = s
        .minutes_to_empty
        .filter(|&m| m > 0 && s.charge_state == Some(crate::status::ChargeState::Discharging))
    {
        let _ = writeln!(out, "  Empty in: {}", format_minutes(m));
    }
    if let Some(t) = s.limit_timer() {
        if t.minutes == 0 {
            let _ = writeln!(out, "  To {}% limit: reached", t.limit_percent);
        } else {
            let _ = writeln!(out, "  To {}% limit: {}", t.limit_percent, format_minutes(t.minutes));
        }
    }
    let _ = writeln!(out, "  Temp:    {}", opt(s.cell_temperature_c, " °C"));
    if let Some(mode) = s.led_mode {
        let _ = writeln!(out, "  Light:   {mode}");
    }
    if let Some(code) = s.error_code.filter(|&c| c != 0) {
        let _ = writeln!(out, "  Error:   code {code}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pb::DisplayPropertyUpload;
    use crate::status::test_device;

    #[test]
    fn advertisement_text_says_battery_unavailable() {
        let s = Status::from_advertisement(test_device(), "no user ID configured");
        let t = render_text(&s);
        assert!(t.contains("advertisement only"));
        assert!(t.contains("Battery data unavailable"));
        assert!(!t.contains("Battery:"));
    }

    #[test]
    fn gatt_text_has_battery_and_state() {
        let p = DisplayPropertyUpload {
            cms_batt_soc: Some(72.0),
            pow_get_bms: Some(100.0),
            pow_in_sum_w: Some(110.0),
            cms_chg_rem_time: Some(95),
            plug_in_info_ac_charger_flag: Some(true),
            ..Default::default()
        };
        let t = render_text(&Status::from_props(test_device(), &p));
        assert!(t.contains("Battery: 72 %"), "{t}");
        assert!(t.contains("charging, AC plugged in"), "{t}");
        assert!(t.contains("Full in: 1h 35m"), "{t}");
        assert!(!t.contains("Light:"), "{t}");

        let lit = DisplayPropertyUpload {
            cms_batt_soc: Some(10.0),
            led_mode: Some(1),
            ..Default::default()
        };
        let t = render_text(&Status::from_props(test_device(), &lit));
        assert!(t.contains("Light:   dim"), "{t}");
    }

    #[test]
    fn text_shows_time_to_discharge_limit() {
        let p = DisplayPropertyUpload {
            cms_batt_soc: Some(42.0),
            pow_get_bms: Some(-14.0),
            cms_dsg_rem_time: Some(477),
            cms_min_dsg_soc: Some(15),
            ..Default::default()
        };
        let t = render_text(&Status::from_props(test_device(), &p));
        assert!(t.contains("Empty in: 7h 57m"), "{t}");
        assert!(t.contains("To 15% limit: 5h 07m"), "{t}");
    }

    #[test]
    fn text_shows_time_to_charge_limit_while_charging() {
        let p = DisplayPropertyUpload {
            cms_batt_soc: Some(60.0),
            pow_get_bms: Some(300.0),
            cms_chg_rem_time: Some(90),
            cms_max_chg_soc: Some(85),
            cms_min_dsg_soc: Some(15),
            cms_dsg_rem_time: Some(400),
            ..Default::default()
        };
        let t = render_text(&Status::from_props(test_device(), &p));
        assert!(t.contains("To 85% limit: 0h 56m"), "{t}");
        assert!(!t.contains("To 15% limit"), "{t}");
    }

    fn report(result: SetResult, before: &str, after: &str, requested: &str) -> SetReport {
        use crate::setting::SettingKind;
        SetReport {
            setting: "charge_limit",
            kind: SettingKind::ChargeLimit,
            requested: requested.into(),
            before: before.into(),
            after: after.into(),
            result,
            confirmed: result != SetResult::Unconfirmed,
            warnings: vec![],
        }
    }

    #[test]
    fn set_text_for_each_result() {
        let t = render_set_text(&report(SetResult::Applied, "85%", "80%", "80%"));
        assert_eq!(t, "charge limit: 85% -> 80% (confirmed)\n");
        let t = render_set_text(&report(SetResult::Unchanged, "85%", "85%", "85%"));
        assert_eq!(t, "charge limit: already 85%\n");
        let t = render_set_text(&report(SetResult::Unconfirmed, "85%", "85%", "80%"));
        assert!(
            t.contains("requested 80%") && t.contains("did not report the change") && t.contains("still 85%"),
            "{t}"
        );
    }

    #[test]
    fn set_json_has_before_after_and_confirmed() {
        let v: serde_json::Value =
            serde_json::from_str(&render_set_json(&report(SetResult::Applied, "85%", "80%", "80%"))).unwrap();
        assert_eq!(v["setting"], "charge_limit");
        assert_eq!((v["before"].as_str(), v["after"].as_str()), (Some("85%"), Some("80%")));
        assert_eq!(v["confirmed"], true);
        assert_eq!(v["result"], "applied");
        assert!(v["warnings"].as_array().unwrap().is_empty());
        let u: serde_json::Value =
            serde_json::from_str(&render_set_json(&report(SetResult::Unconfirmed, "85%", "85%", "80%"))).unwrap();
        assert_eq!(
            (u["confirmed"].as_bool(), u["result"].as_str()),
            (Some(false), Some("unconfirmed"))
        );
    }

    #[test]
    fn json_schema_fields() {
        let s = Status::from_advertisement(test_device(), "x");
        let v: serde_json::Value = serde_json::from_str(&render_json(&s)).unwrap();
        assert_eq!(v["source"], "advertisement");
        assert!(v["battery_percent"].is_null());
        assert_eq!(v["device"]["model"], "River 3 Plus");
        assert!(v["note"].as_str().unwrap().contains("unavailable"));

        let g = Status::from_props(
            test_device(),
            &DisplayPropertyUpload {
                cms_batt_soc: Some(50.0),
                pow_get_bms: Some(-5.0),
                ..Default::default()
            },
        );
        let v: serde_json::Value = serde_json::from_str(&render_json(&g)).unwrap();
        assert_eq!(v["source"], "gatt");
        assert_eq!(v["battery_percent"], 50.0);
        assert_eq!(v["charge_state"], "discharging");
        assert!(v["minutes_to_discharge_limit"].is_null());
        assert!(v["minutes_to_charge_limit"].is_null());
    }
}
