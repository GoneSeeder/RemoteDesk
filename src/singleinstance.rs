//! Single instance: launching RemoteDesk again brings the existing window back instead of
//! starting a second copy (which would add a second tray icon). Uses a named event — the first
//! instance owns it; later launches signal it and exit.

use std::sync::OnceLock;

use eframe::egui;
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows_sys::Win32::System::Threading::{CreateEventW, INFINITE, SetEvent, WaitForSingleObject};

const EVENT_NAME: &str = "RemoteDesk-show-window-event";

static EVENT: OnceLock<usize> = OnceLock::new();

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Returns true if this is the first instance (continue launching). If another instance is
/// already running, signals it to show its window and returns false (this process should exit).
pub fn acquire() -> bool {
    let name = wide(EVENT_NAME);
    let handle = unsafe { CreateEventW(std::ptr::null(), 1, 0, name.as_ptr()) };
    if handle.is_null() {
        return true; // can't tell; allow launch
    }
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        // Another instance owns the event: poke it, then we exit.
        unsafe {
            SetEvent(handle);
            CloseHandle(handle);
        }
        return false;
    }
    let _ = EVENT.set(handle as usize);
    true
}

/// First instance only: wait for later launches to signal, and show the window when they do.
pub fn start_show_listener(ctx: egui::Context) {
    let Some(&h) = EVENT.get() else { return };
    std::thread::spawn(move || {
        let handle = h as HANDLE;
        loop {
            unsafe { WaitForSingleObject(handle, INFINITE) };
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            ctx.request_repaint();
        }
    });
}
