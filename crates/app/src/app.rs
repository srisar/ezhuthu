//! App state and the Win32 event handlers.
//!
//! Tamil mode is toggled with the global hotkey. While it is on, a
//! low-level keyboard hook watches every key:
//! - a letter starts (or extends) a word; the popup shows its candidates
//!   at the caret without taking focus;
//! - Space / Enter / a digit commit the word (Tamil text goes to the app);
//! - when no word is in progress, everything else passes through, so
//!   digits, punctuation, Enter, arrows and shortcuts work as usual;
//! - mid-word, any other key commits the word first, then does its job.
//!
//! Ordering: committed text is queued (`outbox`) and injected from the
//! message loop, tagged with `insert::MARK`. Until every tagged event has
//! come back through the hook, real keys that would pass are queued behind
//! it instead, so output can never overtake earlier text.
//!
//! State lives in a thread-local `RefCell`. Win32 calls back into our
//! procedures synchronously, so handlers borrow it briefly through
//! `with_app` and never hold it across calls that pump messages; a
//! re-entrant call finds it busy and falls back to a safe default.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

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
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    EVENT_SYSTEM_FOREGROUND, GetForegroundWindow, GetMessageW, GetWindowThreadProcessId, HHOOK,
    HWND_TOPMOST, IsDialogMessageW, KBDLLHOOKSTRUCT, LLKHF_EXTENDED, MA_NOACTIVATE, MSG,
    PostMessageW, PostQuitMessage, RegisterClassExW, RegisterWindowMessageW, SW_HIDE,
    SWP_NOACTIVATE, SWP_SHOWWINDOW, SetWindowPos, SetWindowsHookExW, ShowWindow, TranslateMessage,
    UnhookWindowsHookEx, WH_KEYBOARD_LL, WINEVENT_OUTOFCONTEXT, WM_APP, WM_CLOSE, WM_CONTEXTMENU,
    WM_DESTROY, WM_DPICHANGED, WM_ERASEBKGND, WM_HOTKEY, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEACTIVATE, WM_PAINT, WM_RBUTTONUP, WM_SYSKEYDOWN, WM_USER, WNDCLASSEXW,
};
use windows::core::w;

use crate::caret;
use crate::compose::Composer;
use crate::config::{Config, InsertMode};
use crate::engine::{Engine, Event};
use crate::focus;
use crate::hotkey::{Hotkey, MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN};
use crate::insert::{self, Out};
use crate::popup::{Popup, View};
use crate::settings;
use crate::tray::{self, Tray};
use crate::win::{error_box, instance};

const WM_APP_TRAY: u32 = WM_APP + 1;
const WM_APP_ENGINE: u32 = WM_APP + 2;
const WM_APP_TOGGLE: u32 = WM_APP + 3;
const WM_APP_FLUSH: u32 = WM_APP + 4;
const WM_APP_FOREGROUND: u32 = WM_APP + 5;
/// Opens Settings; sent by a second launch of ezhuthu.exe (see main.rs).
pub const WM_APP_SETTINGS: u32 = WM_APP + 6;
const NIN_SELECT: u32 = WM_USER;
const HOTKEY_ID: i32 = 1;
/// Tamil candidates shown at most (the popup adds a row for English).
const MAX_ROWS: usize = 9;
/// Words kept in the candidate cache before it is cleared.
const MAX_CACHE: usize = 2000;
/// Give up waiting for our injected events after this long (e.g. UIPI
/// silently dropped them), so real keys are never held back for good.
const IN_FLIGHT_TIMEOUT: Duration = Duration::from_millis(500);

