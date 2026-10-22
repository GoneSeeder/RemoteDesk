#![windows_subsystem = "windows"]

mod app;
mod capture;
mod config;
mod input;
mod localinput;
mod net;
mod proto;
mod shortid;
mod singleinstance;
mod transfer;
mod autostart;
mod security;
mod updater;
mod winclip;

use eframe::egui;

/// New logo: the exe file, taskbar and window title-bar icon.
const WINDOW_ICON_RGBA: &[u8] = include_bytes!("../assets/window_icon_128.rgba");
/// Old logo: kept for the small header logo drawn inside the app.
const HEADER_ICON_RGBA: &[u8] = include_bytes!("../assets/icon_128.rgba");

pub fn icon() -> egui::IconData {
    egui::IconData { rgba: WINDOW_ICON_RGBA.to_vec(), width: 128, height: 128 }
}

/// The in-app header logo (kept as the old mark at the user's request).
pub fn header_icon() -> egui::IconData {
    egui::IconData { rgba: HEADER_ICON_RGBA.to_vec(), width: 128, height: 128 }
}

fn main() -> eframe::Result {
    // Dev check: `remotedesk --codec-bench` encodes+decodes moving 1080p frames and reports speed.
    #[cfg(debug_assertions)]
    if std::env::args().nth(1).as_deref() == Some("--codec-bench") {
        use openh264::decoder::Decoder;
        use openh264::encoder::{BitRate, Encoder, EncoderConfig, FrameRate, RateControlMode, UsageType};
        use openh264::formats::{RgbaSliceU8, YUVBuffer};
        let (w, h) = (1920usize, 1080usize);
        let config = EncoderConfig::new()
            .bitrate(BitRate::from_bps(6_000_000))
            .max_frame_rate(FrameRate::from_hz(30.0))
            .usage_type(UsageType::ScreenContentRealTime)
            .rate_control_mode(RateControlMode::Bitrate);
        let mut enc = Encoder::with_api_config(openh264::OpenH264API::from_source(), config).unwrap();
        let mut dec = Decoder::new().unwrap();
        let mut rgba = vec![40u8; w * h * 4];
        let (mut bytes, mut enc_ms, mut dec_ms, frames) = (0usize, 0f64, 0f64, 90);
        let mut out = vec![0u8; w * h * 4];
        for f in 0..frames {
            // A window-sized block sliding across a static desktop, like dragging a window.
            for y in 300..700 {
                for x in 0..600 {
                    let px = ((x + f * 12) % w + y * w) * 4;
                    rgba[px..px + 4].copy_from_slice(&[(f * 3) as u8, 120, 200, 255]);
                }
            }
            let t = std::time::Instant::now();
            let yuv = YUVBuffer::from_rgba8_source(RgbaSliceU8::new(&rgba, (w, h)));
            let data = enc.encode(&yuv).unwrap().to_vec();
            enc_ms += t.elapsed().as_secs_f64() * 1000.0;
            bytes += data.len();
            let t = std::time::Instant::now();
            if let Ok(Some(img)) = dec.decode(&data) {
                img.write_rgba8(&mut out);
            }
            dec_ms += t.elapsed().as_secs_f64() * 1000.0;
        }
        println!(
            "{frames} frames 1080p: encode {:.1} ms/frame, decode {:.1} ms/frame, avg {:.0} KB/frame, {:.0} KB/s at 30fps",
            enc_ms / frames as f64,
            dec_ms / frames as f64,
            bytes as f64 / frames as f64 / 1024.0,
            bytes as f64 / frames as f64 * 30.0 / 1024.0
        );
        std::process::exit(0);
    }

    // Dev check: `remotedesk --clip-test` round-trips a file list through the clipboard.
    #[cfg(debug_assertions)]
    if std::env::args().nth(1).as_deref() == Some("--clip-test") {
        let f1 = std::env::temp_dir().join("rd-clip-a.txt");
        let f2 = std::env::temp_dir().join("rd-clip-b.txt");
        std::fs::write(&f1, "a").ok();
        std::fs::write(&f2, "b").ok();
        let set = winclip::set_files(&[f1.clone(), f2.clone()]);
        let got = winclip::get_files();
        let ok = got.as_ref().map(|v| v.len() == 2 && v[0] == f1 && v[1] == f2).unwrap_or(false);
        println!("set={set} get={got:?} roundtrip_ok={ok}");
        std::process::exit(if ok { 0 } else { 1 });
    }

    // Release tooling: `remotedesk --sign <exe> <key file>` writes <exe>.sig.
    if let [_, flag, exe, key] = std::env::args().collect::<Vec<_>>().as_slice() {
        if flag == "--sign" {
            match updater::sign_cli(exe, key) {
                Ok(()) => std::process::exit(0),
                Err(e) => {
                    eprintln!("sign failed: {e:#}");
                    std::process::exit(1);
                }
            }
        }
    }
    #[cfg(debug_assertions)]
    if let [_, flag, log] = std::env::args().collect::<Vec<_>>().as_slice() {
        if flag == "--selftest" {
            selftest(log);
        }
    }
    #[cfg(debug_assertions)]
    if let [_, flag, out, mode] = std::env::args().collect::<Vec<_>>().as_slice() {
        if flag == "--preview" {
            let options = eframe::NativeOptions {
                viewport: egui::ViewportBuilder::default()
                    .with_inner_size([
                        std::env::var("RD_W").ok().and_then(|v| v.parse().ok()).unwrap_or(1100.0),
                        std::env::var("RD_H").ok().and_then(|v| v.parse().ok()).unwrap_or(700.0),
                    ])
                    .with_icon(icon())
                    .with_position([-32000.0, -32000.0])
                    .with_taskbar(false)
                    .with_active(false),
                ..Default::default()
            };
            let (out, mode) = (out.clone(), mode.clone());
            return eframe::run_native("RemoteDesk preview", options, Box::new(move |cc| Ok(Box::new(app::App::new_preview(cc, out, &mode)))));
        }
    }
    // If RemoteDesk is already running, tell it to show its window and exit instead of adding
    // a second instance.
    if !singleinstance::acquire() {
        return Ok(());
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("RemoteDesk")
            .with_icon(icon())
            .with_inner_size([960.0, 600.0])
            .with_min_inner_size([480.0, 440.0]),
        ..Default::default()
    };
    eframe::run_native("RemoteDesk", options, Box::new(|cc| Ok(Box::new(app::App::new(cc)?))))
}

