//! Small Win32 helpers shared by the modules.

use windows::Win32::Foundation::{HINSTANCE, HWND};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
use windows::core::PCWSTR;

/// NUL-terminated UTF-16 for Win32 string parameters.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Copies `s` into a fixed-size Win32 string field, truncating and NUL-terminating.
pub fn copy_into(dst: &mut [u16], s: &str) {
    let n = dst.len() - 1;
    let mut i = 0;
    for unit in s.encode_utf16().take(n) {
        dst[i] = unit;
        i += 1;
    }
    dst[i] = 0;
}

pub fn instance() -> HINSTANCE {
    // SAFETY: a null module name returns the handle of the running exe.
    unsafe { GetModuleHandleW(None) }
        .map(Into::into)
        .unwrap_or_default()
}

/// Scales a 96-DPI length to `dpi`.
pub fn scale(v: i32, dpi: u32) -> i32 {
    (v as i64 * dpi as i64 / 96) as i32
}

pub fn error_box(owner: Option<HWND>, text: &str) {
    let (text, title) = (wide(text), wide("ezhuthu"));
    // SAFETY: both strings are NUL-terminated and outlive the call.
    unsafe {
        MessageBoxW(
            owner,
            PCWSTR(text.as_ptr()),
            PCWSTR(title.as_ptr()),
            MB_OK | MB_ICONERROR,
        );
    }
}
