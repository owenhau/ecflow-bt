//! Keyboard state machine. Pure (no terminal, no I/O) so every key path is unit-testable.

use super::view::Toast;
use crate::setting::{Setting, SettingKind};
use crate::status::{LedMode, Status};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

const LIMIT_MAX: u32 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Dashboard only.
    #[default]
    Normal,
    /// Settings overlay open, selecting a row.
    Settings,
    /// Editing a numeric limit (see [`UiState::edit`]).
    Editing,
}

/// A limit being edited in the overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub kind: SettingKind,
    /// Candidate value, always within 0..=100.
    pub value: u32,
    /// Digits typed so far (empty after arrow-key adjustments).
    pub typed: String,
    /// Why the last attempt to apply was refused.
    pub error: Option<String>,
}

#[derive(Debug, Default)]
pub struct UiState {
    pub mode: Mode,
    /// Index into [`SettingKind::ALL`].
    pub selected: usize,
    pub edit: Option<Edit>,
}

impl UiState {
    pub fn selected_kind(&self) -> SettingKind {
        SettingKind::ALL[self.selected % SettingKind::ALL.len()]
    }
}

/// What the event loop should do after a key press.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    None,
    Quit,
    /// Send this change to the device.
    Apply(Setting),
    /// Show a message without sending anything.
    Toast(Toast),
}

/// What the key handler needs to know about the live session.
#[derive(Debug, Clone, Copy)]
pub struct Context<'a> {
    pub status: Option<&'a Status>,
    /// A live GATT session exists, so writes can be sent.
    pub connected: bool,
}

/// The change Enter makes on a row, or why it cannot be made.
fn enter_action(kind: SettingKind, status: &Status) -> Action {
    let setting = match kind {
        SettingKind::Dc12v => match status.dc12v_output_on {
            Some(on) => Setting::Dc12v(!on),
            None => return Action::Toast(Toast::err("12V output state unknown; cannot toggle")),
        },
        SettingKind::AcOutput => match status.ac_output_on {
            Some(on) => Setting::AcOutput(!on),
            None => return Action::Toast(Toast::err("AC output state unknown; cannot toggle")),
        },
        SettingKind::Light => Setting::Light(status.led_mode.unwrap_or(LedMode::Off).next()),
        // Limits are edited with a value editor (see `start_edit`), not toggled.
        SettingKind::ChargeLimit | SettingKind::DischargeLimit => return Action::None,
    };
    match setting.validate(status) {
        Ok(()) => Action::Apply(setting),
        Err(e) => Action::Toast(Toast::err(e.to_string())),
    }
}

fn current_limit(kind: SettingKind, status: &Status) -> Option<u32> {
    match kind {
        SettingKind::ChargeLimit => status.charge_limit_max_percent,
        SettingKind::DischargeLimit => status.discharge_limit_min_percent,
        _ => None,
    }
}

/// Open the editor on a limit row, starting from the value the device reports.
fn start_edit(ui: &mut UiState, kind: SettingKind, status: &Status) -> Action {
    let Some(value) = current_limit(kind, status) else {
        return Action::Toast(Toast::err(format!("{} unknown; cannot edit", capitalize(kind.label()))));
    };
    ui.edit = Some(Edit {
        kind,
        value: value.min(LIMIT_MAX),
        typed: String::new(),
        error: None,
    });
    ui.mode = Mode::Editing;
    Action::None
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map_or_else(String::new, |f| f.to_uppercase().collect::<String>() + c.as_str())
}

