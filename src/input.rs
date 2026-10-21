//! Mouse/keyboard injection on the controlled machine, plus the egui -> Windows virtual-key
//! mapping used by the controlling side.

use std::sync::mpsc::{Receiver, Sender, channel};

use eframe::egui;
use enigo::{Direction, Enigo, Key, Keyboard, Settings};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, MOUSE_EVENT_FLAGS, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN,
    MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN,
    MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEINPUT, SendInput,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

use crate::capture::Geometry;
use crate::proto::Input;

const VK_SHIFT: u16 = 0x10;
const VK_CONTROL: u16 = 0x11;
const VK_MENU: u16 = 0x12;

pub enum Cmd {
    Input(Geometry, Input),
    /// Release every held key/button (session ended).
    Reset,
}

/// Starts the single injection thread and returns a sender to it.
pub fn spawn() -> Sender<Cmd> {
    let (tx, rx) = channel();
    crate::localinput::start();
    std::thread::spawn(move || run(rx));
    tx
}

/// Small diagnostic log on the controlled machine: `%APPDATA%\RemoteDesk\input.log`.
fn log(msg: &str) {
    use std::io::Write;
    let path = std::env::var_os("APPDATA").map(std::path::PathBuf::from).unwrap_or_default().join("RemoteDesk").join("input.log");
    if std::fs::metadata(&path).map(|m| m.len() > 512 * 1024).unwrap_or(false) {
        let _ = std::fs::remove_file(&path);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let _ = writeln!(f, "{t} {msg}");
    }
}

fn send_mouse(flags: MOUSE_EVENT_FLAGS, dx: i32, dy: i32, data: i32) -> bool {
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 { mi: MOUSEINPUT { dx, dy, mouseData: data as u32, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
    };
    unsafe { SendInput(1, &input, std::mem::size_of::<INPUT>() as i32) == 1 }
}

/// Moves the cursor to physical pixel (x, y) anywhere on the virtual desktop.
fn mouse_to(x: i32, y: i32) -> bool {
    let (vx, vy, vw, vh) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN).max(2),
            GetSystemMetrics(SM_CYVIRTUALSCREEN).max(2),
        )
    };
    let nx = ((x - vx) as i64 * 65535 / (vw - 1) as i64) as i32;
    let ny = ((y - vy) as i64 * 65535 / (vh - 1) as i64) as i32;
    send_mouse(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK, nx, ny, 0)
}

fn send_key(vk: u16, scan: u16, extended: bool, down: bool) -> bool {
    let mut flags = 0;
    if extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    if !down {
        flags |= KEYEVENTF_KEYUP;
    }
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
    };
    unsafe { SendInput(1, &input, std::mem::size_of::<INPUT>() as i32) == 1 }
}

fn button_flags(button: u8, down: bool) -> MOUSE_EVENT_FLAGS {
    match (button, down) {
        (1, true) => MOUSEEVENTF_RIGHTDOWN,
        (1, false) => MOUSEEVENTF_RIGHTUP,
        (2, true) => MOUSEEVENTF_MIDDLEDOWN,
        (2, false) => MOUSEEVENTF_MIDDLEUP,
        (_, true) => MOUSEEVENTF_LEFTDOWN,
        (_, false) => MOUSEEVENTF_LEFTUP,
    }
}

