//! Live dashboard (ratatui). A worker task keeps the BLE session alive, sends [`Update`]s and
//! executes [`Command`]s (setting changes); the UI thread only renders state (see [`view`]).

pub mod input;
pub mod view;

use crate::ble::{self, BleTransport};
use crate::config::Settings;
use crate::flow;
use crate::session::{Session, SessionError, Transport};
use crate::setting::Setting;
use crate::status::Status;
use anyhow::Result;
use btleplug::platform::Peripheral;
use crossterm::event::{self, Event, KeyEvent, KeyEventKind};
use input::Action;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{Notify, mpsc};
pub use view::{Conn, Toast, Update, ViewState, render};

type SharedPeripheral = Arc<Mutex<Option<Peripheral>>>;

async fn park() {
    std::future::pending::<()>().await;
}

/// Request from the UI to the worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Apply(Setting),
}

struct Pending {
    setting: Setting,
    deadline: tokio::time::Instant,
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map_or_else(String::new, |f| f.to_uppercase().collect::<String>() + c.as_str())
}

/// "Light set to dim"
pub fn applied_text(s: Setting) -> String {
    format!("{} set to {}", capitalize(s.kind().label()), s.value_text())
}

/// Serve one live session: forward status pushes to the UI and carry out [`Command`]s.
///
/// A write is confirmed when a later status push reports the new value. Returns the error that
/// ended the session (the caller reconnects), or `Ok(())` when the UI has gone away.
pub async fn run_connected<T: Transport>(
    session: &mut Session<T>,
    cmd_rx: &mut mpsc::UnboundedReceiver<Command>,
    tx: &mpsc::UnboundedSender<Update>,
    confirm_timeout: Duration,
) -> Result<(), SessionError> {
    let toast = |t: Toast| {
        let _ = tx.send(Update::Toast(t));
    };
    let mut pending: Vec<Pending> = Vec::new();

    loop {
        let next_deadline = pending.iter().map(|p| p.deadline).min();
        let timeout = async {
            match next_deadline {
                Some(d) => tokio::time::sleep_until(d).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            status = session.next_status() => {
                let status = status?;
                pending.retain(|p| {
                    let done = p.setting.matches(&status);
                    if done {
                        toast(Toast::ok(applied_text(p.setting)));
                    }
                    !done
                });
                if tx.send(Update::Status(Box::new(status))).is_err() {
                    return Ok(());
                }
            }
            cmd = cmd_rx.recv() => {
                let Some(Command::Apply(setting)) = cmd else { return Ok(()) };
                let current = session.current_status();
                if let Err(e) = setting.validate(&current) {
                    toast(Toast::err(e.to_string()));
                } else if setting.matches(&current) {
                    toast(Toast::ok(format!("{} already {}", capitalize(setting.kind().label()), setting.value_text())));
                } else {
                    session.write(&setting.to_config()).await?;
                    pending.push(Pending { setting, deadline: tokio::time::Instant::now() + confirm_timeout });
                }
            }
            () = timeout => {
                let now = tokio::time::Instant::now();
                pending.retain(|p| {
                    let expired = p.deadline <= now;
                    if expired {
                        toast(Toast::err(format!(
                            "Not confirmed: {} (no update from the device after {}s)",
                            p.setting.describe(),
                            confirm_timeout.as_secs()
                        )));
                    }
                    !expired
                });
            }
        }
    }
}

/// Keeps a session alive: scan, connect, stream statuses, reconnect with backoff.
async fn worker(
    settings: Settings,
    tx: mpsc::UnboundedSender<Update>,
    mut cmd_rx: mpsc::UnboundedReceiver<Command>,
    current: SharedPeripheral,
) {
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
            // Commands queued while connecting are stale (the UI only sends when connected).
            while cmd_rx.try_recv().is_ok() {}
            send(Update::Conn(Conn::Connected));
            failures = 0;
            run_connected(&mut session, &mut cmd_rx, &tx, settings.timeout).await
        }
        .await;
        ble::disconnect(&dev.peripheral).await;

        let Err(e) = result else { return }; // UI closed
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
    Key(KeyEvent),
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
                && tx.send(Input::Key(k)).is_err()
            {
                return;
            }
        }
    });
}

