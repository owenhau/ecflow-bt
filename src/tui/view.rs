//! Pure rendering: `ViewState` + clock -> widgets. No I/O, so it is testable with `TestBackend`.

use crate::status::{ChargeState, LedMode, LimitTimer, MAX_PLAUSIBLE_MINUTES, Source, Status};
use chrono::{DateTime, Local};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Sparkline, Wrap};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

pub const HISTORY_LEN: usize = 120;
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
const STALE_AFTER: Duration = Duration::from_secs(15);
/// Minimum width for the power-flow and details panels to sit side by side.
const SIDE_BY_SIDE_MIN_WIDTH: u16 = 72;
const FLOW_HEIGHT: u16 = 13;
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const PARTIAL_BLOCKS: [char; 7] = ['▏', '▎', '▍', '▌', '▋', '▊', '▉'];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Conn {
    Scanning,
    Connecting(u32),
    Connected,
    /// Link lost or failed; retrying in `retry_secs`.
    Reconnecting {
        reason: String,
        retry_secs: u64,
    },
    /// GATT is not possible (e.g. wrong user ID); showing advertisement data only.
    Fallback(String),
}

#[derive(Debug)]
pub enum Update {
    Conn(Conn),
    Status(Box<Status>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub input: f64,
    pub output: f64,
}

#[derive(Debug)]
pub struct ViewState {
    pub conn: Conn,
    pub status: Option<Status>,
    pub last_update: Option<Instant>,
    pub history: VecDeque<Sample>,
    last_sample: Option<Instant>,
}

impl ViewState {
    pub fn new() -> Self {
        Self {
            conn: Conn::Scanning,
            status: None,
            last_update: None,
            history: VecDeque::with_capacity(HISTORY_LEN),
            last_sample: None,
        }
    }

    pub fn apply(&mut self, u: Update, now: Instant) {
        match u {
            Update::Conn(c) => self.conn = c,
            Update::Status(s) => {
                if s.source == Source::Gatt
                    && self
                        .last_sample
                        .is_none_or(|t| now.duration_since(t) >= SAMPLE_INTERVAL)
                {
                    if self.history.len() == HISTORY_LEN {
                        self.history.pop_front();
                    }
                    self.history.push_back(Sample {
                        input: s.input_watts.unwrap_or(0.0).max(0.0),
                        output: s.output_watts.unwrap_or(0.0).abs(),
                    });
                    self.last_sample = Some(now);
                }
                self.status = Some(*s);
                self.last_update = Some(now);
            }
        }
    }
}

impl Default for ViewState {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------- formatting helpers

/// `13.7 W` (one decimal below 100 W), `150 W`, `1.25 kW`; `n/a` when unknown.
pub fn fmt_watts(v: Option<f64>) -> String {
    match v {
        None => "n/a".into(),
        Some(w) if w.abs() >= 1000.0 => format!("{:.2} kW", w / 1000.0),
        Some(w) if w.abs() < 100.0 && w != 0.0 => format!("{w:.1} W"),
        Some(w) => format!("{w:.0} W"),
    }
}

/// `7h 57m`, `1d 03h`.
pub fn fmt_duration(minutes: u32) -> String {
    if minutes >= 24 * 60 {
        format!("{}d {:02}h", minutes / (24 * 60), (minutes / 60) % 24)
    } else {
        format!("{}h {:02}m", minutes / 60, minutes % 60)
    }
}

/// A time estimate the device actually means (0 and huge placeholder values are not).
pub fn valid_minutes(m: Option<u32>) -> Option<u32> {
    m.filter(|&m| m > 0 && m <= MAX_PLAUSIBLE_MINUTES)
}

fn fmt_eta(wall: DateTime<Local>, minutes: u32) -> String {
    let eta = wall + chrono::Duration::minutes(i64::from(minutes));
    if eta.date_naive() == wall.date_naive() {
        eta.format("%H:%M").to_string()
    } else {
        eta.format("%a %H:%M").to_string()
    }
}

/// Smallest "nice" full-scale value (W) that fits `peak`, so bars stay readable.
pub fn nice_scale(peak: f64) -> f64 {
    [100.0, 200.0, 300.0, 600.0, 1000.0, 1500.0, 2000.0, 3000.0]
        .into_iter()
        .find(|&s| peak <= s)
        .unwrap_or(peak.ceil())
}

/// Horizontal bar of `width` cells filled to `ratio`, with smooth partial-block edge.
pub fn bar_cells(ratio: f64, width: usize) -> Vec<char> {
    let exact = ratio.clamp(0.0, 1.0) * width as f64;
    let full = exact.floor() as usize;
    let frac = exact - full as f64;
    let mut cells = vec!['░'; width];
    cells.iter_mut().take(full).for_each(|c| *c = '█');
    if full < width {
        let idx = (frac * 8.0).floor() as usize;
        if idx >= 1 {
            cells[full] = PARTIAL_BLOCKS[(idx - 1).min(6)];
        }
    }
    cells
}

/// Row with `▲` under each limit position and a short label next to it.
pub fn marker_row(width: usize, markers: &[(u32, String)]) -> String {
    let mut row = vec![' '; width];
    if width == 0 {
        return String::new();
    }
    let mut last_end = 0usize;
    for (pct, label) in markers {
        let x = ((f64::from(*pct) / 100.0) * width as f64) as usize;
        let x = x.min(width - 1);
        let text: Vec<char> = std::iter::once('▲').chain(label.chars()).collect();
        // Labels grow to the right, or to the left when near the right edge.
        let start = if x + text.len() <= width {
            x
        } else {
            width.saturating_sub(text.len())
        };
        if start < last_end {
            row[x] = '▲'; // no room for the label without overlapping the previous one
            continue;
        }
        let ordered: Vec<char> = if start == x {
            text
        } else {
            label.chars().chain(std::iter::once('▲')).collect()
        };
        for (i, ch) in ordered.iter().enumerate() {
            if start + i < width {
                row[start + i] = *ch;
            }
        }
        last_end = start + ordered.len() + 1;
    }
    row.into_iter().collect()
}

// ---------------------------------------------------------------- styling helpers

fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}
fn bold() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}

