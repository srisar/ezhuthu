//! Delivers the Tamil text to the target app, which still has focus (the
//! popup never activates): typed as Unicode keystrokes, or pasted.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    SetClipboardData,
};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP,
    KEYEVENTF_UNICODE, SendInput, VIRTUAL_KEY, VK_CONTROL,
};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

use crate::config::InsertMode;

pub fn insert(target: HWND, text: &str, mode: InsertMode) -> Result<()> {
    // SAFETY: plain query.
    if unsafe { GetForegroundWindow() } != target {
        bail!("the window you were typing in is no longer active");
    }
    match mode {
        InsertMode::Unicode => send_unicode(text),
        InsertMode::Clipboard => paste(text),
    }
}

fn key(vk: VIRTUAL_KEY, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn send(inputs: &[INPUT]) -> Result<()> {
    // SAFETY: `inputs` is a valid slice of INPUT structs.
    let sent = unsafe { SendInput(inputs, size_of::<INPUT>() as i32) };
    if sent as usize != inputs.len() {
        bail!("SendInput delivered {sent} of {} events", inputs.len());
    }
    Ok(())
}

/// One key-down/key-up pair per UTF-16 unit, in a single batch.
fn send_unicode(text: &str) -> Result<()> {
    let inputs: Vec<INPUT> = text
        .encode_utf16()
        .flat_map(|u| {
            [
                key(VIRTUAL_KEY(0), u, KEYEVENTF_UNICODE),
                key(VIRTUAL_KEY(0), u, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP),
            ]
        })
        .collect();
    send(&inputs)
}

/// Puts `text` on the clipboard, sends Ctrl+V, then restores the previous
/// clipboard text (other clipboard formats are not preserved).
fn paste(text: &str) -> Result<()> {
    let saved = read_clipboard_text();
    write_clipboard_text(Some(text))?;
    let v = VIRTUAL_KEY(b'V' as u16);
    send(&[
        key(VK_CONTROL, 0, KEYBD_EVENT_FLAGS(0)),
        key(v, 0, KEYBD_EVENT_FLAGS(0)),
        key(v, 0, KEYEVENTF_KEYUP),
        key(VK_CONTROL, 0, KEYEVENTF_KEYUP),
    ])?;
    // Give the target time to read the clipboard before restoring it.
    std::thread::sleep(Duration::from_millis(200));
    write_clipboard_text(saved.as_deref())
}

/// RAII guard: the clipboard is open while this lives.
struct Clipboard;

impl Clipboard {
    fn open() -> Result<Self> {
        for _ in 0..10 {
            // SAFETY: opening without an owner window is allowed.
            if unsafe { OpenClipboard(None) }.is_ok() {
                return Ok(Self);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        bail!("the clipboard is busy")
    }
}

impl Drop for Clipboard {
    fn drop(&mut self) {
        // SAFETY: we opened it in `open`.
        let _ = unsafe { CloseClipboard() };
    }
}

pub fn read_clipboard_text() -> Option<String> {
    let _clip = Clipboard::open().ok()?;
    // SAFETY: the clipboard is open; the handle is a NUL-terminated UTF-16
    // HGLOBAL while CF_UNICODETEXT is available, and is unlocked again.
    unsafe {
        IsClipboardFormatAvailable(CF_UNICODETEXT.0 as u32).ok()?;
        let h = GetClipboardData(CF_UNICODETEXT.0 as u32).ok()?;
        let hglobal = windows::Win32::Foundation::HGLOBAL(h.0);
        let p = GlobalLock(hglobal) as *const u16;
        if p.is_null() {
            return None;
        }
        let len = (0..).take_while(|&i| *p.add(i) != 0).count();
        let text = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
        let _ = GlobalUnlock(hglobal);
        Some(text)
    }
}

fn write_clipboard_text(text: Option<&str>) -> Result<()> {
    let _clip = Clipboard::open()?;
    // SAFETY: the clipboard is open. The HGLOBAL is sized for the text plus
    // NUL; after SetClipboardData succeeds the system owns it.
    unsafe {
        EmptyClipboard().context("emptying the clipboard")?;
        let Some(text) = text else { return Ok(()) };
        let units: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let hmem = GlobalAlloc(GMEM_MOVEABLE, units.len() * 2)?;
        let p = GlobalLock(hmem) as *mut u16;
        if p.is_null() {
            bail!("GlobalLock failed");
        }
        std::ptr::copy_nonoverlapping(units.as_ptr(), p, units.len());
        let _ = GlobalUnlock(hmem);
        SetClipboardData(CF_UNICODETEXT.0 as u32, Some(HANDLE(hmem.0)))
            .context("setting the clipboard")?;
    }
    Ok(())
}

/// True when `hwnd` belongs to an elevated process and we are not elevated:
/// Windows (UIPI) then drops both our keyboard hook events and SendInput.
pub fn blocked_by_uipi(hwnd: HWND) -> bool {
    // SAFETY: GetCurrentProcess is a pseudo-handle that needs no closing.
    if process_elevated(unsafe { GetCurrentProcess() }) == Some(true) {
        return false;
    }
    let mut pid = 0;
    // SAFETY: plain query.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    // SAFETY: the handle is closed below.
    let Ok(process) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) })
    else {
        return false;
    };
    // Unreadable token: elevated processes refuse TOKEN_QUERY to us.
    let elevated = process_elevated(process).unwrap_or(true);
    // SAFETY: we own the handle.
    let _ = unsafe { CloseHandle(process) };
    elevated
}

fn process_elevated(process: HANDLE) -> Option<bool> {
    // SAFETY: the token handle is closed below; TOKEN_ELEVATION is sized.
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(process, TOKEN_QUERY, &mut token).ok()?;
        let mut elevation = TOKEN_ELEVATION::default();
        let mut len = 0;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut _),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        );
        let _ = CloseHandle(token);
        ok.ok()?;
        Some(elevation.TokenIsElevated != 0)
    }
}
