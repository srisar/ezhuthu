//! The Settings window (tray menu > Settings...): hotkey, insert method,
//! suggestions per word, Start with Windows. Modeless; it runs in the app's
//! message loop (which routes its keys through IsDialogMessageW).
//!
//! OK calls the app's `apply` callback, which validates and saves; if that
//! fails (e.g. the hotkey is taken) the window stays open with the message.

use std::cell::RefCell;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    COLOR_WINDOW, CreateFontIndirectW, DeleteObject, GetMonitorInfoW, GetSysColorBrush, HBRUSH,
    HDC, HFONT, HGDIOBJ, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint, SetBkMode,
    TRANSPARENT,
};
use windows::Win32::UI::Controls::{
    BST_CHECKED, BST_UNCHECKED, CheckDlgButton, HKCOMB_NONE, HKCOMB_S, HKM_GETHOTKEY,
    HKM_SETHOTKEY, HKM_SETRULES, HOTKEY_CLASSW, HOTKEYF_ALT, HOTKEYF_CONTROL, ICC_HOTKEY_CLASS,
    INITCOMMONCONTROLSEX, InitCommonControlsEx, IsDlgButtonChecked,
};
use windows::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, GetDpiForSystem, SystemParametersInfoForDpi,
};
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::{
    BS_AUTOCHECKBOX, BS_AUTORADIOBUTTON, BS_DEFPUSHBUTTON, BS_PUSHBUTTON, CB_ADDSTRING,
    CB_GETCURSEL, CB_SETCURSEL, CBS_DROPDOWNLIST, CreateWindowExW, DefWindowProcW, DestroyWindow,
    GetDlgItem, HMENU, IDC_ARROW, IDCANCEL, IDOK, LoadCursorW, MB_ICONWARNING, MB_OK, MessageBoxW,
    NONCLIENTMETRICSW, RegisterClassExW, SPI_GETNONCLIENTMETRICS, SW_SHOW, SendMessageW,
    SetForegroundWindow, ShowWindow, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_COMMAND,
    WM_CTLCOLORSTATIC, WM_DESTROY, WM_SETFONT, WNDCLASSEXW, WS_CAPTION, WS_CHILD, WS_EX_CLIENTEDGE,
    WS_GROUP, WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};
use windows::core::{HSTRING, PCWSTR, w};

use crate::config::InsertMode;
use crate::hotkey::Hotkey;
use crate::win::instance;

const ID_HOTKEY: i32 = 10;
const ID_TYPE: i32 = 11;
const ID_PASTE: i32 = 12;
const ID_BEAM: i32 = 13;
const ID_AUTOSTART: i32 = 14;
/// Suggestions per word offered in the dropdown.
const BEAM_CHOICES: std::ops::RangeInclusive<usize> = 3..=9;

#[derive(Debug, Clone, PartialEq)]
pub struct Values {
    pub hotkey: String,
    pub insert_mode: InsertMode,
    pub beam: usize,
    pub autostart: bool,
}

/// Validates, applies and saves; Err(message) keeps the window open.
pub type Apply = fn(&Values) -> Result<(), String>;

struct Window {
    hwnd: HWND,
    font: HFONT,
    initial: Values,
    apply: Apply,
    on_close: fn(),
}

thread_local! {
    static WINDOW: RefCell<Option<Window>> = const { RefCell::new(None) };
}

/// The open Settings window, if any.
pub fn hwnd() -> Option<HWND> {
    WINDOW.with(|w| w.borrow().as_ref().map(|w| w.hwnd))
}

/// Opens the Settings window (or brings it to the front). `on_close` runs
/// when it closes, whether by OK or Cancel.
pub fn open(values: Values, apply: Apply, on_close: fn()) {
    if let Some(h) = hwnd() {
        // SAFETY: bringing our own window to the front.
        let _ = unsafe { SetForegroundWindow(h) };
        return;
    }
    if let Err(e) = create(values, apply, on_close) {
        crate::win::error_box(None, &format!("Couldn't open Settings:\n\n{e:#}"));
        on_close();
    }
}

