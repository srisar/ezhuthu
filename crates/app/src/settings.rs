//! The Settings window (tray menu > Settings...), in four tabs:
//! - General: typing (suggestions per word, insert method, remembering
//!   picks), appearance (popup theme) and startup;
//! - Shortcuts: Tamil mode's (presets or a custom combination) and convert
//!   selection's;
//! - Words: the learned words, to search, change, add and remove;
//! - About: the app, who made it, and credits for the model and libraries.
//!
//! Modeless; it runs in the app's message loop (which routes its keys
//! through IsDialogMessageW). OK calls the app's `apply` callback, which
//! validates and saves; if that fails (e.g. the shortcut is taken) the
//! window stays open with the message.

use std::cell::RefCell;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CLR_INVALID, COLOR_GRAYTEXT, COLOR_WINDOW, CreateCompatibleBitmap, CreateCompatibleDC,
    CreateFontIndirectW, CreateSolidBrush, DeleteDC, DeleteObject, GetDC, GetMonitorInfoW,
    GetPixel, GetSysColor, GetSysColorBrush, GetTextExtentPoint32W, HBRUSH, HDC, HFONT, HGDIOBJ,
    MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint, ReleaseDC, SelectObject, SetBkMode,
    SetTextColor, TRANSPARENT,
};
use windows::Win32::UI::Controls::{
    BST_CHECKED, BST_UNCHECKED, CheckDlgButton, EM_SETCUEBANNER, ICC_LINK_CLASS,
    ICC_LISTVIEW_CLASSES, ICC_TAB_CLASSES, INITCOMMONCONTROLSEX, InitCommonControlsEx,
    IsDlgButtonChecked, LVCF_TEXT, LVCF_WIDTH, LVCOLUMNW, LVIF_PARAM, LVIF_TEXT, LVIS_SELECTED,
    LVITEMW, LVM_DELETEALLITEMS, LVM_ENSUREVISIBLE, LVM_GETITEMW, LVM_GETNEXTITEM,
    LVM_INSERTCOLUMNW, LVM_INSERTITEMW, LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETITEMSTATE,
    LVM_SETITEMTEXTW, LVN_ITEMCHANGED, LVN_KEYDOWN, LVNI_SELECTED, LVS_EX_DOUBLEBUFFER,
    LVS_EX_FULLROWSELECT, LVS_NOSORTHEADER, LVS_REPORT, LVS_SHOWSELALWAYS, NM_CLICK, NM_RETURN,
    NMHDR, NMLINK, NMLVKEYDOWN, TCIF_TEXT, TCITEMW, TCM_GETCURSEL, TCM_INSERTITEMW, TCN_SELCHANGE,
    WC_LINK, WC_LISTVIEW, WC_TABCONTROLW,
};
use windows::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, GetDpiForSystem, SystemParametersInfoForDpi,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetFocus, GetKeyState, SetFocus, VK_CONTROL, VK_DELETE, VK_LWIN, VK_MENU,
    VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass, ShellExecuteW};
use windows::Win32::UI::WindowsAndMessaging::{
    BS_AUTOCHECKBOX, BS_AUTORADIOBUTTON, BS_DEFPUSHBUTTON, BS_PUSHBUTTON, CB_ADDSTRING,
    CB_GETCURSEL, CB_RESETCONTENT, CB_SETCURSEL, CBS_AUTOHSCROLL, CBS_DROPDOWN, CBS_DROPDOWNLIST,
    CreateWindowExW, DLGC_WANTALLKEYS, DefWindowProcW, DestroyIcon, DestroyWindow, EN_CHANGE,
    ES_AUTOHSCROLL, ES_READONLY, GetClientRect, GetDlgCtrlID, GetDlgItem, GetParent,
    GetWindowTextLengthW, GetWindowTextW, HICON, HMENU, IDC_ARROW, IDCANCEL, IDOK, IDYES,
    IMAGE_ICON, LR_DEFAULTCOLOR, LoadCursorW, LoadIconW, LoadImageW, MB_ICONQUESTION,
    MB_ICONWARNING, MB_OK, MB_YESNO, MessageBoxW, NONCLIENTMETRICSW, PRF_CLIENT, RegisterClassExW,
    SPI_GETNONCLIENTMETRICS, STM_SETICON, SW_HIDE, SW_SHOW, SW_SHOWNORMAL, SendMessageW,
    SetForegroundWindow, SetWindowTextW, ShowWindow, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CHAR,
    WM_CLOSE, WM_COMMAND, WM_CTLCOLORSTATIC, WM_DESTROY, WM_GETDLGCODE, WM_KEYDOWN, WM_NOTIFY,
    WM_PRINTCLIENT, WM_SETFONT, WM_SYSCHAR, WM_SYSKEYDOWN, WNDCLASSEXW, WS_CAPTION, WS_CHILD,
    WS_CLIPSIBLINGS, WS_EX_CLIENTEDGE, WS_GROUP, WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE,
    WS_VSCROLL,
};
use windows::core::{HSTRING, PCWSTR, PWSTR, w};

use crate::config::{Config, InsertMode, Theme};
use crate::hotkey::{Hotkey, MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN};
use crate::learned;
use crate::win::{instance, wide};

const ID_HOTKEY: i32 = 10;
const ID_TYPE: i32 = 11;
const ID_PASTE: i32 = 12;
const ID_BEAM: i32 = 13;
const ID_AUTOSTART: i32 = 14;
const ID_TABS: i32 = 15;
const ID_REMEMBER: i32 = 16;
const ID_CONVERT_ON: i32 = 18;
const ID_CONVERT_KEY: i32 = 19;
const ID_THEME: i32 = 30;
/// The Words tab: list, search box, count, the edit row and its buttons.
const ID_WORDS: i32 = 31;
const ID_WORD_SEARCH: i32 = 32;
const ID_WORD_COUNT: i32 = 33;
const ID_WORD_KEY: i32 = 34;
const ID_WORD_TAMIL: i32 = 35;
const ID_WORD_SAVE: i32 = 36;
const ID_WORD_REMOVE: i32 = 37;
const ID_WORD_CLEAR: i32 = 38;
const ID_PREDICT: i32 = 39;
const ID_CLEAR_HISTORY: i32 = 42;
/// Radio buttons for the presets, then "Custom".
const ID_PRESET: i32 = 20;
const ID_CUSTOM: i32 = ID_PRESET + PRESETS.len() as i32;
/// Labels drawn in the muted (grey) text colour.
const ID_MUTED: i32 = 40;
/// The red heart in the "Built by" line.
const ID_HEART: i32 = 41;

/// Popup theme choices, in the dropdown's order.
const THEMES: [(Theme, &str); 3] = [
    (Theme::System, "Match Windows"),
    (Theme::Light, "Light"),
    (Theme::Dark, "Dark"),
];
/// Suggestions per word offered in the dropdown.
const BEAM_CHOICES: std::ops::RangeInclusive<usize> = 3..=9;
/// Shortcut presets and an optional note shown beside each.
const PRESETS: &[(&str, &str)] = &[
    ("Ctrl+Alt+T", "default"),
    ("Ctrl+Space", "also used by VS Code suggestions"),
    ("Ctrl+Shift+Space", ""),
    ("Alt+Shift+T", ""),
];
const VERSION: &str = env!("CARGO_PKG_VERSION");
const REPO_URL: &str = "https://github.com/srisar/ezhuthu";

