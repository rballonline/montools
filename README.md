# montools

Switch your monitor's input — and its built-in KVM's USB devices — from the system tray, a global hotkey, or the command line. Works over DDC/CI, so there's no vendor software (e.g. Dell Display Manager) to keep running.

- **Tray app**: one click to "Switch to *Other PC*", a status line showing what's on screen, and a settings window.
- **Hotkey**: a global shortcut toggles between your two computers.
- **USB / KVM mapping**: choose which USB upstream port follows each input. Changes are applied with a 5-second "keep or revert" confirmation, so a wrong choice can't strand your keyboard and mouse.
- **CLI** (`montools-cli`): the same features for scripts and desktop-environment shortcuts.
- **Cross-platform**: Windows and Linux. macOS should work but is untested.

## Supported monitors

Any monitor with DDC/CI enabled can switch inputs (MCCS VCP code `0x60`).

Remapping USB per input uses an undocumented, vendor-specific register. Monitors with a known layout:

| Monitor | VCP | Notes |
| --- | --- | --- |
| Dell U4919DW | `0xE7` | Upstreams USB-B1, USB-B2, USB-C; inputs DP, HDMI1, HDMI2, USB-C |

Monitors without a known KVM layout are hidden by default. Show them with **Show monitors without a KVM** in Settings or `montools-cli list --all`. To add a monitor, see [Adding a KVM monitor](#adding-a-kvm-monitor).

## Install & run

There are no prebuilt releases yet, so build from source (see [Development](#development)). `pnpm tauri build` produces installers in `src-tauri/target/release/bundle/`:

- **Windows**: NSIS installer (`nsis/montools_*_x64-setup.exe`)
- **Linux**: `.deb`, `.rpm` and AppImage
- **macOS**: `.dmg` / `.app`

The CLI is built alongside the app at `src-tauri/target/release/montools-cli(.exe)`.

### Platform setup

**Windows**: nothing to install. Make sure DDC/CI is enabled in the monitor's on-screen menu (on Dell: *Others → DDC/CI*).

**Linux**: DDC/CI is exposed through `/dev/i2c-*`.

```sh
sudo modprobe i2c-dev
echo i2c-dev | sudo tee /etc/modules-load.d/i2c-dev.conf   # load at boot
sudo groupadd -f i2c
sudo usermod -aG i2c "$USER"
echo 'KERNEL=="i2c-[0-9]*", GROUP="i2c", MODE="0660"' | sudo tee /etc/udev/rules.d/45-i2c.rules
sudo udevadm control --reload && sudo udevadm trigger
# log out and back in for the group change
```

- **GNOME** needs the [AppIndicator extension](https://extensions.gnome.org/extension/615/appindicator-support/) to show tray icons. KDE and most other desktops show them natively.
- **Wayland** doesn't let apps register global hotkeys. Instead, bind `montools-cli switch toggle` to a shortcut in your desktop's keyboard settings. The Settings window shows this note when it detects Wayland.

## Configuring

On first launch the Settings window opens. Afterwards, open it from the tray menu.

1. **Monitor**: picks the monitor to control. The first monitor with a KVM is chosen automatically.
2. **Computers**: set the input each computer is plugged into, and optionally a name ("Work laptop"). *This PC* defaults to the input you're currently on.
3. **Switch to**: one button per configured computer.
4. **USB / KVM mapping**: for each computer, choose the USB upstream port its cable is plugged into, then **Save**. Confirm with **Keep** within 5 seconds, or it reverts. The mapping is stored in the monitor, so it keeps working after montools quits, and from the other computer too.
5. **Shortcut & startup**: record a global hotkey and choose whether to start at login.

Settings are stored as JSON and shared by the app and the CLI:

| OS | Path |
| --- | --- |
| Windows | `%APPDATA%\dev.montools.app\config.json` |
| Linux | `~/.config/dev.montools.app/config.json` |
| macOS | `~/Library/Application Support/dev.montools.app/config.json` |

Install montools on both computers if you want to switch back from either side. Each computer has its own config, so "This PC" and "Other PC" are swapped on the second machine.

### CLI

```text
montools-cli [--monitor <key>] <command>

  list [--all]                   KVM monitors (or all with --all), inputs and state
  status                         Current input of the selected monitor
  switch <input>                 Switch input: hdmi1, hdmi2, dp, usbc, 0x11…
  switch toggle|this|other       Switch using the inputs saved in the config
  kvm show                       Which USB upstream each input uses
  kvm set <input> <port>         Map an input to a USB port: b1, b2, usbc
  config                         Show config path and contents
  config set this|other <input>
  config name this|other <name>  Friendly name for a computer (empty clears it)
```

`kvm set` writes immediately, with no confirmation and no revert.

## Development

### Prerequisites

- [Rust](https://rustup.rs/) (stable)
- Node.js 20+ and [pnpm](https://pnpm.io/)
- The [Tauri system dependencies](https://tauri.app/start/prerequisites/) for your OS. On Debian/Ubuntu:

  ```sh
  sudo apt install libwebkit2gtk-4.1-dev build-essential curl wget file \
    libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev
  ```

### Commands

```sh
pnpm install
pnpm tauri dev                                          # run the tray app with hot reload
pnpm tauri build                                        # release build + installers
cargo run --manifest-path src-tauri/Cargo.toml --bin montools-cli -- list   # run the CLI
cargo test --manifest-path src-tauri/Cargo.toml         # unit tests (KVM encode/decode)
```

The app starts hidden in the tray. Running it a second time opens the Settings window of the instance that's already running. On Windows 11 the icon may sit in the tray overflow (^) at first; you can drag it out to keep it visible.

### Project layout

```text
src/App.vue                Settings window (Vue 3 + TypeScript)
src-tauri/src/
  lib.rs                   Tray menu, hotkey, Tauri commands, KVM save/confirm/revert
  worker.rs                Dedicated DDC thread (display handles aren't Send); retries after re-enumerating
  monitors.rs              Enumerates monitors, reads/writes input + KVM VCP codes
  kvm.rs                   Per-model KVM profiles: encode/decode the USB mapping register
  inputs.rs                MCCS input codes ↔ names ("HDMI1" = 0x11, …)
  config.rs                Config file shared by app and CLI
  bin/montools-cli.rs      Command-line interface
src-tauri/icons/app-icon.svg   Icon source (regenerate with: pnpm tauri icon src-tauri/icons/app-icon.svg)
```

DDC/CI access goes through the [`ddc-hi`](https://crates.io/crates/ddc-hi) crate (WinAPI on Windows, i2c-dev on Linux, IOKit on macOS). The NVAPI backend is disabled because it lists every monitor twice.

### Adding a KVM monitor

Each monitor stores its per-input USB routing in a vendor-specific VCP code. Finding it:

1. Dump every VCP value: on Windows use NirSoft [ControlMyMonitor](https://www.nirsoft.net/utils/control_my_monitor.html), on Linux `ddcutil getvcp ALL` / `ddcutil dumpvcp`.
2. Change one input's USB upstream in the vendor's tool or the on-screen menu, then dump again.
3. Diff the two dumps. The code that changed is the mapping register. Repeat for each input and port to work out the bit layout.

Then add a `KvmProfile` in `src-tauri/src/kvm.rs`:

```rust
pub const DELL_U4919DW: KvmProfile = KvmProfile {
    models: &["U4919DW"],                        // model name as reported (without "DELL ")
    vcp: 0xE7,                                   // register holding the mapping
    slots: &[(0x0F, 6), (0x1B, 4), (0x12, 2), (0x11, 0)], // (input code, bit shift of its 2-bit slot)
    ports: &[(0, "USB-B1"), (1, "USB-B2"), (2, "USB-C")], // slot value → upstream port
};
```

Add it to `PROFILES`, add a decode/encode test with values read from your monitor, and open a PR. Include the raw register values you observed, because other people can't check them without the same monitor. If your monitor's layout doesn't fit 2-bit slots, `KvmProfile` will need extending.

### Contributing

Issues and PRs are welcome, especially new monitor profiles and Linux/macOS testing reports. Before submitting, please run `cargo test`, `cargo clippy` and `pnpm build`.