const HINT: &str = "↑↓ or 1–5 choose · Space / Enter insert · Esc discard";

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
    hotkey: Option<Hotkey>,
    engine: Engine,
    xlit: Option<Arc<Transliterator>>,
    status: Status,
    cache: HashMap<String, Vec<String>>,
    pending: HashSet<String>,
    /// The word (cache key) whose candidates the popup shows; see `shown`.
    shown_key: Option<String>,
    /// Tamil mode: the keyboard hook is installed.
    on: bool,
    kb_hook: Option<HHOOK>,
    fg_hook: Option<HWINEVENTHOOK>,
    composer: Composer,
    visible: bool,
    /// Keys whose key-down we swallowed; their key-up is swallowed too.
    swallowed: HashSet<u16>,
    /// Output waiting to be injected, in order.
    outbox: Vec<Out>,
    flush_posted: bool,
    /// Our injected events that haven't come back through the hook yet.
    in_flight: usize,
    in_flight_since: Instant,
    /// One-off message shown in the hint line until the next key.
    flash: Option<String>,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
    static TASKBAR_CREATED: Cell<u32> = const { Cell::new(0) };
}

fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|cell| match cell.try_borrow_mut() {
        Ok(mut app) => app.as_mut().map(f),
        Err(_) => None,
    })
}

fn post(hwnd: HWND, msg: u32) {
    // SAFETY: posting to our own window.
    let _ = unsafe { PostMessageW(Some(hwnd), msg, WPARAM(0), LPARAM(0)) };
}

// ---------------------------------------------------------------- startup