fn handle_edit_key(ui: &mut UiState, key: KeyEvent, ctx: &Context) -> Action {
    let Some(edit) = ui.edit.as_mut() else {
        ui.mode = Mode::Settings;
        return Action::None;
    };
    let step = if key.modifiers.contains(KeyModifiers::SHIFT) {
        5
    } else {
        1
    };
    match key.code {
        KeyCode::Esc => {
            ui.edit = None;
            ui.mode = Mode::Settings;
        }
        KeyCode::Left | KeyCode::Down => {
            edit.typed.clear();
            edit.error = None;
            edit.value = edit.value.saturating_sub(step);
        }
        KeyCode::Right | KeyCode::Up => {
            edit.typed.clear();
            edit.error = None;
            edit.value = (edit.value + step).min(LIMIT_MAX);
        }
        KeyCode::Char(c) if c.is_ascii_digit() => {
            if edit.typed.len() < 3 {
                edit.typed.push(c);
            }
            edit.error = None;
            edit.value = edit.typed.parse::<u32>().unwrap_or(0).min(LIMIT_MAX);
        }
        KeyCode::Backspace => {
            edit.typed.pop();
            edit.error = None;
            if !edit.typed.is_empty() {
                edit.value = edit.typed.parse::<u32>().unwrap_or(0).min(LIMIT_MAX);
            }
        }
        KeyCode::Enter => {
            let (true, Some(status)) = (ctx.connected, ctx.status) else {
                ui.edit = None;
                ui.mode = Mode::Settings;
                return Action::Toast(Toast::err("Not connected: cannot change settings"));
            };
            let setting = match edit.kind {
                SettingKind::ChargeLimit => Setting::ChargeLimit(edit.value),
                _ => Setting::DischargeLimit(edit.value),
            };
            match setting.validate(status) {
                Ok(()) => {
                    ui.edit = None;
                    ui.mode = Mode::Settings;
                    return Action::Apply(setting);
                }
                Err(e) => edit.error = Some(e.to_string()),
            }
        }
        _ => {}
    }
    Action::None
}

pub fn handle_key(ui: &mut UiState, key: KeyEvent, ctx: &Context) -> Action {
    // Ctrl-C always quits, whatever is open.
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return Action::Quit;
    }
    let n = SettingKind::ALL.len();
    match ui.mode {
        Mode::Normal => match key.code {
            KeyCode::Char('q') | KeyCode::Esc => Action::Quit,
            KeyCode::Char('s') => {
                ui.mode = Mode::Settings;
                Action::None
            }
            _ => Action::None,
        },
        Mode::Settings => {
            match key.code {
                KeyCode::Esc | KeyCode::Char('s') => ui.mode = Mode::Normal,
                KeyCode::Up | KeyCode::Char('k') => ui.selected = (ui.selected + n - 1) % n,
                KeyCode::Down | KeyCode::Char('j') => ui.selected = (ui.selected + 1) % n,
                KeyCode::Char('q') => return Action::Quit,
                KeyCode::Enter => {
                    let (true, Some(status)) = (ctx.connected, ctx.status) else {
                        return Action::Toast(Toast::err("Not connected: cannot change settings"));
                    };
                    let kind = ui.selected_kind();
                    if matches!(kind, SettingKind::ChargeLimit | SettingKind::DischargeLimit) {
                        return start_edit(ui, kind, status);
                    }
                    return enter_action(kind, status);
                }
                _ => {}
            }
            Action::None
        }
        Mode::Editing => handle_edit_key(ui, key, ctx),
    }
}

