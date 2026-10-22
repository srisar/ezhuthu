//! App state and the Win32 event handlers.
//!
//! Flow: the global hotkey opens the popup over the focused app without
//! activating it. While it is visible a low-level keyboard hook swallows the
//! keys and posts them to the popup; the composer turns them into words,
//! the engine thread transliterates, and Enter types the Tamil phrase into
//! the still-focused target.
//!
//! State lives in a thread-local `RefCell`. Win32 can call back into our
//! window procedures synchronously, so handlers borrow it briefly through
//! `with_app` and never hold the borrow across calls that may dispatch
//! messages to us (a re-entrant call just finds it busy and is skipped).

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use ezhuthu_xlit::Transliterator;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT};
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, GetKeyState, GetKeyboardLayout, HOT_KEY_MODIFIERS, MOD_NOREPEAT,
    RegisterHotKey, ToUnicodeEx, UnregisterHotKey, VK_BACK, VK_CAPITAL, VK_CONTROL, VK_DOWN,
    VK_ESCAPE, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_MENU, VK_RCONTROL, VK_RETURN,
    VK_RMENU, VK_RSHIFT, VK_RWIN, VK_SHIFT, VK_SPACE, VK_UP,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    EVENT_SYSTEM_FOREGROUND, GetForegroundWindow, GetMessageW, GetWindowThreadProcessId, HHOOK,
    HWND_TOPMOST, KBDLLHOOKSTRUCT, MA_NOACTIVATE, MSG, PostMessageW, PostQuitMessage,
    RegisterClassExW, RegisterWindowMessageW, SW_HIDE, SW_SHOWNORMAL, SWP_NOACTIVATE,
    SWP_SHOWWINDOW, SetWindowPos, SetWindowsHookExW, ShowWindow, TranslateMessage,
    UnhookWindowsHookEx, WH_KEYBOARD_LL, WINEVENT_OUTOFCONTEXT, WM_APP, WM_CONTEXTMENU, WM_DESTROY,
    WM_DPICHANGED, WM_ERASEBKGND, WM_HOTKEY, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEACTIVATE, WM_PAINT, WM_RBUTTONUP, WM_SYSKEYDOWN, WM_USER, WNDCLASSEXW,
};
use windows::core::{PCWSTR, w};

use crate::caret;
use crate::compose::Composer;
use crate::config::{Config, InsertMode};
use crate::engine::{Engine, Event};
use crate::hotkey::{Hotkey, MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN};
use crate::insert;
use crate::popup::{Popup, View};
use crate::tray::Tray;
use crate::win::{error_box, instance, wide};

const WM_APP_TRAY: u32 = WM_APP + 1;
const WM_APP_ENGINE: u32 = WM_APP + 2;
/// Popup key: wparam = virtual key | CTRL_BIT, lparam = typed char (0 = none).
const WM_APP_KEY: u32 = WM_APP + 3;
const WM_APP_CANCEL: u32 = WM_APP + 4;
const CTRL_BIT: usize = 1 << 16;
const NIN_SELECT: u32 = WM_USER;
const HOTKEY_ID: i32 = 1;
const MAX_ROWS: usize = 9;

const HINT: &str = "↑↓ or 1–5 choose · Space next word · Enter insert · Esc cancel";

enum Status {
    Loading,
    Ready,
    Failed(String),
}

struct App {
    main: HWND,
    popup: Popup,
    tray: Tray,
    config: Config,
    config_path: PathBuf,
    engine: Engine,
    xlit: Option<Arc<Transliterator>>,
    status: Status,
    cache: HashMap<String, Vec<String>>,
    pending: HashSet<String>,
    composer: Composer,
    target: HWND,
    visible: bool,
    kb_hook: Option<HHOOK>,
    fg_hook: Option<HWINEVENTHOOK>,
    /// One-off message shown in the hint line until the next key.
    flash: Option<String>,
}

