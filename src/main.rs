use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use ecflow_bt::ble::{self, BleDevice};
use ecflow_bt::config::{self, Overrides, Settings};
use ecflow_bt::flow;
use ecflow_bt::output::{render_json, render_text};
use ecflow_bt::pb::DisplayPropertyUpload;
use ecflow_bt::protocol::packet::Packet;
use ecflow_bt::status::Status;
use prost::Message;
use std::process::ExitCode;

/// Exit codes: 0 = live GATT data, 1 = error, 2 = fallback (advertisement data only).
const EXIT_FALLBACK: u8 = 2;

#[derive(Parser)]
#[command(
    version,
    about = "Read battery and charging status from an EcoFlow River 3 / River 3 Plus over Bluetooth LE"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Cmd>,

    /// EcoFlow account user ID (also EF_USER_ID or `user_id` in config.toml)
    #[arg(long, global = true)]
    user_id: Option<String>,
    /// Device serial number (also EF_SERIAL or `serial`); selects the device and is used if the advertisement lacks it
    #[arg(long, global = true)]
    serial: Option<String>,
    /// Seconds to wait per stage (scan, connect, handshake, first data)
    #[arg(long, global = true)]
    timeout: Option<u64>,
    /// Print JSON instead of text
    #[arg(long)]
    json: bool,
    /// Open the live dashboard
    #[arg(long)]
    tui: bool,
    /// Increase log detail on stderr (-v info, -vv debug, -vvv trace)
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show the resolved settings (user ID masked) and the config file path
    Config,
    /// List supported devices seen over Bluetooth
    Scan,
    /// Decode a captured, decrypted inner packet given as hex
    DecodePacket {
        /// Hex bytes, e.g. "aa 03 ..." (spaces and colons allowed)
        hex: String,
    },
}

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

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
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
        None if cli.tui => ecflow_bt::tui::run(settings(&cli)?).await?,
        None => return one_shot(&settings(&cli)?, cli.json).await,
    }
    Ok(ExitCode::SUCCESS)
}