fn create(values: Values, apply: Apply, on_close: fn()) -> windows::core::Result<()> {
    // SAFETY: standard window creation on the UI thread; all strings outlive
    // the calls that use them; the font is deleted in WM_DESTROY.
    unsafe {
        let icc = INITCOMMONCONTROLSEX {
            dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_HOTKEY_CLASS,
        };
        let _ = InitCommonControlsEx(&icc);
        let class = w!("ezhuthu.settings");
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(window_proc),
            hInstance: instance(),
            hbrBackground: GetSysColorBrush(COLOR_WINDOW),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassExW(&wc);

        let dpi = GetDpiForSystem();
        let s = |v: i32| v * dpi as i32 / 96;
        let mut ncm = NONCLIENTMETRICSW {
            cbSize: size_of::<NONCLIENTMETRICSW>() as u32,
            ..Default::default()
        };
        let _ = SystemParametersInfoForDpi(
            SPI_GETNONCLIENTMETRICS.0,
            ncm.cbSize,
            Some(&mut ncm as *mut _ as *mut _),
            0,
            dpi,
        );
        let font = CreateFontIndirectW(&ncm.lfMessageFont);

        // Client area 440 x 292 (96-DPI units), centred on the primary monitor.
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU;
        let mut r = RECT {
            left: 0,
            top: 0,
            right: s(440),
            bottom: s(292),
        };
        let _ = AdjustWindowRectExForDpi(&mut r, style, false, WINDOW_EX_STYLE(0), dpi);
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        let mut mi = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let _ = GetMonitorInfoW(
            MonitorFromPoint(Default::default(), MONITOR_DEFAULTTOPRIMARY),
            &mut mi,
        );
        let work = mi.rcWork;
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            w!("ezhuthu settings"),
            style,
            work.left + (work.right - work.left - w) / 2,
            work.top + (work.bottom - work.top - h) / 2,
            w,
            h,
            None,
            None,
            Some(instance()),
            None,
        )?;

        let child = |class: PCWSTR,
                     text: &str,
                     style: u32,
                     ex: WINDOW_EX_STYLE,
                     id: i32,
                     rect: (i32, i32, i32, i32)| {
            let ctl = CreateWindowExW(
                ex,
                class,
                &HSTRING::from(text),
                WS_CHILD | WS_VISIBLE | WINDOW_STYLE(style),
                s(rect.0),
                s(rect.1),
                s(rect.2),
                s(rect.3),
                Some(hwnd),
                Some(HMENU(id as isize as *mut _)),
                Some(instance()),
                None,
            );
            if let Ok(ctl) = ctl {
                SendMessageW(
                    ctl,
                    WM_SETFONT,
                    Some(WPARAM(font.0 as usize)),
                    Some(LPARAM(1)),
                );
            }
            ctl
        };
        let none = WINDOW_EX_STYLE(0);
        let tab = WS_TABSTOP.0;

        child(
            w!("STATIC"),
            "Tamil mode on/off:",
            0,
            none,
            -1,
            (16, 18, 150, 20),
        )?;
        let hotkey = child(
            HOTKEY_CLASSW,
            "",
            tab,
            WS_EX_CLIENTEDGE,
            ID_HOTKEY,
            (170, 14, 180, 26),
        )?;
        child(
            w!("STATIC"),
            "Click the box, then press the new keys.",
            0,
            none,
            -1,
            (170, 44, 260, 18),
        )?;

        child(
            w!("STATIC"),
            "Insert text by:",
            0,
            none,
            -1,
            (16, 76, 400, 20),
        )?;
        child(
            w!("BUTTON"),
            "Typing the characters (works in most apps)",
            tab | WS_GROUP.0 | BS_AUTORADIOBUTTON as u32,
            none,
            ID_TYPE,
            (32, 98, 392, 22),
        )?;
        child(
            w!("BUTTON"),
            "Pasting through the clipboard (for apps that garble typed Tamil)",
            BS_AUTORADIOBUTTON as u32,
            none,
            ID_PASTE,
            (32, 122, 392, 22),
        )?;

        child(
            w!("STATIC"),
            "Suggestions per word:",
            0,
            none,
            -1,
            (16, 162, 150, 20),
        )?;
        let beam = child(
            w!("COMBOBOX"),
            "",
            tab | WS_GROUP.0 | WS_VSCROLL.0 | CBS_DROPDOWNLIST as u32,
            none,
            ID_BEAM,
            (170, 158, 80, 200),
        )?;
        child(
            w!("BUTTON"),
            "Start ezhuthu when I sign in to Windows",
            tab | WS_GROUP.0 | BS_AUTOCHECKBOX as u32,
            none,
            ID_AUTOSTART,
            (16, 200, 408, 22),
        )?;
        child(
            w!("BUTTON"),
            "OK",
            tab | WS_GROUP.0 | BS_DEFPUSHBUTTON as u32,
            none,
            IDOK.0,
            (256, 248, 80, 28),
        )?;
        child(
            w!("BUTTON"),
            "Cancel",
            tab | BS_PUSHBUTTON as u32,
            none,
            IDCANCEL.0,
            (344, 248, 80, 28),
        )?;

        // Current values.
        // Rules: a bare key or Shift+key is invalid; the control turns it into Ctrl+Alt+key.
        SendMessageW(
            hotkey,
            HKM_SETRULES,
            Some(WPARAM((HKCOMB_NONE | HKCOMB_S) as usize)),
            Some(LPARAM((HOTKEYF_CONTROL | HOTKEYF_ALT) as isize)),
        );
        let control_value = Hotkey::parse(&values.hotkey)
            .ok()
            .and_then(Hotkey::to_control);
        SendMessageW(
            hotkey,
            HKM_SETHOTKEY,
            Some(WPARAM(control_value.unwrap_or(0) as usize)),
            None,
        );
        let radio = if values.insert_mode == InsertMode::Clipboard {
            ID_PASTE
        } else {
            ID_TYPE
        };
        let _ = CheckDlgButton(hwnd, radio, BST_CHECKED);
        for n in BEAM_CHOICES {
            SendMessageW(
                beam,
                CB_ADDSTRING,
                None,
                Some(LPARAM(HSTRING::from(n.to_string()).as_ptr() as isize)),
            );
        }
        let beam_index = values
            .beam
            .clamp(*BEAM_CHOICES.start(), *BEAM_CHOICES.end())
            - BEAM_CHOICES.start();
        SendMessageW(beam, CB_SETCURSEL, Some(WPARAM(beam_index)), None);
        let _ = CheckDlgButton(
            hwnd,
            ID_AUTOSTART,
            if values.autostart {
                BST_CHECKED
            } else {
                BST_UNCHECKED
            },
        );

        WINDOW.with(|cell| {
            *cell.borrow_mut() = Some(Window {
                hwnd,
                font,
                initial: values,
                apply,
                on_close,
            });
        });
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(hotkey));
    }
    Ok(())
}