/// `remotedesk --selftest <log file>`: runs a host and a controller in one process, connects them
/// over the real network and writes what happened to the log file.
#[cfg(debug_assertions)]
fn selftest(log_path: &str) {
    use std::fmt::Write;
    use std::time::{Duration, Instant};

    let mut log = String::new();
    let ctx = egui::Context::default();
    // Password-only by default here; the ask-on-connect prompt is tested on its own below.
    let mut host_cfg = config::Config::fresh();
    host_cfg.ask_on_connect = false;
    let password = host_cfg.password.clone();
    let host = net::Net::start(host_cfg, ctx.clone()).expect("start host");
    let host_id = host.shared.lock().unwrap().my_id.clone();
    let mut ctl_cfg = config::Config::fresh();
    ctl_cfg.contacts.push(config::Contact { id: host_id.clone(), name: "host".into(), ..Default::default() });
    let ctl = net::Net::start(ctl_cfg, ctx).expect("start controller");
    writeln!(log, "host id: {host_id}").ok();
    if let (Ok(exe), Ok(sig)) = (std::fs::read("RemoteDesk.exe"), std::fs::read_to_string("RemoteDesk.exe.sig")) {
        let mut tampered = exe.clone();
        tampered[5000] ^= 1;
        writeln!(log, "update signature: genuine ok={} tampered rejected={}", updater::verify(&exe, &sig).is_ok(), updater::verify(&tampered, &sig).is_err()).ok();
    }
    let machine = config::Config::load().secret_key().public();
    let again = config::Config::load().secret_key().public();
    writeln!(log, "machine key stable across loads: {}", machine == again).ok();
    writeln!(log, "this machine's permanent short id: {}", shortid::format(&shortid::short_id_for(&machine))).ok();

    let wait = |what: &dyn Fn() -> bool, secs: u64| {
        let start = Instant::now();
        while !what() && start.elapsed() < Duration::from_secs(secs) {
            std::thread::sleep(Duration::from_millis(100));
        }
        start.elapsed()
    };

    let t = wait(&|| host.shared.lock().unwrap().online && ctl.shared.lock().unwrap().online, 30);
    writeln!(log, "both online after {t:?}").ok();

    let t = wait(&|| host.shared.lock().unwrap().short_id.is_some(), 30);
    let short = host.shared.lock().unwrap().short_id.clone().unwrap_or_default();
    writeln!(log, "host short id {} after {t:?}", shortid::format(&short)).ok();
    // Give the alias endpoint time to publish its record.
    std::thread::sleep(Duration::from_secs(8));
    let v = ctl.connect(shortid::format(&short), "wrong-password".into());
    let t = wait(&|| v.lock().unwrap().status != net::ViewStatus::Connecting, 60);
    let status = match &v.lock().unwrap().status {
        net::ViewStatus::Failed(m) => format!("Failed({m})"),
        net::ViewStatus::Connected => "Connected (BAD)".into(),
        _ => "other".into(),
    };
    writeln!(log, "wrong password -> {status} after {t:?}").ok();

    // Cancel while connecting: nothing may keep streaming afterwards.
    let c = ctl.connect(shortid::format(&short), password.clone());
    std::thread::sleep(Duration::from_millis(300));
    c.lock().unwrap().disconnect();
    std::thread::sleep(Duration::from_secs(12));
    {
        let cv = c.lock().unwrap();
        writeln!(
            log,
            "cancel while connecting: status_closed={} frames_after_cancel={} pending_patches={} host_sessions={}",
            cv.status == net::ViewStatus::Closed,
            cv.frames,
            cv.patches.len(),
            host.shared.lock().unwrap().incoming.len()
        )
        .ok();
    }
    let resolve_start = Instant::now();
    let v = ctl.connect(shortid::format(&short), password.clone());
    let t = wait(&|| v.lock().unwrap().frames >= 1, 60);
    writeln!(log, "first frame via short id after {t:?} (total {:?}), status ok={}", resolve_start.elapsed(), v.lock().unwrap().status == net::ViewStatus::Connected).ok();
    writeln!(log, "resolved peer id matches: {}", v.lock().unwrap().peer_id == host_id).ok();
    let start = Instant::now();
    std::thread::sleep(Duration::from_secs(5));
    {
        let vv = v.lock().unwrap();
        let px: usize = vv.patches.iter().map(|p| p.size[0] * p.size[1]).sum();
        let screen = vv.patches.first().map(|p| p.screen);
        writeln!(
            log,
            "frames={} bytes={} direct={} screen={:?} patch_pixels={} in {:?}",
            vv.frames, vv.bytes, vv.direct, screen, px, start.elapsed()
        )
        .ok();
        writeln!(log, "host sees incoming sessions: {}", host.shared.lock().unwrap().incoming.len()).ok();
        // Save the decoded picture next to the log so its quality can be inspected.
        if let Some(p) = vv.patches.last() {
            let mut jpeg = Vec::new();
            if jpeg_encoder::Encoder::new(&mut jpeg, 95)
                .encode(&p.rgba, p.size[0] as u16, p.size[1] as u16, jpeg_encoder::ColorType::Rgba)
                .is_ok()
            {
                std::fs::write(format!("{log_path}.frame.jpg"), jpeg).ok();
            }
        }
    }

    // Chat both ways.
    v.lock().unwrap().send_chat("hi from controller".into());
    if let Some(inc) = host.shared.lock().unwrap().incoming.first_mut() {
        inc.send_chat("hi from host".into());
    }
    wait(
        &|| {
            v.lock().unwrap().chat.len() >= 2
                && host.shared.lock().unwrap().incoming.first().map(|i| i.chat.len()).unwrap_or(0) >= 2
        },
        20,
    );
    {
        let ctl_chat = v.lock().unwrap().chat.len();
        let host_chat = host.shared.lock().unwrap().incoming.first().map(|i| i.chat.len()).unwrap_or(0);
        let ctl_got_host = v.lock().unwrap().chat.iter().any(|m| !m.from_me && m.text == "hi from host");
        writeln!(log, "chat: controller has {ctl_chat} msgs, host has {host_chat} msgs, controller got host msg={ctl_got_host}").ok();
    }

    // File transfer both ways, compared byte for byte.
    let dir = std::env::temp_dir().join("remotedesk-selftest");
    std::fs::create_dir_all(&dir).ok();
    let payload: Vec<u8> = (0..3_000_000u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
    let src = dir.join("selftest-payload.bin");
    std::fs::write(&src, &payload).ok();
    let conn = v.lock().unwrap().connection().expect("connection");
    let start = Instant::now();
    ctl.send_files(conn, "host".into(), vec![src.clone()]);
    let host_conn = host.shared.lock().unwrap().incoming[0].conn.clone();
    host.send_files(host_conn, "ctl".into(), vec![src.clone()]);
    let done = |n: &net::Net| {
        let t = n.shared.lock().unwrap().transfers.clone();
        let t = t.lock().unwrap();
        t.iter().filter(|t| t.incoming && t.finished).filter_map(|t| t.path.clone().map(|p| (p, t.error.clone()))).collect::<Vec<_>>()
    };
    wait(&|| !done(&host).is_empty() && !done(&ctl).is_empty(), 60);
    for (who, n) in [("host received", &host), ("controller received", &ctl)] {
        for (path, err) in done(n) {
            let same = std::fs::read(&path).map(|b| b == payload).unwrap_or(false);
            writeln!(log, "{who}: identical={same} error={err:?} in {:?}", start.elapsed()).ok();
            std::fs::remove_file(&path).ok();
        }
    }
    std::fs::remove_dir_all(&dir).ok();
    v.lock().unwrap().disconnect();
    std::thread::sleep(Duration::from_secs(2));
    writeln!(log, "after disconnect host incoming sessions: {}", host.shared.lock().unwrap().incoming.len()).ok();

    let t = wait(&|| matches!(ctl.shared.lock().unwrap().presence.get(&host_id), Some(net::Presence::Online { .. })), 30);
    let presence = match ctl.shared.lock().unwrap().presence.get(&host_id) {
        Some(net::Presence::Online { name }) => format!("Online({name})"),
        Some(net::Presence::Offline) => "Offline".into(),
        None => "unknown".into(),
    };
    writeln!(log, "presence of host: {presence} after {t:?}").ok();

    // Accept/deny prompt: with ask_on_connect on, a correct password still needs the host to
    // allow it; denying yields the DENIED error on the controller.
    host.shared.lock().unwrap().cfg.ask_on_connect = true;
    let d = ctl.connect(shortid::format(&short), password.clone());
    wait(&|| !host.shared.lock().unwrap().requests.is_empty(), 30);
    let got_request = !host.shared.lock().unwrap().requests.is_empty();
    if let Some(req) = host.shared.lock().unwrap().requests.first_mut() {
        req.answer(None);
    }
    wait(&|| matches!(d.lock().unwrap().status, net::ViewStatus::Failed(_)), 30);
    let denied = matches!(&d.lock().unwrap().status, net::ViewStatus::Failed(m) if m == proto::DENIED);
    writeln!(log, "ask-on-connect: prompt shown={got_request} deny produced DENIED={denied}").ok();

    // Grant view-only (control off): the controller should connect and see perms.control=false.
    let vo = ctl.connect(shortid::format(&short), password.clone());
    wait(&|| !host.shared.lock().unwrap().requests.is_empty(), 30);
    if let Some(req) = host.shared.lock().unwrap().requests.first_mut() {
        req.answer(Some(proto::Permissions { control: false, clipboard: true, files: false, admin: false }));
    }
    wait(&|| vo.lock().unwrap().status == net::ViewStatus::Connected, 30);
    let p = vo.lock().unwrap().perms;
    writeln!(log, "view-only grant: connected={} control={} files={}", vo.lock().unwrap().status == net::ViewStatus::Connected, p.control, p.files).ok();
    vo.lock().unwrap().disconnect();
    host.shared.lock().unwrap().cfg.ask_on_connect = false;

    // Lockout: 5 wrong passwords lock the peer, so the 6th attempt is refused as "locked".
    let mut saw_locked = false;
    for i in 0..6 {
        let bad = ctl.connect(shortid::format(&short), format!("nope-{i}"));
        wait(&|| matches!(bad.lock().unwrap().status, net::ViewStatus::Failed(_)), 30);
        if let net::ViewStatus::Failed(msg) = &bad.lock().unwrap().status {
            if msg == proto::LOCKED {
                saw_locked = true;
            }
        }
    }
    writeln!(log, "lockout after repeated wrong passwords triggered: {saw_locked}").ok();

    // Access log exists and records events.
    let log_file = config::config_dir().join("access.log");
    let entries = std::fs::read_to_string(&log_file).unwrap_or_default();
    writeln!(
        log,
        "access log has connected={} wrong-password={} lines",
        entries.matches("\tconnected\t").count(),
        entries.matches("\twrong-password\t").count()
    )
    .ok();

    std::fs::write(log_path, log).ok();
    std::process::exit(0);
}
