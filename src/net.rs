//! Peer-to-peer networking over iroh. Peers are addressed by their public key; iroh finds them via
//! n0's free discovery service, hole-punches a direct UDP path when possible and otherwise falls
//! back to n0's free public relays. Everything is end-to-end encrypted (QUIC/TLS).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, bail};
use eframe::egui;
use iroh::endpoint::{Connection, presets};
use iroh::address_lookup::UserData;
use iroh::{Endpoint, EndpointId};
use n0_future::StreamExt;
use tokio::sync::mpsc;
use tokio::time::{sleep, timeout};
use zune_jpeg::JpegDecoder;
use zune_jpeg::zune_core::{bytestream::ZCursor, colorspace::ColorSpace, options::DecoderOptions};

use crate::capture::{Capturer, Geometry};
use crate::config::Config;
use crate::input::{self, Cmd};
use crate::proto::*;
use crate::shortid;
use crate::transfer::{self, Clipboard, Transfers};

const PRESENCE_INTERVAL: Duration = Duration::from_secs(10);
const PING_TIMEOUT: Duration = Duration::from_secs(9);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(25);

#[derive(Clone, PartialEq)]
pub enum Presence {
    Online { name: String },
    Offline,
}

/// Someone currently controlling this machine.
pub struct Incoming {
    session: u64,
    pub name: String,
    pub conn: Connection,
}

pub struct Shared {
    pub cfg: Config,
    pub my_id: String,
    /// 10-digit address; `None` until it has been computed.
    pub short_id: Option<String>,
    pub online: bool,
    pub presence: HashMap<String, Presence>,
    pub incoming: Vec<Incoming>,
    pub transfers: Transfers,
    pub clipboard: Clipboard,
}

#[derive(Clone, PartialEq)]
pub enum ViewStatus {
    Connecting,
    Connected,
    Failed(String),
    Closed,
}

pub struct Patch {
    pub screen: [usize; 2],
    pub pos: [usize; 2],
    pub size: [usize; 2],
    pub rgba: Vec<u8>,
}

/// State of one outgoing remote-control session, shared between the network task and the GUI.
pub struct Viewer {
    /// What the user typed until resolved, then the peer's full public key.
    pub peer_id: String,
    pub peer_short: String,
    pub peer_name: String,
    pub status: ViewStatus,
    pub direct: bool,
    /// The remote user is using their own mouse/keyboard right now.
    pub remote_busy: bool,
    pub patches: Vec<Patch>,
    pub bytes: u64,
    pub frames: u64,
    pub input: Option<mpsc::UnboundedSender<Input>>,
    conn: Option<Connection>,
}

impl Viewer {
    pub fn connection(&self) -> Option<Connection> {
        self.conn.clone()
    }

    pub fn send(&self, input: Input) {
        if let Some(tx) = &self.input {
            let _ = tx.send(input);
        }
    }

    pub fn disconnect(&mut self) {
        if let Some(conn) = self.conn.take() {
            conn.close(0u32.into(), b"bye");
        }
        self.input = None;
        self.status = ViewStatus::Closed;
    }
}

pub struct Net {
    rt: tokio::runtime::Runtime,
    endpoint: Endpoint,
    pub shared: Arc<Mutex<Shared>>,
    ctx: egui::Context,
}

impl Net {
    pub fn start(cfg: Config, ctx: egui::Context) -> Result<Net> {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
        let endpoint = rt.block_on(
            Endpoint::builder(presets::N0)
                .secret_key(cfg.secret_key())
                .alpns(vec![ALPN_CTL.to_vec(), ALPN_PING.to_vec()])
                .bind(),
        )?;
        let my_id = endpoint.id().to_string();
        let cached_short = (!cfg.short_id.is_empty() && cfg.short_id_key == my_id).then(|| cfg.short_id.clone());
        let shared = Arc::new(Mutex::new(Shared {
            cfg,
            my_id,
            short_id: cached_short,
            online: false,
            presence: HashMap::new(),
            incoming: Vec::new(),
            transfers: Transfers::default(),
            clipboard: Clipboard::start(),
        }));
        let input = input::spawn();

        let (ep, sh, cx) = (endpoint.clone(), shared.clone(), ctx.clone());
        rt.spawn(async move {
            ep.online().await;
            sh.lock().unwrap().online = true;
            cx.request_repaint();
        });

        let (ep, sh, cx) = (endpoint.clone(), shared.clone(), ctx.clone());
        rt.spawn(accept_loop(ep, sh, input, cx));

        let (id, sh, cx) = (endpoint.id(), shared.clone(), ctx.clone());
        rt.spawn(async move {
            if let Err(e) = publish_short_id(id, sh, cx).await {
                eprintln!("short id: {e:#}");
            }
        });

        let net = Net { rt, endpoint, shared, ctx };
        net.spawn_presence_loop();
        Ok(net)
    }

