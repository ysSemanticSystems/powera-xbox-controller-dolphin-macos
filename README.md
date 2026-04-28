# PowerA Xbox Controller → Dolphin (macOS)

If you plugged in a wired Xbox-style controller on macOS and **nothing happens in Dolphin**, you’re not alone.
Some controllers show up on USB but don’t have a macOS driver that understands their data.

This project is a small background program (“daemon”) that:

- Reads a specific PowerA wired controller over USB
- Translates its inputs (sticks/buttons/triggers)
- Sends them to **Dolphin Emulator** using Dolphin’s built-in **Pipe** controller backend

No kernel extensions. No SIP changes. No entitlements. No virtual HID.

### Supported controller

- **PowerA Xbox Series X Advantage Hall Effect Wired Controller**
  - Vendor ID: `0x20D6`
  - Product ID: `0x2079`

## Status

- **USB open + claim + init packet**: implemented
- **Input parsing**: implements the common GIP `0x20` input packet layout described in the prompt
- **Dolphin Pipe output**: implemented (PRESS/RELEASE + SET MAIN/C/L/R)
- **Auto payload offset detection**: implemented (handles extra bytes on some PowerA devices)

## Quick start

1. **Run the daemon**

On macOS, claiming a **vendor-specific USB interface** commonly requires root privileges, so we run it with `sudo`:

```bash
make run
```

2. **Tell Dolphin to use the Pipe backend**

The daemon writes to this pipe (it will create the directory and FIFO automatically if missing):

- `~/Library/Application Support/Dolphin/Pipes/powera`

In Dolphin:

- Controllers → Standard Controller → Configure → Device dropdown: `Pipe/0/powera`

3. **Map controls inside Dolphin**

In Dolphin’s controller mapping UI, bind buttons/axes as you prefer. This daemon emits:

- Buttons: `A B X Y Z START D_UP D_DOWN D_LEFT D_RIGHT`
- Sticks: `SET MAIN x y` (left stick), `SET C x y` (right stick)
- Triggers: `SET L value`, `SET R value` (analog 0–1)

## Build (optional)

```bash
make build
```

If you want to build/run without `make`:

```bash
CARGO_TARGET_DIR=target cargo build --release
sudo ./target/release/xbox_controller_macos_gip
```

## Debugging / failure modes

- **USB claim fails**: likely permission/device-busy. Try unplug/replug, quit any software that might open it.
- **Init sends but no input arrives**: endpoints may be different; we auto-detect interrupt IN/OUT from descriptors.
- **Input arrives but parses wrong**: the program prints raw hex for the first few parse failures.
- **Pipe doesn’t connect**: Dolphin hasn’t opened it yet. Set Device to `Pipe/0/powera` and keep the config window open.

