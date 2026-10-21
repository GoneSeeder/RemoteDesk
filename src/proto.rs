//! Wire protocol. Every message is a little-endian u32 length followed by the payload.
//! Control messages are JSON; video frames are a compact binary layout.

use anyhow::{Result, bail};
use sha2::{Digest, Sha256};
use iroh::endpoint::{RecvStream, SendStream};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

pub const ALPN_CTL: &[u8] = b"remotedesk/ctl/2";
pub const ALPN_PING: &[u8] = b"remotedesk/ping/1";

pub const WRONG_PASSWORD: &str = "รหัสผ่านไม่ถูกต้อง";
pub const NOT_FOUND: &str = "ไม่พบเครื่องนี้ (ID ผิด หรือเครื่องปลายทางไม่ได้เปิดโปรแกรม)";

const MAX_MSG: usize = 32 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
pub struct Hello {
    pub name: String,
}

/// The host never sees the password itself, only a proof bound to a fresh nonce and to both
/// machines' keys, so a proof captured by an impostor cannot be replayed against the real host.
#[derive(Serialize, Deserialize)]
pub struct Challenge {
    pub nonce: String,
}

#[derive(Serialize, Deserialize)]
pub struct Auth {
    pub proof: String,
}

pub fn auth_proof(password: &str, nonce: &str, host_id: &str, client_id: &str) -> String {
    let mut h = Sha256::new();
    for part in ["remotedesk-auth-v1", password, nonce, host_id, client_id] {
        h.update((part.len() as u32).to_le_bytes());
        h.update(part.as_bytes());
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Serialize, Deserialize)]
pub struct Welcome {
    pub ok: bool,
    pub msg: String,
    pub name: String,
    #[serde(default)]
    pub short: String,
}

#[derive(Serialize, Deserialize)]
pub struct PingReply {
    pub name: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Input {
    /// Normalised 0..1 position on the remote screen.
    MouseMove { x: f32, y: f32 },
    /// 0 = left, 1 = right, 2 = middle.
    Button { button: u8, down: bool },
    /// Whole wheel notches; positive y scrolls down.
    Scroll { dx: i32, dy: i32 },
    /// Windows virtual-key code plus the modifier state at that moment.
    Key { vk: u16, down: bool, ctrl: bool, shift: bool, alt: bool },
    /// Type literal text (used for pasting the controller's clipboard).
    Text(String),
    /// The controller's clipboard changed.
    Clipboard(String),
}

/// Messages from the controlled machine on its second uni stream.
#[derive(Serialize, Deserialize)]
pub enum HostMsg {
    Clipboard(String),
    /// The person at the controlled machine is using its mouse/keyboard; remote input is paused.
    LocalActive(bool),
}

pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
    pub jpeg: Vec<u8>,
}

pub struct Frame {
    pub screen_w: u16,
    pub screen_h: u16,
    pub rects: Vec<Rect>,
}

impl Frame {
    pub fn encode(&self) -> Vec<u8> {
        let size: usize = 6 + self.rects.iter().map(|r| 12 + r.jpeg.len()).sum::<usize>();
        let mut out = Vec::with_capacity(size);
        out.extend_from_slice(&self.screen_w.to_le_bytes());
        out.extend_from_slice(&self.screen_h.to_le_bytes());
        out.extend_from_slice(&(self.rects.len() as u16).to_le_bytes());
        for r in &self.rects {
            for v in [r.x, r.y, r.w, r.h] {
                out.extend_from_slice(&v.to_le_bytes());
            }
            out.extend_from_slice(&(r.jpeg.len() as u32).to_le_bytes());
            out.extend_from_slice(&r.jpeg);
        }
        out
    }

    pub fn decode(buf: &[u8]) -> Result<Frame> {
        let mut pos = 0;
        let mut take = |n: usize| -> Result<&[u8]> {
            if pos + n > buf.len() {
                bail!("truncated frame");
            }
            let s = &buf[pos..pos + n];
            pos += n;
            Ok(s)
        };
        let u16_at = |s: &[u8]| u16::from_le_bytes([s[0], s[1]]);
        let screen_w = u16_at(take(2)?);
        let screen_h = u16_at(take(2)?);
        let n = u16_at(take(2)?);
        let mut rects = Vec::with_capacity(n as usize);
        for _ in 0..n {
            let h = take(12)?;
            let len = u32::from_le_bytes([h[8], h[9], h[10], h[11]]) as usize;
            let (x, y, w, hh) = (u16_at(&h[0..]), u16_at(&h[2..]), u16_at(&h[4..]), u16_at(&h[6..]));
            rects.push(Rect { x, y, w, h: hh, jpeg: take(len)?.to_vec() });
        }
        Ok(Frame { screen_w, screen_h, rects })
    }
}

pub async fn write_msg(send: &mut SendStream, payload: &[u8]) -> Result<()> {
    send.write_all(&(payload.len() as u32).to_le_bytes()).await?;
    send.write_all(payload).await?;
    Ok(())
}

pub async fn read_msg(recv: &mut RecvStream) -> Result<Vec<u8>> {
    let mut len = [0u8; 4];
    recv.read_exact(&mut len).await?;
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_MSG {
        bail!("message too large");
    }
    let mut buf = vec![0u8; len];
    recv.read_exact(&mut buf).await?;
    Ok(buf)
}

pub async fn write_json<T: Serialize>(send: &mut SendStream, value: &T) -> Result<()> {
    write_msg(send, &serde_json::to_vec(value)?).await
}

pub async fn read_json<T: DeserializeOwned>(recv: &mut RecvStream) -> Result<T> {
    Ok(serde_json::from_slice(&read_msg(recv).await?)?)
}
