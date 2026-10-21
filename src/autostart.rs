//! Start RemoteDesk when the user logs in, via the per-user Run key. Off unless the user turns it
//! on in Settings. Per-user (HKCU) so it needs no admin rights to set.

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE: &str = "RemoteDesk";

/// Turns logon autostart on or off to match `enabled`.
pub fn set_enabled(enabled: bool) {
    use winreg::{RegKey, enums::HKEY_CURRENT_USER};
    let Ok((key, _)) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(RUN_KEY) else { return };
    if enabled {
        if let Ok(exe) = std::env::current_exe() {
            // Quote the path so a space in it is handled correctly.
            let _ = key.set_value(VALUE, &format!("\"{}\"", exe.display()));
        }
    } else {
        let _ = key.delete_value(VALUE);
    }
}