fn warn(hwnd: HWND, text: &str) {
    // SAFETY: both strings are valid for the duration of the call.
    unsafe {
        MessageBoxW(
            Some(hwnd),
            &HSTRING::from(text),
            w!("ezhuthu settings"),
            MB_OK | MB_ICONWARNING,
        );
    }
}

/// Reads the controls into `Values`; Err(message) for an unusable hotkey.
fn read(hwnd: HWND, initial: &Values) -> Result<Values, String> {
    // SAFETY: reading our own controls.
    unsafe {
        let item = |id| GetDlgItem(Some(hwnd), id).unwrap_or_default();
        let value = SendMessageW(item(ID_HOTKEY), HKM_GETHOTKEY, None, None).0 as u16;
        let initial_value = Hotkey::parse(&initial.hotkey)
            .ok()
            .and_then(Hotkey::to_control);
        // Untouched (or a Win combination the control can't show): keep it.
        let hotkey = if value == 0 || Some(value) == initial_value {
            initial.hotkey.clone()
        } else {
            match Hotkey::from_control(value) {
                Some(h) => h.to_string(),
                None => {
                    return Err(
                        "Use Ctrl, Alt or Shift together with a letter, digit, F-key, \
                                Space, Enter or Tab."
                            .into(),
                    );
                }
            }
        };
        let insert_mode = if IsDlgButtonChecked(hwnd, ID_PASTE) == BST_CHECKED.0 {
            InsertMode::Clipboard
        } else {
            InsertMode::Unicode
        };
        let index = SendMessageW(item(ID_BEAM), CB_GETCURSEL, None, None)
            .0
            .max(0) as usize;
        let beam = (BEAM_CHOICES.start() + index).min(*BEAM_CHOICES.end());
        let autostart = IsDlgButtonChecked(hwnd, ID_AUTOSTART) == BST_CHECKED.0;
        Ok(Values {
            hotkey,
            insert_mode,
            beam,
            autostart,
        })
    }
}

