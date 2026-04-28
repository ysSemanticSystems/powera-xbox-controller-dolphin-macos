# gipbridge

A small userspace bridge that gets wired Xbox-style game controllers working on macOS by talking to them directly over USB and feeding inputs into Dolphin Emulator's named-pipe controller backend.

No kernel extension. No DriverKit extension. No virtual HID device. No SIP changes. No Apple Developer account. No Bluetooth required.

## What problem this solves

macOS has no built-in driver for third-party wired Xbox-style controllers (PowerA, PDP, HORI, and many others). They use Microsoft's Game Input Protocol (GIP) over a vendor-specific USB class, which macOS doesn't understand. The controller enumerates on the USB bus but no application sees any input.

The historical macOS solutions are all dead ends:

  - The `360Controller` kext is unmaintained and won't load on modern macOS (kexts are deprecated).
  - DriverKit-based virtual HID requires an Apple Developer account ($99/year) and a granted entitlement, with no signed community drivers available for these controllers.
  - Bluetooth pairing only works for Xbox controllers with native Bluetooth — most third-party "wired" controllers have none.
  - Steam's Xbox Extended Feature Support driver does not claim third-party PowerA/PDP/HORI VID:PIDs reliably.

`gipbridge` sidesteps the entire macOS input stack. It opens the controller as a regular USB device using libusb, parses the GIP input report, and writes the resulting button/axis state into a Unix named pipe. Dolphin Emulator has had built-in named-pipe controller input since 2015, so it just reads the pipe directly. macOS is never asked to provide a driver, so no entitlement is needed.

## Supported controllers

This bridge works for any wired controller that speaks Microsoft's standard GIP `0x20` input report. The known-good list is seeded from the Linux kernel `xpad` driver source and includes most PowerA, PDP, HORI, and Microsoft wired Xbox One / Series X|S controllers. Run `gipbridge --list` to see the full list.

If your controller isn't in the list, you can still try it with `--vid` and `--pid`. The auto-payload-offset detector handles minor packet-layout differences between vendors. If it works, please open a pull request adding the VID/PID + controller name to the list.

Confirmed working (the model the author developed against):

  - PowerA Xbox Series X Advantage Hall Effect Wired Controller — VID `0x20D6`, PID `0x2079`

## How it works

1. **Find and open the controller.** Scan connected USB devices for any matching VID/PID in the known-good list (or use `--vid`/`--pid` overrides). Claim the interface that has both interrupt-IN and interrupt-OUT endpoints.
2. **Wake the controller.** Send the GIP init packet `05 20 00 01 00` to the OUT endpoint. Without this, GIP controllers stay silent.
3. **Auto-detect the payload offset.** Different vendors put the input-state block at slightly different offsets inside the GIP `0x20` payload. The bridge spends ~2 seconds collecting statistics across candidate offsets and locks the offset that scores best as a sparse-button + 10-bit-trigger + 4-i16-axes layout.
4. **Parse the GIP input report.** Standard GIP `0x20` button bitfield (Menu / View / A / B / X / Y / d-pad / bumpers / stick clicks), two 10-bit triggers, four 16-bit signed analog axes for the sticks. Per-axis stick-center calibration during the first ~750 ms after startup, configurable radial deadzone after that.
5. **Write to Dolphin's pipe.** Open `~/Library/Application Support/Dolphin/Pipes/<name>` (default name `powera` for backwards compatibility). Emit `PRESS`/`RELEASE` on button edges, throttled `SET MAIN`/`SET C`/`SET L`/`SET R` on analog changes, at up to 120 Hz.

That's the whole architecture. Roughly 600 lines of Rust.

## Requirements

  - macOS (Apple Silicon or Intel). Tested on Apple Silicon.
  - Rust toolchain (`rustup` recommended).
  - Dolphin Emulator (any build with Pipe input support — i.e. since 2015).
  - `sudo` to claim the vendor-class USB interface.

## Quick start

```bash
git clone <this-repo>
cd gipbridge
make run
```

In Dolphin: Controllers → Standard Controller → Configure → Device dropdown: `Pipe/0/powera`. Bind buttons by clicking each slot in Dolphin's mapping screen and pressing the corresponding physical control.

## Command-line options