fn battery_color(pct: Option<f64>, state: Option<ChargeState>) -> Color {
    match (state, pct) {
        (Some(ChargeState::Charging), _) => Color::Green,
        (_, Some(p)) if p < 15.0 => Color::Red,
        (_, Some(p)) if p < 30.0 => Color::Yellow,
        _ => Color::Cyan,
    }
}

fn label(text: &str) -> Span<'static> {
    Span::styled(format!("{text:<15}"), Style::default().fg(Color::Gray))
}

fn spinner(wall: DateTime<Local>) -> &'static str {
    SPINNER[((wall.timestamp_millis() / 100).unsigned_abs() as usize) % SPINNER.len()]
}

/// Left and right spans on one line, padded to `width`.
fn justify(left: Vec<Span<'static>>, right: Vec<Span<'static>>, width: usize) -> Line<'static> {
    let used: usize = left.iter().chain(right.iter()).map(Span::width).sum();
    let mut spans = left;
    spans.push(Span::raw(" ".repeat(width.saturating_sub(used).max(1))));
    spans.extend(right);
    Line::from(spans)
}

pub fn connection_label(c: &Conn) -> String {
    match c {
        Conn::Scanning => "scanning for device…".into(),
        Conn::Connecting(n) => format!("connecting (attempt {n})…"),
        Conn::Connected => "connected".into(),
        Conn::Reconnecting { reason, retry_secs } => format!("reconnecting in {retry_secs}s: {reason}"),
        Conn::Fallback(r) => format!("advertisement only: {r}"),
    }
}

fn connection_badge(c: &Conn, wall: DateTime<Local>) -> Vec<Span<'static>> {
    let (text, color) = match c {
        Conn::Connected => ("● connected".to_string(), Color::Green),
        Conn::Scanning | Conn::Connecting(_) => (format!("{} {}", spinner(wall), connection_label(c)), Color::Yellow),
        Conn::Reconnecting { .. } => (format!("↻ {}", connection_label(c)), Color::Red),
        Conn::Fallback(_) => ("◌ advertisement only".to_string(), Color::Yellow),
    };
    vec![Span::styled(
        format!(" {text} "),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )]
}

// ---------------------------------------------------------------- panels

pub fn render(f: &mut Frame, v: &ViewState, now: Instant, wall: DateTime<Local>) {
    let [header, body, footer] =
        Layout::vertical([Constraint::Length(3), Constraint::Min(0), Constraint::Length(1)]).areas(f.area());
    render_header(f, header, v, wall);
    match &v.status {
        Some(s) if s.source == Source::Gatt => render_dashboard(f, body, v, s, wall),
        Some(s) => render_fallback(f, body, s, &v.conn),
        None => render_waiting(f, body, &v.conn, wall),
    }
    render_footer(f, footer, v, now);
}

fn render_header(f: &mut Frame, area: Rect, v: &ViewState, wall: DateTime<Local>) {
    let model = v.status.as_ref().map_or("EcoFlow River 3", |s| s.device.model.as_str());
    let block = Block::bordered()
        .border_style(Style::default().fg(Color::Cyan))
        .title(Line::from(vec![
            Span::raw(" ⚡ "),
            Span::styled(model.to_string(), bold().fg(Color::Cyan)),
            Span::raw(" "),
        ]))
        .title_top(Line::from(connection_badge(&v.conn, wall)).right_aligned());
    let inner = block.inner(area);
    f.render_widget(block, area);

    let Some(s) = &v.status else { return };
    let d = &s.device;
    let sep = Span::styled("  ·  ", dim());
    let mut spans = vec![
        Span::raw(d.serial.clone().unwrap_or_else(|| "serial n/a".into())),
        sep.clone(),
    ];
    spans.push(Span::raw(d.address.clone()));
    if let Some(n) = &d.name {
        spans.push(sep.clone());
        spans.push(Span::raw(n.clone()));
    }
    spans.push(sep);
    spans.push(Span::raw(format!(
        "signal {}",
        d.rssi.map_or("n/a".into(), |r| format!("{r} dBm"))
    )));
    f.render_widget(Paragraph::new(Line::from(spans)), inner);
}