    fn spawn_presence_loop(&self) {
        let (ep, sh, cx) = (self.endpoint.clone(), self.shared.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            loop {
                let ids: Vec<String> = sh.lock().unwrap().cfg.contacts.iter().map(|c| c.id.clone()).collect();
                for id in ids {
                    tokio::spawn(check_presence(ep.clone(), id, sh.clone(), cx.clone()));
                }
                sleep(PRESENCE_INTERVAL).await;
            }
        });
    }

    pub fn runtime(&self) -> &tokio::runtime::Runtime {
        &self.rt
    }

    pub fn send_files(&self, conn: Connection, peer: String, paths: Vec<std::path::PathBuf>) {
        let transfers = self.shared.lock().unwrap().transfers.clone();
        for path in paths.into_iter().filter(|p| p.is_file()) {
            self.rt.spawn(transfer::send_file(conn.clone(), path, peer.clone(), transfers.clone(), self.ctx.clone()));
        }
    }

    pub fn connect(&self, peer_id: String, password: String) -> Arc<Mutex<Viewer>> {
        let viewer = Arc::new(Mutex::new(Viewer {
            peer_id: peer_id.clone(),
            peer_short: String::new(),
            peer_name: String::new(),
            status: ViewStatus::Connecting,
            direct: false,
            remote_busy: false,
            patches: Vec::new(),
            bytes: 0,
            frames: 0,
            input: None,
            conn: None,
        }));
        let my_name = self.shared.lock().unwrap().cfg.device_name.clone();
        let (ep, v, cx, sh) = (self.endpoint.clone(), viewer.clone(), self.ctx.clone(), self.shared.clone());
        self.rt.spawn(async move {
            let result = run_viewer(&ep, &peer_id, password, my_name, &v, &cx, &sh).await;
            let mut v = v.lock().unwrap();
            if v.status == ViewStatus::Connecting {
                let msg = match result {
                    Err(e) => friendly_error(&e),
                    Ok(()) => "การเชื่อมต่อถูกปิด".into(),
                };
                v.status = ViewStatus::Failed(msg);
            } else {
                v.status = ViewStatus::Closed;
            }
            v.input = None;
            cx.request_repaint();
        });
        viewer
    }
}

fn friendly_error(e: &anyhow::Error) -> String {
    let text = e.to_string();
    if text.contains("deadline") || text.contains("elapsed") {
        "เชื่อมต่อไม่ได้ (เครื่องปลายทางอาจปิดอยู่หรือออฟไลน์)".into()
    } else if text.contains("No addressing information") || text.contains(NOT_FOUND) {
        NOT_FOUND.into()
    } else if text.contains("invalid") && text.contains("key") || text.contains("parse") {
        "ID ไม่ถูกต้อง".into()
    } else {
        text
    }
}

