//! System-tray icon. Closing the window hides it here instead of quitting; the tray's menu
//! restores the window or quits for real. Runs on its own thread with its own Win32 message loop,
//! so it is independent of eframe's event loop.

use std::sync::OnceLock;

use eframe::egui;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    ExtractIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, GetCursorPos, PostQuitMessage,
    RegisterClassW, SetForegroundWindow, TrackPopupMenu, WM_APP, WM_COMMAND, WM_DESTROY, WM_LBUTTONDBLCLK, WM_LBUTTONUP,
    WM_RBUTTONUP, WNDCLASSW,
};

const CALLBACK_MSG: u32 = WM_APP + 1;
const ID_OPEN: usize = 1;
const ID_QUIT: usize = 2;

/// Set by the GUI so the tray thread can show the window again.
static CTX: OnceLock<egui::Context> = OnceLock::new();
static ICON_DATA: std::sync::Mutex<Option<NotifyIcon>> = std::sync::Mutex::new(None);

struct NotifyIcon(NOTIFYICONDATAW);
unsafe impl Send for NotifyIcon {}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Brings the window back to the foreground.
fn show_window() {
    if let Some(ctx) = CTX.get() {
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        ctx.request_repaint();
    }
}

/// Removes the tray icon and exits the whole process.
fn quit() {
    if let Some(icon) = ICON_DATA.lock().unwrap().as_mut() {
        unsafe { Shell_NotifyIconW(NIM_DELETE, &icon.0) };
    }
    std::process::exit(0);
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        CALLBACK_MSG => {
            let event = (lparam as u32) & 0xffff;
            match event {
                WM_LBUTTONUP | WM_LBUTTONDBLCLK => show_window(),
                WM_RBUTTONUP => unsafe { show_menu(hwnd) },
                _ => {}
            }
            0
        }
        WM_COMMAND => {
            match (wparam & 0xffff) as usize {
                ID_OPEN => show_window(),
                ID_QUIT => quit(),
                _ => {}
            }
            0
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

unsafe fn show_menu(hwnd: HWND) {
    let menu = unsafe { CreatePopupMenu() };
    if menu.is_null() {
        return;
    }
    let open = wide("เปิด RemoteDesk");
    let quit = wide("ออกจากโปรแกรม");
    const MF_STRING: u32 = 0;
    unsafe {
        AppendMenuW(menu, MF_STRING, ID_OPEN, open.as_ptr());
        AppendMenuW(menu, MF_STRING, ID_QUIT, quit.as_ptr());
        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);
        // Required so the menu closes when the user clicks elsewhere.
        SetForegroundWindow(hwnd);
        const TPM_RIGHTBUTTON: u32 = 2;
        TrackPopupMenu(menu, TPM_RIGHTBUTTON, pt.x, pt.y, 0, hwnd, std::ptr::null());
        DestroyMenu(menu);
    }
}

/// Starts the tray icon. Call once, after the GUI context exists.
pub fn start(ctx: egui::Context) {
    let _ = CTX.set(ctx);
    std::thread::spawn(|| unsafe {
        let hinstance = GetModuleHandleW(std::ptr::null());
        let class_name = wide("RemoteDeskTray");
        let mut wc: WNDCLASSW = std::mem::zeroed();
        wc.lpfnWndProc = Some(wnd_proc);
        wc.hInstance = hinstance;
        wc.lpszClassName = class_name.as_ptr();
        RegisterClassW(&wc);

        // Message-only window (HWND_MESSAGE parent) just to receive tray callbacks.
        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            wide("RemoteDesk").as_ptr(),
            0,
            0,
            0,
            0,
            0,
            -3isize as HWND, // HWND_MESSAGE
            std::ptr::null_mut(),
            hinstance,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            return;
        }

        // The app icon, taken straight from this exe's resources.
        let exe: Vec<u16> = std::env::current_exe()
            .ok()
            .map(|p| wide(&p.to_string_lossy()))
            .unwrap_or_else(|| wide(""));
        let hicon = ExtractIconW(hinstance, exe.as_ptr(), 0);

        let mut data: NOTIFYICONDATAW = std::mem::zeroed();
        data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = hwnd;
        data.uID = 1;
        data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        data.uCallbackMessage = CALLBACK_MSG;
        data.hIcon = hicon;
        let tip = wide("RemoteDesk");
        data.szTip[..tip.len()].copy_from_slice(&tip);
        Shell_NotifyIconW(NIM_ADD, &data);
        *ICON_DATA.lock().unwrap() = Some(NotifyIcon(data));

        // Pump messages for this thread's windows until quit.
        use windows_sys::Win32::UI::WindowsAndMessaging::{DispatchMessageW, GetMessageW, MSG, TranslateMessage};
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    });
}
