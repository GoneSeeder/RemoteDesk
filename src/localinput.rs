//! Detects physical mouse/keyboard use on the controlled machine, so the person sitting there
//! takes priority over the remote controller (like AnyDesk). Input we inject ourselves carries
//! Windows' "injected" flag and is ignored here.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::proto::Input;

use windows_sys::Win32::System::SystemInformation::GetTickCount64;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, KBDLLHOOKSTRUCT, LLKHF_EXTENDED, LLKHF_INJECTED, LLKHF_UP, LLMHF_INJECTED, MSG,
    MSLLHOOKSTRUCT, SetWindowsHookExW, WH_KEYBOARD_LL, WH_MOUSE_LL,
};

/// How long remote input stays blocked after the last local action.
pub const LOCAL_PRIORITY_MS: u64 = 1000;

static LAST_LOCAL: AtomicU64 = AtomicU64::new(0);

/// While a remote session window is focused, every physical key goes to this sink and is
/// swallowed locally (like AnyDesk), so shortcuts such as Win+E act on the remote machine.
static CAPTURE: AtomicBool = AtomicBool::new(false);
static KEY_SINK: Mutex<Option<Box<dyn Fn(Input) + Send>>> = Mutex::new(None);
/// Keys currently held down on the remote because of us: (vk, scan, extended).
static HELD: Mutex<Vec<(u16, u16, bool)>> = Mutex::new(Vec::new());

/// Starts or stops sending the local keyboard to the remote. Stopping releases any key the
/// remote still thinks is held (e.g. Alt when the window lost focus during Alt+Tab).
pub fn set_key_capture(sink: Option<Box<dyn Fn(Input) + Send>>) {
    let on = sink.is_some();
    let mut current = KEY_SINK.lock().unwrap();
    if !on {
        if let Some(old) = current.as_ref() {
            for (vk, scan, extended) in HELD.lock().unwrap().drain(..) {
                old(Input::RawKey { vk, scan, extended, down: false });
            }
        }
    }
    *current = sink;
    CAPTURE.store(on, Ordering::Relaxed);
}

pub fn key_capture_active() -> bool {
    CAPTURE.load(Ordering::Relaxed)
}

unsafe extern "system" fn mouse_hook(code: i32, wparam: usize, lparam: isize) -> isize {
    if code >= 0 {
        let info = unsafe { &*(lparam as *const MSLLHOOKSTRUCT) };
        if info.flags & LLMHF_INJECTED == 0 {
            LAST_LOCAL.store(unsafe { GetTickCount64() }, Ordering::Relaxed);
        }
    }
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

unsafe extern "system" fn keyboard_hook(code: i32, wparam: usize, lparam: isize) -> isize {
    if code >= 0 {
        let info = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
        if info.flags & LLKHF_INJECTED == 0 {
            if CAPTURE.load(Ordering::Relaxed) {
                if let Some(sink) = KEY_SINK.lock().unwrap().as_ref() {
                    let (vk, scan) = (info.vkCode as u16, info.scanCode as u16);
                    let extended = info.flags & LLKHF_EXTENDED != 0;
                    let down = info.flags & LLKHF_UP == 0;
                    let mut held = HELD.lock().unwrap();
                    held.retain(|&(v, _, _)| v != vk);
                    if down {
                        held.push((vk, scan, extended));
                    }
                    sink(Input::RawKey { vk, scan, extended, down });
                    return 1; // swallow locally
                }
            }
            LAST_LOCAL.store(unsafe { GetTickCount64() }, Ordering::Relaxed);
        }
    }
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

/// Installs the low-level hooks on their own thread (hooks need a message loop).
pub fn start() {
    std::thread::spawn(|| unsafe {
        let module = std::ptr::null_mut();
        SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), module, 0);
        SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), module, 0);
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {}
    });
}

/// Whether this process runs as administrator. Without it, Windows blocks our clicks and keys
/// from reaching windows that run elevated (Task Manager, installers, "Run as administrator" apps).
pub fn is_elevated() -> bool {
    unsafe { windows_sys::Win32::UI::Shell::IsUserAnAdmin() != 0 }
}

/// Restarts this program with administrator rights (Windows shows its UAC prompt).
pub fn relaunch_as_admin() -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let (verb, file) = (wide("runas"), wide(&exe.to_string_lossy()));
    let r = unsafe {
        windows_sys::Win32::UI::Shell::ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
        )
    };
    r as isize > 32
}

/// True while the local user has priority.
pub fn local_active() -> bool {
    let last = LAST_LOCAL.load(Ordering::Relaxed);
    last != 0 && unsafe { GetTickCount64() }.saturating_sub(last) < LOCAL_PRIORITY_MS
}