async fn run_viewer(
    ep: &Endpoint,
    peer_id: &str,
    password: String,
    my_name: String,
    viewer: &Arc<Mutex<Viewer>>,
    ctx: &egui::Context,
    shared: &Arc<Mutex<Shared>>,
) -> Result<()> {
    let peer = resolve(ep, peer_id).await?;
    viewer.lock().unwrap().peer_id = peer.to_string();
    let conn = timeout(CONNECT_TIMEOUT, ep.connect(peer, ALPN_CTL)).await??;
    let (mut send, mut recv) = conn.open_bi().await?;
    write_json(&mut send, &Hello { name: my_name }).await?;
    let challenge: Challenge = timeout(CONNECT_TIMEOUT, read_json(&mut recv)).await??;
    let proof = auth_proof(&password, &challenge.nonce, &peer.to_string(), &ep.id().to_string());
    write_json(&mut send, &Auth { proof }).await?;
    let welcome: Welcome = timeout(CONNECT_TIMEOUT, read_json(&mut recv)).await??;
    if !welcome.ok {
        bail!(welcome.msg);
    }
    let mut frames = conn.accept_uni().await?;
    let peer_name = welcome.name.clone();

    let (tx, mut rx) = mpsc::unbounded_channel::<Input>();
    let (clipboard, transfers) = {
        let s = shared.lock().unwrap();
        (s.clipboard.clone(), s.transfers.clone())
    };
    // Host -> us: clipboard events on the second uni stream, then one uni stream per file.
    {
        let (conn, clipboard, ctx, v) = (conn.clone(), clipboard.clone(), ctx.clone(), viewer.clone());
        tokio::spawn(async move {
            let Ok(mut events) = conn.accept_uni().await else { return };
            let files = {
                let (conn, ctx) = (conn.clone(), ctx.clone());
                async move {
                    while let Ok(recv) = conn.accept_uni().await {
                        tokio::spawn(transfer::receive_file(recv, peer_name.clone(), transfers.clone(), ctx.clone()));
                    }
                }
            };
            let clip = async move {
                while let Ok(msg) = read_json::<HostMsg>(&mut events).await {
                    match msg {
                        HostMsg::Clipboard(text) => clipboard.set(text),
                        HostMsg::LocalActive(busy) => {
                            v.lock().unwrap().remote_busy = busy;
                            ctx.request_repaint();
                        }
                    }
                }
            };
            tokio::join!(files, clip);
        });
    }
    let (clip_id, mut clip_rx) = clipboard.subscribe();
    {
        let tx = tx.clone();
        tokio::spawn(async move {
            while let Some(text) = clip_rx.recv().await {
                if tx.send(Input::Clipboard(text)).is_err() {
                    break;
                }
            }
        });
    }
    {
        let mut v = viewer.lock().unwrap();
        v.status = ViewStatus::Connected;
        v.peer_name = welcome.name;
        v.peer_short = welcome.short;
        v.input = Some(tx);
        v.conn = Some(conn.clone());
    }
    ctx.request_repaint();

    tokio::spawn(async move {
        while let Some(input) = rx.recv().await {
            if write_json(&mut send, &input).await.is_err() {
                break;
            }
        }
    });

    let opts = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA);
    loop {
        let buf = read_msg(&mut frames).await?;
        let frame = Frame::decode(&buf)?;
        let screen = [frame.screen_w as usize, frame.screen_h as usize];
        let mut patches = Vec::with_capacity(frame.rects.len());
        for r in frame.rects {
            let rgba = JpegDecoder::new_with_options(ZCursor::new(&r.jpeg[..]), opts).decode()?;
            let size = [r.w as usize, r.h as usize];
            if rgba.len() == size[0] * size[1] * 4 {
                patches.push(Patch { screen, pos: [r.x as usize, r.y as usize], size, rgba });
            }
        }
        let direct = conn.paths().iter().any(|p| p.is_selected() && p.is_ip());
        let mut v = viewer.lock().unwrap();
        if v.status != ViewStatus::Connected {
            break;
        }
        v.patches.extend(patches);
        v.bytes += buf.len() as u64;
        v.frames += 1;
        v.direct = direct;
        drop(v);
        ctx.request_repaint();
    }
    clipboard.unsubscribe(clip_id);
    Ok(())
}

