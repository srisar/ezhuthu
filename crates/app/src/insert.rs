//! Sends Tamil mode's output to the focused app: committed words (typed as
//! Unicode keystrokes, or pasted) and real keys replayed behind them.
//!
//! Every event we inject carries `MARK` in `dwExtraInfo`, so our keyboard
//! hook recognises its own output and lets it through untouched.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HGLOBAL, HWND};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData,
    GetClipboardSequenceNumber, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, SendInput, VIRTUAL_KEY, VK_CONTROL,
};
use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
use windows::core::{PCWSTR, w};

use crate::config::InsertMode;

/// Tag on every event we inject ("EZTH").
pub const MARK: usize = 0x455A_5448;

#[derive(Debug, Clone, PartialEq)]
pub enum Out {
    /// A committed Tamil word (plus its space, if any).
    Text(String),
    /// A real key event held back so it lands after earlier output.
    Key {
        vk: u16,
        scan: u16,
        up: bool,
        extended: bool,
    },
    /// Ctrl+C, to copy the selection (convert selection).
    Copy,
    /// Text pasted over the selection whatever the insert mode, keeping line
    /// breaks and tabs (convert selection's result).
    Paste(String),
}

/// Sends `items` in order. Returns how many events were injected (each of
/// them will pass through our hook) and the first error, if any.
pub fn deliver(items: &[Out], mode: InsertMode) -> (usize, Result<()>) {
    let mut injected = 0;
    let mut batch = Vec::new();
    for item in items {
        let pasted = match item {
            Out::Key {
                vk,
                scan,
                up,
                extended,
            } => {
                let mut flags = KEYBD_EVENT_FLAGS(0);
                if *up {
                    flags |= KEYEVENTF_KEYUP;
                }
                if *extended {
                    flags |= KEYEVENTF_EXTENDEDKEY;
                }
                batch.push(key(VIRTUAL_KEY(*vk), *scan, flags));
                None
            }
            Out::Copy => {
                batch.extend(ctrl_plus(b'C'));
                None
            }
            Out::Text(text) if mode == InsertMode::Unicode => {
                batch.extend(unicode_events(text));
                None
            }
            Out::Text(text) => Some(printable(text)),
            Out::Paste(text) => Some(keep_lines(text)),
        };
        if let Some(text) = pasted {
            // Keys queued before this text must land before the paste.
            if let Err(e) = send(&std::mem::take(&mut batch), &mut injected) {
                return (injected, Err(e));
            }
            if let Err(e) = paste(&text, &mut injected) {
                return (injected, Err(e));
            }
        }
    }
    let result = send(&batch, &mut injected);
    (injected, result)
}

/// How many events `deliver` will inject for `items` if nothing fails.
pub fn event_count(items: &[Out], mode: InsertMode) -> usize {
    items
        .iter()
        .map(|item| match (item, mode) {
            (Out::Key { .. }, _) => 1,
            (Out::Text(t), InsertMode::Unicode) => 2 * printable(t).encode_utf16().count(),
            // Ctrl down, C/V down, C/V up, Ctrl up
            (Out::Text(_), InsertMode::Clipboard) | (Out::Copy | Out::Paste(_), _) => 4,
        })
        .sum()
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
                dwExtraInfo: MARK,
            },
        },
    }
}

/// Injects `inputs`, adding the number actually delivered to `injected`.
fn send(inputs: &[INPUT], injected: &mut usize) -> Result<()> {
    if inputs.is_empty() {
        return Ok(());
    }
    // SAFETY: `inputs` is a valid slice of INPUT structs.
    let sent = unsafe { SendInput(inputs, size_of::<INPUT>() as i32) } as usize;
    *injected += sent;
    if sent != inputs.len() {
        bail!("SendInput delivered {sent} of {} events", inputs.len());
    }
    Ok(())
}

/// Drops control characters (Enter, Tab, ...) so text built from the model's
/// vocabulary can never trigger keystrokes in the target app.
fn printable(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}

/// Like `printable`, but keeps line breaks and tabs (pasted, never typed).
fn keep_lines(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || matches!(c, '\r' | '\n' | '\t'))
        .collect()
}