pub fn run() -> Result<()> {
    let config_path = Config::path()?;
    let (config, config_error) = match Config::load_or_create(&config_path) {
        Ok(c) => (c, None),
        Err(e) => (Config::default(), Some(format!("{e:#}"))),
    };

    let main = create_main_window()?;
    let popup = Popup::create(Some(popup_proc), popup_rows(config.beam))?;
    let tray = Tray::add(main, WM_APP_TRAY, &tooltip(&config, false))?;
    // SAFETY: the string is a valid NUL-terminated literal.
    TASKBAR_CREATED.set(unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) });

    focus::start();
    let engine = Engine::start(config.assets_dir(), config.beam, main, WM_APP_ENGINE);
    let mut app = App {
        main,
        popup,
        tray,
        config,
        config_path,
        hotkey: None,
        engine,
        xlit: None,
        status: Status::Loading,
        cache: HashMap::new(),
        pending: HashSet::new(),
        shown_key: None,
        on: false,
        kb_hook: None,
        fg_hook: None,
        composer: Composer::default(),
        visible: false,
        swallowed: HashSet::new(),
        outbox: Vec::new(),
        flush_posted: false,
        in_flight: 0,
        in_flight_since: Instant::now(),
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
            // Tab, Enter and Esc in the Settings window behave like a dialog.
            if settings::hwnd().is_some_and(|h| IsDialogMessageW(h, &msg).as_bool()) {
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    APP.with(|cell| cell.borrow_mut().take());
    Ok(())
}

/// Popup rows for `beam` suggestions: the Tamil ones that fit, plus English.
fn popup_rows(beam: usize) -> usize {
    beam.min(MAX_ROWS) + 1
}

fn tooltip(config: &Config, on: bool) -> String {
    let state = if on {
        "Tamil mode ON"
    } else {
        "Tamil mode off"
    };
    format!("Ezhuthu: {state} ({} to toggle)", config.hotkey)
}

fn create_main_window() -> Result<HWND> {
    let class = w!("ezhuthu.main");
    // SAFETY: the class struct is fully initialised. The window is never
    // shown; it receives the hotkey, tray, engine and flush messages.
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

// ---------------------------------------------------------------- app logic

/// A key event as seen by the low-level hook.
#[derive(Clone, Copy)]
struct KeyEvent {
    vk: u16,
    scan: u32,
    down: bool,
    extended: bool,
    ours: bool,
}

impl App {
    fn register_hotkey(&mut self) {
        // SAFETY: unregistering an id that isn't registered is harmless.
        let _ = unsafe { UnregisterHotKey(Some(self.main), HOTKEY_ID) };
        self.hotkey = None;
        let hotkey = match Hotkey::parse(&self.config.hotkey) {
            Ok(h) => h,
            Err(e) => {
                self.tray
                    .notify("Invalid hotkey in settings", &format!("{e:#}"), true);
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
        self.hotkey = Some(hotkey);
    }

    fn set_mode(&mut self, on: bool) {
        if on == self.on {
            return;
        }
        self.on = on;
        // Events still on their way back would never be counted once the
        // hook is gone; start each mode change with a clean slate.
        self.in_flight = 0;
        if on {
            // SAFETY: installing our own hooks; removed in the `else` branch.
            unsafe {
                self.kb_hook =
                    SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), Some(instance()), 0)
                        .ok();
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
                self.on = false;
                self.tray.notify(
                    "Couldn't start Tamil mode",
                    "Windows refused the keyboard hook.",
                    true,
                );
            } else if let Status::Failed(e) = &self.status {
                let e = e.clone();
                self.tray.notify("The model isn't loaded", &e, true);
            }
            // SAFETY: plain query.
            let fg = unsafe { GetForegroundWindow() };
            if !fg.0.is_null() && insert::blocked_by_uipi(fg) {
                self.tray.notify(
                    "This window runs as administrator",
                    "Windows blocks Tamil typing into it; other windows work.",
                    true,
                );
            }
        } else {
            self.discard();
            // SAFETY: unhooking our own hooks.
            unsafe {
                if let Some(h) = self.kb_hook.take() {
                    let _ = UnhookWindowsHookEx(h);
                }
                if let Some(h) = self.fg_hook.take() {
                    let _ = UnhookWinEvent(h);
                }
            }
            self.swallowed.clear();
            // Forget what was typed this session.
            self.cache.clear();
        }
        let tip = tooltip(&self.config, self.on);
        self.tray.set_state(self.on, &tip);
    }

    /// Reloads the model with a new number of suggestions (beam size).
    fn restart_engine(&mut self, beam: usize) {
        self.discard();
        // Dropping the old engine ends its thread once its queue is empty.
        self.engine = Engine::start(self.config.assets_dir(), beam, self.main, WM_APP_ENGINE);
        self.xlit = None;
        self.status = Status::Loading;
        self.cache.clear();
        self.pending.clear();
        self.popup.set_rows(popup_rows(beam));
    }

    /// The popup's options once the word's Tamil candidates are known: those
    /// that fit, then the word as typed, for English without leaving Tamil mode.
    fn candidates(&self) -> Vec<String> {
        self.options(self.composer.key().as_deref())
    }

    /// What the popup shows: the current options, or until they arrive the
    /// last ones shown for this word, so the rows don't blank out (flicker)
    /// on every letter.
    fn shown(&self) -> Vec<String> {
        match self.candidates() {
            list if list.is_empty() => self.options(self.shown_key.as_deref()),
            list => list,
        }
    }

    /// The Tamil candidates of `key` that fit, then the current word as typed.
    fn options(&self, key: Option<&str>) -> Vec<String> {
        let Some(tamil) = key.and_then(|k| self.cache.get(k)) else {
            return Vec::new();
        };
        let mut list: Vec<String> = tamil.iter().take(self.rows() - 1).cloned().collect();
        list.push(self.composer.core().to_string());
        list
    }

    /// Once the current word's candidates are known, they are the ones on show.
    fn note_shown(&mut self) {
        if let Some(key) = self.composer.key()
            && self.cache.contains_key(&key)
        {
            self.shown_key = Some(key);
        }
    }

    /// Fills in the current word's candidates now if the engine hasn't yet
    /// (~40 ms), for a key that commits before it answers.
    fn ensure_candidates(&mut self) {
        if let (Some(key), Some(xlit)) = (self.composer.key(), &self.xlit)
            && !self.cache.contains_key(&key)
        {
            let cands = xlit
                .transliterate(&key)
                .map(|c| c.into_iter().map(|c| c.text).collect())
                .unwrap_or_default();
            self.cache.insert(key, cands);
        }
    }

    fn rows(&self) -> usize {
        self.popup.rows()
    }

    /// Requests the current word from the engine unless cached or pending.
    fn request(&mut self) {
        if let Some(key) = self.composer.key()
            && !self.cache.contains_key(&key)
            && self.pending.insert(key.clone())
        {
            self.engine.request(key);
        }
    }

    fn after_edit(&mut self) {
        if self.composer.is_empty() {
            self.hide_popup();
        } else {
            self.request();
            self.note_shown();
            self.invalidate();
        }
    }

    /// First letter of a word: open the popup at the caret.
    fn start_word(&mut self, c: char) {
        // SAFETY: plain query.
        let anchor = caret::find(unsafe { GetForegroundWindow() });
        self.composer.clear();
        self.composer.push(c);
        self.shown_key = None;
        self.request();
        self.note_shown();
        self.popup.set_dpi(caret::dpi_at(anchor));
        let (w, h) = self.popup.size();
        let (x, y) = caret::place(anchor, w, h, self.popup.gap());
        self.visible = true;
        // SAFETY: our own window; SWP_NOACTIVATE keeps focus in the app.
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
        }
        self.invalidate();
    }

    fn hide_popup(&mut self) {
        if self.visible {
            self.visible = false;
            // SAFETY: hiding our own window.
            let _ = unsafe { ShowWindow(self.popup.hwnd, SW_HIDE) };
        }
    }

    fn invalidate(&self) {
        if self.visible {
            // SAFETY: our own window; painting happens later via WM_PAINT.
            let _ = unsafe { InvalidateRect(Some(self.popup.hwnd), None, false) };
        }
    }

    fn discard(&mut self) {
        self.composer.clear();
        self.shown_key = None;
        self.flash = None;
        self.hide_popup();
    }

    /// Commits the current word with its selected candidate plus `suffix`.
    fn commit(&mut self, suffix: &str) {
        if self.composer.is_empty() {
            return;
        }
        self.ensure_candidates();
        let text = self.composer.render(Some(&self.candidates())) + suffix;
        self.discard();
        self.emit(Out::Text(text));
    }

    fn pick(&mut self, index: usize) -> bool {
        self.ensure_candidates();
        if index >= self.candidates().len() {
            return false;
        }
        self.composer.select(index);
        self.commit(" ");
        true
    }

    fn emit(&mut self, out: Out) {
        self.outbox.push(out);
        if !self.flush_posted && !self.waiting() {
            self.flush_posted = true;
            post(self.main, WM_APP_FLUSH);
        }
    }

    /// True while injected events are still travelling back through the hook.
    fn waiting(&mut self) -> bool {
        if self.in_flight > 0 && self.in_flight_since.elapsed() > IN_FLIGHT_TIMEOUT {
            self.in_flight = 0;
        }
        self.in_flight > 0
    }

    /// The hook's verdict for one key event: true = swallow it.
    fn on_hook(&mut self, ev: KeyEvent) -> bool {
        if ev.ours {
            self.in_flight = self.in_flight.saturating_sub(1);
            if self.in_flight == 0 && !self.outbox.is_empty() && !self.flush_posted {
                self.flush_posted = true;
                post(self.main, WM_APP_FLUSH);
            }
            return false;
        }
        if !self.on || is_modifier(ev.vk) {
            return false;
        }
        // Keys for our own Settings window (e.g. a new hotkey) go straight to it.
        // SAFETY: plain query.
        if settings::hwnd().is_some_and(|h| unsafe { GetForegroundWindow() } == h) {
            return false;
        }
        let swallow = if ev.down {
            let s = self.on_key_down(ev);
            if s {
                self.swallowed.insert(ev.vk);
            }
            s
        } else {
            self.swallowed.remove(&ev.vk)
        };
        if swallow {
            return true;
        }
        // Passing through: keep it behind any output we haven't delivered.
        if !self.outbox.is_empty() || self.waiting() {
            self.emit(Out::Key {
                vk: ev.vk,
                scan: ev.scan as u16,
                up: !ev.down,
                extended: ev.extended,
            });
            return true;
        }
        false
    }

    fn on_key_down(&mut self, ev: KeyEvent) -> bool {
        let (ctrl, alt, shift) = (down(VK_CONTROL.0), down(VK_MENU.0), down(VK_SHIFT.0));
        let win = down(VK_LWIN.0) || down(VK_RWIN.0);
        if let Some(h) = self.hotkey {
            let mods = (ctrl as u32 * MOD_CONTROL)
                | (alt as u32 * MOD_ALT)
                | (shift as u32 * MOD_SHIFT)
                | (win as u32 * MOD_WIN);
            if ev.vk as u32 == h.vk && mods == h.modifiers {
                post(self.main, WM_APP_TOGGLE);
                return true;
            }
        }
        if focus::in_password_field() {
            // Password field: never compose, show or cache; keys type as-is.
            self.discard();
            return false;
        }
        let composing = !self.composer.is_empty();
        self.flash = None;
        if alt || win {
            // Alt+Tab, Win+D and friends: drop the word, let the key through.
            self.discard();
            return false;
        }
        if ctrl {
            if !composing {
                return false;
            }
            match ev.vk {
                v if v == VK_BACK.0 => self.discard(),
                v if v == b'R' as u16 => {
                    self.flash = Some("Refine (Ctrl+R) is not available yet".into());
                    self.invalidate();
                }
                // Ctrl+S, Ctrl+V…: insert the word, then let the shortcut act.
                _ => {
                    self.commit("");
                    return false;
                }
            }
            return true;
        }
        match ev.vk {
            v if v == VK_ESCAPE.0 && composing => self.discard(),
            v if v == VK_RETURN.0 && composing => self.commit(""),
            v if v == VK_SPACE.0 && composing => self.commit(" "),
            v if v == VK_BACK.0 && composing => {
                self.composer.pop();
                self.after_edit();
            }
            v if (v == VK_UP.0 || v == VK_DOWN.0) && composing => {
                let n = self.shown().len();
                self.composer
                    .move_selection(if v == VK_UP.0 { -1 } else { 1 }, n);
                self.invalidate();
            }
            _ => {
                // Control characters (Tab, Esc, Enter…) count as "not text".
                let ch = key_char(ev.vk as u32, ev.scan).filter(|c| !c.is_control());
                match (composing, ch) {
                    (true, Some(c)) => {
                        // 1–9 pick rows 1–9, and 0 the tenth.
                        let picked = c
                            .to_digit(10)
                            .is_some_and(|d| self.pick((d as usize + 9) % 10));
                        if !picked {
                            self.composer.push(c);
                            self.after_edit();
                        }
                    }
                    (false, Some(c)) if c.is_ascii_alphabetic() => self.start_word(c),
                    // Mid-word Tab, arrows, Delete…: insert the word, then the key.
                    (true, None) => {
                        self.commit("");
                        return false;
                    }
                    (false, _) => return false,
                }
            }
        }
        true
    }

    fn view(&self) -> View {
        let candidates = self.shown();
        let (hint, warn) = if let Some(f) = &self.flash {
            (f.clone(), true)
        } else {
            match &self.status {
                Status::Loading => ("Loading the model…".to_string(), false),
                Status::Failed(e) => (format!("Model failed to load: {e}"), true),
                Status::Ready => {
                    let keys = match self.rows() {
                        n @ ..=9 => format!("1–{n}"),
                        _ => "1–9, 0".to_string(),
                    };
                    (HINT.replace("1–5", &keys), false)
                }
            }
        };
        View {
            input: self.composer.text().to_string(),
            preview: self.composer.render(Some(&candidates)),
            english: candidates.len().checked_sub(1),
            candidates,
            selected: self.composer.selected(),
            hint,
            warn,
        }
    }
}

// ---------------------------------------------------------------- handlers

fn toggle() {
    with_app(|a| {
        let on = !a.on;
        a.set_mode(on);
    });
}

/// Injects queued output. Runs from the message loop, never inside the hook.
fn flush() {
    let job = with_app(|a| {
        a.flush_posted = false;
        if a.waiting() || a.outbox.is_empty() {
            return None;
        }
        let items = std::mem::take(&mut a.outbox);
        let mode = a.config.insert_mode;
        // SendInput runs our hook re-entrantly, before it returns, so the
        // events must be counted as in flight before they are sent.
        let expected = insert::event_count(&items, mode);
        a.in_flight += expected;
        a.in_flight_since = Instant::now();
        Some((items, mode, expected))
    })
    .flatten();
    let Some((items, mode, expected)) = job else {
        return;
    };
    let (injected, result) = insert::deliver(&items, mode);
    with_app(|a| {
        // Events that were never sent will never come back.
        a.in_flight = a
            .in_flight
            .saturating_sub(expected.saturating_sub(injected));
        if a.in_flight == 0 && !a.outbox.is_empty() && !a.flush_posted {
            a.flush_posted = true;
            post(a.main, WM_APP_FLUSH);
        }
        if let Err(e) = result {
            let hint = if mode == InsertMode::Unicode {
                "\nIf this app mishandles typed text, set insert_mode = \"clipboard\" in the settings."
            } else {
                ""
            };
            a.tray
                .notify("Couldn't insert the text", &format!("{e:#}{hint}"), true);
        }
    });
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
                    // Bounded: typed words should not pile up in memory.
                    if a.cache.len() >= MAX_CACHE {
                        a.cache.clear();
                    }
                    a.cache.insert(key, candidates);
                }
            }
        }
        a.note_shown();
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

