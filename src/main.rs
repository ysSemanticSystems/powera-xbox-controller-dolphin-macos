use anyhow::{anyhow, bail, Context, Result};
use rusb::{Context as RusbContext, DeviceHandle, Direction, TransferType, UsbContext};
use std::ffi::CString;
use std::fs;
use std::io::{self, Write};
use std::os::fd::FromRawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const VID: u16 = 0x20D6;
const PID: u16 = 0x2079;

const GIP_INIT_PACKET: [u8; 5] = [0x05, 0x20, 0x00, 0x01, 0x00];

const RAW_DUMP_PACKETS: usize = 10;

const ANALOG_EPS: f32 = 0.0025;
const ANALOG_MAX_HZ: f32 = 120.0;

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

fn detect_payload_offset(pkt: &[u8]) -> Option<usize> {
    if pkt.len() < 2 || pkt[0] != 0x20 {
        return None;
    }
    let payload = &pkt[2..];
    let candidates = [0usize, 2, 4, 6, 8, 10, 12, 14, 16];
    let mut best: Option<(usize, i32)> = None;

    for &off in &candidates {
        if payload.len() < off + 14 {
            continue;
        }
        let p = &payload[off..];
        let lt_raw = le_u16(&p[2..4]);
        let rt_raw = le_u16(&p[4..6]);
        let lt_hi = lt_raw & !0x03FF;
        let rt_hi = rt_raw & !0x03FF;

        let mut score = 0i32;
        if lt_hi == 0 {
            score += 2;
        }
        if rt_hi == 0 {
            score += 2;
        }
        let buttons = le_u16(&p[0..2]);
        if buttons != 0xFFFF {
            score += 1;
        }
        let _ = le_i16(&p[6..8]);
        let _ = le_i16(&p[8..10]);
        let _ = le_i16(&p[10..12]);
        let _ = le_i16(&p[12..14]);
        score += 1;

        match best {
            None => best = Some((off, score)),
            Some((_, best_score)) if score > best_score => best = Some((off, score)),
            _ => {}
        }
    }

    best.map(|(off, _)| off)
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

fn norm_i16_to_01(v: i16) -> f32 {
    clamp01((v as f32 + 32768.0) / 65535.0)
}

fn norm_trig10_to_01(v10: u16) -> f32 {
    clamp01((v10.min(1023) as f32) / 1023.0)
}

fn parsed_to_dolphin(p: ParsedInput) -> DolphinState {
    let b = p.buttons;

    let d_up = (b & 0x0001) != 0;
    let d_down = (b & 0x0002) != 0;
    let d_left = (b & 0x0004) != 0;
    let d_right = (b & 0x0008) != 0;

    let start = (b & 0x0010) != 0;
    let view = (b & 0x0020) != 0;

    let a = (b & 0x1000) != 0;
    let bb = (b & 0x2000) != 0;
    let x = (b & 0x4000) != 0;
    let y = (b & 0x8000) != 0;

    let z = view;

    let main_x = norm_i16_to_01(p.lx);
    let main_y = 1.0 - norm_i16_to_01(p.ly);
    let c_x = norm_i16_to_01(p.rx);
    let c_y = 1.0 - norm_i16_to_01(p.ry);

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

fn default_pipe_path() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join("Dolphin")
        .join("Pipes")
        .join("powera"))
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

    bail!(
        "could not find interface with BOTH interrupt IN and interrupt OUT endpoints (VID=0x{VID:04X}, PID=0x{PID:04X})"
    )
}

fn open_controller(ctx: &RusbContext) -> Result<DeviceHandle<RusbContext>> {
    let handle = ctx
        .open_device_with_vid_pid(VID, PID)
        .ok_or_else(|| anyhow!("device not found (VID=0x{VID:04X}, PID=0x{PID:04X})"))?;

    let dev = handle.device();
    let cfg = dev.active_config_descriptor().ok().map(|c| c.number()).unwrap_or(1);
    let _ = handle.set_active_configuration(cfg);
    let _ = handle.set_auto_detach_kernel_driver(true);
    Ok(handle)
}

fn main() -> Result<()> {
    println!("Opening PowerA controller (VID=0x{VID:04X}, PID=0x{PID:04X})…");
    println!("Note: on macOS this usually needs to run as root (sudo) to claim a vendor-class USB interface.");
    let _ = io::stdout().flush();

    let pipe_path = default_pipe_path()?;
    ensure_fifo(&pipe_path)?;
    println!("Dolphin Pipe path: {}", pipe_path.display());
    println!("In Dolphin: Controllers → Standard Controller → Configure → Device: Pipe/0/powera");
    println!("Waiting for Dolphin to open the pipe…");
    let _ = io::stdout().flush();
    let mut pipe = open_pipe_writer_wait(&pipe_path)?;
    println!("Pipe connected.");
    let _ = io::stdout().flush();

    let usb = RusbContext::new().context("libusb init")?;
    let mut handle = open_controller(&usb)?;
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
    let mut prev_state = DolphinState::default();
    let mut last_analog_emit = Instant::now();

    loop {
        let n = match handle.read_interrupt(in_ep, &mut buf, Duration::from_secs(1)) {
            Ok(n) => n,
            Err(rusb::Error::Timeout) => continue,
            Err(e) => return Err(anyhow!(e)).context("read_interrupt"),
        };

        let pkt = &buf[..n];

        if payload_offset.is_none() {
            payload_offset = detect_payload_offset(pkt);
            if let Some(off) = payload_offset {
                println!("Auto-detected GIP payload offset: {off} bytes");
            }
        }

        let off = payload_offset.unwrap_or(0);
        match parse_gip_input_packet(pkt, off) {
            Ok(parsed) => {
                let now_state = parsed_to_dolphin(parsed);
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

