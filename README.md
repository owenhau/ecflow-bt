# ecflow-bt

Read battery and charging status from an **EcoFlow River 3 / River 3 Plus** over Bluetooth LE (Linux, BlueZ), and
change a few of its settings.

* One-shot text or `--json` output, or a live dashboard with `--tui`.
* Connects over GATT with the EcoFlow handshake. If that is impossible, it falls back to the passive
  advertisement (RSSI, serial) and says that battery data is unavailable.
* **Read/write.** `ecflow-bt set …` and the TUI Settings overlay change the charge limit, discharge limit, light mode,
  12V output and AC output. Changes are applied **immediately, with no confirmation prompt**, and are checked by reading
  the value back from the device. See [Changing settings](#changing-settings) for the risks.

Supported serial prefixes: River 3 `R651 R653 R654 R655`, River 3 Plus `R631 R634 R635`.

## Build

```
cargo build --release      # binary: target/release/ecflow-bt
```

libdbus is built from source (`libdbus-sys` `vendored`), so only a C compiler is needed. At runtime you need BlueZ
and a powered-on adapter; your user must be allowed to talk to `bluetoothd` (normally true for a desktop session).

## Configure

Settings resolve as `--flag` > environment > `~/.config/ecflow-bt/config.toml`:

```toml
user_id = "1234567890"      # EF_USER_ID / --user-id   (required for battery data)
serial  = "R631XXXXXXXXXXXX" # EF_SERIAL  / --serial    (optional; picks the device when several are near)
timeout_secs = 15            # EF_TIMEOUT / --timeout
```

`ecflow-bt config` prints the resolved values (user ID masked).

### Finding your user ID

The device authenticates with `md5(user_id + serial)`, so the app needs your numeric EcoFlow **user ID**. The mobile app
does not show it. The reference project (`ha-ef-ble`) gets it from the EcoFlow login API. This tool never takes your
password; if you want to look the ID up yourself, something like this works (untested here; use `api-e`, `api-a`, … for
other regions):

```
read -rp 'EcoFlow email: ' E; read -rsp 'Password: ' P; echo
curl -s https://api.ecoflow.com/auth/login -H 'Content-Type: application/json' \
  -d "$(jq -n --arg e "$E" --arg p "$(printf %s "$P" | base64)" \
  '{scene:"IOT_APP",appVersion:"1.0.0",password:$p,oauth:{bundleId:"com.ef.EcoFlow"},userType:"ECOFLOW",email:$e}')" \
  | jq -r .data.user.userId
```

## Use

```
ecflow-bt                 # one-shot, text
ecflow-bt --json          # one-shot, JSON
ecflow-bt --tui           # live dashboard (q / Esc / Ctrl-C to quit), see below
ecflow-bt set light dim   # change a setting (see "Changing settings")
ecflow-bt scan            # list the device seen over Bluetooth
ecflow-bt decode-packet "<hex>"   # decode a captured, decrypted inner packet
ecflow-bt -vv             # debug logs on stderr
```

**Exit codes:** `0` live data over GATT (or a setting applied), `1` error (no adapter, device not found, bad config,
invalid arguments or a rejected setting), `2` fallback (advertisement only; the reason is printed), `3` a setting was
sent but the device did not report the change.

### Dashboard (`--tui`)

* **Header:** model, serial, address, signal, and a connection badge (connected, scanning, reconnecting, advertisement only).
* **Battery bar:** percentage, colour by level and charging state, charge/discharge power, time left or time to full with a
  clock-time estimate, a second timer for reaching the limit you are heading toward (discharge limit while discharging, charge limit while charging), and markers at your configured discharge-min and charge-max limits.
* **Power flow:** input and output bars (auto-scaled) with per-port rows (AC in, solar/DC, AC out, 12 V, USB-C, USB-A) and
  net battery power.
* **Details:** state, temperature, limits, port on/off, extra battery, light mode (when reported), error code.
* **Power history:** input and output sparklines for the last 120 s (shown on terminals 30+ rows tall).
* The footer shows how long ago the last update arrived and flags stale data. Layout adapts down to about 60x20; on
  small terminals the details panel is kept and the power-flow panel is dropped.
* Time estimates the device reports as placeholders (0, or more than 72 h) are hidden rather than shown.

### Changing settings

```
ecflow-bt set charge-limit 85      # upper battery limit, 0-100 %
ecflow-bt set discharge-limit 15   # lower battery limit, 0-100 %
ecflow-bt set light dim            # off | dim | bright | sos
ecflow-bt set dc12v on             # 12V DC output, on | off
ecflow-bt set ac off               # AC output, on | off
ecflow-bt set light off --json     # machine-readable result
```

Each command connects, reads the current state, **validates** the request, sends it, waits up to `--timeout` seconds for
the device to report the new value, prints `before -> after` and exits. Example: `charge limit: 85% -> 80% (confirmed)`.

* The charge limit may not be below the discharge limit and the reverse; change the other limit first if needed.
  Values outside 0-100 are rejected before anything is sent. The firmware may enforce narrower ranges of its own.
* Setting a value the device already has sends nothing (`already 85%`).
* Writes need a working session. Unlike reading, there is **no advertisement fallback**: a missing or wrong user ID is an
  error (exit 1) and nothing is written.
* Exit `3` means the write was sent but the device did not report the new value in time. Check the device and read the
  status again before retrying.

> **No confirmation prompts, by design.** `set ac off` cuts power to everything on the AC outlets right away, and
> `set light sos` starts the flashing SOS light. Lowering the discharge limit or raising the charge limit changes how far
> the battery is drained or charged. Power-off, charging speed, DC charging type and backup reserve are not supported.

`set --json` prints `{"setting","requested","before","after","result","confirmed","warnings"}`, where `result` is
`applied`, `unchanged` or `unconfirmed`.

In the dashboard press **`s`** to open the Settings overlay:

| Key | Action |
|---|---|
| `↑` `↓` (or `k` `j`) | select a row |
| `Enter` on **12V output** / **AC output** | toggle it immediately |
| `Enter` on **light** | cycle off → dim → bright → sos → off |
| `Enter` on a **limit** | edit it: `←` `→` adjust by 1 (`Shift` by 5), or type digits; `Enter` applies, `Esc` cancels |
| `Esc` / `s` | close the overlay (`q` or `Ctrl-C` quits) |

A toast in the footer reports the result ("Light set to dim", or "Not confirmed: …"). An invalid limit is refused with an
inline message and stays in the editor. When there is no live session (scanning, reconnecting, advertisement only) the
overlay is greyed out and nothing can be sent.

> The device accepts one app connection at a time and stops advertising while connected. **Close the EcoFlow phone app
> (or turn off its Bluetooth) before running this**, otherwise the scan will not find the device.

### JSON

```json
{
  "source": "gatt",                // "gatt" | "advertisement"
  "device": { "model": "...", "serial": "...", "name": "...", "address": "...", "rssi": -55, "manufacturer_data": "hex" },
  "note": null,                    // why battery data is missing (advertisement only)
  "battery_percent": 87.5,         // total incl. extra battery
  "main_battery_percent": 87.5,
  "charge_state": "charging",      // "charging" | "discharging" | "idle"
  "ac_plugged_in": true,
  "extra_battery_attached": false,
  "input_watts": 150.0, "output_watts": 10.0, "battery_watts": 140.0,   // battery_watts > 0 = charging
  "ac_input_watts": 150.0, "solar_input_watts": 0.0,
  "ac_output_watts": 0.0, "dc12v_output_watts": 0.0, "usbc_output_watts": 10.0, "usba_output_watts": 0.0,
  "minutes_to_full": 80, "minutes_to_empty": 0,
  "minutes_to_discharge_limit": null,   // only while discharging; estimated, see below
  "minutes_to_charge_limit": 56,        // only while charging; estimated, see below
  "cell_temperature_c": 27,
  "charge_limit_max_percent": 100, "discharge_limit_min_percent": 0,
  "ac_output_on": false, "dc12v_output_on": false,
  "led_mode": "off",               // front light: "off" | "dim" | "bright" | "sos" | "unknown(N)"; null if not reported
  "error_code": 0
}
```

Fields the device did not report are `null`.

`minutes_to_discharge_limit` and `minutes_to_charge_limit` are not reported by the device; they are derived from its
time-to-empty and time-to-full estimates, scaled by the energy between the battery level and the limit:

* discharging: `minutes_to_empty × (soc − discharge_limit) / soc`
* charging: `minutes_to_full × (charge_limit − soc) / (100 − soc)`

Both assume the load or charging power stays constant, and are `0` once the limit is reached. Only the timer for the
current direction is set; the dashboard and text output show it as `To <limit>% limit` (the lower limit while
discharging, the upper limit while charging). Charging state uses the BMS power with a ±0.5 W idle band, and falls back to
input vs. output power when the BMS value is missing.

## Manual real-device checklist (River 3 Plus)

Reading:

1. `ecflow-bt scan` finds the device (phone app closed).
2. `ecflow-bt` with no user ID prints the advertisement-only fallback and exits 2.
3. With the correct user ID the battery % matches the device display; exit code 0.
4. Plug in AC: state becomes `charging`. Unplug: `discharging`/`idle`.
5. A wrong user ID falls back with "wrong user ID or serial number".
6. `--tui` refreshes, and the terminal is restored on `q` and on Ctrl-C.

Writing, lowest risk first (note your current values so you can restore them):

7. `ecflow-bt set light dim`, then `bright`, then `off`. The light changes and each command prints `(confirmed)`.
8. In `--tui`, press `s`, select **light** and press `Enter`. The light cycles, a green toast appears and the
   Details panel's Light row follows.
9. `ecflow-bt set discharge-limit 20`, then back to your value. In the TUI the `min` marker on the battery bar moves.
   Try `set discharge-limit 95` (above the charge limit): it must be refused with a message, exit 1, nothing written.
10. In the TUI edit a limit (`Enter` on it, type a value, `Enter`). Try a conflicting value: it must show an inline error
    and stay open.
11. `ecflow-bt set dc12v on`, then `off`. Then `set charge-limit 90` and back to your usual value.
12. Only with nothing important plugged in: `ecflow-bt set ac on`, then `off`.
13. A wrong `--user-id` makes `set` fail with exit code 1 and writes nothing.
14. If any write ends with exit `3` (not confirmed), run `ecflow-bt -vv set …` and `ecflow-bt` again to see whether the
    device changed anyway; the raw packets can be inspected with `decode-packet`.

## Tests

`cargo test` runs unit tests (CRC, packet and frame codecs, secp160r1 ECDH against python-ecdsa vectors, a simulated
device that exercises the full handshake and configuration writes, setting validation, CLI parsing, TUI key handling and
render tests) and fixture tests. The fixtures in `tests/fixtures/` are synthetic or encoded with the reference
implementation (`status_packet.hex`, `config_write.txt`); see `tests/fixtures/README.md`. Redact user IDs and serials before committing any real capture.

## Credits

Protocol knowledge and the session-key table come from [rabits/ha-ef-ble](https://github.com/rabits/ha-ef-ble)
(Apache-2.0). See `NOTICE`. Not affiliated with EcoFlow.
