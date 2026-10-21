//! Self-update from GitHub Releases.
//!
//! The latest release of the public repo is checked at startup and every few hours. A release
//! must carry `RemoteDesk.exe` and `RemoteDesk.exe.sig` (hex Ed25519 signature of the exe). The
//! signature is checked against the public key below, so a compromised GitHub account alone
//! cannot push code to every installed copy.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use eframe::egui;
use serde::Deserialize;

const REPO: &str = "GoneSeeder/RemoteDesk";
const UPDATE_PUBLIC_KEY: &str = "5465bbb9c77cd0f24d168a3a1af480a5d0017829854894a1c1d8af2d6a8cd562";
const CHECK_INTERVAL: Duration = Duration::from_secs(3 * 60 * 60);
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, PartialEq)]
pub enum UpdateState {
    None,
    Available { version: String, notes: String },
    Downloading { version: String, done: u64, total: u64 },
    Failed(String),
}

#[derive(Clone)]
pub struct Updater {
    pub state: Arc<Mutex<UpdateState>>,
    exe_url: Arc<Mutex<Option<(String, String)>>>,
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

fn parse_version(v: &str) -> Vec<u64> {
    v.trim_start_matches('v').split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder().user_agent(format!("RemoteDesk/{VERSION}")).timeout(Duration::from_secs(120)).build()?)
}

/// Removes the previous exe left behind by the last update.
pub fn cleanup_old() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(exe.with_extension("exe.old"));
    }
}

impl Updater {
    pub fn start(rt: &tokio::runtime::Runtime, ctx: egui::Context) -> Updater {
        cleanup_old();
        let up = Updater { state: Arc::new(Mutex::new(UpdateState::None)), exe_url: Default::default() };
        let u = up.clone();
        rt.spawn(async move {
            loop {
                if let Ok(Some((version, notes, exe, sig))) = check().await {
                    *u.exe_url.lock().unwrap() = Some((exe, sig));
                    let mut st = u.state.lock().unwrap();
                    if matches!(*st, UpdateState::None) {
                        *st = UpdateState::Available { version, notes };
                    }
                    ctx.request_repaint();
                }
                tokio::time::sleep(CHECK_INTERVAL).await;
            }
        });
        up
    }

    /// Downloads, verifies and installs the update, then restarts the app.
    pub fn install(&self, rt: &tokio::runtime::Runtime, ctx: egui::Context) {
        let UpdateState::Available { version, .. } = self.state.lock().unwrap().clone() else { return };
        let Some((exe_url, sig_url)) = self.exe_url.lock().unwrap().clone() else { return };
        let state = self.state.clone();
        *state.lock().unwrap() = UpdateState::Downloading { version: version.clone(), done: 0, total: 0 };
        rt.spawn(async move {
            match download_and_install(&exe_url, &sig_url, &version, &state, &ctx).await {
                Ok(new_exe) => {
                    let _ = std::process::Command::new(new_exe).spawn();
                    std::process::exit(0);
                }
                Err(e) => *state.lock().unwrap() = UpdateState::Failed(format!("อัปเดตไม่สำเร็จ: {e}")),
            }
            ctx.request_repaint();
        });
    }
}

async fn check() -> Result<Option<(String, String, String, String)>> {
    let release: Release = client()?
        .get(format!("https://api.github.com/repos/{REPO}/releases/latest"))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    if release.draft || release.prerelease || parse_version(&release.tag_name) <= parse_version(VERSION) {
        return Ok(None);
    }
    let url = |name: &str| release.assets.iter().find(|a| a.name == name).map(|a| a.browser_download_url.clone());
    let (Some(exe), Some(sig)) = (url("RemoteDesk.exe"), url("RemoteDesk.exe.sig")) else { return Ok(None) };
    Ok(Some((release.tag_name.trim_start_matches('v').to_string(), release.body, exe, sig)))
}