fn render_footer(f: &mut Frame, area: Rect, v: &ViewState, now: Instant) {
    let (age_text, stale) = match v.last_update {
        None => ("never".to_string(), false),
        Some(t) => {
            let age = now.duration_since(t);
            (format!("{}s ago", age.as_secs()), age > STALE_AFTER)
        }
    };
    let age_style = if stale {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    } else {
        dim()
    };
    let line = justify(
        vec![
            Span::styled(" updated ", dim()),
            Span::styled(age_text, age_style),
            Span::styled(if stale { "  (stale)" } else { "" }, age_style),
        ],
        vec![Span::styled("q", bold()), Span::styled(" quit ", dim())],
        area.width as usize,
    );
    f.render_widget(Paragraph::new(line), area);
}

fn centered_panel(f: &mut Frame, area: Rect, title: &str, color: Color, lines: Vec<Line<'static>>) {
    f.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true })
            .block(
                Block::bordered()
                    .title(format!(" {title} "))
                    .border_style(Style::default().fg(color)),
            ),
        area,
    );
}

fn render_waiting(f: &mut Frame, area: Rect, conn: &Conn, wall: DateTime<Local>) {
    let mut lines = vec![
        Line::raw(""),
        Line::styled(
            format!("{} {}", spinner(wall), connection_label(conn)),
            Style::default().fg(Color::Yellow),
        ),
    ];
    if let Conn::Scanning = conn {
        lines.push(Line::styled(
            "Tip: close the EcoFlow phone app; the device allows one connection.",
            dim(),
        ));
    }
    centered_panel(f, area, "Waiting for data", Color::Yellow, lines);
}

fn render_fallback(f: &mut Frame, area: Rect, s: &Status, conn: &Conn) {
    let mut lines = vec![
        Line::raw(""),
        Line::styled("Battery data unavailable", bold().fg(Color::Yellow)),
        Line::raw(""),
    ];
    if let Some(n) = &s.note {
        lines.push(Line::raw(n.clone()));
    }
    if let Conn::Fallback(r) = conn {
        lines.push(Line::styled(r.clone(), dim()));
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "Check user_id / serial in ~/.config/ecflow-bt/config.toml, or run `ecflow-bt config`.",
        dim(),
    ));
    centered_panel(f, area, "Advertisement only", Color::Yellow, lines);
}

fn render_dashboard(f: &mut Frame, area: Rect, v: &ViewState, s: &Status, wall: DateTime<Local>) {
    let history_h = if area.height >= 28 { 6 } else { 0 };
    let battery_h = match area.height {
        24.. => 7,
        16.. => 6,
        _ => 4,
    };
    let [battery, middle, history] = Layout::vertical([
        Constraint::Length(battery_h),
        Constraint::Min(0),
        Constraint::Length(history_h),
    ])
    .areas(area);

    render_battery(f, battery, s, wall);
    let peak = v.history.iter().fold(0.0f64, |m, x| m.max(x.input).max(x.output));
    let scale = nice_scale(
        peak.max(s.input_watts.unwrap_or(0.0))
            .max(s.output_watts.unwrap_or(0.0)),
    );
    if middle.width >= SIDE_BY_SIDE_MIN_WIDTH {
        let [l, r] = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(middle);
        render_flow(f, l, s, scale);
        render_details(f, r, s, wall);
    } else if middle.height >= FLOW_HEIGHT + 12 {
        let [top, bottom] = Layout::vertical([Constraint::Length(FLOW_HEIGHT), Constraint::Min(0)]).areas(middle);
        render_flow(f, top, s, scale);
        render_details(f, bottom, s, wall);
    } else {
        // Not enough room for both: keep the more useful one.
        render_details(f, middle, s, wall);
    }
    if history_h > 0 {
        render_history(f, history, v);
    }
}

fn charge_headline(s: &Status) -> Vec<Span<'static>> {
    let net = s.battery_watts.or_else(|| Some(s.input_watts? - s.output_watts?.abs()));
    match s.charge_state {
        Some(ChargeState::Charging) => vec![
            Span::styled("▲ Charging", bold().fg(Color::Green)),
            Span::styled(
                format!("  +{}", fmt_watts(net.map(f64::abs))),
                Style::default().fg(Color::Green),
            ),
        ],
        Some(ChargeState::Discharging) => vec![
            Span::styled("▼ Discharging", bold().fg(Color::Yellow)),
            Span::styled(
                format!("  -{}", fmt_watts(net.map(f64::abs))),
                Style::default().fg(Color::Yellow),
            ),
        ],
        Some(ChargeState::Idle) => vec![Span::styled("● Idle", bold().fg(Color::Gray))],
        None => vec![Span::styled("? Unknown", dim())],
    }
}

/// "7h 57m left (empty ~21:14)" / "full in 1h 20m (~18:20)"; empty when unknown.
fn remaining_text(s: &Status, wall: DateTime<Local>) -> Option<String> {
    match s.charge_state {
        Some(ChargeState::Charging) => {
            let m = valid_minutes(s.minutes_to_full)?;
            Some(format!("full in {} (~{})", fmt_duration(m), fmt_eta(wall, m)))
        }
        Some(ChargeState::Discharging) => {
            let m = valid_minutes(s.minutes_to_empty)?;
            Some(format!("{} left (empty ~{})", fmt_duration(m), fmt_eta(wall, m)))
        }
        _ => None,
    }
}

