# ecflow-bt

Read battery and charging status from an **EcoFlow River 3 / River 3 Plus** over Bluetooth LE (Linux, BlueZ).

* One-shot text or `--json` output, or a live read-only dashboard with `--tui`.
* Connects over GATT with the EcoFlow handshake. If that is impossible, it falls back to the passive
  advertisement (RSSI, serial) and says that battery data is unavailable.
* Read-only. The only thing sent to the device besides the handshake is the time-sync reply it asks for.

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
ecflow-bt scan            # list the device seen over Bluetooth
ecflow-bt decode-packet "<hex>"   # decode a captured, decrypted inner packet
ecflow-bt -vv             # debug logs on stderr
```

**Exit codes:** `0` live data over GATT, `2` fallback (advertisement only; the reason is printed), `1` error
(no adapter, device not found, bad config).

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

1. `ecflow-bt scan` finds the device (phone app closed).
2. `ecflow-bt` with no user ID prints the advertisement-only fallback and exits 2.
3. With the correct user ID the battery % matches the device display; exit code 0.
4. Plug in AC: state becomes `charging`. Unplug: `discharging`/`idle`.
5. A wrong user ID falls back with "wrong user ID or serial number".
6. `--tui` refreshes, and the terminal is restored on `q` and on Ctrl-C.

## Tests

`cargo test` runs unit tests (CRC, packet and frame codecs, secp160r1 ECDH against python-ecdsa vectors, a simulated
device that exercises the full handshake, TUI render tests) and a fixture test. `tests/fixtures/status_packet.hex` is
synthetic; see `tests/fixtures/README.md`. Redact user IDs and serials before committing any real capture.

## Credits

Protocol knowledge and the session-key table come from [rabits/ha-ef-ble](https://github.com/rabits/ha-ef-ble)
(Apache-2.0). See `NOTICE`. Not affiliated with EcoFlow.
