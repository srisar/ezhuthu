//! Small Win32 helpers shared by the modules.

use windows::Win32::Foundation::{HINSTANCE, HWND};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::Win32::UI::WindowsAndMessaging::{
    IDYES, MB_ICONERROR, MB_ICONINFORMATION, MB_ICONQUESTION, MB_OK, MB_YESNO, MESSAGEBOX_RESULT,
    MESSAGEBOX_STYLE, MessageBoxW,
};
use windows::core::{PCWSTR, w};

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

/// Windows' app mode (Settings > Personalisation > Colours): true for dark.
/// Light when the setting can't be read (older Windows).
pub fn apps_use_dark_theme() -> bool {
    let mut value = 1u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: `value` is a u32 and `size` its size; RegGetValueW writes at
    // most that many bytes for a REG_DWORD.
    let result = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut _),
            Some(&mut size),
        )
    };
    result.is_ok() && value == 0
}

pub fn error_box(owner: Option<HWND>, text: &str) {
    message(owner, text, MB_OK | MB_ICONERROR);
}

pub fn info_box(text: &str) {
    message(None, text, MB_OK | MB_ICONINFORMATION);
}

/// Yes/No question; true for Yes.
pub fn ask(text: &str) -> bool {
    message(None, text, MB_YESNO | MB_ICONQUESTION) == IDYES
}

fn message(owner: Option<HWND>, text: &str, style: MESSAGEBOX_STYLE) -> MESSAGEBOX_RESULT {
    let (text, title) = (wide(text), wide("Ezhuthu"));
    // SAFETY: both strings are NUL-terminated and outlive the call.
    unsafe { MessageBoxW(owner, PCWSTR(text.as_ptr()), PCWSTR(title.as_ptr()), style) }
}
