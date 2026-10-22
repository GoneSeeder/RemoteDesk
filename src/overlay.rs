//! On the controlled machine: a purple pointer drawn where the controller is pointing, so the
//! person sitting there can tell the remote is moving the mouse. It is a topmost, click-through
//! layered window that follows remote mouse moves and hides once they stop.

use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::mpsc::{Sender, channel};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{POINT, SIZE};
use windows_sys::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, CreateCompatibleDC,
    CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, HWND_TOPMOST, MSG, PM_REMOVE, PeekMessageW, RegisterClassW,
    SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOSIZE, SetWindowPos, ShowWindow, TranslateMessage, ULW_ALPHA,
    UpdateLayeredWindow, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_EX_TRANSPARENT, WS_POPUP,
};

/// Pointer bitmap size in pixels.
const W: i32 = 24;
const H: i32 = 30;
/// Hide the pointer this long after the last remote move.
const HIDE_AFTER: Duration = Duration::from_millis(1500);

static TX: OnceLock<Mutex<Sender<(i32, i32)>>> = OnceLock::new();

/// A remote mouse move landed at screen position (x, y).
pub fn moved(x: i32, y: i32) {
    let tx = TX.get_or_init(|| {
        let (tx, rx) = channel();
        std::thread::spawn(move || run(rx));
        Mutex::new(tx)
    });
    let _ = tx.lock().unwrap().send((x, y));
}

/// Hides the pointer right away (session ended, or the local user took over).
pub fn hide() {
    if let Some(tx) = TX.get() {
        let _ = tx.lock().unwrap().send((i32::MIN, i32::MIN));
    }
}

fn run(rx: std::sync::mpsc::Receiver<(i32, i32)>) {
    unsafe {
        let class: Vec<u16> = "RemoteDeskPointer\0".encode_utf16().collect();
        let wc = WNDCLASSW {
            lpfnWndProc: Some(DefWindowProcW),
            lpszClassName: class.as_ptr(),
            ..std::mem::zeroed()
        };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class.as_ptr(),
            class.as_ptr(),
            WS_POPUP,
            0,
            0,
            W,
            H,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null(),
        );
        if hwnd.is_null() {
            return;
        }

        // Render the pointer once into a premultiplied BGRA bitmap.
        let screen = GetDC(std::ptr::null_mut());
        let mem = CreateCompatibleDC(screen);
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: W,
            biHeight: -H, // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..std::mem::zeroed()
        };
        let mut bits = std::ptr::null_mut();
        let bmp = CreateDIBSection(mem, &info, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
        let old = SelectObject(mem, bmp);
        std::slice::from_raw_parts_mut(bits as *mut u32, (W * H) as usize).copy_from_slice(&render());

        let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
        let size = SIZE { cx: W, cy: H };
        let src = POINT { x: 0, y: 0 };
        let mut shown = false;
        let mut last = Instant::now();
        loop {
            let mut msg: MSG = std::mem::zeroed();
            while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok((i32::MIN, _)) => {
                    ShowWindow(hwnd, SW_HIDE);
                    shown = false;
                }
                Ok((x, y)) => {
                    // Keep only the latest position when moves queue up.
                    let (mut x, mut y) = (x, y);
                    while let Ok((nx, ny)) = rx.try_recv() {
                        if nx == i32::MIN {
                            break;
                        }
                        (x, y) = (nx, ny);
                    }
                    let dst = POINT { x, y };
                    UpdateLayeredWindow(hwnd, screen, &dst, &size, mem, &src, 0, &blend, ULW_ALPHA);
                    SetWindowPos(hwnd, HWND_TOPMOST, x, y, 0, 0, SWP_NOSIZE | SWP_NOACTIVATE);
                    if !shown {
                        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                        shown = true;
                    }
                    last = Instant::now();
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => break,
            }
            if shown && (last.elapsed() > HIDE_AFTER || crate::localinput::local_active()) {
                ShowWindow(hwnd, SW_HIDE);
                shown = false;
            }
        }
        SelectObject(mem, old);
        DeleteObject(bmp);
        DeleteDC(mem);
        ReleaseDC(std::ptr::null_mut(), screen);
    }
}

/// The arrow (same shape as the controller-side purple pointer) with a white outline,
/// anti-aliased by 4x4 supersampling. Returns premultiplied BGRA pixels.
fn render() -> Vec<u32> {
    const SCALE: f32 = 1.4;
    let poly: Vec<(f32, f32)> = [(0.0, 0.0), (0.0, 17.0), (4.5, 13.0), (7.5, 19.5), (10.5, 18.0), (7.5, 11.8), (13.0, 11.8)]
        .iter()
        .map(|&(x, y)| (x * SCALE + 1.5, y * SCALE + 1.5))
        .collect();
    let inside = |px: f32, py: f32| {
        let mut c = false;
        for i in 0..poly.len() {
            let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
            if (a.1 > py) != (b.1 > py) && px < (b.0 - a.0) * (py - a.1) / (b.1 - a.1) + a.0 {
                c = !c;
            }
        }
        c
    };
    let edge_dist = |px: f32, py: f32| {
        let mut best = f32::MAX;
        for i in 0..poly.len() {
            let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            let t = (((px - a.0) * dx + (py - a.1) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
            let (ex, ey) = (a.0 + t * dx - px, a.1 + t * dy - py);
            best = best.min((ex * ex + ey * ey).sqrt());
        }
        best
    };
    // Purple fill (ACCENT_LIGHT), white outline.
    let (pr, pg, pb) = (163.0, 104.0, 252.0);
    let mut out = vec![0u32; (W * H) as usize];
    for y in 0..H {
        for x in 0..W {
            let (mut r, mut g, mut b, mut a) = (0.0f32, 0.0, 0.0, 0.0);
            for sy in 0..4 {
                for sx in 0..4 {
                    let (px, py) = (x as f32 + (sx as f32 + 0.5) / 4.0, y as f32 + (sy as f32 + 0.5) / 4.0);
                    let d = edge_dist(px, py);
                    let ins = inside(px, py);
                    if ins && d > 1.4 {
                        (r, g, b, a) = (r + pr, g + pg, b + pb, a + 1.0);
                    } else if ins || d < 1.0 {
                        (r, g, b, a) = (r + 255.0, g + 255.0, b + 255.0, a + 1.0);
                    }
                }
            }
            let n = 16.0;
            let (r, g, b, a) = (r / n, g / n, b / n, a / n);
            // Already premultiplied: each sample contributed full color with full alpha.
            out[(y * W + x) as usize] =
                ((a * 255.0) as u32) << 24 | (r as u32) << 16 | (g as u32) << 8 | b as u32;
        }
    }
    out
}