fn on_tray(event: u32) {
    if !matches!(
        event,
        WM_CONTEXTMENU | WM_RBUTTONUP | WM_LBUTTONUP | NIN_SELECT
    ) {
        return;
    }
    let Some((main, on, header)) = with_app(|a| (a.main, a.on, tooltip(&a.config, a.on))) else {
        return;
    };
    const TOGGLE: usize = 1;
    const SETTINGS: usize = 2;
    const QUIT: usize = 3;
    let toggle_label = if on {
        "Turn Tamil mode off"
    } else {
        "Turn Tamil mode on"
    };
    let items = [
        (0, header.as_str()),
        (0, "-"),
        (TOGGLE, toggle_label),
        (SETTINGS, "Settings..."),
        (0, "-"),
        (QUIT, "Quit"),
    ];
    // Modal loop: the app state must not be borrowed meanwhile.
    match tray::menu(main, &items, &[]) {
        TOGGLE => toggle(),
        SETTINGS => open_settings(),
        QUIT => quit(),
        _ => {}
    }
}

/// Opens the Settings window. While it is open the global hotkey is
/// unregistered (so pressing it records it in the hotkey box instead of
/// toggling Tamil mode) and the keyboard hook ignores keys meant for it.
fn open_settings() {
    if settings::hwnd().is_some() {
        settings::open(current_values(), apply_settings, settings_closed);
        return;
    }
    with_app(|a| {
        // SAFETY: our registration; re-registered in `settings_closed`.
        let _ = unsafe { UnregisterHotKey(Some(a.main), HOTKEY_ID) };
        a.discard();
    });
    settings::open(current_values(), apply_settings, settings_closed);
}