#[derive(Debug, Clone, PartialEq)]
pub struct Values {
    pub hotkey: String,
    /// Empty: convert selection is off.
    pub convert_hotkey: String,
    pub insert_mode: InsertMode,
    pub beam: usize,
    pub remember_picks: bool,
    pub predict: bool,
    pub theme: Theme,
    pub autostart: bool,
    /// The learned words as (romanized key, Tamil word), sorted by key.
    pub learned: Vec<(String, String)>,
}

/// Validates, applies and saves; Err(message) keeps the window open.
pub type Apply = fn(&Values) -> Result<(), String>;

/// What the window asks of the app.
pub struct Actions {
    pub apply: Apply,
    /// The model's suggestions for a romanized word (the Words tab's dropdown).
    pub suggest: fn(&str) -> Vec<String>,
    /// Clears the typing history now (the "Clear typing history…" button).
    pub clear_history: fn() -> Result<(), String>,
    /// Runs when the window closes, whether by OK or Cancel.
    pub on_close: fn(),
}

struct Window {
    hwnd: HWND,
    font: HFONT,
    title_font: HFONT,
    logo: Option<HICON>,
    /// The tab page's background, behind the labels and buttons on it.
    page_brush: HBRUSH,
    /// The combinations shown in the key boxes (updated as keys are pressed):
    /// Tamil mode's custom one, and convert selection's.
    hotkey: String,
    convert: String,
    /// Controls of each tab, shown or hidden as the tab changes.
    pages: [Vec<HWND>; 4],
    /// The learned words being edited, sorted by key (applied on OK).
    learned: Vec<(String, String)>,
    /// Set while the Words tab's boxes are filled in by code, so their
    /// change notifications are ignored.
    filling: bool,
    apply: Apply,
    suggest: fn(&str) -> Vec<String>,
    clear_history: fn() -> Result<(), String>,
    on_close: fn(),
}

thread_local! {
    static WINDOW: RefCell<Option<Window>> = const { RefCell::new(None) };
}

/// The open Settings window, if any.
pub fn hwnd() -> Option<HWND> {
    WINDOW.with(|w| w.borrow().as_ref().map(|w| w.hwnd))
}

/// Opens the Settings window (or brings it to the front).
pub fn open(values: Values, actions: Actions) {
    if let Some(h) = hwnd() {
        // SAFETY: bringing our own window to the front.
        let _ = unsafe { SetForegroundWindow(h) };
        return;
    }
    let on_close = actions.on_close;
    if let Err(e) = create(values, actions) {
        crate::win::error_box(None, &format!("Couldn't open Settings:\n\n{e:#}"));
        on_close();
    }
}

/// Which preset `hotkey` is, if any (compared as parsed combinations).
fn preset_index(hotkey: &str) -> Option<usize> {
    let h = Hotkey::parse(hotkey).ok()?;
    PRESETS
        .iter()
        .position(|(p, _)| Hotkey::parse(p).ok() == Some(h))
}