/// The limit being approached and the time to reach it: `(15, "5h 07m (~19:07)")` or "reached".
fn limit_value(s: &Status, wall: DateTime<Local>) -> Option<(LimitTimer, String)> {
    let t = s.limit_timer()?;
    let value = match t.minutes {
        0 => "reached".to_string(),
        m => format!("{} (~{})", fmt_duration(m), fmt_eta(wall, m)),
    };
    Some((t, value))
}

fn limit_text(s: &Status, wall: DateTime<Local>) -> Option<String> {
    let (t, value) = limit_value(s, wall)?;
    Some(if t.minutes == 0 {
        format!("{}% limit reached", t.limit_percent)
    } else {
        format!("{}% limit in {value}", t.limit_percent)
    })
}

/// Green while charging toward the upper limit; yellow while draining toward the lower one,
/// red once the lower limit is hit.
fn limit_style(t: LimitTimer) -> Style {
    match (t.charging, t.minutes == 0) {
        (true, true) => bold().fg(Color::Green),
        (true, false) => Style::default().fg(Color::Green),
        (false, true) => bold().fg(Color::Red),
        (false, false) => Style::default().fg(Color::Yellow),
    }
}

fn render_battery(f: &mut Frame, area: Rect, s: &Status, wall: DateTime<Local>) {
    let color = battery_color(s.battery_percent, s.charge_state);
    let block = Block::bordered()
        .title(" Battery ")
        .border_style(Style::default().fg(color));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let width = inner.width as usize;

    let pct_text = s.battery_percent.map_or("n/a".to_string(), |p| format!("{p:.0}%"));
    let mut left = vec![Span::styled(pct_text, bold().fg(color)), Span::raw("   ")];
    left.extend(charge_headline(s));
    let right = remaining_text(s, wall)
        .map(|t| vec![Span::styled(t, bold())])
        .unwrap_or_default();

    let ratio = s.battery_percent.map_or(0.0, |p| p / 100.0);
    let cells = bar_cells(ratio, width);
    let limit_cells: Vec<usize> = [s.discharge_limit_min_percent, s.charge_limit_max_percent]
        .into_iter()
        .flatten()
        .filter(|&p| p > 0 && p < 100)
        .map(|p| ((f64::from(p) / 100.0 * width as f64) as usize).min(width.saturating_sub(1)))
        .collect();
    let bar_line = || {
        Line::from(
            cells
                .iter()
                .enumerate()
                .map(|(i, &c)| {
                    if limit_cells.contains(&i) {
                        let filled = c != '░';
                        let style = if filled {
                            Style::default().fg(Color::Black).bg(color)
                        } else {
                            Style::default().fg(Color::White)
                        };
                        Span::styled("┃", style)
                    } else if c == '░' {
                        Span::styled("░", dim())
                    } else {
                        Span::styled(c.to_string(), Style::default().fg(color))
                    }
                })
                .collect::<Vec<_>>(),
        )
    };

    let mut lines = vec![justify(left, right, width), bar_line()];
    if inner.height >= 4 {
        lines.push(bar_line());
    }
    if inner.height >= 3 {
        let mut markers = Vec::new();
        if let Some(p) = s.discharge_limit_min_percent.filter(|&p| p > 0 && p < 100) {
            markers.push((p, format!("{p}% min")));
        }
        if let Some(p) = s.charge_limit_max_percent.filter(|&p| p > 0 && p < 100) {
            markers.push((p, format!("{p}% max")));
        }
        lines.push(Line::styled(marker_row(width, &markers), dim()));
    }
    if inner.height >= 5
        && let Some((t, _)) = limit_value(s, wall)
        && let Some(text) = limit_text(s, wall)
    {
        lines.push(Line::from(vec![
            Span::styled("⏱ ", dim()),
            Span::styled(text, limit_style(t)),
        ]));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn flow_bar(name: &str, watts: Option<f64>, scale: f64, width: usize, color: Color) -> Line<'static> {
    let w = watts.unwrap_or(0.0).abs();
    let value = format!("{:>9}", fmt_watts(watts));
    let bar_w = width
        .saturating_sub(name.chars().count() + value.chars().count() + 3)
        .max(4);
    let cells: String = bar_cells(w / scale, bar_w).into_iter().collect();
    Line::from(vec![
        Span::styled(format!("{name} "), bold()),
        Span::styled("▕", dim()),
        Span::styled(cells, Style::default().fg(color)),
        Span::styled("▏", dim()),
        Span::styled(value, bold()),
    ])
}

fn port_row(name: &str, watts: Option<f64>, on: Option<bool>, off_text: &'static str) -> Line<'static> {
    let active = watts.is_some_and(|w| w.abs() >= 0.05);
    let style = if active {
        Style::default().fg(Color::White)
    } else {
        dim()
    };
    let mut spans = vec![
        Span::styled(
            if active { "  ● " } else { "  ○ " },
            if active {
                Style::default().fg(Color::Green)
            } else {
                dim()
            },
        ),
        Span::styled(format!("{name:<10}"), style),
        Span::styled(format!("{:>9}", fmt_watts(watts)), style),
    ];
    if on == Some(false) && !active {
        spans.push(Span::styled(format!("   {off_text}"), dim()));
    }
    Line::from(spans)
}

fn render_flow(f: &mut Frame, area: Rect, s: &Status, scale: f64) {
    let block = Block::bordered().title(format!(" Power flow (scale {}) ", fmt_watts(Some(scale))));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let w = inner.width as usize;
    let net = s.battery_watts.or_else(|| Some(s.input_watts? - s.output_watts?.abs()));
    let net_style = match net {
        Some(n) if n > 0.5 => Style::default().fg(Color::Green),
        Some(n) if n < -0.5 => Style::default().fg(Color::Yellow),
        _ => dim(),
    };
    let lines = vec![
        flow_bar("IN ", s.input_watts, scale, w, Color::Green),
        port_row("AC in", s.ac_input_watts, s.ac_plugged_in, "unplugged"),
        port_row("Solar/DC", s.solar_input_watts, None, ""),
        Line::raw(""),
        flow_bar("OUT", s.output_watts, scale, w, Color::Yellow),
        port_row("AC out", s.ac_output_watts, s.ac_output_on, "off"),
        port_row("12V DC", s.dc12v_output_watts, s.dc12v_output_on, "off"),
        port_row("USB-C", s.usbc_output_watts, None, ""),
        port_row("USB-A", s.usba_output_watts, None, ""),
        Line::raw(""),
        Line::from(vec![
            Span::styled("NET ", bold()),
            Span::styled(
                net.map_or("n/a".to_string(), |n| {
                    format!("{}{}", if n > 0.5 { "+" } else { "" }, fmt_watts(Some(n)))
                }),
                net_style.add_modifier(Modifier::BOLD),
            ),
            Span::styled("  battery power", dim()),
        ]),
    ];
    f.render_widget(Paragraph::new(lines), inner);
}

fn yes_no(v: Option<bool>, yes: &'static str, no: &'static str) -> Span<'static> {
    match v {
        Some(true) => Span::styled(yes, Style::default().fg(Color::Green)),
        Some(false) => Span::styled(no, dim()),
        None => Span::styled("n/a", dim()),
    }
}

/// Light status line: symbol + mode, SOS highlighted.
fn light_spans(mode: Option<LedMode>) -> Vec<Span<'static>> {
    match mode {
        None => vec![Span::styled("n/a", dim())],
        Some(LedMode::Off) => vec![Span::styled("○ off", dim())],
        Some(LedMode::Dim) => vec![Span::styled("◐ dim", Style::default().fg(Color::Yellow))],
        Some(m @ LedMode::Bright) => vec![Span::styled(format!("● {m}"), bold().fg(Color::Yellow))],
        Some(LedMode::Sos) => vec![Span::styled("⚠ SOS", bold().fg(Color::Red))],
        Some(m @ LedMode::Unknown(_)) => vec![Span::styled(format!("? {m}"), Style::default().fg(Color::Yellow))],
    }
}