fn current_values() -> settings::Values {
    let autostart = ezhuthu_install::autostart_enabled();
    with_app(|a| settings::Values {
        hotkey: a.config.hotkey.clone(),
        insert_mode: a.config.insert_mode,
        beam: a.config.beam,
        autostart,
    })
    .unwrap_or(settings::Values {
        hotkey: Config::default().hotkey,
        insert_mode: InsertMode::Unicode,
        beam: ezhuthu_xlit::DEFAULT_BEAM,
        autostart,
    })
}

fn settings_closed() {
    with_app(|a| a.register_hotkey());
}

/// Settings window OK: validate, apply live and save.
fn apply_settings(v: &settings::Values) -> Result<(), String> {
    let hotkey = Hotkey::parse(&v.hotkey).map_err(|e| format!("{e:#}"))?;
    let result = with_app(|a| {
        if v.hotkey != a.config.hotkey {
            // The current hotkey is unregistered while Settings is open, so a
            // trial registration tells whether another app holds the new one.
            let mods = HOT_KEY_MODIFIERS(hotkey.modifiers) | MOD_NOREPEAT;
            // SAFETY: a trial registration, removed straight away.
            let free = unsafe { RegisterHotKey(Some(a.main), HOTKEY_ID, mods, hotkey.vk) }.is_ok();
            let _ = unsafe { UnregisterHotKey(Some(a.main), HOTKEY_ID) };
            if !free {
                return Err(format!(
                    "{} is already used by another app. Choose another.",
                    v.hotkey
                ));
            }
        }
        let exe = std::env::current_exe().map_err(|e| format!("can't find ezhuthu.exe: {e}"))?;
        ezhuthu_install::set_autostart(v.autostart.then_some(exe.as_path()))
            .map_err(|e| format!("Couldn't change Start with Windows: {e:#}"))?;

        let mut new = a.config.clone();
        new.hotkey = v.hotkey.clone();
        new.insert_mode = v.insert_mode;
        new.beam = v.beam;
        new.save(&a.config_path)
            .map_err(|e| format!("Couldn't save the settings: {e:#}"))?;
        if new.beam != a.config.beam {
            a.restart_engine(new.beam);
        }
        a.config = new;
        let tip = tooltip(&a.config, a.on);
        a.tray.set_state(a.on, &tip);
        Ok(())
    });
    result.unwrap_or_else(|| Err("Ezhuthu is busy; try again".into()))
}

