//! Read-only live dashboard (ratatui). A worker task keeps the BLE session alive and
//! sends [`Update`]s; the UI thread only renders state (see [`view`]).

pub mod view;

use crate::ble::{self, BleTransport};
use crate::config::Settings;
use crate::flow;
use crate::session::{Session, SessionError};
use crate::status::Status;
use anyhow::Result;
use btleplug::platform::Peripheral;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{Notify, mpsc};
pub use view::{Conn, Update, ViewState, render};

type SharedPeripheral = Arc<Mutex<Option<Peripheral>>>;

async fn park() {
    std::future::pending::<()>().await;
}

/// Keeps a session alive: scan, connect, stream statuses, reconnect with backoff.
async fn worker(settings: Settings, tx: mpsc::UnboundedSender<Update>, current: SharedPeripheral) {
    let send = |u: Update| {
        let _ = tx.send(u);
    };
    let mut failures: u32 = 0;
    loop {
        send(Update::Conn(Conn::Scanning));
        let dev = match ble::find_device(settings.timeout, settings.serial.as_deref()).await {
            Ok(d) => d,
            Err(e) => {
                failures += 1;
                let retry = u64::from(failures.min(5)) * 2;
                send(Update::Conn(Conn::Reconnecting {
                    reason: e.to_string(),
                    retry_secs: retry,
                }));
                tokio::time::sleep(Duration::from_secs(retry)).await;
                continue;
            }
        };
        *current.lock().unwrap() = Some(dev.peripheral.clone());

        let (serial, user_id) = match flow::credentials(&dev.info, &settings) {
            Ok(c) => c,
            Err(reason) => {
                send(Update::Status(Box::new(Status::from_advertisement(dev.info, &reason))));
                send(Update::Conn(Conn::Fallback(reason)));
                park().await;
                return;
            }
        };

        send(Update::Conn(Conn::Connecting(failures + 1)));
        let result: Result<(), SessionError> = async {
            let transport = BleTransport::open(&dev.peripheral, settings.timeout).await?;
            let mut session =
                Session::establish(transport, dev.info.clone(), &serial, &user_id, settings.timeout).await?;
            send(Update::Conn(Conn::Connected));
            failures = 0;
            loop {
                send(Update::Status(Box::new(session.next_status().await?)));
            }
        }
        .await;
        ble::disconnect(&dev.peripheral).await;

        let Err(e) = result else { continue };
        if e.is_permanent() {
            send(Update::Status(Box::new(Status::from_advertisement(
                dev.info,
                e.to_string(),
            ))));
            send(Update::Conn(Conn::Fallback(e.to_string())));
            park().await;
            return;
        }
        failures += 1;
        let retry = u64::from(failures.min(5)) * 2;
        send(Update::Conn(Conn::Reconnecting {
            reason: e.to_string(),
            retry_secs: retry,
        }));
        tokio::time::sleep(Duration::from_secs(retry)).await;
    }
}

enum Input {
    Quit,
}

fn spawn_input_thread(tx: mpsc::UnboundedSender<Input>) {
    std::thread::spawn(move || {
        loop {
            if tx.is_closed() {
                return;
            }
            if !event::poll(Duration::from_millis(200)).unwrap_or(false) {
                continue;
            }
            if let Ok(Event::Key(k)) = event::read()
                && k.kind == KeyEventKind::Press
            {
                let quit = matches!(k.code, KeyCode::Char('q') | KeyCode::Esc)
                    || (k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL));
                if quit && tx.send(Input::Quit).is_err() {
                    return;
                }
            }
        }
    });
}

pub async fn run(settings: Settings) -> Result<()> {
    let (utx, mut urx) = mpsc::unbounded_channel();
    let (itx, mut irx) = mpsc::unbounded_channel();
    let current: SharedPeripheral = Arc::default();
    let stop = Arc::new(Notify::new());

    let handle = {
        let (current, stop) = (current.clone(), stop.clone());
        tokio::spawn(async move {
            tokio::select! {
                () = worker(settings, utx, current) => {},
                () = stop.notified() => {},
            }
        })
    };

    // `init` installs a panic hook that restores the terminal.
    let mut terminal = ratatui::init();
    spawn_input_thread(itx);
    let mut view = ViewState::new();
    let mut tick = tokio::time::interval(Duration::from_millis(250));

    let result: Result<()> = async {
        loop {
            terminal.draw(|f| render(f, &view, Instant::now(), chrono::Local::now()))?;
            tokio::select! {
                Some(u) = urx.recv() => view.apply(u, Instant::now()),
                Some(Input::Quit) = irx.recv() => return Ok(()),
                _ = tick.tick() => {},
            }
        }
    }
    .await;
    ratatui::restore();

    stop.notify_one();
    let _ = handle.await;
    let peripheral = current.lock().unwrap().take();
    if let Some(p) = peripheral {
        ble::disconnect(&p).await;
    }
    result
}