fn run(rx: Receiver<Cmd>) {
    let mut enigo = match Enigo::new(&Settings::default()) {
        Ok(e) => Some(e),
        Err(e) => {
            log(&format!("keyboard init failed: {e:?}"));
            None
        }
    };
    log("input thread started");
    let mut mods = [false; 3]; // ctrl, shift, alt as currently held by us
    let mut held_keys: Vec<u16> = Vec::new();
    let mut held_buttons: Vec<u8> = Vec::new();
    let mut held_raw: Vec<(u16, u16, bool)> = Vec::new();
    let (mut received, mut paused, mut failed) = (0u64, 0u64, 0u64);

    let set_mod = |enigo: &mut Option<Enigo>, held: &mut bool, vk: u16, want: bool| {
        if *held != want {
            let dir = if want { Direction::Press } else { Direction::Release };
            if let Some(e) = enigo {
                let _ = e.key(Key::Other(vk as u32), dir);
            }
            *held = want;
        }
    };

    for cmd in rx {
        // The local user wins: drop remote moves and presses while they are active. Releases
        // still go through so nothing stays held down.
        if let Cmd::Input(_, ev) = &cmd {
            let press = matches!(
                ev,
                Input::MouseMove { .. }
                    | Input::Scroll { .. }
                    | Input::Button { down: true, .. }
                    | Input::Key { down: true, .. }
                    | Input::RawKey { down: true, .. }
                    | Input::Text(_)
            );
            received += 1;
            if press && crate::localinput::local_active() {
                paused += 1;
                if paused % 50 == 1 {
                    log(&format!("paused: local user active (paused={paused} received={received})"));
                }
                continue;
            }
            if received % 500 == 1 {
                log(&format!("inputs received={received} paused={paused} failed={failed}"));
            }
        }
        match cmd {
            Cmd::Input(g, Input::MouseMove { x, y }) => {
                let px = g.x + (x.clamp(0.0, 1.0) * (g.w.saturating_sub(1)) as f32).round() as i32;
                let py = g.y + (y.clamp(0.0, 1.0) * (g.h.saturating_sub(1)) as f32).round() as i32;
                if !mouse_to(px, py) {
                    failed += 1;
                    if failed % 50 == 1 {
                        log(&format!("SendInput(move) failed, error {}", std::io::Error::last_os_error()));
                    }
                }
            }
            Cmd::Input(_, Input::Button { button, down }) => {
                if !send_mouse(button_flags(button, down), 0, 0, 0) {
                    failed += 1;
                    log(&format!("SendInput(button) failed, error {}", std::io::Error::last_os_error()));
                }
                held_buttons.retain(|&h| h != button);
                if down {
                    held_buttons.push(button);
                }
            }
            Cmd::Input(_, Input::Scroll { dx, dy }) => {
                // Our dy is positive for "scroll down"; Windows wheel delta is positive for up.
                if dy != 0 {
                    send_mouse(MOUSEEVENTF_WHEEL, 0, 0, -dy * 120);
                }
                if dx != 0 {
                    send_mouse(MOUSEEVENTF_HWHEEL, 0, 0, dx * 120);
                }
            }
            Cmd::Input(_, Input::Key { vk, down, ctrl, shift, alt }) => {
                set_mod(&mut enigo, &mut mods[0], VK_CONTROL, ctrl);
                set_mod(&mut enigo, &mut mods[1], VK_SHIFT, shift);
                set_mod(&mut enigo, &mut mods[2], VK_MENU, alt);
                if let Some(e) = &mut enigo {
                    let _ = e.key(Key::Other(vk as u32), if down { Direction::Press } else { Direction::Release });
                }
                held_keys.retain(|&k| k != vk);
                if down {
                    held_keys.push(vk);
                }
            }
            Cmd::Input(_, Input::Text(text)) => {
                set_mod(&mut enigo, &mut mods[0], VK_CONTROL, false);
                set_mod(&mut enigo, &mut mods[1], VK_SHIFT, false);
                set_mod(&mut enigo, &mut mods[2], VK_MENU, false);
                if let Some(e) = &mut enigo {
                    let _ = e.text(&text);
                }
            }
            Cmd::Input(_, Input::Clipboard(_)) => {}
            Cmd::Input(_, Input::RawKey { vk, scan, extended, down }) => {
                if !send_key(vk, scan, extended, down) {
                    failed += 1;
                    log(&format!("SendInput(key) failed, error {}", std::io::Error::last_os_error()));
                }
                held_raw.retain(|&(v, _, _)| v != vk);
                if down {
                    held_raw.push((vk, scan, extended));
                }
            }
            Cmd::Reset => {
                for vk in held_keys.drain(..) {
                    if let Some(e) = &mut enigo {
                        let _ = e.key(Key::Other(vk as u32), Direction::Release);
                    }
                }
                for (vk, scan, extended) in held_raw.drain(..) {
                    send_key(vk, scan, extended, false);
                }
                for b in held_buttons.drain(..) {
                    send_mouse(button_flags(b, false), 0, 0, 0);
                }
                set_mod(&mut enigo, &mut mods[0], VK_CONTROL, false);
                set_mod(&mut enigo, &mut mods[1], VK_SHIFT, false);
                set_mod(&mut enigo, &mut mods[2], VK_MENU, false);
            }
        }
    }
}

/// Maps an egui key to a Windows virtual-key code.
pub fn vk_for(key: egui::Key) -> Option<u16> {
    use egui::Key as K;
    let name = key.name();
    // Letters and digits map directly to their ASCII code.
    if name.len() == 1 {
        let c = name.as_bytes()[0].to_ascii_uppercase();
        if c.is_ascii_uppercase() || c.is_ascii_digit() {
            return Some(c as u16);
        }
    }
    Some(match key {
        K::Escape => 0x1B,
        K::Tab => 0x09,
        K::Backspace => 0x08,
        K::Enter => 0x0D,
        K::Space => 0x20,
        K::Insert => 0x2D,
        K::Delete => 0x2E,
        K::Home => 0x24,
        K::End => 0x23,
        K::PageUp => 0x21,
        K::PageDown => 0x22,
        K::ArrowLeft => 0x25,
        K::ArrowUp => 0x26,
        K::ArrowRight => 0x27,
        K::ArrowDown => 0x28,
        K::Minus => 0xBD,
        K::Plus | K::Equals => 0xBB,
        K::Comma => 0xBC,
        K::Period => 0xBE,
        K::Semicolon | K::Colon => 0xBA,
        K::Slash | K::Questionmark => 0xBF,
        K::Backtick => 0xC0,
        K::OpenBracket | K::OpenCurlyBracket => 0xDB,
        K::Backslash | K::Pipe => 0xDC,
        K::CloseBracket | K::CloseCurlyBracket => 0xDD,
        K::Quote => 0xDE,
        K::F1 => 0x70,
        K::F2 => 0x71,
        K::F3 => 0x72,
        K::F4 => 0x73,
        K::F5 => 0x74,
        K::F6 => 0x75,
        K::F7 => 0x76,
        K::F8 => 0x77,
        K::F9 => 0x78,
        K::F10 => 0x79,
        K::F11 => 0x7A,
        K::F12 => 0x7B,
        _ => return None,
    })
}