fn create(values: Values, actions: Actions) -> windows::core::Result<()> {
    // SAFETY: standard window creation on the UI thread; all strings outlive
    // the calls that use them; fonts and the logo are freed in WM_DESTROY.
    unsafe {
        let icc = INITCOMMONCONTROLSEX {
            dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_TAB_CLASSES | ICC_LINK_CLASS | ICC_LISTVIEW_CLASSES,
        };
        let _ = InitCommonControlsEx(&icc);
        let class = w!("ezhuthu.settings");
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(window_proc),
            hInstance: instance(),
            // The app icon linked in by build.rs (resource id 1), for the title
            // bar and taskbar.
            hIcon: LoadIconW(Some(instance()), PCWSTR(1 as _)).unwrap_or_default(),
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
        let mut title_lf = ncm.lfMessageFont;
        title_lf.lfHeight = title_lf.lfHeight * 3 / 2;
        title_lf.lfWeight = 700;
        let title_font = CreateFontIndirectW(&title_lf);

        // Client area 500 x 476 (96-DPI units), centred on the primary monitor.
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU;
        let mut r = RECT {
            left: 0,
            top: 0,
            right: s(500),
            bottom: s(476),
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
            w!("Ezhuthu settings"),
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

        // Width of `text` in the message font, in 96-DPI units.
        let text_width = |text: &str| {
            let dc = GetDC(Some(hwnd));
            let old = SelectObject(dc, HGDIOBJ(font.0));
            let units: Vec<u16> = text.encode_utf16().collect();
            let mut size = SIZE::default();
            let _ = GetTextExtentPoint32W(dc, &units, &mut size);
            SelectObject(dc, old);
            ReleaseDC(Some(hwnd), dc);
            size.cx * 96 / dpi as i32
        };

        let child = |class: PCWSTR,
                     text: &str,
                     style: u32,
                     ex: WINDOW_EX_STYLE,
                     id: i32,
                     rect: (i32, i32, i32, i32)|
         -> windows::core::Result<HWND> {
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
            )?;
            SendMessageW(
                ctl,
                WM_SETFONT,
                Some(WPARAM(font.0 as usize)),
                Some(LPARAM(1)),
            );
            Ok(ctl)
        };
        let none = WINDOW_EX_STYLE(0);
        let tab = WS_TABSTOP.0;
        const SS_ETCHEDHORZ: u32 = 0x10;
        const SS_ICON: u32 = 0x3;
        const SS_REALSIZECONTROL: u32 = 0x40;
        const X: i32 = 28; // left edge of tab content
        const RIGHT: i32 = 472; // right edge of tab content

        // The tab strip; its pages' controls are siblings drawn on top of it.
        let tabs = child(
            WC_TABCONTROLW,
            "",
            tab | WS_CLIPSIBLINGS.0,
            none,
            ID_TABS,
            (12, 10, 476, 414),
        )?;
        for (i, name) in ["General", "Shortcuts", "Words", "About"]
            .iter()
            .enumerate()
        {
            let mut text: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
            let item = TCITEMW {
                mask: TCIF_TEXT,
                pszText: PWSTR(text.as_mut_ptr()),
                ..Default::default()
            };
            SendMessageW(
                tabs,
                TCM_INSERTITEMW,
                Some(WPARAM(i)),
                Some(LPARAM(&item as *const _ as isize)),
            );
        }

        let page_brush = page_brush(tabs, s(24), s(60));

        // A section heading: its name and a divider line to the right.
        let section = |name: &str, y: i32| -> windows::core::Result<Vec<HWND>> {
            let width = text_width(name) + 2;
            Ok(vec![
                child(w!("STATIC"), name, 0, none, -1, (X, y, width, 20))?,
                child(
                    w!("STATIC"),
                    "",
                    SS_ETCHEDHORZ,
                    none,
                    -1,
                    (X + width + 8, y + 10, RIGHT - X - width - 8, 1),
                )?,
            ])
        };
        let muted = |text: &str, rect| child(w!("STATIC"), text, 0, none, ID_MUTED, rect);

        // ---------------------------------------------------------------- General
        let mut general = Vec::new();
        general.extend(section("Typing", 48)?);
        general.push(child(
            w!("STATIC"),
            "Suggestions per word",
            0,
            none,
            -1,
            (X, 74, 200, 20),
        )?);
        let beam = child(
            w!("COMBOBOX"),
            "",
            tab | WS_GROUP.0 | WS_VSCROLL.0 | CBS_DROPDOWNLIST as u32,
            none,
            ID_BEAM,
            (RIGHT - 80, 70, 80, 200),
        )?;
        general.push(beam);
        general.push(child(
            w!("STATIC"),
            "Insert text by",
            0,
            none,
            -1,
            (X, 104, 200, 20),
        )?);
        for (i, (label, note)) in [
            ("Typing the characters", "works in most apps"),
            ("Pasting", "for apps that garble typed Tamil"),
        ]
        .iter()
        .enumerate()
        {
            let y = 126 + 24 * i as i32;
            let first = if i == 0 { WS_GROUP.0 | tab } else { 0 };
            general.push(child(
                w!("BUTTON"),
                label,
                first | BS_AUTORADIOBUTTON as u32,
                none,
                ID_TYPE + i as i32,
                (X + 16, y, 180, 22),
            )?);
            general.push(muted(note, (X + 200, y + 2, 240, 18))?);
        }
        general.push(child(
            w!("BUTTON"),
            "Remember the suggestions I pick",
            tab | WS_GROUP.0 | BS_AUTOCHECKBOX as u32,
            none,
            ID_REMEMBER,
            (X, 180, 270, 22),
        )?);
        general.push(muted(
            "see and edit them on the Words tab",
            (X + 280, 182, RIGHT - X - 280, 18),
        )?);
        general.push(child(
            w!("BUTTON"),
            "Complete words as I type (Tab)",
            tab | WS_GROUP.0 | BS_AUTOCHECKBOX as u32,
            none,
            ID_PREDICT,
            (X, 210, 250, 22),
        )?);
        general.push(child(
            w!("BUTTON"),
            "Clear typing history…",
            tab | BS_PUSHBUTTON as u32,
            none,
            ID_CLEAR_HISTORY,
            (RIGHT - 160, 208, 160, 26),
        )?);

        general.extend(section("Appearance", 246)?);
        general.push(child(
            w!("STATIC"),
            "Popup theme",
            0,
            none,
            -1,
            (X, 272, 200, 20),
        )?);
        let theme = child(
            w!("COMBOBOX"),
            "",
            tab | WS_GROUP.0 | WS_VSCROLL.0 | CBS_DROPDOWNLIST as u32,
            none,
            ID_THEME,
            (RIGHT - 160, 268, 160, 200),
        )?;
        general.push(theme);

        general.extend(section("Startup", 310)?);
        general.push(child(
            w!("BUTTON"),
            "Start Ezhuthu when I sign in to Windows",
            tab | WS_GROUP.0 | BS_AUTOCHECKBOX as u32,
            none,
            ID_AUTOSTART,
            (X, 334, 420, 22),
        )?);

        // -------------------------------------------------------------- Shortcuts
        let mut shortcuts = Vec::new();
        shortcuts.extend(section("Tamil mode", 48)?);
        shortcuts.push(muted(
            "Press it anywhere to turn Tamil typing on or off.",
            (X, 72, 420, 18),
        )?);
        for (i, (keys, note)) in PRESETS.iter().enumerate() {
            let y = 96 + 24 * i as i32;
            let first = if i == 0 { WS_GROUP.0 | tab } else { 0 };
            shortcuts.push(child(
                w!("BUTTON"),
                keys,
                first | BS_AUTORADIOBUTTON as u32,
                none,
                ID_PRESET + i as i32,
                (X, y, 150, 22),
            )?);
            if !note.is_empty() {
                shortcuts.push(muted(note, (X + 160, y + 2, 260, 18))?);
            }
        }
        let custom_y = 96 + 24 * PRESETS.len() as i32;
        shortcuts.push(child(
            w!("BUTTON"),
            "Custom",
            BS_AUTORADIOBUTTON as u32,
            none,
            ID_CUSTOM,
            (X, custom_y, 80, 22),
        )?);
        // Read-only edits that record the pressed combination (the stock
        // hotkey control refuses Space, so Ctrl+Space couldn't be chosen).
        let key_box = |text: &str, id: i32, x: i32, y: i32| -> windows::core::Result<HWND> {
            let ctl = child(
                w!("EDIT"),
                text,
                tab | WS_GROUP.0 | ES_READONLY as u32,
                WS_EX_CLIENTEDGE,
                id,
                (x, y, 180, 26),
            )?;
            let _ = SetWindowSubclass(ctl, Some(key_box_proc), 1, 0);
            Ok(ctl)
        };
        let preset = preset_index(&values.hotkey);
        let custom_text = if preset.is_none() {
            values.hotkey.clone()
        } else {
            String::new()
        };
        let custom_box = key_box(&custom_text, ID_HOTKEY, X + 92, custom_y - 2)?;
        shortcuts.push(custom_box);
        shortcuts.push(muted(
            "click, then press keys",
            (X + 282, custom_y + 2, 150, 18),
        )?);

        shortcuts.extend(section("Convert selection", 232)?);
        shortcuts.push(muted(
            "Select text typed in English letters in any app, then press this to",
            (X, 256, 440, 18),
        )?);
        shortcuts.push(muted("turn it into Tamil.", (X, 274, 440, 18))?);
        shortcuts.push(child(
            w!("BUTTON"),
            "Shortcut",
            tab | WS_GROUP.0 | BS_AUTOCHECKBOX as u32,
            none,
            ID_CONVERT_ON,
            (X, 302, 84, 22),
        )?);
        let convert_on = !values.convert_hotkey.is_empty();
        let convert_text = if convert_on {
            values.convert_hotkey.clone()
        } else {
            Config::default().convert_hotkey
        };
        let convert_box = key_box(&convert_text, ID_CONVERT_KEY, X + 92, 300)?;
        shortcuts.push(convert_box);
        shortcuts.push(muted("click, then press keys", (X + 282, 304, 150, 18))?);

        // ------------------------------------------------------------------ Words
        let mut words = Vec::new();
        words.extend(section("Learned words", 48)?);
        words.push(muted(
            "Suggestions you picked over the first one. They come first next time.",
            (X, 72, RIGHT - X, 18),
        )?);
        let search = child(
            w!("EDIT"),
            "",
            tab | WS_GROUP.0 | ES_AUTOHSCROLL as u32,
            WS_EX_CLIENTEDGE,
            ID_WORD_SEARCH,
            (X, 96, 220, 24),
        )?;
        let cue = wide("Search");
        SendMessageW(
            search,
            EM_SETCUEBANNER,
            Some(WPARAM(1)),
            Some(LPARAM(cue.as_ptr() as isize)),
        );
        words.push(search);
        const SS_RIGHT: u32 = 0x2;
        words.push(child(
            w!("STATIC"),
            "",
            SS_RIGHT,
            none,
            ID_WORD_COUNT,
            (RIGHT - 200, 100, 200, 18),
        )?);
        let list = child(
            WC_LISTVIEW,
            "",
            tab | LVS_REPORT | LVS_SHOWSELALWAYS | LVS_NOSORTHEADER,
            WS_EX_CLIENTEDGE,
            ID_WORDS,
            (X, 128, RIGHT - X, 174),
        )?;
        let ex = (LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER) as usize;
        SendMessageW(
            list,
            LVM_SETEXTENDEDLISTVIEWSTYLE,
            Some(WPARAM(ex)),
            Some(LPARAM(ex as isize)),
        );
        for (i, (title, width)) in [("You type", 150), ("Shows first", RIGHT - X - 150 - 24)]
            .into_iter()
            .enumerate()
        {
            let mut text = wide(title);
            let column = LVCOLUMNW {
                mask: LVCF_TEXT | LVCF_WIDTH,
                cx: s(width),
                pszText: PWSTR(text.as_mut_ptr()),
                ..Default::default()
            };
            SendMessageW(
                list,
                LVM_INSERTCOLUMNW,
                Some(WPARAM(i)),
                Some(LPARAM(&column as *const _ as isize)),
            );
        }
        words.push(list);
        words.push(child(
            w!("STATIC"),
            "You type",
            0,
            none,
            -1,
            (X, 312, 160, 18),
        )?);
        words.push(child(
            w!("STATIC"),
            "Tamil word",
            0,
            none,
            -1,
            (X + 172, 312, 200, 18),
        )?);
        words.push(child(
            w!("EDIT"),
            "",
            tab | WS_GROUP.0 | ES_AUTOHSCROLL as u32,
            WS_EX_CLIENTEDGE,
            ID_WORD_KEY,
            (X, 332, 160, 24),
        )?);
        words.push(child(
            w!("COMBOBOX"),
            "",
            tab | WS_VSCROLL.0 | (CBS_DROPDOWN | CBS_AUTOHSCROLL) as u32,
            none,
            ID_WORD_TAMIL,
            (X + 172, 331, RIGHT - X - 172, 200),
        )?);
        words.push(child(
            w!("BUTTON"),
            "Save word",
            tab | WS_GROUP.0 | BS_PUSHBUTTON as u32,
            none,
            ID_WORD_SAVE,
            (X, 368, 100, 26),
        )?);
        words.push(child(
            w!("BUTTON"),
            "Remove",
            tab | BS_PUSHBUTTON as u32,
            none,
            ID_WORD_REMOVE,
            (X + 108, 368, 100, 26),
        )?);
        words.push(child(
            w!("BUTTON"),
            "Remove all…",
            tab | BS_PUSHBUTTON as u32,
            none,
            ID_WORD_CLEAR,
            (RIGHT - 110, 368, 110, 26),
        )?);

        // ---------------------------------------------------------------- About
        let mut about = Vec::new();
        let logo_ctl = child(
            w!("STATIC"),
            "",
            SS_ICON | SS_REALSIZECONTROL,
            none,
            -1,
            (X, 54, 48, 48),
        )?;
        let logo = LoadImageW(
            Some(instance()),
            PCWSTR(1 as _),
            IMAGE_ICON,
            s(48),
            s(48),
            LR_DEFAULTCOLOR,
        )
        .ok()
        .map(|h| HICON(h.0));
        if let Some(icon) = logo {
            SendMessageW(logo_ctl, STM_SETICON, Some(WPARAM(icon.0 as usize)), None);
        }
        about.push(logo_ctl);
        let title = child(w!("STATIC"), "Ezhuthu", 0, none, -1, (X + 62, 52, 300, 30))?;
        SendMessageW(
            title,
            WM_SETFONT,
            Some(WPARAM(title_font.0 as usize)),
            Some(LPARAM(1)),
        );
        about.push(title);
        about.push(muted(
            &format!("Version {VERSION} · Free and open source (MIT license)"),
            (X + 62, 82, 360, 20),
        )?);
        let built = "Built by Gravitide from Sri Lanka with Love";
        about.push(child(
            WC_LINK,
            "Built by <a href=\"https://gravitide.dev\">Gravitide</a> from Sri Lanka with Love",
            tab,
            none,
            -1,
            (X, 122, text_width(built) + 2, 20),
        )?);
        about.push(child(
            w!("STATIC"),
            "\u{2764}",
            0,
            none,
            ID_HEART,
            (X + text_width(built) + 1, 122, 20, 20),
        )?);
        about.push(child(
            WC_LINK,
            &format!(
                "Source code: <a href=\"{REPO_URL}\">{}</a>",
                REPO_URL.trim_start_matches("https://")
            ),
            tab,
            none,
            -1,
            (X, 146, 440, 20),
        )?);

        about.extend(section("Credits", 184)?);
        let credits: [(&str, &str, &str); 4] = [
            (
                "Transliteration model: IndicXlit by AI4Bharat",
                "https://github.com/AI4Bharat/IndicXlit",
                "MIT license",
            ),
            (
                "Word frequencies: IndicXlit word-probability data by AI4Bharat",
                "",
                "MIT license",
            ),
            (
                "Model runtime: tract by Sonos",
                "https://github.com/sonos/tract",
                "MIT / Apache-2.0",
            ),
            (
                "Logo letter: Anek Tamil by Ek Type",
                "https://fonts.google.com/specimen/Anek+Tamil",
                "SIL Open Font License 1.1",
            ),
        ];
        for (i, (what, url, license)) in credits.iter().enumerate() {
            let y = 210 + 50 * i as i32;
            about.push(child(w!("STATIC"), what, 0, none, -1, (X, y, 440, 20))?);
            if url.is_empty() {
                about.push(muted(license, (X, y + 20, 440, 20))?);
            } else {
                let shown = url.trim_start_matches("https://");
                about.push(child(
                    WC_LINK,
                    &format!("<a href=\"{url}\">{shown}</a> · {license}"),
                    tab,
                    none,
                    -1,
                    (X, y + 20, 440, 20),
                )?);
            }
        }
        for ctl in shortcuts.iter().chain(&words).chain(&about) {
            let _ = ShowWindow(*ctl, SW_HIDE);
        }

        child(
            w!("BUTTON"),
            "OK",
            tab | WS_GROUP.0 | BS_DEFPUSHBUTTON as u32,
            none,
            IDOK.0,
            (320, 436, 80, 28),
        )?;
        child(
            w!("BUTTON"),
            "Cancel",
            tab | BS_PUSHBUTTON as u32,
            none,
            IDCANCEL.0,
            (408, 436, 80, 28),
        )?;

        // Current values.
        let selected = preset.map_or(ID_CUSTOM, |i| ID_PRESET + i as i32);
        let _ = CheckDlgButton(hwnd, selected, BST_CHECKED);
        let _ = EnableWindow(custom_box, preset.is_none());
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
        let check = |on: bool| if on { BST_CHECKED } else { BST_UNCHECKED };
        for (_, label) in THEMES {
            SendMessageW(
                theme,
                CB_ADDSTRING,
                None,
                Some(LPARAM(HSTRING::from(label).as_ptr() as isize)),
            );
        }
        let theme_index = THEMES.iter().position(|(t, _)| *t == values.theme);
        SendMessageW(
            theme,
            CB_SETCURSEL,
            Some(WPARAM(theme_index.unwrap_or(0))),
            None,
        );
        let _ = CheckDlgButton(hwnd, ID_REMEMBER, check(values.remember_picks));
        let _ = CheckDlgButton(hwnd, ID_PREDICT, check(values.predict));
        let _ = CheckDlgButton(hwnd, ID_AUTOSTART, check(values.autostart));
        let _ = CheckDlgButton(hwnd, ID_CONVERT_ON, check(convert_on));
        let _ = EnableWindow(convert_box, convert_on);

        WINDOW.with(|cell| {
            *cell.borrow_mut() = Some(Window {
                hwnd,
                font,
                title_font,
                logo,
                page_brush,
                hotkey: custom_text,
                convert: convert_text,
                pages: [general, shortcuts, words, about],
                learned: values.learned.clone(),
                filling: false,
                apply: actions.apply,
                suggest: actions.suggest,
                clear_history: actions.clear_history,
                on_close: actions.on_close,
            });
        });
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        refresh_words(hwnd);
        let _ = SetFocus(Some(beam));
    }
    Ok(())
}

