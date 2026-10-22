//! Detects physical mouse/keyboard use on the controlled machine, so the person sitting there
//! takes priority over the remote controller (like AnyDesk). Input we inject ourselves carries
//! Windows' "injected" flag and is ignored here.

use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Sender, channel};

use crate::proto::Input;

use windows_sys::Win32::System::SystemInformation::GetTickCount64;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL, VK_ESCAPE, VK_LWIN, VK_RWIN, VK_TAB};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, KBDLLHOOKSTRUCT, LLKHF_ALTDOWN, LLKHF_EXTENDED, LLKHF_INJECTED, LLKHF_UP,
    LLMHF_INJECTED, MSG,
    MSLLHOOKSTRUCT, SetWindowsHookExW, WH_KEYBOARD_LL, WH_MOUSE_LL,
};

/// How long remote input stays blocked after the last local action.
pub const LOCAL_PRIORITY_MS: u64 = 1000;

static LAST_LOCAL: AtomicU64 = AtomicU64::new(0);

/// While a remote session window is focused, the keys Windows would otherwise grab for itself
/// (Win, Win+anything, Alt+Tab, Alt+Esc, Ctrl+Esc) are forwarded to the remote and swallowed
/// locally. Every other key reaches the window normally and is sent from its egui events.
static CAPTURE: AtomicBool = AtomicBool::new(false);
/// A Win key is held down (touched only by the hook thread).
static WIN_HELD: AtomicBool = AtomicBool::new(false);

/// One captured key: (vk, scan, extended, down). Sent from the low-level hook — which must stay
/// fast or Windows silently drops it — to a forwarder thread that does the real work.
type KeyEvent = (u16, u16, bool, bool);
static KEY_TX: OnceLock<Sender<KeyEvent>> = OnceLock::new();
static KEY_SINK: Mutex<Option<Box<dyn Fn(Input) + Send>>> = Mutex::new(None);
/// Keys currently held on the remote because of us: (vk, scan, extended). Touched only by the
/// forwarder thread and `set_key_capture`, never by the hook, so a lock here is safe.
static HELD: Mutex<Vec<(u16, u16, bool)>> = Mutex::new(Vec::new());

/// Starts or stops sending the local keyboard to the remote. Stopping releases any key the
/// remote still thinks is held (e.g. Alt when the window lost focus during Alt+Tab).
pub fn set_key_capture(sink: Option<Box<dyn Fn(Input) + Send>>) {
    start();
    ensure_forwarder();
    let on = sink.is_some();
    // Flip the atomic first so the hook stops/starts swallowing immediately.
    CAPTURE.store(on, Ordering::Relaxed);
    if on {
        *KEY_SINK.lock().unwrap() = sink;
    } else {
        // Release held keys with the current sink before dropping it.
        let current = KEY_SINK.lock().unwrap().take();
        if let Some(s) = &current {
            for (vk, scan, extended) in HELD.lock().unwrap().drain(..) {
                s(Input::RawKey { vk, scan, extended, down: false });
            }
        }
    }
}

pub fn key_capture_active() -> bool {
    CAPTURE.load(Ordering::Relaxed)
}

/// The forwarder thread: reads keys from the hook, tracks which are held, and calls the current
/// sink. Kept off the hook so the hook never blocks.
fn ensure_forwarder() {
    KEY_TX.get_or_init(|| {
        let (tx, rx) = channel::<KeyEvent>();
        std::thread::spawn(move || {
            while let Ok((vk, scan, extended, down)) = rx.recv() {
                if !CAPTURE.load(Ordering::Relaxed) {
                    continue;
                }
                {
                    let mut held = HELD.lock().unwrap();
                    held.retain(|&(v, _, _)| v != vk);
                    if down {
                        held.push((vk, scan, extended));
                    }
                }
                if let Some(s) = KEY_SINK.lock().unwrap().as_ref() {
                    s(Input::RawKey { vk, scan, extended, down });
                }
            }
        });
        tx
    });
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

/// Whether the window in front belongs to this process. Checked on every key so a stale "focused"
/// flag (the UI only updates when it repaints) can never grab Alt+Tab from another program.
fn foreground_is_ours() -> bool {
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId, IsIconic};
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() || IsIconic(hwnd) != 0 {
            return false;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        pid == GetCurrentProcessId()
    }
}