/// What the keyboard hook needs, readable without borrowing the app.
#[derive(Clone, Copy)]
struct HookState {
    popup: HWND,
    hotkey: Option<Hotkey>,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
    static HOOK: Cell<Option<HookState>> = const { Cell::new(None) };
    static TASKBAR_CREATED: Cell<u32> = const { Cell::new(0) };
}

fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|cell| match cell.try_borrow_mut() {
        Ok(mut app) => app.as_mut().map(f),
        Err(_) => None,
    })
}

// ---------------------------------------------------------------- startup

pub fn run() -> Result<()> {
    let config_path = Config::path()?;
    let (config, config_error) = match Config::load_or_create(&config_path) {
        Ok(c) => (c, None),
        Err(e) => (Config::default(), Some(format!("{e:#}"))),
    };

    let main = create_main_window()?;
    let rows = config.beam.min(MAX_ROWS);
    let popup = Popup::create(Some(popup_proc), rows)?;
    let tray = Tray::add(main, WM_APP_TRAY, &tooltip(&config))?;
    // SAFETY: the string is a valid NUL-terminated literal.
    TASKBAR_CREATED.set(unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) });

    let engine = Engine::start(config.assets_dir(), config.beam, main, WM_APP_ENGINE);
    let mut app = App {
        main,
        popup,
        tray,
        config,
        config_path,
        engine,
        xlit: None,
        status: Status::Loading,
        cache: HashMap::new(),
        pending: HashSet::new(),
        composer: Composer::default(),
        target: HWND::default(),
        visible: false,
        kb_hook: None,
        fg_hook: None,
        flash: None,
    };
    if let Some(e) = config_error {
        app.tray
            .notify("Settings file has an error (using defaults)", &e, true);
    }
    app.register_hotkey();
    APP.with(|cell| *cell.borrow_mut() = Some(app));

    let mut msg = MSG::default();
    // SAFETY: standard message loop on the thread that owns our windows.
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    APP.with(|cell| cell.borrow_mut().take());
    Ok(())
}

fn tooltip(config: &Config) -> String {
    format!("ezhuthu: Tamil input ({})", config.hotkey)
}

fn create_main_window() -> Result<HWND> {
    let class = w!("ezhuthu.main");
    // SAFETY: the class struct is fully initialised. The window is never
    // shown; it receives the hotkey, tray and engine messages.
    unsafe {
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(main_proc),
            hInstance: instance(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        Ok(CreateWindowExW(
            Default::default(),
            class,
            w!("ezhuthu"),
            Default::default(),
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance()),
            None,
        )?)
    }
}

impl App {
    fn register_hotkey(&mut self) {
        // SAFETY: unregistering an id that isn't registered is harmless.
        let _ = unsafe { UnregisterHotKey(Some(self.main), HOTKEY_ID) };
        let hotkey = match Hotkey::parse(&self.config.hotkey) {
            Ok(h) => h,
            Err(e) => {
                self.tray
                    .notify("Invalid hotkey in settings", &format!("{e:#}"), true);
                HOOK.set(None);
                return;
            }
        };
        let mods = HOT_KEY_MODIFIERS(hotkey.modifiers) | MOD_NOREPEAT;
        // SAFETY: plain registration for our window.
        let registered = unsafe { RegisterHotKey(Some(self.main), HOTKEY_ID, mods, hotkey.vk) };
        eprintln!("hotkey {}: {:?}", self.config.hotkey, registered);
        if registered.is_err() {
            self.tray.notify(
                "Hotkey unavailable",
                &format!(
                    "{} is used by another app. Change it in the settings file.",
                    self.config.hotkey
                ),
                true,
            );
        }
        HOOK.set(Some(HookState {
            popup: self.popup.hwnd,
            hotkey: Some(hotkey),
        }));
    }