fn warn(hwnd: HWND, text: &str) {
    // SAFETY: both strings are valid for the duration of the call.
    unsafe {
        MessageBoxW(
            Some(hwnd),
            &HSTRING::from(text),
            w!("Ezhuthu settings"),
            MB_OK | MB_ICONWARNING,
        );
    }
}

/// The selected item of a dropdown (0 if none).
fn selection(hwnd: HWND, id: i32) -> usize {
    // SAFETY: reading our own control.
    unsafe {
        let combo = GetDlgItem(Some(hwnd), id).unwrap_or_default();
        SendMessageW(combo, CB_GETCURSEL, None, None).0.max(0) as usize
    }
}

fn checked(hwnd: HWND, id: i32) -> bool {
    // SAFETY: reading our own control.
    unsafe { IsDlgButtonChecked(hwnd, id) == BST_CHECKED.0 }
}

/// Reads the controls into `Values`; Err if Custom is chosen but empty.
fn read(
    hwnd: HWND,
    custom: String,
    convert: String,
    learned: Vec<(String, String)>,
) -> Result<Values, String> {
    let hotkey = match (0..PRESETS.len()).find(|&i| checked(hwnd, ID_PRESET + i as i32)) {
        Some(i) => PRESETS[i].0.to_string(),
        None if custom.is_empty() => {
            return Err(
                "Click the Custom box and press the keys you want, or pick a preset.".into(),
            );
        }
        None => custom,
    };
    let convert_hotkey = match checked(hwnd, ID_CONVERT_ON) {
        false => String::new(),
        true if convert.is_empty() => {
            return Err("Click the convert selection box and press the keys you want.".into());
        }
        true => convert,
    };
    let insert_mode = if checked(hwnd, ID_PASTE) {
        InsertMode::Clipboard
    } else {
        InsertMode::Unicode
    };
    let beam = (BEAM_CHOICES.start() + selection(hwnd, ID_BEAM)).min(*BEAM_CHOICES.end());
    Ok(Values {
        hotkey,
        convert_hotkey,
        insert_mode,
        beam,
        remember_picks: checked(hwnd, ID_REMEMBER),
        predict: checked(hwnd, ID_PREDICT),
        theme: THEMES[selection(hwnd, ID_THEME).min(THEMES.len() - 1)].0,
        autostart: checked(hwnd, ID_AUTOSTART),
        learned,
    })
}