/// Ctrl + `letter`, down and up.
fn ctrl_plus(letter: u8) -> [INPUT; 4] {
    let k = VIRTUAL_KEY(letter as u16);
    [
        key(VK_CONTROL, 0, KEYBD_EVENT_FLAGS(0)),
        key(k, 0, KEYBD_EVENT_FLAGS(0)),
        key(k, 0, KEYEVENTF_KEYUP),
        key(VK_CONTROL, 0, KEYEVENTF_KEYUP),
    ]
}

/// One key-down/key-up pair per UTF-16 unit.
fn unicode_events(text: &str) -> Vec<INPUT> {
    printable(text)
        .encode_utf16()
        .flat_map(|u| {
            [
                key(VIRTUAL_KEY(0), u, KEYEVENTF_UNICODE),
                key(VIRTUAL_KEY(0), u, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP),
            ]
        })
        .collect()
}

/// Puts `text` (already filtered) on the clipboard, sends Ctrl+V, then puts
/// back what was there.
///
/// Every memory-backed format is saved and restored, including the privacy
/// markers password managers set, so their entries keep those markers. The
/// temporary Tamil text is itself kept out of clipboard history and cloud sync.
fn paste(text: &str, injected: &mut usize) -> Result<()> {
    let saved = Clipboard::open()?.save();
    let mut temp = vec![(CF_UNICODETEXT.0 as u32, utf16_bytes(text))];
    temp.extend(private_markers());
    Clipboard::open()?.replace(&temp)?;
    send(&ctrl_plus(b'V'), injected)?;
    // Give the target time to read the clipboard before restoring it.
    std::thread::sleep(Duration::from_millis(200));
    Clipboard::open()?.replace(&saved)
}

/// What was on the clipboard, kept to put back later.
pub struct SavedClipboard(Vec<(u32, Vec<u8>)>);

/// Saves the clipboard's contents (see `Clipboard::save`).
pub fn save_clipboard() -> Result<SavedClipboard> {
    Ok(SavedClipboard(Clipboard::open()?.save()))
}

/// Puts saved contents back on the clipboard.
pub fn restore_clipboard(saved: &SavedClipboard) -> Result<()> {
    Clipboard::open()?.replace(&saved.0)
}

/// Changes whenever anything is put on the clipboard.
pub fn clipboard_sequence() -> u32 {
    // SAFETY: plain query.
    unsafe { GetClipboardSequenceNumber() }
}

/// The clipboard's text, if it holds any.
pub fn clipboard_text() -> Result<Option<String>> {
    let _open = Clipboard::open()?;
    // SAFETY: the clipboard is open; the HGLOBAL is locked, read within its
    // GlobalSize (up to the first NUL) and unlocked.
    unsafe {
        let Ok(h) = GetClipboardData(CF_UNICODETEXT.0 as u32) else {
            return Ok(None);
        };
        let hglobal = HGLOBAL(h.0);
        let units = GlobalSize(hglobal) / 2;
        let p = GlobalLock(hglobal) as *const u16;
        if p.is_null() {
            return Ok(None);
        }
        let all = std::slice::from_raw_parts(p, units);
        let len = all.iter().position(|&u| u == 0).unwrap_or(units);
        let text = String::from_utf16_lossy(&all[..len]);
        let _ = GlobalUnlock(hglobal);
        Ok(Some(text))
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_the_events_deliver_will_inject() {
        let key = Out::Key {
            vk: 0x41,
            scan: 0x1E,
            up: false,
            extended: false,
        };
        let items = [Out::Text("அம்மா ".into()), key];
        // 6 UTF-16 units x (down + up) + 1 replayed key
        assert_eq!(event_count(&items, InsertMode::Unicode), 13);
        // Ctrl+V (4 events) + 1 replayed key
        assert_eq!(event_count(&items, InsertMode::Clipboard), 5);
        // control characters are never typed
        assert_eq!(
            event_count(&[Out::Text("a\tb".into())], InsertMode::Unicode),
            4
        );
    }
}