fn on_ok(hwnd: HWND) {
    let Some((initial, apply)) =
        WINDOW.with(|w| w.borrow().as_ref().map(|w| (w.initial.clone(), w.apply)))
    else {
        return;
    };
    let result = read(hwnd, &initial).and_then(|values| apply(&values));
    match result {
        // SAFETY: closing our own window.
        Ok(()) => {
            let _ = unsafe { DestroyWindow(hwnd) };
        }
        Err(msg) => warn(hwnd, &msg),
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_COMMAND => {
            match (wparam.0 & 0xFFFF) as i32 {
                i if i == IDOK.0 => on_ok(hwnd),
                // SAFETY: closing our own window.
                i if i == IDCANCEL.0 => {
                    let _ = unsafe { DestroyWindow(hwnd) };
                }
                _ => {}
            }
            LRESULT(0)
        }
        // White background for labels, radio buttons and the checkbox.
        WM_CTLCOLORSTATIC => {
            // SAFETY: wparam is the control's DC for this message.
            unsafe { SetBkMode(HDC(wparam.0 as *mut _), TRANSPARENT) };
            let brush: HBRUSH = unsafe { GetSysColorBrush(COLOR_WINDOW) };
            LRESULT(brush.0 as isize)
        }
        WM_CLOSE => {
            // SAFETY: closing our own window.
            let _ = unsafe { DestroyWindow(hwnd) };
            LRESULT(0)
        }
        WM_DESTROY => {
            if let Some(w) = WINDOW.with(|w| w.borrow_mut().take()) {
                // SAFETY: the font is no longer used by any control.
                let _ = unsafe { DeleteObject(HGDIOBJ(w.font.0)) };
                (w.on_close)();
            }
            LRESULT(0)
        }
        // SAFETY: default processing for everything else.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, IsDialogMessageW, KillTimer, MSG, PostQuitMessage, SetTimer,
        TranslateMessage,
    };

    /// Opens the window for a few seconds (for screenshots); Cancel or the
    /// timeout closes it. Run with: cargo test -p ezhuthu settings_preview -- --ignored
    #[test]
    #[ignore]
    fn settings_preview() {
        fn apply(_: &Values) -> Result<(), String> {
            Ok(())
        }
        fn closed() {
            // SAFETY: ends the test's message loop.
            unsafe { PostQuitMessage(0) };
        }
        let values = Values {
            hotkey: "Ctrl+Alt+T".into(),
            insert_mode: InsertMode::Unicode,
            beam: 5,
            autostart: true,
        };
        open(values, apply, closed);
        // SAFETY: a plain message loop with a timer that closes the window.
        unsafe {
            let timer = SetTimer(None, 0, 6000, None);
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                // Only our own timer (controls such as the hotkey box run their own).
                if msg.message == windows::Win32::UI::WindowsAndMessaging::WM_TIMER
                    && msg.hwnd.0.is_null()
                    && msg.wParam.0 == timer
                {
                    if let Some(h) = hwnd() {
                        let _ = DestroyWindow(h);
                    }
                    continue;
                }
                if hwnd().is_some_and(|h| IsDialogMessageW(h, &msg).as_bool()) {
                    continue;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            let _ = KillTimer(None, timer);
        }
    }
}