// ------------------------------------------------------------------ Words tab

/// A control of the Settings window by id.
fn item(hwnd: HWND, id: i32) -> HWND {
    // SAFETY: plain query of our own window.
    unsafe { GetDlgItem(Some(hwnd), id) }.unwrap_or_default()
}

fn text_of(ctl: HWND) -> String {
    // SAFETY: the buffer holds the text's length plus the NUL.
    unsafe {
        let len = GetWindowTextLengthW(ctl).max(0) as usize;
        let mut buf = vec![0u16; len + 1];
        let n = GetWindowTextW(ctl, &mut buf).max(0) as usize;
        String::from_utf16_lossy(&buf[..n])
    }
}

/// Sets a control's text without reacting to its change notification.
fn fill_text(ctl: HWND, text: &str) {
    set_filling(true);
    // SAFETY: updating our own control.
    let _ = unsafe { SetWindowTextW(ctl, &HSTRING::from(text)) };
    set_filling(false);
}

fn set_filling(on: bool) {
    WINDOW.with(|w| {
        if let Some(w) = w.borrow_mut().as_mut() {
            w.filling = on;
        }
    });
}

fn learned_words() -> Vec<(String, String)> {
    WINDOW.with(|w| {
        w.borrow()
            .as_ref()
            .map(|w| w.learned.clone())
            .unwrap_or_default()
    })
}

/// Refills the list with the learned words matching the search box; each
/// row's lParam is its index in the learned list.
fn refresh_words(hwnd: HWND) {
    let words = learned_words();
    let filter = text_of(item(hwnd, ID_WORD_SEARCH)).trim().to_lowercase();
    let list = item(hwnd, ID_WORDS);
    let mut rows = 0;
    // SAFETY: the item structs and their strings outlive each message.
    unsafe {
        SendMessageW(list, LVM_DELETEALLITEMS, None, None);
        for (i, (key, word)) in words.iter().enumerate() {
            if !filter.is_empty() && !key.contains(&filter) && !word.contains(filter.as_str()) {
                continue;
            }
            let mut key_text = wide(key);
            let row = LVITEMW {
                mask: LVIF_TEXT | LVIF_PARAM,
                iItem: rows,
                pszText: PWSTR(key_text.as_mut_ptr()),
                lParam: LPARAM(i as isize),
                ..Default::default()
            };
            let at = SendMessageW(
                list,
                LVM_INSERTITEMW,
                None,
                Some(LPARAM(&row as *const _ as isize)),
            )
            .0;
            let mut word_text = wide(word);
            let cell = LVITEMW {
                iSubItem: 1,
                pszText: PWSTR(word_text.as_mut_ptr()),
                ..Default::default()
            };
            SendMessageW(
                list,
                LVM_SETITEMTEXTW,
                Some(WPARAM(at as usize)),
                Some(LPARAM(&cell as *const _ as isize)),
            );
            rows += 1;
        }
    }
    let count = match (filter.is_empty(), words.len()) {
        (true, 1) => "1 word".to_string(),
        (true, n) => format!("{n} words"),
        (false, n) => format!("{rows} of {n}"),
    };
    fill_text(item(hwnd, ID_WORD_COUNT), &count);
}

/// Indexes (into the learned list) of the selected rows.
fn selected_words(hwnd: HWND) -> Vec<usize> {
    let list = item(hwnd, ID_WORDS);
    let mut out = Vec::new();
    let mut at: isize = -1;
    // SAFETY: querying our own list; the item struct outlives each message.
    unsafe {
        loop {
            at = SendMessageW(
                list,
                LVM_GETNEXTITEM,
                Some(WPARAM(at as usize)),
                Some(LPARAM(LVNI_SELECTED as isize)),
            )
            .0;
            if at < 0 {
                break;
            }
            let mut row = LVITEMW {
                mask: LVIF_PARAM,
                iItem: at as i32,
                ..Default::default()
            };
            SendMessageW(
                list,
                LVM_GETITEMW,
                None,
                Some(LPARAM(&mut row as *mut _ as isize)),
            );
            out.push(row.lParam.0 as usize);
        }
    }
    out
}

/// Selects (only) the row showing learned word `index`, if it is shown.
fn select_word(hwnd: HWND, index: usize) {
    let list = item(hwnd, ID_WORDS);
    // SAFETY: setting the state of our own list's rows.
    unsafe {
        let none = LVITEMW {
            stateMask: LVIS_SELECTED,
            ..Default::default()
        };
        SendMessageW(
            list,
            LVM_SETITEMSTATE,
            Some(WPARAM(usize::MAX)),
            Some(LPARAM(&none as *const _ as isize)),
        );
        let mut at: isize = -1;
        loop {
            at = SendMessageW(list, LVM_GETNEXTITEM, Some(WPARAM(at as usize)), None).0;
            if at < 0 {
                return;
            }
            let mut row = LVITEMW {
                mask: LVIF_PARAM,
                iItem: at as i32,
                ..Default::default()
            };
            SendMessageW(
                list,
                LVM_GETITEMW,
                None,
                Some(LPARAM(&mut row as *mut _ as isize)),
            );
            if row.lParam.0 as usize == index {
                let selected = LVITEMW {
                    state: LVIS_SELECTED,
                    stateMask: LVIS_SELECTED,
                    ..Default::default()
                };
                SendMessageW(
                    list,
                    LVM_SETITEMSTATE,
                    Some(WPARAM(at as usize)),
                    Some(LPARAM(&selected as *const _ as isize)),
                );
                SendMessageW(list, LVM_ENSUREVISIBLE, Some(WPARAM(at as usize)), None);
                return;
            }
        }
    }
}