fn quit() {
    let main = with_app(|a| {
        a.set_mode(false);
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
        WM_HOTKEY if wparam.0 as i32 == HOTKEY_ID => toggle(),
        WM_APP_TOGGLE => toggle(),
        WM_APP_FLUSH => flush(),
        WM_APP_TRAY => on_tray((lparam.0 & 0xFFFF) as u32),
        WM_APP_ENGINE => on_engine_events(),
        // The installer asks a running copy to quit this way.
        WM_CLOSE => quit(),
        WM_APP_SETTINGS => open_settings(),
        WM_APP_FOREGROUND => {
            with_app(|a| a.discard());
        }
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
        WM_DPICHANGED => {} // sized explicitly each time a word starts
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
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            with_app(|a| {
                if let Some(row) = a.popup.row_at(x, y) {
                    a.pick(row);
                }
            });
        }
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

unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        // SAFETY: for WH_KEYBOARD_LL with code >= 0, lparam points to a KBDLLHOOKSTRUCT.
        let kb = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let ev = KeyEvent {
            vk: kb.vkCode as u16,
            scan: kb.scanCode,
            down: wparam.0 as u32 == WM_KEYDOWN || wparam.0 as u32 == WM_SYSKEYDOWN,
            extended: kb.flags & LLKHF_EXTENDED == LLKHF_EXTENDED,
            ours: kb.dwExtraInfo == insert::MARK,
        };
        // If the state is busy (re-entrant call), let the key through.
        if with_app(|a| a.on_hook(ev)) == Some(true) {
            return LRESULT(1);
        }
    }
    // SAFETY: forwarding to the rest of the hook chain.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// Another window came to the foreground (click, Alt+Tab): drop the word.
unsafe extern "system" fn foreground_changed(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    _id_object: i32,
    _id_child: i32,
    _thread: u32,
    _time: u32,
) {
    let main = with_app(|a| (!a.composer.is_empty() && hwnd != a.popup.hwnd).then_some(a.main));
    if let Some(Some(main)) = main {
        post(main, WM_APP_FOREGROUND);
    }
}

/// Shows a fatal startup error (no console in release builds).
pub fn report_fatal(e: &anyhow::Error) {
    error_box(None, &format!("Ezhuthu could not start:\n\n{e:#}"));
}