#[cfg(test)]
pub(crate) fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pb::DisplayPropertyUpload;
    use crate::status::test_device;

    fn status(p: DisplayPropertyUpload) -> Status {
        Status::from_props(
            test_device(),
            &DisplayPropertyUpload {
                cms_batt_soc: Some(50.0),
                ..p
            },
        )
    }

    fn full_status() -> Status {
        status(DisplayPropertyUpload {
            cms_max_chg_soc: Some(85),
            cms_min_dsg_soc: Some(15),
            led_mode: Some(1),
            flow_info_12v: Some(0),
            flow_info_ac_out: Some(2),
            ..Default::default()
        })
    }

    fn press_with(ui: &mut UiState, code: KeyCode, status: Option<&Status>, connected: bool) -> Action {
        handle_key(ui, key(code), &Context { status, connected })
    }

    fn press(ui: &mut UiState, code: KeyCode) -> Action {
        press_with(ui, code, None, false)
    }

    fn enter_on(kind: SettingKind, status: Option<&Status>, connected: bool) -> Action {
        let selected = SettingKind::ALL.iter().position(|k| *k == kind).unwrap();
        let mut ui = UiState {
            mode: Mode::Settings,
            selected,
            ..Default::default()
        };
        press_with(&mut ui, KeyCode::Enter, status, connected)
    }

    #[test]
    fn normal_mode_quits_on_q_and_esc() {
        for code in [KeyCode::Char('q'), KeyCode::Esc] {
            let mut ui = UiState::default();
            assert_eq!(press(&mut ui, code), Action::Quit);
        }
    }

    #[test]
    fn ctrl_c_quits_from_every_mode() {
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let ctx = Context {
            status: None,
            connected: false,
        };
        for mode in [Mode::Normal, Mode::Settings] {
            let mut ui = UiState {
                mode,
                ..Default::default()
            };
            assert_eq!(handle_key(&mut ui, ctrl_c, &ctx), Action::Quit, "{mode:?}");
        }
    }

    #[test]
    fn s_opens_and_closes_the_overlay_and_esc_does_not_quit_it() {
        let mut ui = UiState::default();
        assert_eq!(press(&mut ui, KeyCode::Char('s')), Action::None);
        assert_eq!(ui.mode, Mode::Settings);
        assert_eq!(press(&mut ui, KeyCode::Esc), Action::None);
        assert_eq!(ui.mode, Mode::Normal);
        press(&mut ui, KeyCode::Char('s'));
        press(&mut ui, KeyCode::Char('s'));
        assert_eq!(ui.mode, Mode::Normal);
    }

    #[test]
    fn selection_moves_and_wraps() {
        let mut ui = UiState {
            mode: Mode::Settings,
            selected: 0,
            ..Default::default()
        };
        press(&mut ui, KeyCode::Up);
        assert_eq!(ui.selected, SettingKind::ALL.len() - 1); // wraps to the last row
        press(&mut ui, KeyCode::Down);
        assert_eq!(ui.selected, 0);
        press(&mut ui, KeyCode::Char('j'));
        press(&mut ui, KeyCode::Down);
        assert_eq!(ui.selected_kind(), SettingKind::Light);
        press(&mut ui, KeyCode::Char('k'));
        assert_eq!(ui.selected_kind(), SettingKind::DischargeLimit);
    }

    #[test]
    fn unrelated_keys_are_ignored() {
        let mut ui = UiState {
            mode: Mode::Settings,
            selected: 2,
            ..Default::default()
        };
        for code in [KeyCode::Char('x'), KeyCode::Tab, KeyCode::Left] {
            assert_eq!(press(&mut ui, code), Action::None);
        }
        assert_eq!((ui.mode, ui.selected), (Mode::Settings, 2));
    }

    #[test]
    fn q_still_quits_from_the_overlay() {
        let mut ui = UiState {
            mode: Mode::Settings,
            selected: 0,
            ..Default::default()
        };
        assert_eq!(press(&mut ui, KeyCode::Char('q')), Action::Quit);
    }

    #[test]
    fn enter_toggles_the_ports_from_their_current_state() {
        let st = full_status(); // 12V off, AC on
        assert_eq!(
            enter_on(SettingKind::Dc12v, Some(&st), true),
            Action::Apply(Setting::Dc12v(true))
        );
        assert_eq!(
            enter_on(SettingKind::AcOutput, Some(&st), true),
            Action::Apply(Setting::AcOutput(false))
        );
    }

    #[test]
    fn enter_on_light_cycles_through_the_modes() {
        let st = full_status(); // dim
        assert_eq!(
            enter_on(SettingKind::Light, Some(&st), true),
            Action::Apply(Setting::Light(LedMode::Bright))
        );
        let none = status(DisplayPropertyUpload::default());
        assert_eq!(
            enter_on(SettingKind::Light, Some(&none), true),
            Action::Apply(Setting::Light(LedMode::Dim))
        );
        let sos = status(DisplayPropertyUpload {
            led_mode: Some(3),
            ..Default::default()
        });
        assert_eq!(
            enter_on(SettingKind::Light, Some(&sos), true),
            Action::Apply(Setting::Light(LedMode::Off))
        );
    }

    #[test]
    fn enter_when_state_unknown_explains_instead_of_guessing() {
        let none = status(DisplayPropertyUpload::default());
        for kind in [SettingKind::Dc12v, SettingKind::AcOutput] {
            match enter_on(kind, Some(&none), true) {
                Action::Toast(t) => assert!(!t.ok && t.text.contains("unknown"), "{t:?}"),
                other => panic!("{kind:?}: {other:?}"),
            }
        }
    }

    #[test]
    fn enter_without_a_session_never_writes() {
        let st = full_status();
        for kind in SettingKind::ALL {
            for (status, connected) in [(Some(&st), false), (None, true), (None, false)] {
                match enter_on(kind, status, connected) {
                    Action::Toast(t) => assert!(!t.ok && t.text.contains("Not connected"), "{kind:?}: {t:?}"),
                    other => panic!("{kind:?}: {other:?}"),
                }
            }
        }
    }

    // ---- limit editor

    fn editing(kind: SettingKind, st: &Status) -> UiState {
        let mut ui = UiState {
            mode: Mode::Settings,
            selected: SettingKind::ALL.iter().position(|k| *k == kind).unwrap(),
            ..Default::default()
        };
        assert_eq!(press_with(&mut ui, KeyCode::Enter, Some(st), true), Action::None);
        assert_eq!(ui.mode, Mode::Editing);
        ui
    }

    fn edit_key(ui: &mut UiState, code: KeyCode, st: &Status) -> Action {
        press_with(ui, code, Some(st), true)
    }

    fn value(ui: &UiState) -> u32 {
        ui.edit.as_ref().unwrap().value
    }

    #[test]
    fn enter_on_a_limit_opens_the_editor_at_the_current_value() {
        let st = full_status(); // charge 85, discharge 15
        let ui = editing(SettingKind::ChargeLimit, &st);
        assert_eq!(
            (ui.edit.as_ref().unwrap().kind, value(&ui)),
            (SettingKind::ChargeLimit, 85)
        );
        let ui = editing(SettingKind::DischargeLimit, &st);
        assert_eq!(value(&ui), 15);
    }

    #[test]
    fn esc_cancels_without_sending() {
        let st = full_status();
        let mut ui = editing(SettingKind::DischargeLimit, &st);
        edit_key(&mut ui, KeyCode::Right, &st);
        assert_eq!(edit_key(&mut ui, KeyCode::Esc, &st), Action::None);
        assert_eq!((ui.mode, ui.edit.is_none()), (Mode::Settings, true));
    }

    #[test]
    fn arrows_adjust_by_one_and_shift_by_five_and_clamp() {
        let st = full_status();
        let mut ui = editing(SettingKind::DischargeLimit, &st); // 15
        edit_key(&mut ui, KeyCode::Right, &st);
        assert_eq!(value(&ui), 16);
        edit_key(&mut ui, KeyCode::Left, &st);
        edit_key(&mut ui, KeyCode::Left, &st);
        assert_eq!(value(&ui), 14);
        let shift = |code| KeyEvent::new(code, KeyModifiers::SHIFT);
        let ctx = Context {
            status: Some(&st),
            connected: true,
        };
        handle_key(&mut ui, shift(KeyCode::Right), &ctx);
        assert_eq!(value(&ui), 19);
        for _ in 0..10 {
            handle_key(&mut ui, shift(KeyCode::Left), &ctx);
        }
        assert_eq!(value(&ui), 0); // clamps at 0
        for _ in 0..30 {
            handle_key(&mut ui, shift(KeyCode::Up), &ctx);
        }
        assert_eq!(value(&ui), 100); // clamps at 100
    }

    #[test]
    fn typing_digits_sets_the_value_and_clamps() {
        let st = full_status();
        let mut ui = editing(SettingKind::DischargeLimit, &st);
        for c in ['2', '0'] {
            edit_key(&mut ui, KeyCode::Char(c), &st);
        }
        assert_eq!(value(&ui), 20);
        edit_key(&mut ui, KeyCode::Backspace, &st);
        assert_eq!(value(&ui), 2);
        for c in ['5', '0', '9'] {
            edit_key(&mut ui, KeyCode::Char(c), &st);
        }
        assert_eq!(ui.edit.as_ref().unwrap().typed, "250"); // at most three digits
        assert_eq!(value(&ui), 100); // 250 clamps to 100
        edit_key(&mut ui, KeyCode::Char('x'), &st); // non-digits are ignored
        assert_eq!(value(&ui), 100);
        // Arrows start from the current value and forget typed digits.
        edit_key(&mut ui, KeyCode::Left, &st);
        assert_eq!((value(&ui), ui.edit.as_ref().unwrap().typed.as_str()), (99, ""));
    }

    #[test]
    fn enter_applies_a_valid_limit_and_returns_to_the_list() {
        let st = full_status();
        let mut ui = editing(SettingKind::DischargeLimit, &st);
        for c in ['2', '0'] {
            edit_key(&mut ui, KeyCode::Char(c), &st);
        }
        assert_eq!(
            edit_key(&mut ui, KeyCode::Enter, &st),
            Action::Apply(Setting::DischargeLimit(20))
        );
        assert_eq!((ui.mode, ui.edit.is_none()), (Mode::Settings, true));

        let mut ui = editing(SettingKind::ChargeLimit, &st);
        edit_key(&mut ui, KeyCode::Left, &st);
        assert_eq!(
            edit_key(&mut ui, KeyCode::Enter, &st),
            Action::Apply(Setting::ChargeLimit(84))
        );
    }

    #[test]
    fn conflicting_limit_stays_in_the_editor_with_an_inline_error() {
        let st = full_status(); // charge limit 85
        let mut ui = editing(SettingKind::DischargeLimit, &st);
        for c in ['9', '0'] {
            edit_key(&mut ui, KeyCode::Char(c), &st);
        }
        assert_eq!(edit_key(&mut ui, KeyCode::Enter, &st), Action::None);
        assert_eq!(ui.mode, Mode::Editing);
        let err = ui.edit.as_ref().unwrap().error.clone().unwrap();
        assert!(
            err.contains("above the charge limit (85%)") && err.contains("raise the charge limit first"),
            "{err}"
        );
        // Editing again clears the message; a valid value then goes through.
        edit_key(&mut ui, KeyCode::Down, &st);
        assert!(ui.edit.as_ref().unwrap().error.is_none());
        for _ in 0..10 {
            edit_key(&mut ui, KeyCode::Down, &st);
        }
        assert_eq!(
            edit_key(&mut ui, KeyCode::Enter, &st),
            Action::Apply(Setting::DischargeLimit(79))
        );
    }

    #[test]
    fn editor_needs_a_known_limit_and_a_live_session() {
        let unknown = status(DisplayPropertyUpload::default());
        for kind in [SettingKind::ChargeLimit, SettingKind::DischargeLimit] {
            match enter_on(kind, Some(&unknown), true) {
                Action::Toast(t) => assert!(!t.ok && t.text.contains("unknown"), "{t:?}"),
                other => panic!("{kind:?}: {other:?}"),
            }
            assert!(
                matches!(enter_on(kind, Some(&full_status()), false), Action::Toast(t) if t.text.contains("Not connected"))
            );
        }
        // The session dropping while editing: Enter refuses to send and leaves the editor.
        let st = full_status();
        let mut ui = editing(SettingKind::ChargeLimit, &st);
        match press_with(&mut ui, KeyCode::Enter, Some(&st), false) {
            Action::Toast(t) => assert!(!t.ok && t.text.contains("Not connected"), "{t:?}"),
            other => panic!("{other:?}"),
        }
        assert_eq!((ui.mode, ui.edit.is_none()), (Mode::Settings, true));
    }

    #[test]
    fn q_does_not_quit_while_editing_but_ctrl_c_does() {
        let st = full_status();
        let mut ui = editing(SettingKind::ChargeLimit, &st);
        assert_eq!(edit_key(&mut ui, KeyCode::Char('q'), &st), Action::None);
        assert_eq!(ui.mode, Mode::Editing);
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(
            handle_key(
                &mut ui,
                ctrl_c,
                &Context {
                    status: Some(&st),
                    connected: true
                }
            ),
            Action::Quit
        );
    }
}