/// Fills the Tamil dropdown with the model's suggestions for `key`, showing
/// `current` (or the first suggestion).
fn fill_suggestions(hwnd: HWND, key: &str, current: Option<&str>) {
    let Some(suggest) = WINDOW.with(|w| w.borrow().as_ref().map(|w| w.suggest)) else {
        return;
    };
    let mut options = if key.is_empty() {
        Vec::new()
    } else {
        suggest(key)
    };
    if let Some(c) = current
        && !options.iter().any(|o| o == c)
    {
        options.insert(0, c.to_string());
    }
    let combo = item(hwnd, ID_WORD_TAMIL);
    // SAFETY: filling our own combo box; each string outlives its message.
    unsafe {
        SendMessageW(combo, CB_RESETCONTENT, None, None);
        for option in &options {
            SendMessageW(
                combo,
                CB_ADDSTRING,
                None,
                Some(LPARAM(HSTRING::from(option.as_str()).as_ptr() as isize)),
            );
        }
    }
    let shown = current
        .or(options.first().map(String::as_str))
        .unwrap_or("");
    fill_text(combo, shown);
}

/// A row was selected: put its word in the edit boxes.
fn on_word_selected(hwnd: HWND) {
    let [index] = selected_words(hwnd)[..] else {
        return;
    };
    let Some((key, word)) = learned_words().get(index).cloned() else {
        return;
    };
    fill_text(item(hwnd, ID_WORD_KEY), &key);
    fill_suggestions(hwnd, &key, Some(&word));
}

/// The "You type" box changed: offer suggestions for it.
fn on_word_key_changed(hwnd: HWND) {
    let key = text_of(item(hwnd, ID_WORD_KEY)).trim().to_ascii_lowercase();
    let known = learned_words()
        .into_iter()
        .find(|(k, _)| *k == key)
        .map(|(_, w)| w);
    fill_suggestions(hwnd, &key, known.as_deref());
}

/// "Save word": adds the word, or changes its Tamil spelling.
fn on_word_save(hwnd: HWND) {
    let key = text_of(item(hwnd, ID_WORD_KEY)).trim().to_ascii_lowercase();
    let word = text_of(item(hwnd, ID_WORD_TAMIL)).trim().to_string();
    if !learned::valid(&key, &word) {
        warn(
            hwnd,
            "Type the word in English letters (a–z only), then choose its Tamil \
             spelling from the list (or paste one).",
        );
        return;
    }
    let index = WINDOW.with(|w| {
        let mut w = w.borrow_mut();
        let list = &mut w.as_mut()?.learned;
        Some(match list.binary_search_by(|(k, _)| k.as_str().cmp(&key)) {
            Ok(i) => {
                list[i].1 = word;
                i
            }
            Err(i) => {
                list.insert(i, (key, word));
                i
            }
        })
    });
    // Show it, even if the search would hide it.
    fill_text(item(hwnd, ID_WORD_SEARCH), "");
    refresh_words(hwnd);
    if let Some(i) = index {
        select_word(hwnd, i);
    }
}

/// "Remove" (or Delete in the list): removes the selected words.
fn on_word_remove(hwnd: HWND) {
    let mut selected = selected_words(hwnd);
    if selected.is_empty() {
        return;
    }
    selected.sort_unstable_by(|a, b| b.cmp(a));
    WINDOW.with(|w| {
        if let Some(w) = w.borrow_mut().as_mut() {
            for i in selected {
                if i < w.learned.len() {
                    w.learned.remove(i);
                }
            }
        }
    });
    refresh_words(hwnd);
    fill_text(item(hwnd, ID_WORD_KEY), "");
    fill_suggestions(hwnd, "", None);
}

/// "Remove all…": asks, then empties the list.
/// "Clear typing history…": asks, then clears it straight away.
fn on_clear_history(hwnd: HWND) {
    let Some(clear) = WINDOW.with(|w| w.borrow().as_ref().map(|w| w.clear_history)) else {
        return;
    };
    // SAFETY: a message box owned by our window; both strings are valid.
    let answer = unsafe {
        MessageBoxW(
            Some(hwnd),
            w!(
                "Clear the words Ezhuthu has kept from your typing?\n\nPredictions then come from the dictionary only, until you type more."
            ),
            w!("Ezhuthu settings"),
            MB_YESNO | MB_ICONQUESTION,
        )
    };
    if answer == IDYES
        && let Err(msg) = clear()
    {
        warn(hwnd, &msg);
    }
}

fn on_word_clear(hwnd: HWND) {
    if learned_words().is_empty() {
        return;
    }
    // SAFETY: a message box owned by our window; both strings are valid.
    let answer = unsafe {
        MessageBoxW(
            Some(hwnd),
            w!("Remove every learned word?\n\n\
                 Each word goes back to the model's own order when you press OK."),
            w!("Ezhuthu settings"),
            MB_YESNO | MB_ICONQUESTION,
        )
    };
    if answer == IDYES {
        WINDOW.with(|w| {
            if let Some(w) = w.borrow_mut().as_mut() {
                w.learned.clear();
            }
        });
        refresh_words(hwnd);
        fill_text(item(hwnd, ID_WORD_KEY), "");
        fill_suggestions(hwnd, "", None);
    }
}

/// Enter in the Words tab's boxes means "Save word" (or nothing, in the
/// search box) rather than OK.
fn enter_in_words(hwnd: HWND) -> bool {
    // SAFETY: plain queries of our own controls.
    unsafe {
        let focus = GetFocus();
        let id = GetDlgCtrlID(focus);
        // The dropdown's text box is a child of the combo box.
        let parent = GetParent(focus).map(|p| GetDlgCtrlID(p)).unwrap_or(0);
        if id == ID_WORD_KEY || parent == ID_WORD_TAMIL {
            on_word_save(hwnd);
            true
        } else {
            id == ID_WORD_SEARCH
        }
    }
}

fn on_ok(hwnd: HWND) {
    let Some((custom, convert, learned, apply)) = WINDOW.with(|w| {
        let w = w.borrow();
        let w = w.as_ref()?;
        Some((
            w.hotkey.clone(),
            w.convert.clone(),
            w.learned.clone(),
            w.apply,
        ))
    }) else {
        return;
    };
    match read(hwnd, custom, convert, learned).and_then(|v| apply(&v)) {
        // SAFETY: closing our own window.
        Ok(()) => {
            let _ = unsafe { DestroyWindow(hwnd) };
        }
        Err(msg) => warn(hwnd, &msg),
    }
}

/// A shortcut radio button was clicked: the key box is only for Custom.
fn on_shortcut_choice(hwnd: HWND, id: i32) {
    // SAFETY: enabling and focusing our own control.
    unsafe {
        if let Ok(key_box) = GetDlgItem(Some(hwnd), ID_HOTKEY) {
            let custom = id == ID_CUSTOM;
            let _ = EnableWindow(key_box, custom);
            if custom {
                let _ = SetFocus(Some(key_box));
            }
        }
    }
}

