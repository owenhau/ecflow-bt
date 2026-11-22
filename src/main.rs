use anyhow::{Context, Result};
use clap::Parser;
use ecflow_bt::ble::{self, BleDevice};
use ecflow_bt::cli::{Cli, Cmd, SetCmd};
use ecflow_bt::config::{self, Overrides, Settings};
use ecflow_bt::flow;
use ecflow_bt::output::{render_json, render_set_json, render_set_text, render_text};
use ecflow_bt::pb::{CONFIG_WRITE_CMD_ID, CONFIG_WRITE_CMD_SET, ConfigWrite, DisplayPropertyUpload};
use ecflow_bt::protocol::packet::Packet;
use ecflow_bt::status::Status;
use prost::Message;
use std::process::ExitCode;

/// Exit codes: 0 = live GATT data / change applied, 1 = error, 2 = fallback (advertisement data only),
/// 3 = change sent but not confirmed by the device.
const EXIT_FALLBACK: u8 = 2;

fn settings(cli: &Cli) -> Result<Settings> {
    let file = match config::config_path() {
        Some(p) => config::load_file(&p)?,
        None => Default::default(),
    };
    let flags = Overrides {
        user_id: cli.user_id.clone(),
        serial: cli.serial.clone(),
        timeout_secs: cli.timeout,
    };
    config::resolve(flags, Overrides::from_env(), file)
}

fn init_logging(verbose: u8) {
    let default = match verbose {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

fn cmd_config(s: &Settings) {
    match config::config_path() {
        Some(p) => println!(
            "config file: {} ({})",
            p.display(),
            if p.exists() { "found" } else { "not found" }
        ),
        None => println!("config file: <no home directory>"),
    }
    println!(
        "user_id:     {}",
        s.user_id.as_deref().map_or("<not set>".into(), config::mask)
    );
    println!(
        "serial:      {}",
        s.serial.as_deref().unwrap_or("<not set; taken from the advertisement>")
    );
    println!("timeout:     {}s", s.timeout.as_secs());
}

fn cmd_decode(hex_str: &str) -> Result<()> {
    let cleaned: String = hex_str.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    let bytes = hex::decode(&cleaned).context("input is not valid hex")?;
    let p = Packet::from_bytes(&bytes, true).map_err(|e| anyhow::anyhow!("{e}"))?;
    println!(
        "src=0x{:02X} dst=0x{:02X} cmd_set=0x{:02X} cmd_id=0x{:02X} version=0x{:02X} seq={}",
        p.src,
        p.dst,
        p.cmd_set,
        p.cmd_id,
        p.version,
        hex::encode(p.seq)
    );
    println!("payload ({} bytes): {}", p.payload.len(), hex::encode(&p.payload));
    if (p.src, p.cmd_set, p.cmd_id) == (0x02, 0xFE, 0x15) {
        let props =
            DisplayPropertyUpload::decode(p.payload.as_slice()).context("payload is not a DisplayPropertyUpload")?;
        let dummy = ecflow_bt::device::DeviceInfo {
            model: "(decoded packet)".into(),
            serial: None,
            name: None,
            address: "-".into(),
            rssi: None,
            manufacturer_data: None,
            encrypt_type: None,
        };
        print!("{}", render_text(&Status::from_props(dummy, &props)));
    } else if (p.cmd_set, p.cmd_id) == (CONFIG_WRITE_CMD_SET, CONFIG_WRITE_CMD_ID) {
        let cfg = ConfigWrite::decode(p.payload.as_slice()).context("payload is not a ConfigWrite")?;
        println!("ConfigWrite: {}", cfg.summary());
    }
    Ok(())
}

async fn cmd_scan(s: &Settings) -> Result<()> {
    let dev = ble::find_device(s.timeout, s.serial.as_deref()).await?;
    let i = &dev.info;
    println!(
        "{}  {}  RSSI {}",
        i.address,
        i.model,
        i.rssi.map_or("n/a".into(), |r| format!("{r} dBm"))
    );
    println!("  name:   {}", i.name.as_deref().unwrap_or("-"));
    println!("  serial: {}", i.serial.as_deref().unwrap_or("-"));
    Ok(())
}

async fn one_shot(s: &Settings, json: bool) -> Result<ExitCode> {
    let dev: BleDevice = ble::find_device(s.timeout, s.serial.as_deref()).await?;
    let print = |st: &Status| {
        if json {
            println!("{}", render_json(st));
        } else {
            print!("{}", render_text(st));
        }
    };

    let (serial, user_id) = match flow::credentials(&dev.info, s) {
        Ok(c) => c,
        Err(reason) => {
            print(&Status::from_advertisement(dev.info, reason));
            return Ok(ExitCode::from(EXIT_FALLBACK));
        }
    };
    match flow::read_with_retries(&dev, &serial, &user_id, s.timeout).await {
        Ok(st) => {
            print(&st);
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            print(&Status::from_advertisement(dev.info, e.to_string()));
            Ok(ExitCode::from(EXIT_FALLBACK))
        }
    }
}

async fn cmd_set(s: &Settings, what: SetCmd, json: bool) -> Result<ExitCode> {
    let setting = what.to_setting();
    // Writes never fall back to advertisement data: without a session there is nothing to change.
    let dev = ble::find_device(s.timeout, s.serial.as_deref()).await?;
    let (serial, user_id) = flow::credentials(&dev.info, s).map_err(|reason| anyhow::anyhow!(reason))?;
    let report = flow::set_with_retries(&dev, &serial, &user_id, s.timeout, setting).await?;
    for w in &report.warnings {
        eprintln!("warning: {w}");
    }
    if json {
        println!("{}", render_set_json(&report));
    } else {
        print!("{}", render_set_text(&report));
    }
    Ok(ExitCode::from(report.exit_code()))
}

#[tokio::main]
async fn main() -> ExitCode {
    // clap exits with 2 on usage errors, which would clash with our fallback code; use 1 instead.
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            let code = u8::from(e.use_stderr());
            let _ = e.print();
            return ExitCode::from(code);
        }
    };
    if !cli.tui {
        init_logging(cli.verbose);
    }
    match run(cli).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<ExitCode> {
    match &cli.command {
        Some(Cmd::DecodePacket { hex }) => cmd_decode(hex)?,
        Some(Cmd::Config) => cmd_config(&settings(&cli)?),
        Some(Cmd::Scan) => cmd_scan(&settings(&cli)?).await?,
        Some(Cmd::Set { what }) => return cmd_set(&settings(&cli)?, *what, cli.json).await,
        None if cli.tui => ecflow_bt::tui::run(settings(&cli)?).await?,
        None => return one_shot(&settings(&cli)?, cli.json).await,
    }
    Ok(ExitCode::SUCCESS)
}