```
gipbridge [OPTIONS]

  --vid <HEX>          Override VID (e.g. 0x20D6). Requires --pid.
  --pid <HEX>          Override PID (e.g. 0x2079). Requires --vid.
  --pipe-name <NAME>   Filename under ~/Library/Application Support/Dolphin/Pipes/
                       (default: powera)
  --no-y-invert        Do not invert stick Y axes (try this if pushing up moves
                       the in-game character down).
  --deadzone <FLOAT>   Radial stick deadzone, [0.0, 0.5] (default: 0.12).
  --dump               Print raw hex for every input packet (verbose; use when
                       diagnosing a new controller).
  --list               Print known supported controllers and exit.
  -h, --help           Show this help.
```

## Adding support for a new controller

1. Plug in the controller and run `gipbridge --vid 0xYOUR --pid 0xYOUR --dump`.
2. If it finds the interrupt endpoints, sends the init packet, and starts streaming `0x20` packets with the layout offset locking, you have a working controller.
3. Press each face button, the d-pad, the bumpers, and the menu/view buttons one at a time. Sweep each trigger 0→full. Sweep each stick through its full range. Verify the bits and axes flip in expected positions.
4. Open a PR adding `(0xYOUR, 0xYOUR, "Vendor Model Name")` to `KNOWN_CONTROLLERS`.

If the auto-offset detection locks an offset but the bits look wrong, the controller is using a non-standard GIP variant. File an issue with raw `--dump` output.

## What this is, technically

A userspace input bridge. macOS doesn't have a clean term for this category — Linux calls these "userspace USB drivers" routinely (libusb-based input handlers like `xboxdrv` and `xone`'s userland tooling). On macOS, the closest formal category is a "user-mode helper" that opens a vendor-class USB device and exposes its data through some other channel. The "other channel" here is Dolphin's built-in named-pipe controller backend, which is what makes this work without an Apple-granted entitlement.

It is not a driver in the strict OS sense — macOS does not bind it to the device, it doesn't appear under IOKit's driver registry, and it can't replace the missing system input path for arbitrary applications. But for the purpose of "make my controller work in Dolphin," it is functionally complete and indistinguishable from one.

## What this is not

  - Not a system-wide controller driver. Only Dolphin sees the input. Other games and apps still see nothing.
  - Not a virtual HID device. macOS does not expose this controller to any other process.
  - Not a kext or a DEXT. No kernel-side code, no DriverKit extension, no entitlement.
  - Not a Wine/CrossOver shim.
  - Not Bluetooth. This is wired-USB only.

For a system-wide solution that exposes the controller to all macOS applications, you would need a DriverKit extension with the `com.apple.developer.hid.virtual.device` entitlement — which requires an Apple Developer account. That's out of scope here.

## Troubleshooting

  - **`device not found`** — your controller's VID/PID isn't in the known-good list. Find it in `system_profiler SPUSBDataType` or in IOKit (`ioreg -p IOUSB -l -w 0`), then run with `--vid` and `--pid`.
  - **`USB claim fails`** — another process has the device. Quit Steam, quit any Wine/CrossOver instances, unplug and replug the controller, retry with `sudo`.
  - **`Pipe doesn't connect`** — Dolphin hasn't opened the pipe yet. In Dolphin, set Device to `Pipe/0/<your --pipe-name value>` and keep the controller config window open while the bridge is running.
  - **`Buttons mapped to wrong actions`** — try `--dump` and verify the GIP `0x20` payload layout matches the standard one. If a vendor has shifted bits, file an issue with the dump.
  - **`Sticks inverted on Y axis`** — try `--no-y-invert`.
  - **`Stick drift / center off`** — the bridge calibrates the stick center during the first ~750 ms. Make sure you're not touching the sticks at startup. Increase `--deadzone` if drift persists.

## Credits

Protocol references:

  - Linux kernel `xpad` driver (drivers/input/joystick/xpad.c) — canonical VID/PID list and GIP input report layout.
  - `medusalix/xone` — userspace-friendly GIP protocol documentation.
  - Dolphin Emulator's named-pipe controller backend (Source/Core/InputCommon/ControllerInterface/Pipes/) — the escape hatch that makes the whole approach possible without a virtual HID device or DriverKit entitlement.

## License

[choose one — MIT or Apache-2.0 recommended for a project like this]