    fn show(&mut self, anchor: caret::Anchor) {
        self.popup.set_dpi(caret::dpi_at(anchor));
        let (w, h) = self.popup.size();
        let (x, y) = caret::place(anchor, w, h, self.popup.gap());
        self.visible = true;
        // SAFETY: our own window; SWP_NOACTIVATE keeps focus in the target.
        unsafe {
            let _ = SetWindowPos(
                self.popup.hwnd,
                Some(HWND_TOPMOST),
                x,
                y,
                w,
                h,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            let _ = InvalidateRect(Some(self.popup.hwnd), None, false);
            self.kb_hook =
                SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), Some(instance()), 0).ok();
            self.fg_hook = Some(SetWinEventHook(
                EVENT_SYSTEM_FOREGROUND,
                EVENT_SYSTEM_FOREGROUND,
                None,
                Some(foreground_changed),
                0,
                0,
                WINEVENT_OUTOFCONTEXT,
            ));
        }
        if self.kb_hook.is_none() {
            self.flash = Some("Could not capture the keyboard; try again".into());
        }
    }

    fn hide(&mut self) {
        self.visible = false;
        // SAFETY: unhooking our own hooks and hiding our own window.
        unsafe {
            if let Some(h) = self.kb_hook.take() {
                let _ = UnhookWindowsHookEx(h);
            }
            if let Some(h) = self.fg_hook.take() {
                let _ = UnhookWinEvent(h);
            }
            let _ = ShowWindow(self.popup.hwnd, SW_HIDE);
        }
        self.composer.clear();
        self.flash = None;
    }

    fn invalidate(&self) {
        if self.visible {
            // SAFETY: our own window; painting happens later via WM_PAINT.
            let _ = unsafe { InvalidateRect(Some(self.popup.hwnd), None, false) };
        }
    }

    fn current_candidates(&self) -> &[String] {
        self.composer
            .current_key()
            .and_then(|k| self.cache.get(&k))
            .map_or(&[], Vec::as_slice)
    }

    /// Asks the engine for every word not cached or already pending.
    fn request_missing(&mut self) {
        for key in self.composer.keys() {
            if !self.cache.contains_key(&key) && self.pending.insert(key.clone()) {
                self.engine.request(key);
            }
        }
    }

    fn set_text(&mut self, text: &str) {
        self.composer.set_text(text);
        self.request_missing();
    }

    fn push(&mut self, s: &str) {
        let text = format!("{}{s}", self.composer.text());
        self.set_text(&text);
    }

    fn view(&self) -> View {
        let candidates: Vec<String> = self
            .current_candidates()
            .iter()
            .take(self.popup.rows())
            .cloned()
            .collect();
        let (hint, warn) = if let Some(f) = &self.flash {
            (f.clone(), true)
        } else {
            match &self.status {
                Status::Loading => ("Loading the model…".to_string(), false),
                Status::Failed(e) => (format!("Model failed to load: {e}"), true),
                Status::Ready => (
                    HINT.replace("1–5", &format!("1–{}", self.popup.rows())),
                    false,
                ),
            }
        };
        View {
            input: self.composer.text().to_string(),
            preview: self
                .composer
                .render(|k| self.cache.get(k).map(Vec::as_slice)),
            candidates,
            selected: self.composer.selected(),
            hint,
            warn,
        }
    }

    /// Picks candidate `index` for the current word and starts the next one.
    fn pick(&mut self, index: usize) -> bool {
        let cands = self.current_candidates().to_vec();
        if index >= cands.len() {
            return false;
        }
        self.composer.select(index);
        self.composer.commit_current(Some(&cands));
        self.push(" ");
        true
    }

    fn handle_key(&mut self, vk: u16, ctrl: bool, ch: Option<char>) -> Option<Action> {
        self.flash = None;
        match vk {
            v if v == VK_ESCAPE.0 => return Some(Action::Cancel),
            v if v == VK_RETURN.0 => return Some(Action::Accept),
            v if ctrl && v == b'V' as u16 => return Some(Action::Paste),
            v if ctrl && v == b'R' as u16 => {
                self.flash = Some("Refine (Ctrl+R) is not available yet".into());
            }
            v if v == VK_BACK.0 => {
                let mut text = self.composer.text().to_string();
                if ctrl {
                    let trimmed = text.trim_end_matches(' ').len();
                    text.truncate(trimmed);
                    let start = text.rfind(' ').map_or(0, |i| i + 1);
                    text.truncate(start);
                } else {
                    text.pop();
                }
                self.set_text(&text);
            }
            v if v == VK_UP.0 || v == VK_DOWN.0 => {
                let n = self.current_candidates().len().min(self.popup.rows());
                self.composer
                    .move_selection(if v == VK_UP.0 { -1 } else { 1 }, n);
            }
            v if v == VK_SPACE.0 => {
                let cands = self.current_candidates().to_vec();
                self.composer.commit_current(Some(&cands));
                self.push(" ");
            }
            _ => {
                let c = ch?;
                let picked = c
                    .to_digit(10)
                    .filter(|&d| (1..=self.popup.rows() as u32).contains(&d))
                    .is_some_and(|d| self.pick(d as usize - 1));
                if !picked && !c.is_control() {
                    self.push(&c.to_string());
                }
            }
        }
        None
    }

    fn render(&self) -> String {
        self.composer
            .render(|k| self.cache.get(k).map(Vec::as_slice))
    }
}