unsafe extern "system" fn keyboard_hook(code: i32, wparam: usize, lparam: isize) -> isize {
    if code >= 0 {
        let info = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
        if info.flags & LLKHF_INJECTED == 0 {
            let vk = info.vkCode as u16;
            let down = info.flags & LLKHF_UP == 0;
            let is_win = vk == VK_LWIN || vk == VK_RWIN;
            if is_win {
                WIN_HELD.store(down, Ordering::Relaxed);
            }
            let system_key = is_win
                || WIN_HELD.load(Ordering::Relaxed)
                || ((vk == VK_TAB || vk == VK_ESCAPE) && info.flags & LLKHF_ALTDOWN != 0)
                || (vk == VK_ESCAPE && unsafe { GetAsyncKeyState(VK_CONTROL as i32) } < 0);
            if system_key && CAPTURE.load(Ordering::Relaxed) && foreground_is_ours() {
                // Keep this path minimal: just forward the key and swallow it. Doing real work
                // (locks, channel to the network) here can exceed Windows' hook timeout, which
                // silently drops the hook so the shortcut leaks to the local machine instead.
                let scan = info.scanCode as u16;
                let extended = info.flags & LLKHF_EXTENDED != 0;
                if let Some(tx) = KEY_TX.get() {
                    let _ = tx.send((vk, scan, extended, down));
                }
                return 1; // swallow locally
            }
            LAST_LOCAL.store(unsafe { GetTickCount64() }, Ordering::Relaxed);
        }
    }
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

/// Installs the low-level hooks on their own thread (hooks need a message loop).
/// Safe to call more than once: the controller needs the keyboard hook for Win/Alt+Tab, the
/// controlled machine needs both hooks to detect local use.
pub fn start() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| { std::thread::spawn(|| unsafe {
        let module = std::ptr::null_mut();
        SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), module, 0);
        SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), module, 0);
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {}
    }); });
}

/// Whether this process runs as administrator. Without it, Windows blocks our clicks and keys
/// from reaching windows that run elevated (Task Manager, installers, "Run as administrator" apps).
pub fn is_elevated() -> bool {
    unsafe { windows_sys::Win32::UI::Shell::IsUserAnAdmin() != 0 }
}

/// Restarts this program with administrator rights (Windows shows its UAC prompt).
pub fn relaunch_as_admin() -> bool {
    std::env::current_exe().map(|exe| shell_execute("runas", &exe)).unwrap_or(false)
}

/// Starts an exe the way Explorer would, honouring its manifest (UAC prompt if it needs admin).
pub fn shell_open(exe: &std::path::Path) -> bool {
    shell_execute("open", exe)
}

fn shell_execute(verb: &str, exe: &std::path::Path) -> bool {
    let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let (verb, file) = (wide(verb), wide(&exe.to_string_lossy()));
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

/// Whether a RemoteDesk window is open on screen (shown and not minimized).
pub fn own_window_open() -> bool {
    use windows_sys::Win32::Foundation::{HWND, LPARAM};
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    use windows_sys::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowThreadProcessId, IsIconic, IsWindowVisible};
    unsafe extern "system" fn check(hwnd: HWND, found: LPARAM) -> i32 {
        let mut pid = 0u32;
        unsafe {
            GetWindowThreadProcessId(hwnd, &mut pid);
            if pid == GetCurrentProcessId() && IsWindowVisible(hwnd) != 0 && IsIconic(hwnd) == 0 {
                *(found as *mut bool) = true;
                return 0; // stop
            }
        }
        1
    }
    let mut found = false;
    unsafe { EnumWindows(Some(check), &mut found as *mut bool as LPARAM) };
    found
}

/// Whether the window under the mouse belongs to a program running as administrator.
pub fn elevated_at_cursor() -> bool {
    let (x, y) = cursor_pos();
    let hwnd = unsafe { windows_sys::Win32::UI::WindowsAndMessaging::WindowFromPoint(windows_sys::Win32::Foundation::POINT { x, y }) };
    window_elevated(hwnd)
}

/// Whether the focused window belongs to a program running as administrator.
pub fn elevated_foreground() -> bool {
    window_elevated(unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow() })
}

fn window_elevated(hwnd: windows_sys::Win32::Foundation::HWND) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcessId, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
    if hwnd.is_null() {
        return false;
    }
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    // Our own windows run elevated but are not something to protect.
    if pid == 0 || pid == unsafe { GetCurrentProcessId() } {
        return false;
    }
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return true; // can't even look at it: a protected/system process
        }
        let mut token = std::ptr::null_mut();
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut len = 0u32;
        let ok = OpenProcessToken(process, TOKEN_QUERY, &mut token) != 0
            && GetTokenInformation(
                token,
                TokenElevation,
                &mut elevation as *mut _ as *mut _,
                std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                &mut len,
            ) != 0;
        if !token.is_null() {
            CloseHandle(token);
        }
        CloseHandle(process);
        !ok || elevation.TokenIsElevated != 0
    }
}

/// The mouse position in screen coordinates.
pub fn cursor_pos() -> (i32, i32) {
    let mut pt = windows_sys::Win32::Foundation::POINT { x: 0, y: 0 };
    unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut pt) };
    (pt.x, pt.y)
}

/// True while the local user has priority.
pub fn local_active() -> bool {
    let last = LAST_LOCAL.load(Ordering::Relaxed);
    last != 0 && unsafe { GetTickCount64() }.saturating_sub(last) < LOCAL_PRIORITY_MS
}
