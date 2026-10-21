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
use crate::winclip;

const CHUNK: usize = 64 * 1024;
const MAX_CLIPBOARD: usize = 1024 * 1024;
/// Total size allowed for a clipboard file copy (bigger copies are ignored).
const MAX_CLIP_FILES_BYTES: u64 = 300 * 1024 * 1024;

/// Uni-stream kind, sent as the first byte so the receiver knows how to handle it.
const KIND_FILE: u8 = 0;
const KIND_CLIP_FILES: u8 = 1;

#[derive(Serialize, Deserialize, Clone)]
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
        send.write_all(&[KIND_FILE]).await?;
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

#[derive(Serialize, Deserialize)]
struct ClipManifest {
    files: Vec<FileHeader>,
}

fn clip_dir() -> PathBuf {
    crate::config::config_dir().join("clip")
}

/// Sends the files the user just copied, as one clipboard bundle on its own stream.
pub async fn send_clip_bundle(conn: Connection, paths: Vec<PathBuf>) -> Result<()> {
    let mut headers = Vec::new();
    let mut total = 0u64;
    for p in &paths {
        let size = tokio::fs::metadata(p).await.map(|m| m.len()).unwrap_or(0);
        total += size;
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("file").to_string();
        headers.push(FileHeader { name, size });
    }
    if total > MAX_CLIP_FILES_BYTES {
        bail!("clipboard files too large");
    }
    let mut send = conn.open_uni().await?;
    send.write_all(&[KIND_CLIP_FILES]).await?;
    write_json(&mut send, &ClipManifest { files: headers.clone() }).await?;
    let mut buf = vec![0u8; CHUNK];
    for (p, h) in paths.iter().zip(&headers) {
        let mut file = tokio::fs::File::open(p).await?;
        let mut sent = 0u64;
        while sent < h.size {
            let n = tokio::io::AsyncReadExt::read(&mut file, &mut buf).await?;
            if n == 0 {
                break;
            }
            send.write_all(&buf[..n]).await?;
            sent += n as u64;
        }
    }
    send.finish()?;
    send.stopped().await?;
    Ok(())
}

async fn receive_clip_bundle(mut recv: RecvStream, clipboard: Clipboard) -> Result<()> {
    let manifest: ClipManifest = read_json(&mut recv).await?;
    let total: u64 = manifest.files.iter().map(|f| f.size).sum();
    if total > MAX_CLIP_FILES_BYTES {
        bail!("clipboard files too large");
    }
    // A fresh folder per paste keeps distinct copies from clobbering each other.
    let dir = clip_dir().join(format!("{}", next_id()));
    // Keep only the few most recent clip folders.
    if let Ok(entries) = std::fs::read_dir(clip_dir()) {
        let mut dirs: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        dirs.sort();
        for old in dirs.iter().rev().skip(8) {
            let _ = std::fs::remove_dir_all(old);
        }
    }
    tokio::fs::create_dir_all(&dir).await?;
    let mut saved = Vec::new();
    let mut buf = vec![0u8; CHUNK];
    for h in &manifest.files {
        let path = dir.join(sanitize(&h.name));
        let mut file = tokio::fs::File::create(&path).await?;
        let mut left = h.size;
        while left > 0 {
            let want = CHUNK.min(left as usize);
            let n = recv.read(&mut buf[..want]).await?.ok_or_else(|| anyhow::anyhow!("short clip stream"))?;
            file.write_all(&buf[..n]).await?;
            left -= n as u64;
        }
        file.flush().await?;
        saved.push(path);
    }
    clipboard.set_files(saved);
    Ok(())
}

/// Reads the leading kind byte of a uni stream and routes it.
pub async fn receive_uni(
    mut recv: RecvStream,
    peer: String,
    transfers: Transfers,
    clipboard: Clipboard,
    ctx: eframe::egui::Context,
) {
    let mut kind = [0u8; 1];
    if recv.read_exact(&mut kind).await.is_err() {
        return;
    }
    match kind[0] {
        KIND_CLIP_FILES => {
            let _ = receive_clip_bundle(recv, clipboard).await;
        }
        _ => receive_file(recv, peer, transfers, ctx).await,
    }
}

fn sanitize(name: &str) -> String {
    let base = Path::new(name).file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let s: String = base.chars().map(|c| if r#"<>:"/\|?*"#.contains(c) || c.is_control() { '_' } else { c }).collect();
    let s = s.trim().trim_matches('.').to_string();
    if s.is_empty() { "file".into() } else { s }
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

/// A clipboard change to mirror to the peer: either text or a copied file list.
#[derive(Clone)]
pub enum ClipItem {
    Text(String),
    Files(Vec<PathBuf>),
}

/// Shared clipboard state: subscribers get local changes; `set`/`set_files` write what came from
/// the peer without echoing it back.
#[derive(Clone, Default)]
pub struct Clipboard {
    inner: Arc<Mutex<ClipState>>,
}

#[derive(Default)]
struct ClipState {
    last_text: String,
    /// Signature (names+sizes) of the last file list we saw or wrote, to avoid echo loops.
    last_files_sig: String,
    subscribers: Vec<(u64, mpsc::UnboundedSender<ClipItem>)>,
}

fn files_sig(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| {
            let size = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
            format!("{}:{size}", p.display())
        })
        .collect::<Vec<_>>()
        .join("|")
}

impl Clipboard {
    pub fn start() -> Clipboard {
        let clip = Clipboard::default();
        let c = clip.clone();
        std::thread::spawn(move || {
            let mut board = arboard::Clipboard::new().ok();
            if let Some(b) = board.as_mut() {
                if let Ok(t) = b.get_text() {
                    c.inner.lock().unwrap().last_text = t;
                }
            }
            loop {
                std::thread::sleep(Duration::from_millis(500));
                // Files take priority: copying files in Explorer also clears any text.
                if let Some(files) = winclip::get_files() {
                    let sig = files_sig(&files);
                    let total: u64 = files.iter().map(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)).sum();
                    let mut st = c.inner.lock().unwrap();
                    if sig != st.last_files_sig && total <= MAX_CLIP_FILES_BYTES {
                        st.last_files_sig = sig;
                        let item = ClipItem::Files(files);
                        st.subscribers.retain(|(_, tx)| tx.send(item.clone()).is_ok());
                    }
                    continue;
                }
                let Some(b) = board.as_mut() else { continue };
                let Ok(text) = b.get_text() else { continue };
                if text.is_empty() || text.len() > MAX_CLIPBOARD {
                    continue;
                }
                let mut st = c.inner.lock().unwrap();
                if text != st.last_text {
                    st.last_text = text.clone();
                    st.last_files_sig.clear();
                    let item = ClipItem::Text(text);
                    st.subscribers.retain(|(_, tx)| tx.send(item.clone()).is_ok());
                }
            }
        });
        clip
    }

    pub fn subscribe(&self) -> (u64, mpsc::UnboundedReceiver<ClipItem>) {
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
        self.inner.lock().unwrap().last_text = text.clone();
        std::thread::spawn(move || {
            if let Ok(mut board) = arboard::Clipboard::new() {
                let _ = board.set_text(text);
            }
        });
    }

    /// Writes the file list received from the peer, recording its signature so the watcher does
    /// not send it straight back.
    pub fn set_files(&self, paths: Vec<PathBuf>) {
        self.inner.lock().unwrap().last_files_sig = files_sig(&paths);
        std::thread::spawn(move || {
            winclip::set_files(&paths);
        });
    }
}