enum Action {
    Accept,
    Cancel,
    Paste,
}

// ---------------------------------------------------------------- handlers

fn on_hotkey() {
    // SAFETY: plain query.
    let fg = unsafe { GetForegroundWindow() };
    let Some((visible, popup)) = with_app(|a| (a.visible, a.popup.hwnd)) else {
        return;
    };
    if visible {
        cancel();
        return;
    }
    if fg.0.is_null() || fg == popup {
        return;
    }
    if insert::blocked_by_uipi(fg) {
        with_app(|a| {
            a.tray.notify(
                "Can't type into this window",
                "It runs as administrator, and Windows blocks input from normal apps into it.",
                true,
            )
        });
        return;
    }
    let anchor = caret::find(fg);
    with_app(|a| {
        a.target = fg;
        a.composer.clear();
        a.flash = None;
        a.show(anchor);
    });
}

fn on_key(wparam: usize, lparam: isize) {
    let vk = (wparam & 0xFFFF) as u16;
    let ctrl = wparam & CTRL_BIT != 0;
    let ch = char::from_u32(lparam as u32).filter(|&c| c != '\0');
    let action = with_app(|a| a.handle_key(vk, ctrl, ch)).flatten();
    match action {
        Some(Action::Accept) => accept(),
        Some(Action::Cancel) => cancel(),
        Some(Action::Paste) => {
            let text = insert::read_clipboard_text().unwrap_or_default();
            let clean: String = text
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect();
            with_app(|a| a.push(&clean));
        }
        None => {}
    }
    with_app(|a| a.invalidate());
}

fn on_engine_events() {
    let failed = with_app(|a| {
        let mut failed = None;
        while let Ok(ev) = a.engine.events.try_recv() {
            match ev {
                Event::Ready(x) => {
                    eprintln!(
                        "model loaded (word-frequency rescoring: {})",
                        x.has_word_freq()
                    );
                    a.xlit = Some(x);
                    a.status = Status::Ready;
                }
                Event::Failed(e) => {
                    eprintln!("model failed to load: {e}");
                    a.status = Status::Failed(e.clone());
                    failed = Some(e);
                }
                Event::Done { key, candidates } => {
                    a.pending.remove(&key);
                    a.cache.insert(key, candidates);
                }
            }
        }
        a.invalidate();
        failed
    })
    .flatten();
    if let Some(e) = failed {
        with_app(|a| {
            a.tray.notify(
                "The transliteration model failed to load",
                &format!("{e}\nRun tools/fetch-assets.sh or set assets_dir in the settings file."),
                true,
            )
        });
    }
}

