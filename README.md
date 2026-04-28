# PowerA Xbox controller input for Dolphin on macOS

This project bridges certain **wired PowerA Xbox-style controllers** to **Dolphin Emulator** on macOS by using Dolphin’s built-in **Pipe** input backend.

It reads the controller directly over USB (Game Input Protocol / “GIP”), then writes controller events to a named pipe that Dolphin can consume.

- **No kernel extensions**
- **No SIP changes**
- **No virtual HID**

## Supported controller(s)

Confirmed working:

- **PowerA Xbox Series X Advantage Hall Effect Wired Controller**
  - **Vendor ID (VID)**: `0x20D6`
  - **Product ID (PID)**: `0x2079`

Other wired PowerA Xbox-style controllers may work if they use GIP and the packet layout matches (or is close). Adding support usually means whitelisting additional VID/PIDs and verifying the input report layout.

## Requirements

- Rust toolchain
- Dolphin Emulator (any build with Pipe input support)

## Run

On macOS, claiming a vendor-specific USB interface typically requires root privileges:

```bash
make run
```

## Dolphin setup (Pipe backend)

The program writes to:

- `~/Library/Application Support/Dolphin/Pipes/powera`

It will create the directory and FIFO automatically if missing.

In Dolphin:

- Controllers → Standard Controller → Configure → Device: `Pipe/0/powera`

## What it emits (Pipe protocol)

You can bind these in Dolphin’s controller mapping UI:

- **Buttons**: `A B X Y Z START D_UP D_DOWN D_LEFT D_RIGHT`
- **Sticks**:
  - `SET MAIN x y` (left stick)
  - `SET C x y` (right stick)
- **Triggers**:
  - `SET L value` (left trigger, 0–1)
  - `SET R value` (right trigger, 0–1)

## Build (optional)

```bash
make build
```

Or:

```bash
CARGO_TARGET_DIR=target cargo build --release
sudo ./target/release/xbox_controller_macos_gip
```

## Troubleshooting

- **Pipe doesn’t connect**: Dolphin hasn’t opened the pipe yet. Set the Device to `Pipe/0/powera` and keep the controller config window open.
- **USB claim fails**: try unplug/replug, quit other apps that may be accessing the controller, then re-run with `sudo`.
- **Inputs register but are wrong**: re-bind in Dolphin after updates; the program may change its mapping as support improves.

