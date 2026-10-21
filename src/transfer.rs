//! Clipboard sync and file transfer between the two ends of a session.
//!
//! Each file travels on its own QUIC uni stream: a JSON header, then the raw bytes. Received
//! files land in `Downloads\RemoteDesk`. The clipboard is polled; text that changes locally is
//! pushed to every active session, and text received from a peer is written locally without
//! being echoed back.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use iroh::endpoint::{Connection, RecvStream};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;

use crate::proto::{read_json, write_json};

const CHUNK: usize = 64 * 1024;
const MAX_CLIPBOARD: usize = 1024 * 1024;

#[derive(Serialize, Deserialize)]
pub struct FileHeader {
    pub name: String,
    pub size: u64,
}

#[derive(Clone)]
pub struct Transfer {
    pub id: u64,
    pub peer: String,
    pub name: String,
    pub size: u64,
    pub done: u64,
    pub incoming: bool,
    pub finished: bool,
    pub error: Option<String>,
    pub path: Option<PathBuf>,
    pub finished_at: Option<Instant>,
}

pub type Transfers = Arc<Mutex<Vec<Transfer>>>;

fn next_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

pub fn downloads_dir() -> PathBuf {
    let home = std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    home.join("Downloads").join("RemoteDesk")
}

/// Keeps only the file name and strips characters Windows rejects, so a peer cannot write
/// outside the downloads folder.
fn safe_name(name: &str) -> String {
    let base = Path::new(name).file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let cleaned: String =
        base.chars().map(|c| if r#"<>:"/\|?*"#.contains(c) || c.is_control() { '_' } else { c }).collect();
    let cleaned = cleaned.trim().trim_matches('.').to_string();
    if cleaned.is_empty() { "file".into() } else { cleaned }
}

/// Paths claimed by transfers still in progress, so two files with the same name arriving at
/// once don't pick the same destination.
static RESERVED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

fn reserve_path(dir: &Path, name: &str) -> PathBuf {
    let p = Path::new(name);
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
    let ext = p.extension().and_then(|s| s.to_str()).map(|e| format!(".{e}")).unwrap_or_default();
    let mut reserved = RESERVED.lock().unwrap();
    let path = std::iter::once(dir.join(name))
        .chain((1..).map(|i| dir.join(format!("{stem} ({i}){ext}"))))
        .find(|c| !c.exists() && !reserved.contains(c))
        .unwrap();
    reserved.push(path.clone());
    path
}

fn release_path(path: &Path) {
    RESERVED.lock().unwrap().retain(|p| p != path);
}

fn update(transfers: &Transfers, id: u64, f: impl FnOnce(&mut Transfer)) {
    if let Some(t) = transfers.lock().unwrap().iter_mut().find(|t| t.id == id) {
        f(t);
    }
}

pub async fn send_file(conn: Connection, path: PathBuf, peer: String, transfers: Transfers, ctx: eframe::egui::Context) {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file").to_string();
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    let id = next_id();
    transfers.lock().unwrap().push(Transfer {
        id,
        peer,
        name: name.clone(),
        size,
        done: 0,
        incoming: false,
        finished: false,
        error: None,
        path: Some(path.clone()),
        finished_at: None,
    });
    let result: Result<()> = async {
        let mut file = tokio::fs::File::open(&path).await?;
        let mut send = conn.open_uni().await?;
        write_json(&mut send, &FileHeader { name, size }).await?;
        let mut buf = vec![0u8; CHUNK];
        let mut done = 0u64;
        loop {
            let n = tokio::io::AsyncReadExt::read(&mut file, &mut buf).await?;
            if n == 0 {
                break;
            }
            send.write_all(&buf[..n]).await?;
            done += n as u64;
            update(&transfers, id, |t| t.done = done);
            ctx.request_repaint();
        }
        send.finish()?;
        send.stopped().await?;
        Ok(())
    }
    .await;
    update(&transfers, id, |t| {
        t.finished = true;
        t.finished_at = Some(Instant::now());
        t.error = result.err().map(|e| e.to_string());
    });
    ctx.request_repaint();
}

pub async fn receive_file(mut recv: RecvStream, peer: String, transfers: Transfers, ctx: eframe::egui::Context) {
    let id = next_id();
    let mut reserved: Option<PathBuf> = None;
    let result: Result<()> = async {
        let header: FileHeader = read_json(&mut recv).await?;
        let dir = downloads_dir();
        tokio::fs::create_dir_all(&dir).await?;
        let path = reserve_path(&dir, &safe_name(&header.name));
        reserved = Some(path.clone());
        transfers.lock().unwrap().push(Transfer {
            id,
            peer,
            name: header.name.clone(),
            size: header.size,
            done: 0,
            incoming: true,
            finished: false,
            error: None,
            path: Some(path.clone()),
            finished_at: None,
        });
        let partial = path.with_extension("part");
        let mut file = tokio::fs::File::create(&partial).await?;
        let mut buf = vec![0u8; CHUNK];
        let mut done = 0u64;
        while let Some(n) = recv.read(&mut buf).await? {
            done += n as u64;
            if done > header.size {
                bail!("ข้อมูลเกินขนาดที่แจ้งไว้");
            }
            file.write_all(&buf[..n]).await?;
            update(&transfers, id, |t| t.done = done);
            ctx.request_repaint();
        }
        file.flush().await?;
        drop(file);
        if done != header.size {
            let _ = tokio::fs::remove_file(&partial).await;
            bail!("การส่งไฟล์ถูกยกเลิก");
        }
        tokio::fs::rename(&partial, &path).await?;
        Ok(())
    }
    .await;
    if let Some(p) = reserved {
        release_path(&p);
    }
    update(&transfers, id, |t| {
        t.finished = true;
        t.finished_at = Some(Instant::now());
        t.error = result.err().map(|e| e.to_string());
    });
    ctx.request_repaint();
}

/// Shared clipboard state: subscribers get local changes; `set` writes peer text without echo.
#[derive(Clone, Default)]
pub struct Clipboard {
    inner: Arc<Mutex<ClipState>>,
}

#[derive(Default)]
struct ClipState {
    last: String,
    subscribers: Vec<(u64, mpsc::UnboundedSender<String>)>,
}

impl Clipboard {
    pub fn start() -> Clipboard {
        let clip = Clipboard::default();
        let c = clip.clone();
        std::thread::spawn(move || {
            let Ok(mut board) = arboard::Clipboard::new() else { return };
            if let Ok(t) = board.get_text() {
                c.inner.lock().unwrap().last = t;
            }
            loop {
                std::thread::sleep(Duration::from_millis(500));
                let Ok(text) = board.get_text() else { continue };
                if text.is_empty() || text.len() > MAX_CLIPBOARD {
                    continue;
                }
                let mut st = c.inner.lock().unwrap();
                if text != st.last {
                    st.last = text.clone();
                    st.subscribers.retain(|(_, tx)| tx.send(text.clone()).is_ok());
                }
            }
        });
        clip
    }

    pub fn subscribe(&self) -> (u64, mpsc::UnboundedReceiver<String>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let id = next_id();
        self.inner.lock().unwrap().subscribers.push((id, tx));
        (id, rx)
    }

    pub fn unsubscribe(&self, id: u64) {
        self.inner.lock().unwrap().subscribers.retain(|(i, _)| *i != id);
    }

    /// Writes text that came from the peer.
    pub fn set(&self, text: String) {
        if text.len() > MAX_CLIPBOARD {
            return;
        }
        self.inner.lock().unwrap().last = text.clone();
        std::thread::spawn(move || {
            if let Ok(mut board) = arboard::Clipboard::new() {
                let _ = board.set_text(text);
            }
        });
    }
}