/// A brush of the tab control's page colour, read from the pixel at (x, y)
/// of the control as drawn (themes paint the page slightly off-white).
fn page_brush(tabs: HWND, x: i32, y: i32) -> HBRUSH {
    // SAFETY: draws the tab control into a temporary bitmap that is freed
    // before returning; the brush is freed in WM_DESTROY.
    unsafe {
        let mut r = RECT::default();
        let _ = GetClientRect(tabs, &mut r);
        let screen = GetDC(Some(tabs));
        let dc = CreateCompatibleDC(Some(screen));
        let bitmap = CreateCompatibleBitmap(screen, r.right, r.bottom);
        let old = SelectObject(dc, HGDIOBJ(bitmap.0));
        SendMessageW(
            tabs,
            WM_PRINTCLIENT,
            Some(WPARAM(dc.0 as usize)),
            Some(LPARAM(PRF_CLIENT as isize)),
        );
        let mut color = GetPixel(dc, x, y);
        SelectObject(dc, old);
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        let _ = DeleteDC(dc);
        ReleaseDC(Some(tabs), screen);
        if color.0 == CLR_INVALID {
            color = COLORREF(GetSysColor(COLOR_WINDOW));
        }
        CreateSolidBrush(color)
    }
}

/// Shows the controls of tab `index` and hides the other tab's.
fn show_page(index: usize) {
    let pages = WINDOW.with(|w| w.borrow().as_ref().map(|w| w.pages.clone()));
    let Some(pages) = pages else {
        return;
    };
    for (i, page) in pages.iter().enumerate() {
        let cmd = if i == index { SW_SHOW } else { SW_HIDE };
        for ctl in page {
            // SAFETY: showing or hiding our own controls.
            let _ = unsafe { ShowWindow(*ctl, cmd) };
        }
    }
}

