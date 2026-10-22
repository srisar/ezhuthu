//! Delivers the Tamil text to the target app, which still has focus (the
//! popup never activates): typed as Unicode keystrokes, or pasted.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HGLOBAL, HWND};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData,
    IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP,
    KEYEVENTF_UNICODE, SendInput, VIRTUAL_KEY, VK_CONTROL,
};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

use windows::core::{PCWSTR, w};

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

/// Drops control characters (Enter, Tab, ...) so text built from the model's
/// vocabulary can never trigger keystrokes in the target app.
fn printable(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}

/// One key-down/key-up pair per UTF-16 unit, in a single batch.
fn send_unicode(text: &str) -> Result<()> {
    let inputs: Vec<INPUT> = printable(text)
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

/// Puts `text` on the clipboard, sends Ctrl+V, then puts back what was there.
///
/// Every memory-backed format is saved and restored, including the privacy
/// markers password managers set, so their entries keep those markers. The
/// temporary Tamil text is itself kept out of clipboard history and cloud sync.
fn paste(text: &str) -> Result<()> {
    let saved = Clipboard::open()?.save();
    let mut temp = vec![(CF_UNICODETEXT.0 as u32, utf16_bytes(&printable(text)))];
    temp.extend(private_markers());
    Clipboard::open()?.replace(&temp)?;
    let v = VIRTUAL_KEY(b'V' as u16);
    send(&[
        key(VK_CONTROL, 0, KEYBD_EVENT_FLAGS(0)),
        key(v, 0, KEYBD_EVENT_FLAGS(0)),
        key(v, 0, KEYEVENTF_KEYUP),
        key(VK_CONTROL, 0, KEYEVENTF_KEYUP),
    ])?;
    // Give the target time to read the clipboard before restoring it.
    std::thread::sleep(Duration::from_millis(200));
    Clipboard::open()?.replace(&saved)
}

fn utf16_bytes(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect()
}

/// Formats that keep an entry out of clipboard history and cloud clipboard
/// (and away from clipboard monitors).
fn private_markers() -> Vec<(u32, Vec<u8>)> {
    let format = |name: PCWSTR| {
        // SAFETY: registering a named format with a NUL-terminated literal.
        unsafe { RegisterClipboardFormatW(name) }
    };
    vec![
        (
            format(w!("ExcludeClipboardContentFromMonitorProcessing")),
            vec![0],
        ),
        (
            format(w!("CanIncludeInClipboardHistory")),
            0u32.to_le_bytes().to_vec(),
        ),
        (
            format(w!("CanUploadToCloudClipboard")),
            0u32.to_le_bytes().to_vec(),
        ),
    ]
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

    /// Copies every format whose data is an HGLOBAL (text, HTML, RTF, DIB
    /// images, file lists, privacy markers...). GDI-handle formats are
    /// skipped; Windows synthesises CF_BITMAP from CF_DIB on restore.
    fn save(&self) -> Vec<(u32, Vec<u8>)> {
        // Formats whose handle is not an HGLOBAL.
        const NOT_HGLOBAL: [u32; 6] = [2, 3, 9, 14, 0x0080, 0x008E]; // BITMAP, METAFILEPICT, PALETTE, ENHMETAFILE, OWNERDISPLAY, DSPENHMETAFILE
        let mut out = Vec::new();
        let mut format = 0;
        // SAFETY: the clipboard is open (self). Each HGLOBAL is locked, copied
        // within its GlobalSize, and unlocked.
        unsafe {
            loop {
                format = EnumClipboardFormats(format);
                if format == 0 {
                    break;
                }
                if NOT_HGLOBAL.contains(&format) {
                    continue;
                }
                let Ok(h) = GetClipboardData(format) else {
                    continue;
                };
                let hglobal = HGLOBAL(h.0);
                let size = GlobalSize(hglobal);
                let p = GlobalLock(hglobal) as *const u8;
                if p.is_null() {
                    continue;
                }
                out.push((format, std::slice::from_raw_parts(p, size).to_vec()));
                let _ = GlobalUnlock(hglobal);
            }
        }
        out
    }

    /// Empties the clipboard and puts `entries` on it.
    fn replace(&self, entries: &[(u32, Vec<u8>)]) -> Result<()> {
        // SAFETY: the clipboard is open (self). Each HGLOBAL is sized for its
        // bytes; after SetClipboardData succeeds the system owns it.
        unsafe {
            EmptyClipboard().context("emptying the clipboard")?;
            for (format, bytes) in entries {
                let hmem = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1))?;
                let p = GlobalLock(hmem) as *mut u8;
                if p.is_null() {
                    bail!("GlobalLock failed");
                }
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
                let _ = GlobalUnlock(hmem);
                SetClipboardData(*format, Some(HANDLE(hmem.0)))
                    .with_context(|| format!("restoring clipboard format {format}"))?;
            }
        }
        Ok(())
    }
}

impl Drop for Clipboard {
    fn drop(&mut self) {
        // SAFETY: we opened it in `open`.
        let _ = unsafe { CloseClipboard() };
    }
}

/// The clipboard's text, for Ctrl+V into the popup.
pub fn read_clipboard_text() -> Option<String> {
    let _clip = Clipboard::open().ok()?;
    // SAFETY: the clipboard is open; the handle is a NUL-terminated UTF-16
    // HGLOBAL while CF_UNICODETEXT is available, and is unlocked again.
    unsafe {
        IsClipboardFormatAvailable(CF_UNICODETEXT.0 as u32).ok()?;
        let h = GetClipboardData(CF_UNICODETEXT.0 as u32).ok()?;
        let hglobal = HGLOBAL(h.0);
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
