# PowerA Xbox controller input for Dolphin on macOS

## Who this is for

You should care about this project if:

- You have a **wired PowerA Xbox-style controller** on macOS
- Dolphin recognizes the **Pipe** backend, but macOS does not expose your controller as a normal gamepad
- You want controller input in Dolphin **without** kernel extensions, SIP changes, or virtual HID drivers

## How it works (technical overview)

This program is a small userspace bridge:

- Opens the controller over USB using `libusb` (via the Rust `rusb` crate)
- Sends a short initialization packet to start the controller’s **GIP** (Game Input Protocol) input stream
- Reads interrupt packets, parses the GIP `0x20` input report (buttons, sticks, triggers)
- Writes the state to a Unix named pipe using Dolphin’s Pipe protocol (`PRESS/RELEASE/SET ...`)

Dolphin reads the pipe directly, so macOS never needs to provide a controller driver.

It reads the controller directly over USB (Game Input Protocol / “GIP”), then writes controller events to a named pipe that Dolphin can consume.

- **No kernel extensions**
- **No SIP changes**
- **No virtual HID**

## Supported controller(s)

Confirmed working:

- **PowerA Xbox Series X Advantage Hall Effect Wired Controller**
  - **Vendor ID (VID)**: `0x20D6`
  - **Product ID (PID)**: `0x2079`

Other wired PowerA Xbox-style controllers may or may not work. Compatibility is primarily:

- **USB VID/PID match** (this build only opens `0x20D6:0x2079`)
- **GIP packet layout match** (button bits / axes layout)

If you want support for a different controller, the practical requirement is having that controller available for testing.

## Requirements

- Rust toolchain
- Dolphin Emulator (any build with Pipe input support)

## Quick compatibility check

1. Plug the controller in via USB.
2. Run:

```bash
make run
```

If it starts and prints `Claimed interface ...` and then `Locked GIP payload offset: ...` it is reading GIP input successfully.
If it prints `device not found (VID=..., PID=...)`, your controller is not the supported model (or it uses a different VID/PID).

## Run

On macOS, claiming a vendor-specific USB interface typically requires root privileges:

```bash
make run
```

## Make startup easier (auto-run at boot/login)

Because this needs to be running before (or while) Dolphin reads the pipe, you can install it as a `launchd` service.

1. Install the binary:

```bash
make install
```

2. Install and load the LaunchDaemon (runs as root so USB access works):

```bash
sudo cp launchd/com.yssemanticsystems.powera-dolphin-pipe.plist /Library/LaunchDaemons/
sudo launchctl bootstrap system /Library/LaunchDaemons/com.yssemanticsystems.powera-dolphin-pipe.plist
```

Logs:

- `/var/log/powera-dolphin-pipe.log`

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

