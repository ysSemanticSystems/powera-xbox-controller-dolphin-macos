use anyhow::{anyhow, bail, Context, Result};
use clap::Parser;
use rusb::{Context as RusbContext, DeviceHandle, Direction, TransferType, UsbContext};
use std::ffi::CString;
use std::fs;
use std::io::{self, Write};
use std::os::fd::FromRawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const KNOWN_CONTROLLERS: &[(u16, u16, &str)] = &[
    (0x20D6, 0x2079, "PowerA Xbox Series X Advantage Hall Effect Wired"),
    (0x20D6, 0x2009, "PowerA Enhanced Wired Controller for Xbox Series X|S"),
    (0x20D6, 0x200E, "PowerA Spectra Infinity Enhanced Wired Controller"),
    (0x20D6, 0x2064, "PowerA Wired Controller for Xbox"),
    (0x20D6, 0x281F, "PowerA Wired Controller For Xbox 360"),
    (0x20D6, 0x2001, "BDA / PowerA Xbox Series X Wired Controller"),
    (0x20D6, 0x2003, "PowerA Xbox Series X Fusion Pro 2 Wired"),
    (
        0x20D6,
        0x2004,
        "PowerA Enhanced Wired Controller (Xbox Series X EnWired Pink Inline)",
    ),
    (0x0E6F, 0x0139, "PDP Afterglow Prismatic Wired Xbox One"),
    (0x0E6F, 0x013A, "PDP Xbox One Controller"),
    (0x0E6F, 0x0146, "PDP Rock Candy Wired for Xbox One"),
    (0x0E6F, 0x0161, "PDP Xbox One Controller"),
    (0x0E6F, 0x0162, "PDP Xbox One Controller"),
    (0x0E6F, 0x0163, "PDP Xbox One Controller"),
    (0x0E6F, 0x0164, "PDP Battlefield 1 Xbox One"),
    (0x0E6F, 0x0165, "PDP Titanfall 2 Xbox One"),
    (0x0F0D, 0x0067, "HORI Pad Pro Xbox One"),
    (0x0F0D, 0x0078, "HORI Real Arcade Pro V Kai Xbox One"),
    (0x24C6, 0x541A, "PowerA Xbox One Mini"),
    (0x24C6, 0x542A, "PowerA Xbox One Spectra"),
    (0x24C6, 0x543A, "PowerA Xbox One"),
    (0x24C6, 0x551A, "PowerA Fusion Pro Wired Xbox One"),
    (0x24C6, 0x561A, "PowerA Xbox One Cabled"),
    (0x24C6, 0x581A, "PowerA Enhanced Wired (3rd party)"),
    (0x045E, 0x02DD, "Microsoft Xbox One Controller (wired, post-2015)"),
    (0x045E, 0x02E3, "Microsoft Xbox One Elite Controller (wired)"),
    (0x045E, 0x02EA, "Microsoft Xbox One S Controller (wired)"),
    (
        0x045E,
        0x02FD,
        "Microsoft Xbox One S Controller (Bluetooth firmware, USB)",
    ),
    (0x045E, 0x0B00, "Microsoft Xbox One Elite Series 2 (wired)"),
    (0x045E, 0x0B12, "Microsoft Xbox Series X|S Controller (wired)"),
];

const GIP_INIT_PACKET: [u8; 5] = [0x05, 0x20, 0x00, 0x01, 0x00];

const RAW_DUMP_PACKETS: usize = 10;

const ANALOG_EPS: f32 = 0.0025;
const ANALOG_MAX_HZ: f32 = 120.0;

// Stick tuning.
const CALIBRATION_WINDOW: Duration = Duration::from_millis(750);
const CALIBRATION_MAX_RADIUS: f32 = 0.25; // only learn center when stick is near center

// GIP layout detection.
// We scan for where the buttons/triggers/axes block actually starts inside the 0x20 packet payload.
// This must be robust because getting the offset wrong causes "everything triggers everything".
const LAYOUT_DETECT_WINDOW: Duration = Duration::from_secs(2);
const LAYOUT_MIN_SAMPLES: u32 = 120;