/// Accepts either a 10-digit short ID or a full public key.
async fn resolve(ep: &Endpoint, input: &str) -> Result<EndpointId> {
    let Some(digits) = shortid::parse_short(input) else {
        return Ok(input.trim().parse()?);
    };
    let alias = shortid::alias_secret(&digits).public();
    let mut results = ep.address_lookup()?.resolve(alias);
    let found = timeout(Duration::from_secs(15), async {
        while let Some(item) = results.next().await {
            let Ok(Ok(item)) = item else { continue };
            let Some(data) = item.user_data() else { continue };
            let Ok(id) = data.as_ref().parse::<EndpointId>() else { continue };
            // Only trust the alias if the real key hashes back to the same digits.
            let d = digits.clone();
            if tokio::task::spawn_blocking(move || shortid::short_id_for(&id) == d).await? {
                return anyhow::Ok(Some(id));
            }
        }
        Ok(None)
    })
    .await;
    match found {
        Ok(Ok(Some(id))) => Ok(id),
        _ => bail!(NOT_FOUND),
    }
}

/// Computes this machine's short ID (once) and keeps an alias endpoint running that maps it to
/// the real public key in the discovery service.
async fn publish_short_id(id: EndpointId, shared: Arc<Mutex<Shared>>, ctx: egui::Context) -> Result<()> {
    let cached = shared.lock().unwrap().short_id.clone();
    let short = match cached {
        Some(s) => s,
        None => {
            let s = tokio::task::spawn_blocking(move || shortid::short_id_for(&id)).await?;
            let mut sh = shared.lock().unwrap();
            sh.short_id = Some(s.clone());
            sh.cfg.short_id = s.clone();
            sh.cfg.short_id_key = id.to_string();
            sh.cfg.save();
            s
        }
    };
    ctx.request_repaint();
    let _alias = Endpoint::builder(presets::N0)
        .secret_key(shortid::alias_secret(&short))
        .user_data_for_address_lookup(id.to_string().parse::<UserData>()?)
        .bind()
        .await?;
    // The alias endpoint republishes on its own; keep it alive for the life of the app.
    std::future::pending::<()>().await;
    Ok(())
}

async fn check_presence(ep: Endpoint, id: String, shared: Arc<Mutex<Shared>>, ctx: egui::Context) {
    let result = timeout(PING_TIMEOUT, async {
        let peer: EndpointId = id.trim().parse()?;
        let conn = ep.connect(peer, ALPN_PING).await?;
        let (mut send, mut recv) = conn.open_bi().await?;
        write_msg(&mut send, b"?").await?;
        let reply: PingReply = read_json(&mut recv).await?;
        conn.close(0u32.into(), b"ok");
        anyhow::Ok(reply)
    })
    .await;
    let presence = match result {
        Ok(Ok(reply)) => Presence::Online { name: reply.name },
        _ => Presence::Offline,
    };
    shared.lock().unwrap().presence.insert(id, presence);
    ctx.request_repaint();
}

async fn accept_loop(ep: Endpoint, shared: Arc<Mutex<Shared>>, input: std::sync::mpsc::Sender<Cmd>, ctx: egui::Context) {
    while let Some(incoming) = ep.accept().await {
        let (shared, input, ctx) = (shared.clone(), input.clone(), ctx.clone());
        tokio::spawn(async move {
            let Ok(conn) = incoming.await else { return };
            let alpn = conn.alpn().to_vec();
            if alpn == ALPN_PING {
                let _ = serve_ping(&conn, &shared).await;
            } else if alpn == ALPN_CTL {
                static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);
                let session = NEXT_SESSION.fetch_add(1, Ordering::Relaxed);
                let _ = serve_control(session, &conn, &shared, &input, &ctx).await;
                input.send(Cmd::Reset).ok();
                shared.lock().unwrap().incoming.retain(|i| i.session != session);
                ctx.request_repaint();
            }
            conn.close(0u32.into(), b"bye");
        });
    }
}

async fn serve_ping(conn: &Connection, shared: &Arc<Mutex<Shared>>) -> Result<()> {
    let (mut send, mut recv) = conn.accept_bi().await?;
    read_msg(&mut recv).await?;
    let name = shared.lock().unwrap().cfg.device_name.clone();
    write_json(&mut send, &PingReply { name }).await?;
    send.finish()?;
    let _ = timeout(Duration::from_secs(5), conn.closed()).await;
    Ok(())
}