/// Enter: finish any words still being transliterated, then type the phrase.
fn accept() {
    enum Prep {
        Empty,
        NotLoaded,
        Run(Option<Arc<Transliterator>>, Vec<String>),
    }
    let Some(prep) = with_app(|a| {
        if a.composer.text().trim().is_empty() {
            return Prep::Empty;
        }
        let missing: Vec<String> = a
            .composer
            .keys()
            .into_iter()
            .filter(|k| !a.cache.contains_key(k))
            .collect();
        match (&a.xlit, missing.is_empty()) {
            (_, true) => Prep::Run(None, missing),
            (Some(x), false) => Prep::Run(Some(x.clone()), missing),
            (None, false) => Prep::NotLoaded,
        }
    }) else {
        return;
    };
    match prep {
        Prep::Empty => return cancel(),
        Prep::NotLoaded => {
            with_app(|a| {
                a.flash = Some("The model is still loading; try Enter again in a moment".into());
                a.invalidate();
            });
            return;
        }
        Prep::Run(Some(xlit), missing) => {
            // Rare: Enter pressed before the engine answered. Takes ~40 ms/word.
            let results: Vec<(String, Vec<String>)> = missing
                .into_iter()
                .map(|k| {
                    let c = xlit
                        .transliterate(&k)
                        .map(|c| c.into_iter().map(|c| c.text).collect());
                    (k, c.unwrap_or_default())
                })
                .collect();
            with_app(|a| a.cache.extend(results));
        }
        Prep::Run(None, _) => {}
    }
    let Some((text, target, mode)) = with_app(|a| (a.render(), a.target, a.config.insert_mode))
    else {
        return;
    };
    with_app(|a| a.hide());
    if let Err(e) = insert::insert(target, &text, mode) {
        let hint = if mode == InsertMode::Unicode {
            "\nIf this app mishandles typed text, set insert_mode = \"clipboard\" in the settings."
        } else {
            ""
        };
        with_app(|a| {
            a.tray
                .notify("Couldn't insert the text", &format!("{e:#}{hint}"), true)
        });
    }
}

fn cancel() {
    with_app(|a| a.hide());
}

fn on_tray(event: u32) {
    if !matches!(
        event,
        WM_CONTEXTMENU | WM_RBUTTONUP | WM_LBUTTONUP | NIN_SELECT
    ) {
        return;
    }
    let Some((header, path)) = with_app(|a| (tooltip(&a.config), a.config_path.clone())) else {
        return;
    };
    const SETTINGS: usize = 1;
    const RELOAD: usize = 2;
    const QUIT: usize = 3;
    let items = [
        (0, header.as_str()),
        (0, "-"),
        (SETTINGS, "Open settings file"),
        (RELOAD, "Reload settings"),
        (0, "-"),
        (QUIT, "Quit"),
    ];
    // The menu runs a modal loop, so the app must not be borrowed meanwhile.
    let tray_menu = with_app(|a| a.tray.menu(&items));
    match tray_menu.unwrap_or(0) {
        SETTINGS => open_in_notepad(&path),
        RELOAD => reload_settings(),
        QUIT => quit(),
        _ => {}
    }
}

fn open_in_notepad(path: &std::path::Path) {
    let file = wide(&format!("\"{}\"", path.display()));
    // SAFETY: NUL-terminated strings that outlive the call.
    unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            w!("notepad.exe"),
            PCWSTR(file.as_ptr()),
            None,
            SW_SHOWNORMAL,
        );
    }
}