#[derive(Parser, Debug)]
#[command(name = "gipbridge", about = None, long_about = None)]
struct Cli {
    /// Override VID (e.g. 0x20D6). If set, --pid must also be set.
    #[arg(long)]
    vid: Option<String>,
    /// Override PID (e.g. 0x2079). If set, --vid must also be set.
    #[arg(long)]
    pid: Option<String>,
    /// Pipe filename under ~/Library/Application Support/Dolphin/Pipes/ (default: powera)
    #[arg(long, default_value = "powera")]
    pipe_name: String,
    /// Do not invert stick Y axes (default: invert)
    #[arg(long)]
    no_y_invert: bool,
    /// Radial stick deadzone in [0,1] (default: 0.12)
    #[arg(long, default_value_t = 0.12)]
    deadzone: f32,
    /// Print raw hex for every input packet received
    #[arg(long)]
    dump: bool,
    /// Print known supported controllers and exit.
    #[arg(long)]
    list: bool,
}

#[derive(Clone, Copy, Debug)]
struct BridgeConfig {
    y_invert: bool,
    deadzone: f32,
}

fn parse_u16_maybe_hex(s: &str) -> Result<u16> {
    let t = s.trim();
    let t = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")).unwrap_or(t);
    // Prefer hex if it contains hex letters, otherwise accept decimal too.
    let is_hexish = t.chars().any(|c| matches!(c, 'a'..='f' | 'A'..='F'));
    let v = if is_hexish {
        u32::from_str_radix(t, 16).map_err(|e| anyhow!("invalid hex value '{s}': {e}"))?
    } else if t.chars().all(|c| c.is_ascii_digit()) {
        t.parse::<u32>()
            .map_err(|e| anyhow!("invalid decimal value '{s}': {e}"))?
    } else {
        u32::from_str_radix(t, 16).map_err(|e| anyhow!("invalid value '{s}': {e}"))?
    };
    u16::try_from(v).map_err(|_| anyhow!("value out of range for u16: {s}"))
}

#[derive(Clone, Copy, Debug, Default)]
struct ParsedInput {
    buttons: u16,
    lt10: u16,
    rt10: u16,
    lx: i16,
    ly: i16,
    rx: i16,
    ry: i16,
}

fn le_u16(b: &[u8]) -> u16 {
    u16::from_le_bytes([b[0], b[1]])
}

fn le_i16(b: &[u8]) -> i16 {
    i16::from_le_bytes([b[0], b[1]])
}

fn parse_gip_input_packet(pkt: &[u8], payload_offset: usize) -> Result<ParsedInput> {
    if pkt.len() < 2 {
        bail!("packet too short");
    }
    if pkt[0] != 0x20 {
        bail!("unexpected command byte 0x{:02X}", pkt[0]);
    }

    let payload = &pkt[2..];
    if payload.len() < payload_offset + 14 {
        bail!(
            "payload too short: {} bytes (need at least {})",
            payload.len(),
            payload_offset + 14
        );
    }

    let p = &payload[payload_offset..];
    Ok(ParsedInput {
        buttons: le_u16(&p[0..2]),
        lt10: le_u16(&p[2..4]) & 0x03FF,
        rt10: le_u16(&p[4..6]) & 0x03FF,
        lx: le_i16(&p[6..8]),
        ly: le_i16(&p[8..10]),
        rx: le_i16(&p[10..12]),
        ry: le_i16(&p[12..14]),
    })
}

#[derive(Clone, Debug)]
struct LayoutStats {
    samples: u32,
    // triggers look like 10-bit values inside 16-bit words
    trig_hi_zero: u32,
    trig_activity: u32,
    // axes should vary smoothly; extreme saturation is rare
    axes_non_extreme: u32,
    axes_activity: u32,
    // buttons are a bitfield: only a few bits change occasionally (not constant noise)
    buttons_change_count: u32,
    buttons_popcount_sum: u32,
    last_buttons: Option<u16>,
}

impl LayoutStats {
    fn new() -> Self {
        Self {
            samples: 0,
            trig_hi_zero: 0,
            trig_activity: 0,
            axes_non_extreme: 0,
            axes_activity: 0,
            buttons_change_count: 0,
            buttons_popcount_sum: 0,
            last_buttons: None,
        }
    }

