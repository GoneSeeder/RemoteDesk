use std::path::PathBuf;

use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Contact {
    pub id: String,
    #[serde(default)]
    pub short_id: String,
    pub name: String,
    #[serde(default)]
    pub password: String,
    /// Name the user gave this machine; shown instead of its computer name when set.
    #[serde(default)]
    pub alias: String,
}

impl Contact {
    /// What to call this machine in the UI.
    pub fn display_name(&self, live_name: Option<&str>) -> String {
        if !self.alias.trim().is_empty() {
            self.alias.trim().to_string()
        } else {
            live_name.filter(|n| !n.is_empty()).unwrap_or(&self.name).to_string()
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Config {
    pub device_name: String,
    pub password: String,
    /// Cached 10-digit address (slow to compute), valid for `short_id_key`.
    #[serde(default)]
    pub short_id: String,
    #[serde(default)]
    pub short_id_key: String,
    #[serde(default)]
    pub contacts: Vec<Contact>,
    /// Only used by the self-test, which runs two identities in one process.
    #[serde(skip)]
    key_override: Option<[u8; 32]>,
}

fn dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    base.join("RemoteDesk")
}

fn path() -> PathBuf {
    dir().join("config.json")
}

pub fn thumb_path(id: &str) -> PathBuf {
    dir().join("thumbs").join(format!("{}.jpg", &id[..id.len().min(64)]))
}

pub fn random_password() -> String {
    // No 0/O/1/l/I to avoid confusion when read over the phone.
    const CHARS: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
    let mut rng = rand::rng();
    (0..8).map(|_| CHARS[rng.random_range(0..CHARS.len())] as char).collect()
}

/// Windows' per-installation GUID. It survives reinstalling this app and is the same for every
/// user on the machine; only reinstalling Windows changes it.
fn machine_guid() -> Option<String> {
    use winreg::{RegKey, enums::HKEY_LOCAL_MACHINE};
    let key = RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey(r"SOFTWARE\Microsoft\Cryptography").ok()?;
    key.get_value::<String, _>("MachineGuid").ok().filter(|g| !g.trim().is_empty())
}

impl Config {
    pub fn load() -> Config {
        if let Ok(text) = std::fs::read_to_string(path()) {
            if let Ok(cfg) = serde_json::from_str::<Config>(&text) {
                return cfg;
            }
        }
        let cfg = Config::default_settings();
        cfg.save();
        cfg
    }

    fn default_settings() -> Config {
        Config {
            device_name: std::env::var("COMPUTERNAME").unwrap_or_else(|_| "My PC".into()),
            password: random_password(),
            short_id: String::new(),
            short_id_key: String::new(),
            contacts: Vec::new(),
            key_override: None,
        }
    }

    /// A throwaway identity that is not written to disk (self-test only).
    #[cfg(debug_assertions)]
    pub fn fresh() -> Config {
        Config { key_override: Some(iroh::SecretKey::generate().to_bytes()), ..Config::default_settings() }
    }

    pub fn save(&self) {
        if self.key_override.is_some() {
            return;
        }
        let _ = std::fs::create_dir_all(dir());
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path(), text);
        }
    }

    /// The machine's identity key, derived from the Windows MachineGuid so the ID never changes
    /// on this installation of Windows.
    pub fn secret_key(&self) -> iroh::SecretKey {
        if let Some(bytes) = self.key_override {
            return iroh::SecretKey::from_bytes(&bytes);
        }
        let seed = machine_guid().unwrap_or_else(|| std::env::var("COMPUTERNAME").unwrap_or_default());
        let bytes: [u8; 32] = Sha256::digest(format!("remotedesk-machine-key-v1:{}", seed.trim().to_lowercase())).into();
        iroh::SecretKey::from_bytes(&bytes)
    }
}
