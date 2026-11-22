//! Command-line definition (kept in the library so parsing is unit-testable).

use crate::setting::{Setting, SettingError, parse_on_off};
use crate::status::LedMode;
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    version,
    about = "Read battery and charging status from an EcoFlow River 3 / River 3 Plus over Bluetooth LE, and change its settings"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Cmd>,

    /// EcoFlow account user ID (also EF_USER_ID or `user_id` in config.toml)
    #[arg(long, global = true)]
    pub user_id: Option<String>,
    /// Device serial number (also EF_SERIAL or `serial`); selects the device and is used if the advertisement lacks it
    #[arg(long, global = true)]
    pub serial: Option<String>,
    /// Seconds to wait per stage (scan, connect, handshake, first data)
    #[arg(long, global = true)]
    pub timeout: Option<u64>,
    /// Print JSON instead of text
    #[arg(long, global = true)]
    pub json: bool,
    /// Open the live dashboard
    #[arg(long)]
    pub tui: bool,
    /// Increase log detail on stderr (-v info, -vv debug, -vvv trace)
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// Show the resolved settings (user ID masked) and the config file path
    Config,
    /// List supported devices seen over Bluetooth
    Scan,
    /// Decode a captured, decrypted inner packet given as hex
    DecodePacket {
        /// Hex bytes, e.g. "aa 03 ..." (spaces and colons allowed)
        hex: String,
    },
    /// Change a device setting. Applied immediately (no confirmation) and read back from the device.
    Set {
        #[command(subcommand)]
        what: SetCmd,
    },
}

fn on_off(s: &str) -> Result<bool, String> {
    parse_on_off(s).map_err(|e: SettingError| e.to_string())
}

#[derive(Subcommand, Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetCmd {
    /// Upper battery charge limit in percent (0-100); must not be below the discharge limit
    ChargeLimit { percent: u32 },
    /// Lower battery discharge limit in percent (0-100); must not be above the charge limit
    DischargeLimit { percent: u32 },
    /// Front light mode: off, dim, bright or sos
    Light {
        #[arg(value_parser = str::parse::<LedMode>)]
        mode: LedMode,
    },
    /// 12V DC output: on or off
    Dc12v {
        #[arg(value_parser = on_off, action = clap::ArgAction::Set)]
        state: bool,
    },
    /// AC output: on or off. Turning it off cuts power to anything plugged into the AC outlets immediately.
    Ac {
        #[arg(value_parser = on_off, action = clap::ArgAction::Set)]
        state: bool,
    },
}

impl SetCmd {
    pub fn to_setting(self) -> Setting {
        match self {
            Self::ChargeLimit { percent } => Setting::ChargeLimit(percent),
            Self::DischargeLimit { percent } => Setting::DischargeLimit(percent),
            Self::Light { mode } => Setting::Light(mode),
            Self::Dc12v { state } => Setting::Dc12v(state),
            Self::Ac { state } => Setting::AcOutput(state),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("ecflow-bt").chain(args.iter().copied()))
    }

    fn setting(args: &[&str]) -> Setting {
        match parse(args).unwrap().command {
            Some(Cmd::Set { what }) => what.to_setting(),
            other => panic!("not a set command: {other:?}"),
        }
    }

    #[test]
    fn parses_every_set_command() {
        assert_eq!(setting(&["set", "charge-limit", "80"]), Setting::ChargeLimit(80));
        assert_eq!(setting(&["set", "discharge-limit", "20"]), Setting::DischargeLimit(20));
        assert_eq!(setting(&["set", "light", "Dim"]), Setting::Light(LedMode::Dim));
        assert_eq!(setting(&["set", "light", "sos"]), Setting::Light(LedMode::Sos));
        assert_eq!(setting(&["set", "dc12v", "on"]), Setting::Dc12v(true));
        assert_eq!(setting(&["set", "dc12v", "OFF"]), Setting::Dc12v(false));
        assert_eq!(setting(&["set", "ac", "off"]), Setting::AcOutput(false));
        assert_eq!(setting(&["set", "ac", "on"]), Setting::AcOutput(true));
    }

    #[test]
    fn rejects_bad_values() {
        for bad in [
            &["set", "light", "blue"][..],
            &["set", "ac", "maybe"],
            &["set", "charge-limit", "abc"],
            &["set", "charge-limit", "-5"],
            &["set", "charge-limit"],
            &["set", "volume", "3"],
            &["set"],
        ] {
            assert!(parse(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn out_of_range_numbers_parse_and_are_rejected_by_validation_later() {
        // clap only checks that it is a number; Setting::validate owns the 0-100 rule.
        assert_eq!(setting(&["set", "charge-limit", "150"]), Setting::ChargeLimit(150));
    }

    #[test]
    fn global_flags_work_on_either_side_of_set() {
        let a = parse(&["--json", "--timeout", "7", "set", "light", "off"]).unwrap();
        let b = parse(&["set", "light", "off", "--json", "--timeout", "7", "--user-id", "u"]).unwrap();
        assert!(a.json && b.json);
        assert_eq!((a.timeout, b.timeout), (Some(7), Some(7)));
        assert_eq!(b.user_id.as_deref(), Some("u"));
    }

    #[test]
    fn existing_commands_still_parse() {
        assert!(matches!(parse(&["scan"]).unwrap().command, Some(Cmd::Scan)));
        assert!(matches!(parse(&["config"]).unwrap().command, Some(Cmd::Config)));
        assert!(parse(&["--tui"]).unwrap().tui);
        assert!(parse(&[]).unwrap().command.is_none());
    }
}