    fn observe(&mut self, payload: &[u8], off: usize) {
        if payload.len() < off + 14 {
            return;
        }
        let p = &payload[off..];
        let buttons = le_u16(&p[0..2]);
        let lt_raw = le_u16(&p[2..4]);
        let rt_raw = le_u16(&p[4..6]);
        let lx = le_i16(&p[6..8]);
        let ly = le_i16(&p[8..10]);
        let rx = le_i16(&p[10..12]);
        let ry = le_i16(&p[12..14]);

        self.samples += 1;

        let lt_hi = lt_raw & !0x03FF;
        let rt_hi = rt_raw & !0x03FF;
        if lt_hi == 0 && rt_hi == 0 {
            self.trig_hi_zero += 1;
        }
        // Consider triggers "active" if they aren't both at the same small value constantly.
        if (lt_raw & 0x03FF) > 4 || (rt_raw & 0x03FF) > 4 {
            self.trig_activity += 1;
        }

        let axes = [lx, ly, rx, ry];
        if axes.iter().all(|&v| v != i16::MIN && v != i16::MAX) {
            self.axes_non_extreme += 1;
        }
        // Activity: any axis moved noticeably (ignores tiny noise).
        if axes.iter().any(|&v| v.abs() > 512) {
            self.axes_activity += 1;
        }

        if let Some(prev) = self.last_buttons {
            if prev != buttons {
                self.buttons_change_count += 1;
            }
        }
        self.last_buttons = Some(buttons);
        self.buttons_popcount_sum += buttons.count_ones() as u32;
    }