/// Opens a credits link in the default browser (only our own https links).
fn open_link(link: &NMLINK) {
    let len = link.item.szUrl.iter().position(|&c| c == 0).unwrap_or(0);
    let url = String::from_utf16_lossy(&link.item.szUrl[..len]);
    if url.starts_with("https://") {
        // SAFETY: plain shell open of an https URL.
        unsafe {
            ShellExecuteW(
                None,
                w!("open"),
                &HSTRING::from(url),
                None,
                None,
                SW_SHOWNORMAL,
            );
        }
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
                i if i == IDOK.0 => {
                    if !enter_in_words(hwnd) {
                        on_ok(hwnd);
                    }
                }
                // SAFETY: closing our own window.
                i if i == IDCANCEL.0 => {
                    let _ = unsafe { DestroyWindow(hwnd) };
                }
                i if (ID_PRESET..=ID_CUSTOM).contains(&i) => on_shortcut_choice(hwnd, i),
                ID_WORD_SAVE => on_word_save(hwnd),
                ID_WORD_REMOVE => on_word_remove(hwnd),
                ID_WORD_CLEAR => on_word_clear(hwnd),
                ID_CLEAR_HISTORY => on_clear_history(hwnd),
                id @ (ID_WORD_SEARCH | ID_WORD_KEY)
                    if (wparam.0 >> 16) as u32 == EN_CHANGE
                        && !WINDOW.with(|w| w.borrow().as_ref().is_some_and(|w| w.filling)) =>
                {
                    if id == ID_WORD_SEARCH {
                        refresh_words(hwnd);
                    } else {
                        on_word_key_changed(hwnd);
                    }
                }
                ID_CONVERT_ON => {
                    // SAFETY: enabling our own control.
                    unsafe {
                        if let Ok(key_box) = GetDlgItem(Some(hwnd), ID_CONVERT_KEY) {
                            let _ = EnableWindow(key_box, checked(hwnd, ID_CONVERT_ON));
                        }
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_NOTIFY => {
            // SAFETY: for WM_NOTIFY, lparam points to an NMHDR (an NMLINK for
            // SysLink clicks).
            let hdr = unsafe { &*(lparam.0 as *const NMHDR) };
            if hdr.code == TCN_SELCHANGE {
                // SAFETY: asking our own tab control for its selection.
                let index = unsafe { SendMessageW(hdr.hwndFrom, TCM_GETCURSEL, None, None).0 };
                show_page(index.max(0) as usize);
            } else if hdr.idFrom == ID_WORDS as usize && hdr.code == LVN_ITEMCHANGED {
                on_word_selected(hwnd);
            } else if hdr.idFrom == ID_WORDS as usize && hdr.code == LVN_KEYDOWN {
                // SAFETY: LVN_KEYDOWN comes with an NMLVKEYDOWN.
                let key = unsafe { &*(lparam.0 as *const NMLVKEYDOWN) };
                if key.wVKey == VK_DELETE.0 {
                    on_word_remove(hwnd);
                }
            } else if hdr.code == NM_CLICK || hdr.code == NM_RETURN {
                // SAFETY: only our SysLink controls send these to this window.
                open_link(unsafe { &*(lparam.0 as *const NMLINK) });
            }
            LRESULT(0)
        }
        // Labels, radio buttons, checkbox and links take the tab page's
        // background; notes are grey and the heart red. The key box (a
        // read-only edit) stays white like an input.
        WM_CTLCOLORSTATIC => {
            let dc = HDC(wparam.0 as *mut _);
            let page = WINDOW.with(|w| w.borrow().as_ref().map(|w| w.page_brush));
            // SAFETY: wparam is the control's DC and lparam the control.
            unsafe {
                SetBkMode(dc, TRANSPARENT);
                let brush = match GetDlgCtrlID(HWND(lparam.0 as *mut _)) {
                    ID_HOTKEY | ID_CONVERT_KEY => None,
                    ID_MUTED | ID_WORD_COUNT => {
                        SetTextColor(dc, COLORREF(GetSysColor(COLOR_GRAYTEXT)));
                        page
                    }
                    ID_HEART => {
                        SetTextColor(dc, COLORREF(0x0030_30D8)); // BGR: #D83030
                        page
                    }
                    _ => page,
                };
                LRESULT(brush.unwrap_or_else(|| GetSysColorBrush(COLOR_WINDOW)).0 as isize)
            }
        }
        WM_CLOSE => {
            // SAFETY: closing our own window.
            let _ = unsafe { DestroyWindow(hwnd) };
            LRESULT(0)
        }
        WM_DESTROY => {
            if let Some(w) = WINDOW.with(|w| w.borrow_mut().take()) {
                // SAFETY: the fonts and icon are no longer used by any control.
                unsafe {
                    let _ = DeleteObject(HGDIOBJ(w.font.0));
                    let _ = DeleteObject(HGDIOBJ(w.title_font.0));
                    let _ = DeleteObject(HGDIOBJ(w.page_brush.0));
                    if let Some(icon) = w.logo {
                        let _ = DestroyIcon(icon);
                    }
                }
                (w.on_close)();
            }
            LRESULT(0)
        }
        // SAFETY: default processing for everything else.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn held(vk: u16) -> bool {
    // SAFETY: plain query of this thread's keyboard state.
    (unsafe { GetKeyState(vk as i32) }) < 0
}

/// The hotkey box: records the combination pressed while it has focus.
unsafe extern "system" fn key_box_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    match msg {
        // With Ctrl or Alt held, every key comes here (Ctrl+Tab, Ctrl+Enter,
        // Alt+Space…); plain Tab, Enter and Esc still move focus or close.
        WM_GETDLGCODE if held(VK_CONTROL.0) || held(VK_MENU.0) => {
            return LRESULT(DLGC_WANTALLKEYS as isize);
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            record(hwnd, wparam.0 as u32);
            return LRESULT(0);
        }
        // Nothing is typed into the box, and Alt+Space doesn't open the menu.
        WM_CHAR | WM_SYSCHAR => return LRESULT(0),
        _ => {}
    }
    // SAFETY: default processing by the edit control.
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

/// Shows and remembers `vk` with the modifiers held, if it makes a usable hotkey.
fn record(key_box: HWND, vk: u32) {
    let mut modifiers = 0;
    for (vks, bit) in [
        (&[VK_CONTROL][..], MOD_CONTROL),
        (&[VK_MENU][..], MOD_ALT),
        (&[VK_SHIFT][..], MOD_SHIFT),
        (&[VK_LWIN, VK_RWIN][..], MOD_WIN),
    ] {
        if vks.iter().any(|k| held(k.0)) {
            modifiers |= bit;
        }
    }
    let Some(hotkey) = Hotkey::from_keys(modifiers, vk) else {
        return; // a lone modifier, a bare key, or a key we can't store
    };
    let text = hotkey.to_string();
    // SAFETY: updating our own edit control.
    let _ = unsafe { SetWindowTextW(key_box, &HSTRING::from(text.as_str())) };
    WINDOW.with(|w| {
        if let Some(w) = w.borrow_mut().as_mut() {
            // SAFETY: plain query of our own control.
            if unsafe { GetDlgCtrlID(key_box) } == ID_CONVERT_KEY {
                w.convert = text;
            } else {
                w.hotkey = text;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, IsDialogMessageW, KillTimer, MSG, PostQuitMessage, SetTimer,
        TranslateMessage,
    };

    /// The Words tab, driven through its controls: selecting a row fills
    /// the boxes, Save word changes or adds, Remove removes, and OK hands
    /// the edited list to the app.
    #[test]
    fn words_tab_edits_the_list() {
        thread_local! {
            static APPLIED: RefCell<Option<Values>> = const { RefCell::new(None) };
        }
        fn apply(v: &Values) -> Result<(), String> {
            APPLIED.with(|a| *a.borrow_mut() = Some(v.clone()));
            Ok(())
        }
        fn suggest(key: &str) -> Vec<String> {
            match key {
                "nanri" => vec!["நன்றி".into(), "நண்றி".into()],
                _ => vec!["கடல்".into(), "காதல்".into()],
            }
        }
        fn closed() {}
        let pair = |k: &str, w: &str| (k.to_string(), w.to_string());
        let values = Values {
            hotkey: "Ctrl+Alt+T".into(),
            convert_hotkey: String::new(),
            insert_mode: InsertMode::Unicode,
            beam: 5,
            remember_picks: true,
            predict: true,
            theme: Theme::System,
            autostart: false,
            learned: vec![
                pair("amma", "அம்மா"),
                pair("kadal", "கடல்"),
                pair("thamil", "தமில்"),
            ],
        };
        open(
            values,
            Actions {
                apply,
                suggest,
                clear_history: || Ok(()),
                on_close: closed,
            },
        );
        let h = hwnd().expect("settings window");
        let text = |id| text_of(item(h, id));
        assert_eq!(text(ID_WORD_COUNT), "3 words");

        // Selecting a row fills the boxes; change its spelling.
        select_word(h, 1);
        assert_eq!(text(ID_WORD_KEY), "kadal");
        assert_eq!(text(ID_WORD_TAMIL), "கடல்");
        fill_text(item(h, ID_WORD_TAMIL), "காதல்");
        on_word_save(h);
        assert_eq!(learned_words()[1], pair("kadal", "காதல்"));

        // Typing a new word offers its suggestions; save adds it in order.
        // SAFETY: setting our own control's text (sends EN_CHANGE).
        let _ = unsafe { SetWindowTextW(item(h, ID_WORD_KEY), w!("Nanri")) };
        assert_eq!(text(ID_WORD_TAMIL), "நன்றி");
        on_word_save(h);
        assert_eq!(learned_words()[2], pair("nanri", "நன்றி"));
        assert_eq!(text(ID_WORD_COUNT), "4 words");

        // The saved word is selected; Remove removes it.
        assert_eq!(selected_words(h), [2]);
        on_word_remove(h);
        assert_eq!(text(ID_WORD_COUNT), "3 words");

        // Search filters the list.
        fill_text(item(h, ID_WORD_SEARCH), "tha");
        refresh_words(h);
        assert_eq!(text(ID_WORD_COUNT), "1 of 3");

        // A word that isn't Tamil isn't saved (the warning box is skipped
        // by checking the rule directly).
        assert!(!learned::valid("hello", "hello"));

        on_ok(h);
        let applied = APPLIED.with(|a| a.borrow_mut().take()).expect("applied");
        assert_eq!(
            applied.learned,
            [
                pair("amma", "அம்மா"),
                pair("kadal", "காதல்"),
                pair("thamil", "தமில்")
            ]
        );
        assert!(hwnd().is_none(), "OK closes the window");
    }

    #[test]
    fn recognises_presets_however_written() {
        assert_eq!(preset_index("Ctrl+Alt+T"), Some(0));
        assert_eq!(preset_index("alt + ctrl + t"), Some(0));
        assert_eq!(preset_index("Ctrl+Space"), Some(1));
        assert_eq!(preset_index("Ctrl+Alt+K"), None);
    }

    /// Opens the window for a few seconds (for screenshots); Cancel or the
    /// timeout closes it. Run with: cargo test -p ezhuthu settings_preview -- --ignored
    /// (EZHUTHU_PREVIEW_TAB=1 starts on the Shortcuts tab, 2 on Words, 3 on About.)
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
            convert_hotkey: "Ctrl+Alt+Shift+T".into(),
            insert_mode: InsertMode::Unicode,
            beam: 5,
            remember_picks: true,
            predict: true,
            theme: Theme::System,
            autostart: true,
            learned: vec![
                ("amma".into(), "அம்மா".into()),
                ("kadal".into(), "கடல்".into()),
                ("thamil".into(), "தமில்".into()),
            ],
        };
        fn suggest(_: &str) -> Vec<String> {
            vec!["கடல்".into(), "காதல்".into()]
        }
        open(
            values,
            Actions {
                apply,
                suggest,
                clear_history: || Ok(()),
                on_close: closed,
            },
        );
        let start_tab = std::env::var("EZHUTHU_PREVIEW_TAB")
            .ok()
            .and_then(|t| t.parse::<usize>().ok())
            .filter(|&t| (1..=3).contains(&t));
        if let Some(t) = start_tab {
            show_page(t);
            // SAFETY: selecting the About tab on our own tab control.
            unsafe {
                if let Some(h) = hwnd()
                    && let Ok(tabs) = GetDlgItem(Some(h), ID_TABS)
                {
                    SendMessageW(
                        tabs,
                        windows::Win32::UI::Controls::TCM_SETCURSEL,
                        Some(WPARAM(t)),
                        None,
                    );
                }
            }
        }
        // SAFETY: a plain message loop with a timer that closes the window.
        unsafe {
            let timer = SetTimer(None, 0, 12000, None);
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
