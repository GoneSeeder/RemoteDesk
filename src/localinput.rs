//! Detects physical mouse/keyboard use on the controlled machine, so the person sitting there
//! takes priority over the remote controller (like AnyDesk). Input we inject ourselves carries
//! Windows' "injected" flag and is ignored here.

use std::sync::atomic::{AtomicU64, Ordering};

use windows_sys::Win32::System::SystemInformation::GetTickCount64;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, KBDLLHOOKSTRUCT, LLKHF_INJECTED, LLMHF_INJECTED, MSG, MSLLHOOKSTRUCT,
    SetWindowsHookExW, WH_KEYBOARD_LL, WH_MOUSE_LL,
};

/// How long remote input stays blocked after the last local action.
pub const LOCAL_PRIORITY_MS: u64 = 1000;

static LAST_LOCAL: AtomicU64 = AtomicU64::new(0);

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

/// True while the local user has priority.
pub fn local_active() -> bool {
    let last = LAST_LOCAL.load(Ordering::Relaxed);
    last != 0 && unsafe { GetTickCount64() }.saturating_sub(last) < LOCAL_PRIORITY_MS
}