    fn score(&self) -> i32 {
        if self.samples == 0 {
            return i32::MIN / 2;
        }
        let s = self.samples as f32;
        let trig_hi_zero = self.trig_hi_zero as f32 / s;
        let trig_activity = self.trig_activity as f32 / s;
        let axes_non_extreme = self.axes_non_extreme as f32 / s;
        let axes_activity = self.axes_activity as f32 / s;
        let btn_changes = self.buttons_change_count as f32 / s;
        let btn_pop_avg = self.buttons_popcount_sum as f32 / s;

        // Layout selection is deliberately heuristic. We want to find a stable window that behaves like:
        // - a sparse button bitfield (low popcount, occasional edges)
        // - 10-bit triggers in 16-bit words (high bits usually zero)
        // - 4× i16 axes (not saturated, some activity when sticks move)
        let mut score = 0.0f32;
        score += 6.0 * trig_hi_zero;
        score += 2.0 * trig_activity;
        score += 3.0 * axes_non_extreme;
        score += 3.0 * axes_activity;

        // Button change rate sweet spot: ~0.0..0.15 typical; penalize high noise.
        score += if btn_changes < 0.25 { 2.0 } else { -4.0 };
        // Prefer small average popcount.
        score += if btn_pop_avg < 4.0 { 2.0 } else { -3.0 };

        (score * 100.0) as i32
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct DolphinState {
    a: bool,
    b: bool,
    x: bool,
    y: bool,
    z: bool,
    start: bool,
    d_up: bool,
    d_down: bool,
    d_left: bool,
    d_right: bool,
    main_x: f32,
    main_y: f32,
    c_x: f32,
    c_y: f32,
    l: f32,
    r: f32,
}

fn clamp01(v: f32) -> f32 {
    if v.is_nan() {
        0.0
    } else if v < 0.0 {
        0.0
    } else if v > 1.0 {
        1.0
    } else {
        v
    }
}

fn clamp11(v: f32) -> f32 {
    if v.is_nan() {
        0.0
    } else if v < -1.0 {
        -1.0
    } else if v > 1.0 {
        1.0
    } else {
        v
    }
}

fn norm_i16_to_f1(v: i16) -> f32 {
    // Map i16 to [-1, 1]. Keep symmetric behavior around 0.
    if v == i16::MIN {
        -1.0
    } else {
        (v as f32) / 32767.0
    }
}

fn norm_trig10_to_01(v10: u16) -> f32 {
    clamp01((v10.min(1023) as f32) / 1023.0)
}

#[derive(Clone, Copy, Debug, Default)]
struct StickCalibration {
    lx0: f32,
    ly0: f32,
    rx0: f32,
    ry0: f32,
    n: u32,
}

fn apply_radial_deadzone(x: f32, y: f32, dz: f32) -> (f32, f32) {
    let r = (x * x + y * y).sqrt();
    if r <= dz {
        return (0.0, 0.0);
    }
    // Rescale so output starts at 0 at the edge of the deadzone.
    let k = (r - dz) / (1.0 - dz);
    let s = if r > 0.0 { k / r } else { 0.0 };
    (clamp11(x * s), clamp11(y * s))
}

fn parsed_to_dolphin(p: ParsedInput, cal: StickCalibration, cfg: BridgeConfig) -> DolphinState {
    let b = p.buttons;

    // GIP 0x20 button word layout (per Linux xpad / medusalix xone reference drivers):
    //   bit 2 Menu, bit 3 View, bits 4..7 A/B/X/Y, bits 8..11 d-pad U/D/L/R,
    //   bits 12..13 LB/RB, bits 14..15 LS/RS.
    let start = (b & 0x0004) != 0; // Menu  -> GameCube Start
    let view = (b & 0x0008) != 0; // View  -> GameCube Z
    let a = (b & 0x0010) != 0;
    let bb = (b & 0x0020) != 0;
    let x = (b & 0x0040) != 0;
    let y = (b & 0x0080) != 0;
    let d_up = (b & 0x0100) != 0;
    let d_down = (b & 0x0200) != 0;
    let d_left = (b & 0x0400) != 0;
    let d_right = (b & 0x0800) != 0;
    // Bumpers and stick clicks are intentionally not emitted to Dolphin's pipe protocol.
    // The pipe has no L_DIGITAL/R_DIGITAL distinct from analog L/R, and SET L/R is already
    // driven by the analog triggers below. Leave LB/RB/LS/RS unbound at the daemon level.

    let z = view;

    // Convert to [-1,1], subtract learned centers, apply deadzone, then map to [0,1].
    let lx = clamp11(norm_i16_to_f1(p.lx) - cal.lx0);
    let ly = clamp11(norm_i16_to_f1(p.ly) - cal.ly0);
    let rx = clamp11(norm_i16_to_f1(p.rx) - cal.rx0);
    let ry = clamp11(norm_i16_to_f1(p.ry) - cal.ry0);

    let (lx, ly) = apply_radial_deadzone(lx, ly, cfg.deadzone);
    let (rx, ry) = apply_radial_deadzone(rx, ry, cfg.deadzone);

    // Dolphin pipe uses 0..1. Keep Y inverted so up is larger (we can flip later if needed).
    let main_x = clamp01((lx + 1.0) * 0.5);
    let main_y = if cfg.y_invert {
        clamp01(((-ly) + 1.0) * 0.5)
    } else {
        clamp01(((ly) + 1.0) * 0.5)
    };
    let c_x = clamp01((rx + 1.0) * 0.5);
    let c_y = if cfg.y_invert {
        clamp01(((-ry) + 1.0) * 0.5)
    } else {
        clamp01(((ry) + 1.0) * 0.5)
    };

    let l = norm_trig10_to_01(p.lt10);
    let r = norm_trig10_to_01(p.rt10);

    DolphinState {
        a,
        b: bb,
        x,
        y,
        z,
        start,
        d_up,
        d_down,
        d_left,
        d_right,
        main_x,
        main_y,
        c_x,
        c_y,
        l,
        r,
    }
}

fn default_pipe_path(pipe_name: &str) -> Result<PathBuf> {
    if pipe_name.contains('/') {
        bail!("--pipe-name must not contain '/'");
    }
    if pipe_name.as_bytes().iter().any(|&b| b == 0) {
        bail!("--pipe-name must not contain NUL");
    }
    let home = std::env::var("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join("Dolphin")
        .join("Pipes")
        .join(pipe_name))
}

fn ensure_fifo(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create_dir_all({})", parent.display()))?;
    }

    if path.exists() {
        return Ok(());
    }

    let c_path = CString::new(path.as_os_str().as_bytes()).map_err(|_| anyhow!("pipe path contains interior NUL"))?;
    let rc = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
    if rc != 0 {
        bail!("mkfifo({}) failed: {}", path.display(), io::Error::last_os_error());
    }
    Ok(())
}

fn open_pipe_writer_nonblocking(path: &Path) -> Result<fs::File> {
    let c_path = CString::new(path.as_os_str().as_bytes()).map_err(|_| anyhow!("pipe path contains interior NUL"))?;
    let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_WRONLY | libc::O_NONBLOCK) };
    if fd < 0 {
        return Err(anyhow!(io::Error::last_os_error())).with_context(|| format!("open({})", path.display()));
    }
    Ok(unsafe { fs::File::from_raw_fd(fd) })
}

fn open_pipe_writer_wait(path: &Path) -> Result<fs::File> {
    loop {
        match open_pipe_writer_nonblocking(path) {
            Ok(f) => return Ok(f),
            Err(e) => {
                if let Some(ioe) = e.downcast_ref::<io::Error>() {
                    if ioe.raw_os_error() == Some(libc::ENXIO) {
                        // ENXIO is expected when the FIFO exists but no reader (Dolphin) is connected yet.
                        std::thread::sleep(Duration::from_millis(250));
                        continue;
                    }
                }
                return Err(e);
            }
        }
    }
}