fn reload_settings() {
    with_app(|a| {
        let new = match Config::load_or_create(&a.config_path) {
            Ok(c) => c,
            Err(e) => {
                a.tray
                    .notify("Settings not reloaded", &format!("{e:#}"), true);
                return;
            }
        };
        let restart = new.assets_dir != a.config.assets_dir || new.beam != a.config.beam;
        a.config = new;
        a.register_hotkey();
        let tip = tooltip(&a.config);
        a.tray.set_tip(&tip);
        let note = if restart {
            "Restart ezhuthu to apply the model settings (beam, assets_dir)."
        } else {
            "New settings are active."
        };
        a.tray.notify("Settings reloaded", note, false);
    });
}

fn quit() {
    let main = with_app(|a| {
        a.hide();
        a.tray.remove();
        // SAFETY: our registration.
        let _ = unsafe { UnregisterHotKey(Some(a.main), HOTKEY_ID) };
        a.main
    });
    if let Some(main) = main {
        // SAFETY: our own window; WM_DESTROY posts the quit message.
        let _ = unsafe { DestroyWindow(main) };
    }
}

// ---------------------------------------------------------------- window procedures

unsafe extern "system" fn main_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_HOTKEY if wparam.0 as i32 == HOTKEY_ID => on_hotkey(),
        WM_APP_TRAY => on_tray((lparam.0 & 0xFFFF) as u32),
        WM_APP_ENGINE => on_engine_events(),
        WM_APP_CANCEL => cancel(),
        WM_DESTROY => {
            // SAFETY: ends our message loop.
            unsafe { PostQuitMessage(0) };
        }
        m if m != 0 && m == TASKBAR_CREATED.get() => {
            with_app(|a| {
                let _ = a.tray.register();
            });
        }
        // SAFETY: default processing for everything else.
        _ => return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
    LRESULT(0)
}

unsafe extern "system" fn popup_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_ERASEBKGND => return LRESULT(1),
        WM_DPICHANGED => {} // sized explicitly on each show
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            // SAFETY: Begin/EndPaint pair on our own window.
            unsafe {
                let hdc = BeginPaint(hwnd, &mut ps);
                with_app(|a| a.popup.paint(hdc, &a.view()));
                let _ = EndPaint(hwnd, &ps);
            }
        }
        WM_LBUTTONDOWN => {
            let (x, y) = (
                (lparam.0 & 0xFFFF) as i16 as i32,
                ((lparam.0 >> 16) & 0xFFFF) as i16 as i32,
            );
            with_app(|a| {
                if let Some(row) = a.popup.row_at(x, y) {
                    a.pick(row);
                    a.invalidate();
                }
            });
        }
        WM_APP_KEY => on_key(wparam.0, lparam.0),
        WM_APP_CANCEL => cancel(),
        // SAFETY: default processing for everything else.
        _ => return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
    LRESULT(0)
}

// ---------------------------------------------------------------- hooks

fn down(vk: u16) -> bool {
    // SAFETY: plain query.
    (unsafe { GetAsyncKeyState(vk as i32) }) < 0
}

fn is_modifier(vk: u16) -> bool {
    [
        VK_SHIFT,
        VK_LSHIFT,
        VK_RSHIFT,
        VK_CONTROL,
        VK_LCONTROL,
        VK_RCONTROL,
        VK_MENU,
        VK_LMENU,
        VK_RMENU,
        VK_LWIN,
        VK_RWIN,
        VK_CAPITAL,
    ]
    .iter()
    .any(|k| k.0 == vk)
}

/// The character a key would type with the current Shift/Caps state, in
/// the foreground app's keyboard layout. Does not disturb dead-key state.
fn key_char(vk: u32, scan: u32) -> Option<char> {
    let mut state = [0u8; 256];
    if down(VK_SHIFT.0) {
        state[VK_SHIFT.0 as usize] = 0x80;
    }
    // SAFETY: plain queries; ToUnicodeEx writes at most `buf.len()` units.
    unsafe {
        if GetKeyState(VK_CAPITAL.0 as i32) & 1 != 0 {
            state[VK_CAPITAL.0 as usize] = 1;
        }
        let tid = GetWindowThreadProcessId(GetForegroundWindow(), None);
        let layout = GetKeyboardLayout(tid);
        let mut buf = [0u16; 4];
        // Flag 0x4: don't change the keyboard state (Windows 10 1607+).
        let n = ToUnicodeEx(vk, scan, &state, &mut buf, 0x4, Some(layout));
        (n == 1).then(|| char::from_u32(buf[0] as u32)).flatten()
    }
}