fn render_details(f: &mut Frame, area: Rect, s: &Status, wall: DateTime<Local>) {
    let block = Block::bordered().title(" Details ");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let temp_style = match s.cell_temperature_c {
        Some(t) if t >= 45 => Style::default().fg(Color::Red),
        Some(t) if t >= 35 => Style::default().fg(Color::Yellow),
        Some(_) => Style::default().fg(Color::Green),
        None => dim(),
    };
    let limits = match (s.discharge_limit_min_percent, s.charge_limit_max_percent) {
        (Some(lo), Some(hi)) => format!("{lo}% – {hi}%"),
        _ => "n/a".into(),
    };
    let mut state_spans = vec![label("State")];
    state_spans.extend(charge_headline(s));
    let mut lines = vec![
        Line::from(state_spans),
        Line::from(vec![
            label("Time"),
            Span::raw(remaining_text(s, wall).unwrap_or_else(|| match s.charge_state {
                Some(ChargeState::Idle) => "not charging or discharging".into(),
                _ => "n/a".into(),
            })),
        ]),
        Line::from(vec![label("Battery limits"), Span::raw(limits)]),
        Line::from(vec![
            label("Temperature"),
            Span::styled(
                s.cell_temperature_c.map_or("n/a".into(), |t| format!("{t} °C")),
                temp_style,
            ),
        ]),
        Line::raw(""),
        Line::from(vec![
            label("AC charger"),
            yes_no(s.ac_plugged_in, "plugged in", "unplugged"),
        ]),
        Line::from(vec![label("AC output"), yes_no(s.ac_output_on, "on", "off")]),
        Line::from(vec![label("12V output"), yes_no(s.dc12v_output_on, "on", "off")]),
        Line::from(vec![
            label("Extra battery"),
            yes_no(s.extra_battery_attached, "attached", "none"),
        ]),
    ];
    // Only show the light row when the device reports one (River 3 Plus does).
    if s.led_mode.is_some() {
        let mut spans = vec![label("Light")];
        spans.extend(light_spans(s.led_mode));
        lines.push(Line::from(spans));
    }
    if let Some((t, value)) = limit_value(s, wall) {
        lines.insert(
            2,
            Line::from(vec![
                label(&format!("To {}% limit", t.limit_percent)),
                Span::styled(value, limit_style(t)),
            ]),
        );
    }
    if let (Some(main), Some(total)) = (s.main_battery_percent, s.battery_percent)
        && (main - total).abs() > 0.05
    {
        lines.push(Line::from(vec![
            label("Main battery"),
            Span::raw(format!("{main:.0}%")),
        ]));
    }
    lines.push(match s.error_code.filter(|&c| c != 0) {
        Some(code) => Line::from(vec![
            label("Error"),
            Span::styled(format!("code {code}"), bold().fg(Color::Red)),
        ]),
        None => Line::from(vec![
            label("Errors"),
            Span::styled("none", Style::default().fg(Color::Green)),
        ]),
    });
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn render_history(f: &mut Frame, area: Rect, v: &ViewState) {
    let block = Block::bordered().title(format!(" Power history (last {} s) ", HISTORY_LEN));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let rows = Layout::vertical([Constraint::Ratio(1, 2), Constraint::Ratio(1, 2)]).split(inner);

    let series = |pick: fn(&Sample) -> f64| -> Vec<u64> { v.history.iter().map(|s| pick(s).round() as u64).collect() };
    let peak = v.history.iter().fold(0.0f64, |m, x| m.max(x.input).max(x.output));
    let max = nice_scale(peak) as u64;
    let now_in = v.history.back().map(|s| s.input);
    let now_out = v.history.back().map(|s| s.output);

    for (row, (name, data, now, color)) in rows.iter().zip([
        ("IN ", series(|s| s.input), now_in, Color::Green),
        ("OUT", series(|s| s.output), now_out, Color::Yellow),
    ]) {
        let [text, spark] = Layout::horizontal([Constraint::Length(16), Constraint::Min(0)]).areas(*row);
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(format!("{name} "), bold().fg(color)),
                Span::raw(fmt_watts(now)),
            ])),
            text,
        );
        // Newest samples on the right; show only what fits.
        let keep = spark.width as usize;
        let data: Vec<u64> = data.iter().rev().take(keep).rev().copied().collect();
        f.render_widget(
            Sparkline::default()
                .data(data)
                .max(max)
                .style(Style::default().fg(color)),
            spark,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pb::DisplayPropertyUpload;
    use crate::status::test_device;
    use chrono::TimeZone;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn wall() -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 11, 22, 14, 0, 0).unwrap()
    }

    fn draw_sized(v: &ViewState, w: u16, h: u16) -> String {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| render(f, v, Instant::now(), wall())).unwrap();
        let buf = t.backend().buffer();
        buf.content()
            .chunks(buf.area.width as usize)
            .map(|row| row.iter().map(|c| c.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Mirrors the user's real output.json (River 3 Plus, discharging at 14 W).
    fn real_device_state() -> ViewState {
        let p = DisplayPropertyUpload {
            cms_batt_soc: Some(42.0),
            bms_batt_soc: Some(42.0),
            pow_in_sum_w: Some(0.0),
            pow_out_sum_w: Some(14.0),
            pow_get_typec1: Some(-13.74),
            cms_dsg_rem_time: Some(477),
            cms_chg_rem_time: Some(5939),
            bms_max_cell_temp: Some(21),
            cms_max_chg_soc: Some(85),
            cms_min_dsg_soc: Some(15),
            plug_in_info_ac_charger_flag: Some(false),
            flow_info_ac_out: Some(0),
            flow_info_12v: Some(0),
            ..Default::default()
        };
        let mut v = ViewState::new();
        v.apply(
            Update::Status(Box::new(Status::from_props(test_device(), &p))),
            Instant::now(),
        );
        v.apply(Update::Conn(Conn::Connected), Instant::now());
        v
    }

    #[test]
    fn dashboard_shows_real_device_values() {
        let out = draw_sized(&real_device_state(), 110, 32);
        for want in [
            "River 3 Plus",
            "● connected",
            "42%",
            "Discharging",
            "7h 57m left (empty ~21:57)",
            "15% min",
            "85% max",
            "USB-C",
            "13.7 W",
            "Battery limits 15% – 85%",
            "21 °C",
            "Power history",
            "q quit",
        ] {
            assert!(out.contains(want), "missing {want:?} in:\n{out}");
        }
        // 5939 minutes-to-full is a placeholder and must not be shown.
        assert!(!out.contains("98h") && !out.contains("5939"), "{out}");
    }

    #[test]
    fn narrow_and_short_terminals_do_not_panic() {
        let v = real_device_state();
        for (w, h) in [(80, 24), (60, 20), (40, 12), (20, 6), (10, 3), (1, 1)] {
            let _ = draw_sized(&v, w, h);
        }
        let out = draw_sized(&v, 80, 24);
        assert!(out.contains("42%"), "{out}");
        assert!(out.contains("Power flow") && out.contains("Details"), "{out}");
        // Too small for both panels: details win.
        let out = draw_sized(&v, 60, 20);
        assert!(out.contains("Details") && !out.contains("Power flow"), "{out}");
    }

    #[test]
    fn charging_state_shows_full_eta() {
        let p = DisplayPropertyUpload {
            cms_batt_soc: Some(60.0),
            pow_get_bms: Some(300.0),
            pow_in_sum_w: Some(320.0),
            pow_out_sum_w: Some(20.0),
            cms_chg_rem_time: Some(90),
            plug_in_info_ac_charger_flag: Some(true),
            ..Default::default()
        };
        let mut v = ViewState::new();
        v.apply(
            Update::Status(Box::new(Status::from_props(test_device(), &p))),
            Instant::now(),
        );
        let out = draw_sized(&v, 110, 32);
        assert!(out.contains("▲ Charging"), "{out}");
        assert!(out.contains("full in 1h 30m (~15:30)"), "{out}");
        assert!(out.contains("plugged in"), "{out}");
    }

    #[test]
    fn waiting_reconnecting_and_fallback_states() {
        let mut v = ViewState::new();
        let out = draw_sized(&v, 90, 20);
        assert!(out.contains("Waiting for data") && out.contains("scanning"), "{out}");

        v.apply(
            Update::Conn(Conn::Reconnecting {
                reason: "link closed".into(),
                retry_secs: 4,
            }),
            Instant::now(),
        );
        assert!(draw_sized(&v, 100, 20).contains("reconnecting in 4s"));

        let st = Status::from_advertisement(test_device(), "wrong user ID or serial number");
        v.apply(Update::Status(Box::new(st)), Instant::now());
        v.apply(Update::Conn(Conn::Fallback("wrong user ID".into())), Instant::now());
        let out = draw_sized(&v, 100, 20);
        assert!(out.contains("Battery data unavailable"), "{out}");
        assert!(out.contains("advertisement only"), "{out}");
    }

    #[test]
    fn stale_data_is_flagged() {
        let mut v = real_device_state();
        v.last_update = Some(Instant::now() - Duration::from_secs(40));
        assert!(draw_sized(&v, 100, 30).contains("(stale)"));
    }

    #[test]
    fn discharge_limit_timer_is_shown_next_to_empty_timer() {
        // Real-device numbers: 42 %, 15 % limit, 477 min to empty -> 307 min to the limit.
        let p = DisplayPropertyUpload {
            cms_batt_soc: Some(42.0),
            pow_in_sum_w: Some(0.0),
            pow_out_sum_w: Some(14.0),
            cms_dsg_rem_time: Some(477),
            cms_min_dsg_soc: Some(15),
            cms_max_chg_soc: Some(85),
            ..Default::default()
        };
        let mut v = ViewState::new();
        v.apply(
            Update::Status(Box::new(Status::from_props(test_device(), &p))),
            Instant::now(),
        );
        let out = draw_sized(&v, 110, 32);
        assert!(out.contains("7h 57m left (empty ~21:57)"), "{out}");
        assert!(out.contains("15% limit in 5h 07m (~19:07)"), "{out}"); // battery panel
        assert!(out.contains("To 15% limit") && out.contains("5h 07m (~19:07)"), "{out}"); // details
        // Small terminals keep the details row even without room for the battery timer line.
        let out = draw_sized(&v, 80, 24);
        assert!(out.contains("To 15% limit") && out.contains("Errors"), "{out}");
    }

    #[test]
    fn charging_shows_time_to_upper_limit_instead_of_lower() {
        let p = DisplayPropertyUpload {
            cms_batt_soc: Some(60.0),
            pow_get_bms: Some(300.0),
            pow_in_sum_w: Some(320.0),
            cms_chg_rem_time: Some(90),
            cms_max_chg_soc: Some(85),
            cms_min_dsg_soc: Some(15),
            cms_dsg_rem_time: Some(400),
            ..Default::default()
        };
        let mut v = ViewState::new();
        v.apply(
            Update::Status(Box::new(Status::from_props(test_device(), &p))),
            Instant::now(),
        );
        for (w, h) in [(110, 32), (80, 24)] {
            let out = draw_sized(&v, w, h);
            assert!(out.contains("To 85% limit"), "{w}x{h}: {out}");
            assert!(!out.contains("To 15% limit"), "{w}x{h}: {out}");
        }
        let out = draw_sized(&v, 110, 32);
        // 90 min to full * 25/40 = 56 min -> 14:56.
        assert!(out.contains("85% limit in 0h 56m (~14:56)"), "{out}");
        assert!(out.contains("full in 1h 30m"), "{out}");
    }

    #[test]
    fn charge_limit_reached_is_shown() {
        let p = DisplayPropertyUpload {
            cms_batt_soc: Some(85.0),
            pow_get_bms: Some(20.0),
            cms_chg_rem_time: Some(30),
            cms_max_chg_soc: Some(85),
            ..Default::default()
        };
        let mut v = ViewState::new();
        v.apply(
            Update::Status(Box::new(Status::from_props(test_device(), &p))),
            Instant::now(),
        );
        assert!(draw_sized(&v, 110, 32).contains("85% limit reached"));
    }

    #[test]
    fn discharge_limit_reached_is_highlighted() {
        let p = DisplayPropertyUpload {
            cms_batt_soc: Some(14.0),
            pow_out_sum_w: Some(14.0),
            pow_in_sum_w: Some(0.0),
            cms_dsg_rem_time: Some(60),
            cms_min_dsg_soc: Some(15),
            ..Default::default()
        };
        let mut v = ViewState::new();
        v.apply(
            Update::Status(Box::new(Status::from_props(test_device(), &p))),
            Instant::now(),
        );
        assert!(draw_sized(&v, 110, 32).contains("15% limit reached"));
    }

    #[test]
    fn no_limit_timer_while_charging() {
        let p = DisplayPropertyUpload {
            cms_batt_soc: Some(42.0),
            pow_get_bms: Some(100.0),
            cms_min_dsg_soc: Some(15),
            cms_dsg_rem_time: Some(477),
            ..Default::default()
        };
        let mut v = ViewState::new();
        v.apply(
            Update::Status(Box::new(Status::from_props(test_device(), &p))),
            Instant::now(),
        );
        assert!(!draw_sized(&v, 110, 32).contains("limit in"));
    }

    #[test]
    fn light_status_is_shown_for_each_mode() {
        for (code, want) in [(0, "○ off"), (1, "◐ dim"), (2, "● bright"), (3, "⚠ SOS")] {
            let p = DisplayPropertyUpload {
                cms_batt_soc: Some(50.0),
                led_mode: Some(code),
                ..Default::default()
            };
            let mut v = ViewState::new();
            v.apply(
                Update::Status(Box::new(Status::from_props(test_device(), &p))),
                Instant::now(),
            );
            let out = draw_sized(&v, 110, 32);
            assert!(out.contains("Light") && out.contains(want), "mode {code}: {out}");
        }
        // Not reported: no Light row at all.
        let out = draw_sized(&real_device_state(), 110, 32);
        assert!(!out.contains("Light"), "{out}");
    }

    #[test]
    fn error_code_is_shown() {
        let p = DisplayPropertyUpload {
            cms_batt_soc: Some(50.0),
            errcode: Some(7),
            ..Default::default()
        };
        let mut v = ViewState::new();
        v.apply(
            Update::Status(Box::new(Status::from_props(test_device(), &p))),
            Instant::now(),
        );
        assert!(draw_sized(&v, 110, 32).contains("code 7"));
    }

    #[test]
    fn history_is_throttled_and_capped() {
        let mut v = ViewState::new();
        let t0 = Instant::now();
        let st = || {
            Update::Status(Box::new(Status::from_props(
                test_device(),
                &DisplayPropertyUpload::default(),
            )))
        };
        v.apply(st(), t0);
        v.apply(st(), t0 + Duration::from_millis(300)); // too soon, ignored
        assert_eq!(v.history.len(), 1);
        for i in 1..=(HISTORY_LEN as u64 + 20) {
            v.apply(st(), t0 + Duration::from_secs(i));
        }
        assert_eq!(v.history.len(), HISTORY_LEN);
    }

    #[test]
    fn advertisement_status_adds_no_history() {
        let mut v = ViewState::new();
        v.apply(
            Update::Status(Box::new(Status::from_advertisement(test_device(), "x"))),
            Instant::now(),
        );
        assert!(v.history.is_empty());
    }

    #[test]
    fn formatting_helpers() {
        assert_eq!(fmt_watts(None), "n/a");
        assert_eq!(fmt_watts(Some(0.0)), "0 W");
        assert_eq!(fmt_watts(Some(13.74)), "13.7 W");
        assert_eq!(fmt_watts(Some(150.4)), "150 W");
        assert_eq!(fmt_watts(Some(7.25)), "7.2 W");
        assert_eq!(fmt_watts(Some(1250.0)), "1.25 kW");
        assert_eq!(fmt_duration(477), "7h 57m");
        assert_eq!(fmt_duration(30 * 60 + 5), "1d 06h");
        assert_eq!(valid_minutes(Some(0)), None);
        assert_eq!(valid_minutes(Some(5939)), None);
        assert_eq!(valid_minutes(Some(120)), Some(120));
        assert_eq!(nice_scale(0.0), 100.0);
        assert_eq!(nice_scale(250.0), 300.0);
        assert_eq!(nice_scale(5000.0), 5000.0);
    }

    #[test]
    fn eta_crosses_midnight() {
        assert_eq!(fmt_eta(wall(), 90), "15:30");
        assert_eq!(fmt_eta(wall(), 12 * 60), "Mon 02:00");
    }

    #[test]
    fn bar_cells_fill_and_partial() {
        let s: String = bar_cells(0.5, 10).into_iter().collect();
        assert_eq!(s, "█████░░░░░");
        let s: String = bar_cells(0.55, 10).into_iter().collect();
        assert_eq!(s.chars().take(5).collect::<String>(), "█████");
        assert_eq!(s.chars().nth(5), Some('▌'));
        assert_eq!(bar_cells(1.5, 4).into_iter().collect::<String>(), "████");
        assert_eq!(bar_cells(-1.0, 4).into_iter().collect::<String>(), "░░░░");
        assert!(bar_cells(0.5, 0).is_empty());
    }

    #[test]
    fn marker_row_places_limits_without_overlap() {
        let row = marker_row(40, &[(15, "15% min".into()), (85, "85% max".into())]);
        assert_eq!(row.chars().count(), 40);
        assert_eq!(row.chars().nth(6), Some('▲'));
        assert!(row.contains("15% min") && row.contains("85% max"), "{row}");
        // Tight width: markers survive even if labels cannot.
        let tight = marker_row(12, &[(40, "40% min".into()), (50, "50% max".into())]);
        assert_eq!(tight.chars().count(), 12);
        assert!(tight.contains('▲'));
        assert_eq!(marker_row(0, &[(10, "x".into())]), "");
    }

    #[test]
    fn labels() {
        assert_eq!(connection_label(&Conn::Connected), "connected");
        assert!(connection_label(&Conn::Connecting(2)).contains("attempt 2"));
    }
}