fn write_line(file: &mut fs::File, s: &str) -> Result<()> {
    file.write_all(s.as_bytes()).context("write_all")?;
    file.write_all(b"\n").context("write_all(newline)")?;
    Ok(())
}

fn emit_button_delta(file: &mut fs::File, name: &str, prev: bool, now: bool) -> Result<()> {
    if prev == now {
        return Ok(());
    }
    if now {
        write_line(file, &format!("PRESS {name}"))
    } else {
        write_line(file, &format!("RELEASE {name}"))
    }
}

fn emit_state_delta(
    file: &mut fs::File,
    prev: DolphinState,
    now: DolphinState,
    last_analog_emit: &mut Instant,
) -> Result<()> {
    emit_button_delta(file, "A", prev.a, now.a)?;
    emit_button_delta(file, "B", prev.b, now.b)?;
    emit_button_delta(file, "X", prev.x, now.x)?;
    emit_button_delta(file, "Y", prev.y, now.y)?;
    emit_button_delta(file, "Z", prev.z, now.z)?;
    emit_button_delta(file, "START", prev.start, now.start)?;

    emit_button_delta(file, "D_UP", prev.d_up, now.d_up)?;
    emit_button_delta(file, "D_DOWN", prev.d_down, now.d_down)?;
    emit_button_delta(file, "D_LEFT", prev.d_left, now.d_left)?;
    emit_button_delta(file, "D_RIGHT", prev.d_right, now.d_right)?;

    let analog_due = last_analog_emit.elapsed() >= Duration::from_secs_f32(1.0 / ANALOG_MAX_HZ);
    let analog_changed = (prev.main_x - now.main_x).abs() > ANALOG_EPS
        || (prev.main_y - now.main_y).abs() > ANALOG_EPS
        || (prev.c_x - now.c_x).abs() > ANALOG_EPS
        || (prev.c_y - now.c_y).abs() > ANALOG_EPS
        || (prev.l - now.l).abs() > ANALOG_EPS
        || (prev.r - now.r).abs() > ANALOG_EPS;

    if analog_changed && analog_due {
        write_line(file, &format!("SET MAIN {:.4} {:.4}", clamp01(now.main_x), clamp01(now.main_y)))?;
        write_line(file, &format!("SET C {:.4} {:.4}", clamp01(now.c_x), clamp01(now.c_y)))?;
        write_line(file, &format!("SET L {:.4}", clamp01(now.l)))?;
        write_line(file, &format!("SET R {:.4}", clamp01(now.r)))?;
        *last_analog_emit = Instant::now();
    }

    Ok(())
}

fn find_interrupt_endpoints(handle: &mut DeviceHandle<RusbContext>) -> Result<(u8, u8, u8)> {
    let dev = handle.device();
    let cfg = dev
        .config_descriptor(0)
        .or_else(|_| dev.active_config_descriptor())
        .context("config_descriptor")?;

    for interface in cfg.interfaces() {
        for interface_desc in interface.descriptors() {
            let iface = interface_desc.interface_number();

            let mut ep_in: Option<u8> = None;
            let mut ep_out: Option<u8> = None;
            for ep in interface_desc.endpoint_descriptors() {
                if ep.transfer_type() != TransferType::Interrupt {
                    continue;
                }
                match ep.direction() {
                    Direction::In if ep_in.is_none() => ep_in = Some(ep.address()),
                    Direction::Out if ep_out.is_none() => ep_out = Some(ep.address()),
                    _ => {}
                }
            }

            if let (Some(in_ep), Some(out_ep)) = (ep_in, ep_out) {
                if handle.kernel_driver_active(iface).unwrap_or(false) {
                    let _ = handle.detach_kernel_driver(iface);
                }
                handle
                    .claim_interface(iface)
                    .with_context(|| format!("claim_interface({iface})"))?;
                return Ok((iface, in_ep, out_ep));
            }
        }
    }

    bail!("could not find interface with BOTH interrupt IN and interrupt OUT endpoints")
}

fn list_supported_and_exit() -> Result<()> {
    for (vid, pid, name) in KNOWN_CONTROLLERS {
        println!("0x{vid:04X}:0x{pid:04X}  {name}");
    }
    Ok(())
}