pub fn verify(exe: &[u8], sig_hex: &str) -> Result<()> {
    let hex = |s: &str| -> Result<Vec<u8>> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2).context("bad hex")?, 16).context("bad hex")).collect()
    };
    let key: [u8; 32] = hex(UPDATE_PUBLIC_KEY)?.try_into().map_err(|_| anyhow::anyhow!("bad key"))?;
    let sig: [u8; 64] = hex(sig_hex.trim())?.try_into().map_err(|_| anyhow::anyhow!("ลายเซ็นไม่ถูกต้อง"))?;
    iroh::PublicKey::from_bytes(&key)?
        .verify(exe, &iroh::Signature::from_bytes(&sig))
        .map_err(|_| anyhow::anyhow!("ลายเซ็นไม่ตรง ไฟล์อาจถูกปลอมแปลง"))
}

async fn download_and_install(
    exe_url: &str,
    sig_url: &str,
    version: &str,
    state: &Arc<Mutex<UpdateState>>,
    ctx: &egui::Context,
) -> Result<PathBuf> {
    let http = client()?;
    let sig = http.get(sig_url).send().await?.error_for_status()?.text().await?;
    let mut resp = http.get(exe_url).send().await?.error_for_status()?;
    let total = resp.content_length().unwrap_or(0);
    let mut bytes = Vec::with_capacity(total as usize);
    while let Some(chunk) = resp.chunk().await? {
        bytes.extend_from_slice(&chunk);
        *state.lock().unwrap() = UpdateState::Downloading { version: version.into(), done: bytes.len() as u64, total };
        ctx.request_repaint();
    }
    verify(&bytes, &sig)?;
    if bytes.len() < 1024 * 1024 || &bytes[..2] != b"MZ" {
        bail!("ไฟล์ที่ดาวน์โหลดไม่ใช่โปรแกรม");
    }
    // A running exe can't be overwritten on Windows, but it can be renamed.
    let exe = std::env::current_exe()?;
    let old = exe.with_extension("exe.old");
    let _ = std::fs::remove_file(&old);
    std::fs::rename(&exe, &old).context("เปลี่ยนชื่อไฟล์เดิมไม่ได้")?;
    if let Err(e) = std::fs::write(&exe, &bytes) {
        let _ = std::fs::rename(&old, &exe);
        return Err(e).context("เขียนไฟล์ใหม่ไม่ได้");
    }
    Ok(exe)
}

/// `remotedesk --sign <exe> <key file>`: writes `<exe>.sig` (used by the release workflow).
pub fn sign_cli(exe: &str, key_file: &str) -> Result<()> {
    let key_hex = std::env::var("REMOTEDESK_SIGNING_KEY").ok().filter(|k| !k.trim().is_empty())
        .map(Ok)
        .unwrap_or_else(|| std::fs::read_to_string(key_file))?;
    // Never print the key itself, only what is wrong with it.
    let key_hex: String = key_hex.chars().filter(|c| !c.is_whitespace()).collect();
    if key_hex.is_empty() {
        bail!("signing key is empty (is the REMOTEDESK_SIGNING_KEY secret set?)");
    }
    if key_hex.len() != 64 || !key_hex.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("signing key must be exactly 64 hex characters, got {} characters", key_hex.len());
    }
    let mut key = [0u8; 32];
    for (i, b) in key.iter_mut().enumerate() {
        *b = u8::from_str_radix(&key_hex[i * 2..i * 2 + 2], 16)?;
    }
    let secret = iroh::SecretKey::from_bytes(&key);
    if secret.public().to_string() != UPDATE_PUBLIC_KEY {
        bail!("signing key is valid hex but does not match the public key built into the app (wrong key pasted?)");
    }
    let data = std::fs::read(exe)?;
    let sig: String = secret.sign(&data).to_bytes().iter().map(|b| format!("{b:02x}")).collect();
    std::fs::write(format!("{exe}.sig"), sig)?;
    verify(&data, &std::fs::read_to_string(format!("{exe}.sig"))?)?;
    Ok(())
}
