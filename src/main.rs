#![windows_subsystem = "windows"]

mod app;
mod capture;
mod config;
mod input;
mod localinput;
mod net;
mod proto;
mod shortid;
mod transfer;
mod updater;

use eframe::egui;

const ICON_RGBA: &[u8] = include_bytes!("../assets/icon_128.rgba");

pub fn icon() -> egui::IconData {
    egui::IconData { rgba: ICON_RGBA.to_vec(), width: 128, height: 128 }
}

fn main() -> eframe::Result {
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
                    .with_inner_size([1100.0, 700.0])
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
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("RemoteDesk")
            .with_icon(icon())
            .with_inner_size([960.0, 600.0])
            .with_min_inner_size([720.0, 460.0]),
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
    let host_cfg = config::Config::fresh();
    let password = host_cfg.password.clone();
    let host = net::Net::start(host_cfg, ctx.clone()).expect("start host");
    let host_id = host.shared.lock().unwrap().my_id.clone();
    let mut ctl_cfg = config::Config::fresh();
    ctl_cfg.contacts.push(config::Contact { id: host_id.clone(), short_id: String::new(), name: "host".into(), password: String::new() });
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

    let resolve_start = Instant::now();
    let v = ctl.connect(shortid::format(&short), password);
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
    std::fs::write(log_path, log).ok();
    std::process::exit(0);
}