async fn serve_control(
    session: u64,
    conn: &Connection,
    shared: &Arc<Mutex<Shared>>,
    input: &std::sync::mpsc::Sender<Cmd>,
    ctx: &egui::Context,
) -> Result<()> {
    let (mut send, mut recv) = conn.accept_bi().await?;
    let hello: Hello = timeout(Duration::from_secs(15), read_json(&mut recv)).await??;
    let nonce: String = (0..32).map(|_| format!("{:02x}", rand::random::<u8>())).collect();
    write_json(&mut send, &Challenge { nonce: nonce.clone() }).await?;
    let auth: Auth = timeout(Duration::from_secs(15), read_json(&mut recv)).await??;
    let (expected, my_name, short, my_id) = {
        let s = shared.lock().unwrap();
        (s.cfg.password.clone(), s.cfg.device_name.clone(), s.short_id.clone().unwrap_or_default(), s.my_id.clone())
    };
    let wanted = auth_proof(&expected, &nonce, &my_id, &conn.remote_id().to_string());
    if expected.is_empty() || auth.proof != wanted {
        // Slow down password guessing.
        sleep(Duration::from_secs(2)).await;
        write_json(&mut send, &Welcome { ok: false, msg: WRONG_PASSWORD.into(), name: my_name, short }).await?;
        send.finish()?;
        let _ = timeout(Duration::from_secs(3), conn.closed()).await;
        return Ok(());
    }
    write_json(&mut send, &Welcome { ok: true, msg: String::new(), name: my_name, short }).await?;

    shared.lock().unwrap().incoming.push(Incoming {
        session,
        name: hello.name,
        conn: conn.clone(),
    });
    ctx.request_repaint();

    // The capturer holds OS handles that must stay on the thread that created them.
    let geometry = Arc::new(Mutex::new(Geometry { x: 0, y: 0, w: 1, h: 1 }));
    let (ftx, mut frx) = mpsc::channel::<Vec<u8>>(1);
    let geo = geometry.clone();
    std::thread::spawn(move || {
        let Ok(mut capturer) = Capturer::new() else { return };
        while let Ok(frame) = capturer.next_frame() {
            *geo.lock().unwrap() = capturer.geometry();
            if ftx.blocking_send(frame.encode()).is_err() {
                break;
            }
        }
    });

    let mut uni = conn.open_uni().await?;
    let mut events = conn.open_uni().await?;
    let (clipboard, transfers) = {
        let s = shared.lock().unwrap();
        (s.clipboard.clone(), s.transfers.clone())
    };
    let peer_name = shared.lock().unwrap().incoming.iter().find(|i| i.session == session).map(|i| i.name.clone()).unwrap_or_default();
    let (clip_id, mut clip_rx) = clipboard.subscribe();
    let clip_out = async {
        let mut busy = false;
        let mut tick = tokio::time::interval(Duration::from_millis(200));
        loop {
            tokio::select! {
                text = clip_rx.recv() => {
                    let Some(text) = text else { break };
                    write_json(&mut events, &HostMsg::Clipboard(text)).await?;
                }
                _ = tick.tick() => {
                    let now = crate::localinput::local_active();
                    if now != busy {
                        busy = now;
                        write_json(&mut events, &HostMsg::LocalActive(busy)).await?;
                    }
                }
            }
        }
        anyhow::Ok(())
    };
    let files = async {
        while let Ok(recv) = conn.accept_uni().await {
            tokio::spawn(transfer::receive_file(recv, peer_name.clone(), transfers.clone(), ctx.clone()));
        }
    };
    let video = async {
        while let Some(buf) = frx.recv().await {
            write_msg(&mut uni, &buf).await?;
        }
        anyhow::Ok(())
    };
    let inputs = async {
        loop {
            let event: Input = read_json(&mut recv).await?;
            if let Input::Clipboard(text) = event {
                clipboard.set(text);
                continue;
            }
            let g: Geometry = *geometry.lock().unwrap();
            input.send(Cmd::Input(g, event)).ok();
        }
        #[allow(unreachable_code)]
        anyhow::Ok(())
    };
    tokio::select! {
        _ = video => {}
        _ = inputs => {}
        _ = clip_out => {}
        _ = files => {}
        _ = conn.closed() => {}
    }
    clipboard.unsubscribe(clip_id);
    Ok(())
}