fn open_controller(
    ctx: &RusbContext,
    override_vid_pid: Option<(u16, u16)>,
) -> Result<(DeviceHandle<RusbContext>, u16, u16, &'static str)> {
    if let Some((vid, pid)) = override_vid_pid {
        let handle = ctx
            .open_device_with_vid_pid(vid, pid)
            .ok_or_else(|| anyhow!("device not found (VID=0x{vid:04X}, PID=0x{pid:04X})"))?;

        let dev = handle.device();
        let cfg = dev.active_config_descriptor().ok().map(|c| c.number()).unwrap_or(1);
        let _ = handle.set_active_configuration(cfg);
        let _ = handle.set_auto_detach_kernel_driver(true);
        return Ok((handle, vid, pid, "Override device"));
    }

    let devices = ctx.devices().context("usb device list")?;
    for dev in devices.iter() {
        let dd = match dev.device_descriptor() {
            Ok(d) => d,
            Err(_) => continue,
        };
        let vid = dd.vendor_id();
        let pid = dd.product_id();
        if let Some((_, _, name)) = KNOWN_CONTROLLERS
            .iter()
            .find(|(kvid, kpid, _)| *kvid == vid && *kpid == pid)
        {
            let handle = match dev.open() {
                Ok(h) => h,
                Err(_) => continue,
            };
            let cfg = dev.active_config_descriptor().ok().map(|c| c.number()).unwrap_or(1);
            let _ = handle.set_active_configuration(cfg);
            let _ = handle.set_auto_detach_kernel_driver(true);
            return Ok((handle, vid, pid, *name));
        }
    }

    eprintln!("No known supported controller found.");
    eprintln!("Connected USB devices (VID:PID):");
    for dev in ctx.devices().context("usb device list")?.iter() {
        if let Ok(dd) = dev.device_descriptor() {
            eprintln!("  0x{:04X}:0x{:04X}", dd.vendor_id(), dd.product_id());
        }
    }
    eprintln!();
    eprintln!("Try: gipbridge --vid 0xVVVV --pid 0xPPPP");
    bail!("no supported controller connected")
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.list {
        list_supported_and_exit()?;
        return Ok(());
    }

    let override_vid_pid = match (&cli.vid, &cli.pid) {
        (None, None) => None,
        (Some(_), None) | (None, Some(_)) => {
            bail!("--vid and --pid must be provided together");
        }
        (Some(vs), Some(ps)) => Some((parse_u16_maybe_hex(vs)?, parse_u16_maybe_hex(ps)?)),
    };

    let deadzone = cli.deadzone.clamp(0.0, 0.5);
    let cfg = BridgeConfig {
        y_invert: !cli.no_y_invert,
        deadzone,
    };

    println!("gipbridge starting…");
    println!("Note: on macOS this usually needs to run as root (sudo) to claim a vendor-class USB interface.");
    let _ = io::stdout().flush();

    let pipe_path = default_pipe_path(&cli.pipe_name)?;
    ensure_fifo(&pipe_path)?;
    println!("Dolphin Pipe path: {}", pipe_path.display());
    println!(
        "In Dolphin: Controllers → Standard Controller → Configure → Device: Pipe/0/{}",
        cli.pipe_name
    );
    println!("Waiting for Dolphin to open the pipe…");
    let _ = io::stdout().flush();
    let mut pipe = open_pipe_writer_wait(&pipe_path)?;
    println!("Pipe connected.");
    let _ = io::stdout().flush();

    let usb = RusbContext::new().context("libusb init")?;
    let (mut handle, vid, pid, name) = open_controller(&usb, override_vid_pid)?;
    println!("Opened {name} (VID=0x{vid:04X}, PID=0x{pid:04X})");
    let (iface, in_ep, out_ep) = find_interrupt_endpoints(&mut handle)?;
    println!("Claimed interface {iface}, interrupt IN=0x{in_ep:02X}, OUT=0x{out_ep:02X}");
    let _ = io::stdout().flush();

    println!("Sending GIP init packet…");
    let _ = io::stdout().flush();
    handle
        .write_interrupt(out_ep, &GIP_INIT_PACKET, Duration::from_millis(250))
        .context("write_interrupt(init)")?;

    println!("Reading input and writing Dolphin pipe commands…");
    let _ = io::stdout().flush();

    let dump_remaining = AtomicUsize::new(RAW_DUMP_PACKETS);
    let mut buf = [0u8; 64];
    let mut payload_offset: Option<usize> = None;
    let mut layout_stats: Vec<LayoutStats> = Vec::new();
    let layout_start = Instant::now();
    let mut prev_state = DolphinState::default();
    let mut last_analog_emit = Instant::now();
    let mut cal = StickCalibration::default();
    let cal_start = Instant::now();
    let mut emitting = false;

    loop {
        let n = match handle.read_interrupt(in_ep, &mut buf, Duration::from_secs(1)) {
            Ok(n) => n,
            Err(rusb::Error::Timeout) => continue,
            Err(e) => return Err(anyhow!(e)).context("read_interrupt"),
        };

        let pkt = &buf[..n];

        if cli.dump {
            println!("RAW {}", hex::encode(pkt));
        }

        // Only 0x20 carries the input-state payload we care about. Other command bytes are
        // part of the GIP session and should not be interpreted as controller state.
        if pkt.first().copied() != Some(0x20) {
            continue;
        }

        // Robust layout detection: collect stats briefly, then lock and emit inputs.
        // This avoids emitting garbage mappings while the offset is still ambiguous.
        if payload_offset.is_none() {
            let payload = &pkt[2..];
            let max_off = payload.len().saturating_sub(14);
            if layout_stats.len() <= max_off {
                layout_stats.resize_with(max_off + 1, LayoutStats::new);
            }
            for off in (0..=max_off).step_by(2) {
                layout_stats[off].observe(payload, off);
            }

            let samples = layout_stats.iter().map(|s| s.samples).max().unwrap_or(0);
            if layout_start.elapsed() >= LAYOUT_DETECT_WINDOW && samples >= LAYOUT_MIN_SAMPLES {
                if let Some((best_off, _best_score)) = layout_stats
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| i % 2 == 0)
                    .map(|(i, s)| (i, s.score()))
                    .max_by_key(|&(_i, sc)| sc)
                {
                    payload_offset = Some(best_off);
                    println!("Locked GIP payload offset: {best_off} bytes");
                    println!("(Now emitting Dolphin inputs)");
                    let _ = io::stdout().flush();
                    emitting = true;
                }
            }
        }

        let off = payload_offset.unwrap_or(0);
        match parse_gip_input_packet(pkt, off) {
            Ok(parsed) => {
                // Learn stick centers briefly at startup (only when near center).
                if cal_start.elapsed() <= CALIBRATION_WINDOW {
                    let lx = norm_i16_to_f1(parsed.lx);
                    let ly = norm_i16_to_f1(parsed.ly);
                    let rx = norm_i16_to_f1(parsed.rx);
                    let ry = norm_i16_to_f1(parsed.ry);

                    let lrad = (lx * lx + ly * ly).sqrt();
                    let rrad = (rx * rx + ry * ry).sqrt();
                    if lrad <= CALIBRATION_MAX_RADIUS && rrad <= CALIBRATION_MAX_RADIUS {
                        cal.n = cal.n.saturating_add(1);
                        let n = cal.n as f32;
                        // incremental mean
                        cal.lx0 += (lx - cal.lx0) / n;
                        cal.ly0 += (ly - cal.ly0) / n;
                        cal.rx0 += (rx - cal.rx0) / n;
                        cal.ry0 += (ry - cal.ry0) / n;
                    }
                }

                if !emitting {
                    continue;
                }

                let now_state = parsed_to_dolphin(parsed, cal, cfg);
                if let Err(e) = emit_state_delta(&mut pipe, prev_state, now_state, &mut last_analog_emit) {
                    if let Some(ioe) = e.downcast_ref::<io::Error>() {
                        if ioe.raw_os_error() == Some(libc::EPIPE) {
                            println!("Pipe closed by reader; waiting for Dolphin to reconnect…");
                            pipe = open_pipe_writer_wait(&pipe_path)?;
                            println!("Pipe reconnected.");
                        } else {
                            return Err(e);
                        }
                    } else {
                        return Err(e);
                    }
                }
                prev_state = now_state;
            }
            Err(e) => {
                let left = dump_remaining.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| {
                    if v == 0 { None } else { Some(v - 1) }
                });
                if left.is_ok() {
                    eprintln!("Parse error: {e:#}");
                    eprintln!("Raw packet ({} bytes): {}", pkt.len(), hex::encode(pkt));
                }
            }
        }
    }
}

