//! Reading and writing the Windows clipboard's file list (CF_HDROP), which arboard cannot do.
//! Used so copying files on one machine and pasting on the other works like AnyDesk.

use std::path::PathBuf;

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{GHND, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows_sys::Win32::System::Ole::CF_HDROP;
use windows_sys::Win32::UI::Shell::{DROPFILES, DragQueryFileW};

/// Opens the clipboard, retrying briefly since another app may hold it momentarily.
struct ClipboardGuard;

impl ClipboardGuard {
    fn open() -> Option<ClipboardGuard> {
        for _ in 0..10 {
            if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
                return Some(ClipboardGuard);
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        None
    }
}

impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        unsafe { CloseClipboard() };
    }
}

/// The list of files currently on the clipboard, if any were copied (Ctrl+C in Explorer).
pub fn get_files() -> Option<Vec<PathBuf>> {
    let _guard = ClipboardGuard::open()?;
    let hdrop = unsafe { GetClipboardData(CF_HDROP as u32) };
    if hdrop.is_null() {
        return None;
    }
    let count = unsafe { DragQueryFileW(hdrop as _, u32::MAX, std::ptr::null_mut(), 0) };
    let mut files = Vec::with_capacity(count as usize);
    for i in 0..count {
        let len = unsafe { DragQueryFileW(hdrop as _, i, std::ptr::null_mut(), 0) };
        if len == 0 {
            continue;
        }
        let mut buf = vec![0u16; len as usize + 1];
        let got = unsafe { DragQueryFileW(hdrop as _, i, buf.as_mut_ptr(), buf.len() as u32) };
        if got > 0 {
            files.push(PathBuf::from(String::from_utf16_lossy(&buf[..got as usize])));
        }
    }
    (!files.is_empty()).then_some(files)
}

/// Replaces the clipboard with this list of files, so a paste drops them in Explorer etc.
pub fn set_files(paths: &[PathBuf]) -> bool {
    if paths.is_empty() {
        return false;
    }
    // DROPFILES header, then each path as UTF-16, then a final extra NUL.
    let mut wide: Vec<u16> = Vec::new();
    for p in paths {
        wide.extend(p.as_os_str().to_string_lossy().encode_utf16());
        wide.push(0);
    }
    wide.push(0);

    let header = std::mem::size_of::<DROPFILES>();
    let bytes = header + wide.len() * 2;
    unsafe {
        let hmem = GlobalAlloc(GHND, bytes);
        if hmem.is_null() {
            return false;
        }
        let ptr = GlobalLock(hmem) as *mut u8;
        if ptr.is_null() {
            return false;
        }
        let df = ptr as *mut DROPFILES;
        (*df).pFiles = header as u32;
        (*df).pt = std::mem::zeroed();
        (*df).fNC = 0;
        (*df).fWide = 1; // paths are UTF-16
        std::ptr::copy_nonoverlapping(wide.as_ptr() as *const u8, ptr.add(header), wide.len() * 2);
        GlobalUnlock(hmem);

        let Some(_guard) = ClipboardGuard::open() else {
            return false;
        };
        EmptyClipboard();
        // Ownership of hmem passes to the clipboard on success.
        !SetClipboardData(CF_HDROP as u32, hmem as HANDLE).is_null()
    }
}