pub async fn run(settings: Settings) -> Result<()> {
    let (utx, mut urx) = mpsc::unbounded_channel();
    let (itx, mut irx) = mpsc::unbounded_channel();
    let (ctx_tx, crx) = mpsc::unbounded_channel();
    let current: SharedPeripheral = Arc::default();
    let stop = Arc::new(Notify::new());

    let handle = {
        let (current, stop) = (current.clone(), stop.clone());
        tokio::spawn(async move {
            tokio::select! {
                () = worker(settings, utx, crx, current) => {},
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
                Some(Input::Key(k)) = irx.recv() => {
                    let ctx = input::Context { status: view.status.as_ref(), connected: view.connected() };
                    match input::handle_key(&mut view.ui, k, &ctx) {
                        Action::None => {}
                        Action::Quit => return Ok(()),
                        Action::Apply(setting) => {
                            let _ = ctx_tx.send(Command::Apply(setting));
                        }
                        Action::Toast(t) => view.set_toast(t, Instant::now()),
                    }
                }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pb::DisplayPropertyUpload;
    use crate::status::{LedMode, test_device};
    use crate::testutil::{FakeDevice, FakeTransport, LogHandle, WriteBehavior};

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

    struct Run {
        result: Option<Result<(), SessionError>>,
        updates: Vec<Update>,
        log: LogHandle,
    }

    impl Run {
        fn toasts(&self) -> Vec<Toast> {
            self.updates
                .iter()
                .filter_map(|u| {
                    if let Update::Toast(t) = u {
                        Some(t.clone())
                    } else {
                        None
                    }
                })
                .collect()
        }
        fn last_status(&self) -> Option<&Status> {
            self.updates
                .iter()
                .rev()
                .find_map(|u| if let Update::Status(s) = u { Some(&**s) } else { None })
        }
    }

    /// Establish a session against the simulated device, queue `cmds`, and run the connected loop
    /// for a short while (it only ends on error, so a timeout is the normal outcome).
    async fn drive(behavior: WriteBehavior, cmds: &[Command], confirm: Duration, close_ui: bool) -> Run {
        let device = FakeDevice::new(SERIAL, USER, vec![props()])
            .with_write_behavior(behavior)
            .stay_open();
        let (transport, log) = FakeTransport::new(device);
        let mut session = Session::establish(transport, test_device(), SERIAL, USER, Duration::from_millis(500))
            .await
            .unwrap();
        session.read_status(Duration::from_millis(30)).await.unwrap();

        let (ctx, mut crx) = mpsc::unbounded_channel();
        let (utx, mut urx) = mpsc::unbounded_channel();
        for c in cmds {
            ctx.send(*c).unwrap();
        }
        if close_ui {
            drop(ctx);
        }
        let result = tokio::time::timeout(
            Duration::from_millis(400),
            run_connected(&mut session, &mut crx, &utx, confirm),
        )
        .await
        .ok();
        let mut updates = Vec::new();
        while let Ok(u) = urx.try_recv() {
            updates.push(u);
        }
        Run { result, updates, log }
    }

    #[tokio::test]
    async fn toggle_is_written_confirmed_and_announced() {
        let r = drive(
            WriteBehavior::Apply,
            &[Command::Apply(Setting::Dc12v(true))],
            Duration::from_secs(1),
            false,
        )
        .await;
        assert!(r.result.is_none(), "session should still be running");
        assert_eq!(r.toasts(), vec![Toast::ok("12V output set to on")]);
        assert_eq!(r.last_status().unwrap().dc12v_output_on, Some(true));
        assert_eq!(r.log.lock().unwrap().writes.len(), 1);
    }

    #[tokio::test]
    async fn light_change_is_confirmed() {
        let r = drive(
            WriteBehavior::Apply,
            &[Command::Apply(Setting::Light(LedMode::Dim))],
            Duration::from_secs(1),
            false,
        )
        .await;
        assert_eq!(r.toasts(), vec![Toast::ok("Light set to dim")]);
        assert_eq!(r.last_status().unwrap().led_mode, Some(LedMode::Dim));
    }

    #[tokio::test]
    async fn several_commands_are_each_confirmed() {
        let cmds = [
            Command::Apply(Setting::AcOutput(true)),
            Command::Apply(Setting::DischargeLimit(25)),
        ];
        let r = drive(WriteBehavior::Apply, &cmds, Duration::from_secs(1), false).await;
        let texts: Vec<String> = r.toasts().into_iter().map(|t| t.text).collect();
        assert_eq!(texts.len(), 2, "{texts:?}");
        assert!(texts.contains(&"AC output set to on".to_string()), "{texts:?}");
        assert!(texts.contains(&"Discharge limit set to 25%".to_string()), "{texts:?}");
        assert_eq!(r.log.lock().unwrap().writes.len(), 2);
    }

    #[tokio::test]
    async fn invalid_change_toasts_and_sends_nothing() {
        let r = drive(
            WriteBehavior::Apply,
            &[Command::Apply(Setting::DischargeLimit(95))],
            Duration::from_secs(1),
            false,
        )
        .await;
        let t = r.toasts();
        assert_eq!(t.len(), 1);
        assert!(!t[0].ok && t[0].text.contains("above the charge limit"), "{t:?}");
        assert!(r.log.lock().unwrap().writes.is_empty());
    }

    #[tokio::test]
    async fn setting_the_current_value_sends_nothing() {
        let r = drive(
            WriteBehavior::Apply,
            &[Command::Apply(Setting::ChargeLimit(85))],
            Duration::from_secs(1),
            false,
        )
        .await;
        assert_eq!(r.toasts(), vec![Toast::ok("Charge limit already 85%")]);
        assert!(r.log.lock().unwrap().writes.is_empty());
    }

    #[tokio::test]
    async fn ignored_write_reports_not_confirmed() {
        let r = drive(
            WriteBehavior::IgnoreAndStall,
            &[Command::Apply(Setting::Light(LedMode::Bright))],
            Duration::from_millis(100),
            false,
        )
        .await;
        let t = r.toasts();
        assert_eq!(t.len(), 1, "{t:?}");
        assert!(
            !t[0].ok && t[0].text.starts_with("Not confirmed: light -> bright"),
            "{t:?}"
        );
        assert_eq!(r.log.lock().unwrap().writes.len(), 1);
    }

    #[tokio::test]
    async fn dropped_link_ends_the_session_with_an_error() {
        let r = drive(
            WriteBehavior::DropLink,
            &[Command::Apply(Setting::Dc12v(true))],
            Duration::from_secs(1),
            false,
        )
        .await;
        assert!(
            matches!(r.result, Some(Err(SessionError::LinkClosed))),
            "{:?}",
            r.result
        );
    }

    #[tokio::test]
    async fn closing_the_ui_ends_the_loop_cleanly() {
        let r = drive(WriteBehavior::Apply, &[], Duration::from_secs(1), true).await;
        assert!(matches!(r.result, Some(Ok(()))), "{:?}", r.result);
    }

    #[test]
    fn announcement_texts() {
        assert_eq!(applied_text(Setting::ChargeLimit(80)), "Charge limit set to 80%");
        assert_eq!(applied_text(Setting::AcOutput(false)), "AC output set to off");
        assert_eq!(capitalize(""), "");
    }
}