enum HookAction {
    Pass,
    Swallow,
    Post(usize, isize),
    Cancel,
}

/// Decides what to do with a key while the popup is visible.
fn classify(state: HookState, vk: u16, scan: u32, is_down: bool) -> HookAction {
    if is_modifier(vk) {
        return HookAction::Pass;
    }
    let (ctrl, alt, win, shift) = (
        down(VK_CONTROL.0),
        down(VK_MENU.0),
        down(VK_LWIN.0) || down(VK_RWIN.0),
        down(VK_SHIFT.0),
    );
    if let Some(h) = state.hotkey {
        let mods = (ctrl as u32 * MOD_CONTROL)
            | (alt as u32 * MOD_ALT)
            | (shift as u32 * MOD_SHIFT)
            | (win as u32 * MOD_WIN);
        if vk as u32 == h.vk && mods == h.modifiers {
            return if is_down {
                HookAction::Cancel
            } else {
                HookAction::Swallow
            };
        }
    }
    if alt || win {
        // Alt+Tab, Win+D and friends: get out of the way.
        return HookAction::Cancel;
    }
    if ctrl && ![b'V' as u16, b'R' as u16, VK_BACK.0].contains(&vk) {
        return HookAction::Cancel;
    }
    if !is_down {
        return HookAction::Swallow;
    }
    let ch = if ctrl {
        None
    } else {
        key_char(vk as u32, scan)
    };
    let wparam = vk as usize | if ctrl { CTRL_BIT } else { 0 };
    HookAction::Post(wparam, ch.map_or(0, |c| c as isize))
}

unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // Injected keys (remappers, on-screen keyboards) are handled too; our own
    // SendInput only runs after this hook has been removed.
    if code >= 0
        && let Some(state) = HOOK.get()
    {
        // SAFETY: for WH_KEYBOARD_LL with code >= 0, lparam points to a KBDLLHOOKSTRUCT.
        let kb = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let is_down = wparam.0 as u32 == WM_KEYDOWN || wparam.0 as u32 == WM_SYSKEYDOWN;
        match classify(state, kb.vkCode as u16, kb.scanCode, is_down) {
            HookAction::Pass => {}
            HookAction::Swallow => return LRESULT(1),
            HookAction::Post(w, l) => {
                // SAFETY: posting to our own window.
                let _ =
                    unsafe { PostMessageW(Some(state.popup), WM_APP_KEY, WPARAM(w), LPARAM(l)) };
                return LRESULT(1);
            }
            HookAction::Cancel => {
                // SAFETY: posting to our own window; the key passes through.
                let _ =
                    unsafe { PostMessageW(Some(state.popup), WM_APP_CANCEL, WPARAM(0), LPARAM(0)) };
            }
        }
    }
    // SAFETY: forwarding to the rest of the hook chain.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// Another window came to the foreground (click, Alt+Tab): close the popup.
unsafe extern "system" fn foreground_changed(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    _id_object: i32,
    _id_child: i32,
    _thread: u32,
    _time: u32,
) {
    let main =
        with_app(|a| (a.visible && hwnd != a.target && hwnd != a.popup.hwnd).then_some(a.main));
    if let Some(Some(main)) = main {
        // SAFETY: posting to our own window.
        let _ = unsafe { PostMessageW(Some(main), WM_APP_CANCEL, WPARAM(0), LPARAM(0)) };
    }
}

/// Shows a fatal startup error (no console in release builds).
pub fn report_fatal(e: &anyhow::Error) {
    error_box(None, &format!("ezhuthu could not start:\n\n{e:#}"));
}
