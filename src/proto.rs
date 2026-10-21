//! Wire protocol. Every message is a little-endian u32 length followed by the payload.
//! Control messages are JSON; video frames are a compact binary layout.

use anyhow::{Result, bail};
use sha2::{Digest, Sha256};
use iroh::endpoint::{RecvStream, SendStream};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

pub const ALPN_CTL: &[u8] = b"remotedesk/ctl/3";
pub const ALPN_PING: &[u8] = b"remotedesk/ping/1";

pub const WRONG_PASSWORD: &str = "รหัสผ่านไม่ถูกต้อง";
pub const NOT_FOUND: &str = "ไม่พบเครื่องนี้ (ID ผิด หรือเครื่องปลายทางไม่ได้เปิดโปรแกรม)";
pub const DENIED: &str = "เครื่องปลายทางปฏิเสธการเชื่อมต่อ";
pub const LOCKED: &str = "ใส่รหัสผิดหลายครั้ง ถูกล็อกชั่วคราว กรุณาลองใหม่ภายหลัง";

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
    /// A physical key exactly as the controller's keyboard reported it (captured with a
    /// low-level hook, so Win/Alt+Tab/etc. reach the remote instead of the local machine).
    RawKey { vk: u16, scan: u16, extended: bool, down: bool },
}

/// Messages from the controlled machine on its second uni stream.
#[derive(Serialize, Deserialize)]
pub enum HostMsg {
    Clipboard(String),
    /// The person at the controlled machine is using its mouse/keyboard; remote input is paused.
    LocalActive(bool),
}

/// One H.264 access unit of the remote screen.
pub struct VideoFrame {
    pub width: u16,
    pub height: u16,
    pub data: Vec<u8>,
}

impl VideoFrame {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(4 + self.data.len());
        out.extend_from_slice(&self.width.to_le_bytes());
        out.extend_from_slice(&self.height.to_le_bytes());
        out.extend_from_slice(&self.data);
        out
    }

    pub fn decode(buf: &[u8]) -> Result<VideoFrame> {
        if buf.len() < 4 {
            bail!("truncated frame");
        }
        Ok(VideoFrame {
            width: u16::from_le_bytes([buf[0], buf[1]]),
            height: u16::from_le_bytes([buf[2], buf[3]]),
            data: buf[4..].to_vec(),
        })
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
