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
    RegisterHotKey, ToUnicodeEx, UnregisterHotKey, VK_BACK, VK_CAPITAL, VK_CONTROL, VK_DELETE,
    VK_DOWN, VK_END, VK_ESCAPE, VK_HOME, VK_LCONTROL, VK_LEFT, VK_LMENU, VK_LSHIFT, VK_LWIN,
    VK_MENU, VK_RCONTROL, VK_RETURN, VK_RIGHT, VK_RMENU, VK_RSHIFT, VK_RWIN, VK_SHIFT, VK_SNAPSHOT,
    VK_SPACE, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    EVENT_SYSTEM_FOREGROUND, GetForegroundWindow, GetMessageW, GetWindowThreadProcessId, HHOOK,
    HWND_TOPMOST, IsDialogMessageW, KBDLLHOOKSTRUCT, KillTimer, LLKHF_EXTENDED, MA_NOACTIVATE, MSG,
    PostMessageW, PostQuitMessage, RegisterClassExW, RegisterWindowMessageW, SW_HIDE,
    SWP_NOACTIVATE, SWP_NOZORDER, SWP_SHOWWINDOW, SetTimer, SetWindowPos, SetWindowsHookExW,
    ShowWindow, TranslateMessage, UnhookWindowsHookEx, WH_KEYBOARD_LL, WINEVENT_OUTOFCONTEXT,
    WM_APP, WM_CLOSE, WM_CONTEXTMENU, WM_DESTROY, WM_DPICHANGED, WM_ERASEBKGND, WM_HOTKEY,
    WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_PAINT, WM_RBUTTONUP,
    WM_SYSKEYDOWN, WM_TIMER, WM_USER, WNDCLASSEXW,
};
use windows::core::w;

use crate::badge::Badge;
use crate::caret;
use crate::compose::Composer;
use crate::config::{Config, InsertMode};
use crate::convert;
use crate::engine::{Engine, Event, PREDICT_FROM, suggest};
use crate::focus;
use crate::history::History;
use crate::hotkey::{Hotkey, MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN};
use crate::insert::{self, Out, SavedClipboard};
use crate::learned::Learned;
use crate::popup::{Popup, View};
use crate::settings;
use crate::tray::{self, Tray};
use crate::welcome;
use crate::win::{error_box, instance};

const WM_APP_TRAY: u32 = WM_APP + 1;
const WM_APP_ENGINE: u32 = WM_APP + 2;
const WM_APP_TOGGLE: u32 = WM_APP + 3;
const WM_APP_FLUSH: u32 = WM_APP + 4;
const WM_APP_FOREGROUND: u32 = WM_APP + 5;
/// Opens Settings; sent by a second launch of ezhuthu.exe (see main.rs).
pub const WM_APP_SETTINGS: u32 = WM_APP + 6;
/// Saves the learned words after a pick changed them.
const WM_APP_SAVE_LEARNED: u32 = WM_APP + 7;
/// The convert-selection shortcut, caught by the hook in Tamil mode.
const WM_APP_CONVERT: u32 = WM_APP + 8;
const CONVERT_HOTKEY_ID: i32 = 2;
/// Polls convert selection's waits (keys released, clipboard changed).
const CONVERT_TIMER: usize = 1;
const CONVERT_TICK_MS: u32 = 30;
/// Animates convert selection's progress badge (about 60 frames a second).
const BADGE_TIMER: usize = 2;
const BADGE_TICK_MS: u32 = 16;
/// Give up on convert selection if the shortcut's keys are still held, or
/// nothing reaches the clipboard, after this long.
const RELEASE_TIMEOUT: Duration = Duration::from_secs(3);
const COPY_TIMEOUT: Duration = Duration::from_millis(1500);
/// After Print Screen mid-word, the first window to take the focus within
/// this long is taken to be the screen capture tool (see `Capture`).
const CAPTURE_GRACE: Duration = Duration::from_secs(3);
const NIN_SELECT: u32 = WM_USER;
const HOTKEY_ID: i32 = 1;
/// Tamil candidates shown at most (the popup adds a row for English).
const MAX_ROWS: usize = 9;
/// Rows at most: the number keys pick 1–9 and 0.
const MAX_OPTIONS: usize = 10;
/// Whole-word predictions shown at most.
const MAX_PREDICTIONS: usize = 3;
/// Words kept in the candidate cache before it is cleared.
const MAX_CACHE: usize = 2000;
/// Give up waiting for our injected events after this long (e.g. UIPI
/// silently dropped them), so real keys are never held back for good.
const IN_FLIGHT_TIMEOUT: Duration = Duration::from_millis(500);

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
    /// Dictionary predictions per word (cleared with `cache`).
    predicted: HashMap<String, Vec<String>>,
    pending: HashSet<String>,
    /// The word (cache key) whose candidates the popup shows; see `shown`.
    shown_key: Option<String>,
    /// Picks remembered across sessions, and their file (None: not saved).
    learned: Learned,
    learned_path: Option<PathBuf>,
    learned_dirty: bool,
    /// Words inserted, for predictions, and their file (None: not saved).
    history: History,
    history_path: Option<PathBuf>,
    history_dirty: bool,
    /// Tamil mode: the keyboard hook is installed.
    on: bool,
    kb_hook: Option<HHOOK>,
    fg_hook: Option<HWINEVENTHOOK>,
    composer: Composer,
    visible: bool,
    /// Where the popup was placed (its caret) and its current size, so it can
    /// grow or shrink with the list.
    anchor: Option<caret::Anchor>,
    popup_size: (i32, i32),
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
    convert_hotkey: Option<Hotkey>,
    converting: Option<Converting>,
    capture: Option<Capture>,
    badge: Badge,
}

/// Print Screen was pressed mid-word: the word and popup stay while a screen
/// capture tool (e.g. the Snipping Tool overlay) has the focus, so they show
/// in the screenshot, and typing resumes when the focus returns to `target`.
struct Capture {
    target: HWND,
    since: Instant,
    /// The process of the first window that took the focus afterwards.
    tool: Option<u32>,
}

fn process_of(hwnd: HWND) -> u32 {
    let mut pid = 0;
    // SAFETY: plain query.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid
}

/// The popup's list: `tamil` suggestions, then `predicted` whole words,
/// then the word as typed (English).
#[derive(Debug, Default)]
struct Options {
    words: Vec<String>,
    tamil: usize,
    predicted: usize,
}

impl Options {
    fn len(&self) -> usize {
        self.words.len()
    }

    fn is_empty(&self) -> bool {
        self.words.is_empty()
    }
}

/// A convert selection in progress, in `target` (the window it started in;
/// if another window comes to the front it is cancelled).
struct Converting {
    target: HWND,
    stage: Stage,
}

enum Stage {
    /// Waiting for the user to let go of the shortcut's keys, so our Ctrl+C
    /// isn't read as Ctrl+Alt+C.
    Release { since: Instant },
    /// Ctrl+C sent; waiting for the selection to reach the clipboard.
    Copy {
        since: Instant,
        sequence: u32,
        saved: SavedClipboard,
    },
    /// Waiting for the engine to transliterate the words.
    Words { text: String, keys: Vec<String> },
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
    let badge = Badge::create()?;
    let tray = Tray::add(main, WM_APP_TRAY, &tooltip(&config, false))?;
    // SAFETY: the string is a valid NUL-terminated literal.
    TASKBAR_CREATED.set(unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) });

    // An unreadable file is reported but not overwritten: picks are then
    // only kept for this session.
    let learned_path = Learned::path().ok();
    let learned = match learned_path.as_deref().map(Learned::load) {
        Some(Ok(l)) => Some(l),
        Some(Err(e)) => {
            eprintln!("learned words not loaded: {e:#}");
            None
        }
        None => None,
    };
    let learned_path = learned_path.filter(|_| learned.is_some());
    let learned = learned.unwrap_or_default();
    let history_path = History::path().ok();
    let history = match history_path.as_deref().map(History::load) {
        Some(Ok(h)) => Some(h),
        Some(Err(e)) => {
            eprintln!("typing history not loaded: {e:#}");
            None
        }
        None => None,
    };
    let history_path = history_path.filter(|_| history.is_some());
    let history = history.unwrap_or_default();

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
        predicted: HashMap::new(),
        pending: HashSet::new(),
        shown_key: None,
        learned,
        learned_path,
        learned_dirty: false,
        history,
        history_path,
        history_dirty: false,
        on: false,
        kb_hook: None,
        fg_hook: None,
        composer: Composer::default(),
        visible: false,
        anchor: None,
        popup_size: (0, 0),
        swallowed: HashSet::new(),
        outbox: Vec::new(),
        flush_posted: false,
        in_flight: 0,
        in_flight_since: Instant::now(),
        flash: None,
        convert_hotkey: None,
        converting: None,
        capture: None,
        badge,
    };
    if let Some(e) = config_error {
        app.tray
            .notify("Settings file has an error (using defaults)", &e, true);
    }
    app.register_hotkeys();
    let first_run = !app.config.welcome_shown;
    APP.with(|cell| *cell.borrow_mut() = Some(app));
    if first_run {
        open_welcome();
    }

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
    /// Registers both shortcuts: Tamil mode's and convert selection's.
    fn register_hotkeys(&mut self) {
        self.register_hotkey();
        self.register_convert_hotkey();
    }

    fn register_convert_hotkey(&mut self) {
        // SAFETY: unregistering an id that isn't registered is harmless.
        let _ = unsafe { UnregisterHotKey(Some(self.main), CONVERT_HOTKEY_ID) };
        self.convert_hotkey = None;
        if self.config.convert_hotkey.is_empty() {
            return;
        }
        let hotkey = match Hotkey::parse(&self.config.convert_hotkey) {
            Ok(h) => h,
            Err(e) => {
                self.tray.notify(
                    "Invalid convert shortcut in settings",
                    &format!("{e:#}"),
                    true,
                );
                return;
            }
        };
        let mods = HOT_KEY_MODIFIERS(hotkey.modifiers) | MOD_NOREPEAT;
        // SAFETY: plain registration for our window.
        let registered =
            unsafe { RegisterHotKey(Some(self.main), CONVERT_HOTKEY_ID, mods, hotkey.vk) };
        if registered.is_err() {
            self.tray.notify(
                "Convert shortcut unavailable",
                &format!(
                    "{} is used by another app. Change it in Settings.",
                    self.config.convert_hotkey
                ),
                true,
            );
        }
        self.convert_hotkey = Some(hotkey);
    }

    // ------------------------------------------------------ convert selection

    /// The convert shortcut was pressed: start copying the selection once
    /// its keys are released.
    fn start_convert(&mut self) {
        if self.converting.is_some() {
            return;
        }
        match &self.status {
            Status::Ready => {}
            Status::Loading => {
                self.tray.notify(
                    "Ezhuthu",
                    "The model is still loading; try again in a moment.",
                    false,
                );
                return;
            }
            Status::Failed(e) => {
                let e = e.clone();
                self.tray.notify("The model isn't loaded", &e, true);
                return;
            }
        }
        // SAFETY: plain query.
        let target = unsafe { GetForegroundWindow() };
        if target.0.is_null() || settings::hwnd() == Some(target) || focus::in_password_field() {
            return;
        }
        if insert::blocked_by_uipi(target) {
            self.tray.notify(
                "This window runs as administrator",
                "Windows blocks Ezhuthu from changing its text; other windows work.",
                true,
            );
            return;
        }
        self.discard();
        self.converting = Some(Converting {
            target,
            stage: Stage::Release {
                since: Instant::now(),
            },
        });
        self.badge.start(target);
        // SAFETY: timers on our own window; the convert timer is killed in
        // `end_convert_wait`, the badge timer once the badge has faded out.
        unsafe {
            SetTimer(Some(self.main), CONVERT_TIMER, CONVERT_TICK_MS, None);
            SetTimer(Some(self.main), BADGE_TIMER, BADGE_TICK_MS, None);
        }
    }

    fn end_convert_wait(&self) {
        // SAFETY: killing our own timer (harmless if it isn't running).
        let _ = unsafe { KillTimer(Some(self.main), CONVERT_TIMER) };
    }

    /// Stops convert selection, telling the user why (if `why` is given).
    fn cancel_convert(&mut self, why: Option<&str>) {
        self.end_convert_wait();
        if let Some(Converting {
            stage: Stage::Copy { saved, .. },
            ..
        }) = self.converting.take()
        {
            let _ = insert::restore_clipboard(&saved);
        }
        if let Some(why) = why {
            self.tray.notify("Convert selection", why, false);
        }
    }

    /// Timer tick: advances the waits of convert selection.
    fn convert_tick(&mut self) {
        let Some(job) = self.converting.take() else {
            self.end_convert_wait();
            return;
        };
        // SAFETY: plain query.
        if unsafe { GetForegroundWindow() } != job.target {
            self.converting = Some(job);
            self.cancel_convert(Some("Cancelled: another window came to the front."));
            return;
        }
        let target = job.target;
        match job.stage {
            Stage::Release { since } => {
                let held = [VK_CONTROL, VK_MENU, VK_SHIFT, VK_LWIN, VK_RWIN]
                    .iter()
                    .any(|k| down(k.0));
                if held {
                    if since.elapsed() < RELEASE_TIMEOUT {
                        self.converting = Some(Converting {
                            target,
                            stage: Stage::Release { since },
                        });
                    } else {
                        self.end_convert_wait();
                    }
                    return;
                }
                let saved = match insert::save_clipboard() {
                    Ok(saved) => saved,
                    Err(e) => {
                        self.end_convert_wait();
                        self.tray
                            .notify("Convert selection", &format!("{e:#}"), true);
                        return;
                    }
                };
                let sequence = insert::clipboard_sequence();
                self.emit(Out::Copy);
                self.converting = Some(Converting {
                    target,
                    stage: Stage::Copy {
                        since: Instant::now(),
                        sequence,
                        saved,
                    },
                });
            }
            Stage::Copy {
                since,
                sequence,
                saved,
            } => {
                // Some apps empty the clipboard before writing to it, so wait
                // for text, not just for a change.
                let text = if insert::clipboard_sequence() != sequence {
                    insert::clipboard_text().ok().flatten()
                } else {
                    None
                };
                let Some(text) = text else {
                    self.converting = Some(Converting {
                        target,
                        stage: Stage::Copy {
                            since,
                            sequence,
                            saved,
                        },
                    });
                    if since.elapsed() > COPY_TIMEOUT {
                        self.cancel_convert(Some(
                            "Nothing was copied. Select some text first, then press the shortcut.",
                        ));
                    }
                    return;
                };
                self.end_convert_wait();
                if let Err(e) = insert::restore_clipboard(&saved) {
                    eprintln!("clipboard not restored: {e:#}");
                }
                self.convert_text(target, text);
            }
            // Driven by engine events, not the timer.
            stage @ Stage::Words { .. } => {
                self.end_convert_wait();
                self.converting = Some(Converting { target, stage });
            }
        }
    }

    /// The selection was copied: get its words transliterated.
    fn convert_text(&mut self, target: HWND, text: String) {
        let keys = convert::keys(&text);
        let why = if keys.is_empty() {
            Some("The selection has no English letters to convert.".to_string())
        } else if text.chars().count() > convert::MAX_CHARS {
            Some(format!(
                "Select less text: up to {} characters at a time.",
                convert::MAX_CHARS
            ))
        } else if keys.len() > convert::MAX_WORDS {
            Some(format!(
                "Select less text: up to {} different words at a time.",
                convert::MAX_WORDS
            ))
        } else {
            None
        };
        if let Some(why) = why {
            self.tray.notify("Convert selection", &why, false);
            return;
        }
        let missing = keys.iter().filter(|k| !self.cache.contains_key(*k)).count();
        if missing > 30 {
            self.tray.notify(
                "Convert selection",
                &format!("Converting {missing} words…"),
                false,
            );
        }
        self.converting = Some(Converting {
            target,
            stage: Stage::Words { text, keys },
        });
        self.convert_ready();
    }

    /// Pastes the converted text once every word is transliterated
    /// (re-requesting any the cache lost meanwhile).
    fn convert_ready(&mut self) {
        let Some(Converting {
            stage: Stage::Words { keys, .. },
            ..
        }) = &self.converting
        else {
            return;
        };
        let missing: Vec<String> = keys
            .iter()
            .filter(|k| !self.cache.contains_key(*k))
            .cloned()
            .collect();
        if !missing.is_empty() {
            for key in missing {
                self.request_key(key);
            }
            return;
        }
        let Some(Converting {
            target,
            stage: Stage::Words { text, .. },
        }) = self.converting.take()
        else {
            return;
        };
        // SAFETY: plain query.
        if unsafe { GetForegroundWindow() } != target {
            self.tray.notify(
                "Convert selection",
                "Cancelled: another window came to the front.",
                false,
            );
            return;
        }
        let tamil = convert::convert(&text, |key| {
            let mut list = self.cache.get(key)?.clone();
            if self.config.remember_picks {
                self.learned.apply(key, &mut list);
            }
            list.into_iter().next()
        });
        self.emit(Out::Paste(tamil));
        self.badge.done();
    }

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
            self.predicted.clear();
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
        self.predicted.clear();
        self.pending.clear();
        self.popup.set_rows(popup_rows(beam));
        self.cancel_convert(None);
    }

    /// The popup's options once the word's Tamil candidates are known: those
    /// that fit, then the word as typed, for English without leaving Tamil mode.
    fn candidates(&self) -> Options {
        self.options(self.composer.key().as_deref())
    }

    /// What the popup shows: the current options, or until they arrive the
    /// last ones shown for this word, so the rows don't blank out (flicker)
    /// on every letter.
    fn shown(&self) -> Options {
        match self.candidates() {
            list if list.is_empty() => self.options(self.shown_key.as_deref()),
            list => list,
        }
    }

    /// The Tamil candidates of `key` that fit, then whole words it may
    /// become (from `PREDICT_FROM` letters on), then the current word as
    /// typed; at most `MAX_OPTIONS` (the number keys reach 10).
    fn options(&self, key: Option<&str>) -> Options {
        let (Some(key), Some(tamil)) = (key, key.and_then(|k| self.cache.get(k))) else {
            return Options::default();
        };
        let mut words = tamil.clone();
        if self.config.remember_picks {
            self.learned.apply(key, &mut words);
        }
        words.truncate(self.tamil_rows());
        let tamil = words.len();
        let room = MAX_OPTIONS.saturating_sub(tamil + 1).min(MAX_PREDICTIONS);
        let mut predicted = 0;
        if self.config.predict && key.len() >= PREDICT_FROM && room > 0 {
            // Words typed before come first, then the dictionary's.
            let mut more = self.history.predict(key, &words, room);
            for word in self.predicted.get(key).into_iter().flatten() {
                if more.len() < room && !words.contains(word) && !more.contains(word) {
                    more.push(word.clone());
                }
            }
            predicted = more.len();
            words.extend(more);
        }
        words.push(self.composer.core().to_string());
        Options {
            words,
            tamil,
            predicted,
        }
    }

    /// Records the word being inserted: a Tamil pick (not a prediction or
    /// the English row) may be learned, and any Tamil word goes into the
    /// typing history; both are saved soon after.
    fn learn(&mut self, options: &Options) {
        let selected = self.composer.selected();
        let chosen = if selected < options.len() {
            selected
        } else {
            0
        };
        let Some(key) = self.composer.key() else {
            return;
        };
        let Some(word) = options.words.get(chosen) else {
            return;
        };
        let mut changed = false;
        if self.config.remember_picks && chosen < options.tamil {
            let model_top = self.cache.get(&key).and_then(|c| c.first());
            if self
                .learned
                .record(&key, word, model_top.map(String::as_str))
            {
                self.learned_dirty = true;
                changed = true;
            }
        }
        if self.config.predict && chosen < options.tamil + options.predicted {
            self.history.record(&key, word);
            self.history_dirty = true;
            changed = true;
        }
        if changed {
            post(self.main, WM_APP_SAVE_LEARNED);
        }
    }

    /// Writes the learned words and the typing history if they changed
    /// (posted, so the file writes never happen inside the keyboard hook).
    fn save_learned(&mut self) {
        if std::mem::take(&mut self.learned_dirty)
            && let Some(path) = &self.learned_path
            && let Err(e) = self.learned.save(path)
        {
            eprintln!("learned words not saved: {e:#}");
        }
        if std::mem::take(&mut self.history_dirty)
            && let Some(path) = &self.history_path
            && let Err(e) = self.history.save(path)
        {
            eprintln!("typing history not saved: {e:#}");
        }
    }

    /// Once the current word's candidates are known, they are the ones on show.
    fn note_shown(&mut self) {
        if let Some(key) = self.composer.key()
            && self.cache.contains_key(&key)
        {
            self.shown_key = Some(key);
        }
    }

    /// Fills in the current word's candidates (and predictions) now if the
    /// engine hasn't yet (~40 ms), for a key that commits before it answers.
    fn ensure_candidates(&mut self) {
        if let (Some(key), Some(xlit)) = (self.composer.key(), &self.xlit)
            && !self.cache.contains_key(&key)
        {
            let (cands, predictions) = suggest(xlit, &key);
            self.predicted.insert(key.clone(), predictions);
            self.cache.insert(key, cands);
        }
    }

    /// Tamil suggestions shown per word (the beam, up to `MAX_ROWS`).
    fn tamil_rows(&self) -> usize {
        self.config.beam.min(MAX_ROWS)
    }

    /// Requests the current word from the engine unless cached or pending.
    fn request(&mut self) {
        if let Some(key) = self.composer.key() {
            self.request_key(key);
        }
    }

    fn request_key(&mut self, key: String) {
        if !self.cache.contains_key(&key) && self.pending.insert(key.clone()) {
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
        // Read each time, so a change to Windows' light/dark mode shows at once.
        self.popup.set_dark(self.config.theme.is_dark());
        self.popup.set_dpi(caret::dpi_at(anchor));
        self.popup.set_list(self.tamil_rows() + 1, None);
        let (w, h) = self.popup.size();
        let (x, y) = caret::place(anchor, w, h, self.popup.gap());
        self.anchor = Some(anchor);
        self.popup_size = (w, h);
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

    fn invalidate(&mut self) {
        if self.visible {
            self.fit_popup();
            // SAFETY: our own window; painting happens later via WM_PAINT.
            let _ = unsafe { InvalidateRect(Some(self.popup.hwnd), None, false) };
        }
    }

    /// Grows or shrinks the popup to the list (predictions come and go),
    /// placed again by its caret so it stays on screen.
    fn fit_popup(&mut self) {
        let list = self.shown();
        let rows = if list.is_empty() {
            self.tamil_rows() + 1
        } else {
            list.len()
        };
        let section = (list.predicted > 0).then_some(list.tamil);
        self.popup.set_list(rows, section);
        let (w, h) = self.popup.size();
        let Some(anchor) = self.anchor else {
            return;
        };
        if (w, h) == self.popup_size {
            return;
        }
        self.popup_size = (w, h);
        let (x, y) = caret::place(anchor, w, h, self.popup.gap());
        // SAFETY: our own window; SWP_NOACTIVATE keeps focus in the app.
        unsafe {
            let _ = SetWindowPos(
                self.popup.hwnd,
                None,
                x,
                y,
                w,
                h,
                SWP_NOACTIVATE | SWP_NOZORDER,
            );
        }
    }

    /// Another window came to the front: drop the word, unless it is the
    /// screen capture tool after Print Screen (see `Capture`) or the focus
    /// is back where the word is being typed.
    fn on_foreground(&mut self, fg: HWND) {
        if let Some(capture) = &mut self.capture {
            if fg == capture.target {
                self.capture = None;
                return;
            }
            let pid = process_of(fg);
            match capture.tool {
                None if capture.since.elapsed() < CAPTURE_GRACE => {
                    capture.tool = Some(pid);
                    return;
                }
                Some(tool) if tool == pid => return,
                _ => {}
            }
        }
        self.discard();
    }

    fn discard(&mut self) {
        self.composer.clear();
        self.capture = None;
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
        let options = self.candidates();
        self.learn(&options);
        let text = self.composer.render(Some(&options.words)) + suffix;
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
        let fg = unsafe { GetForegroundWindow() };
        if settings::hwnd() == Some(fg) || welcome::hwnd() == Some(fg) {
            return false;
        }
        // Print Screen mid-word: let the screenshot be taken with the popup
        // still showing, rather than inserting the word first.
        if ev.vk == VK_SNAPSHOT.0 {
            if !self.composer.is_empty() && self.capture.is_none() {
                self.capture = Some(Capture {
                    target: fg,
                    since: Instant::now(),
                    tool: None,
                });
            }
            return false;
        }
        // Keys for the screen capture tool (Esc to cancel, …) aren't typing.
        if self.capture.as_ref().is_some_and(|c| c.target != fg) {
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
            if let Some(c) = self.convert_hotkey
                && ev.vk as u32 == c.vk
                && mods == c.modifiers
            {
                post(self.main, WM_APP_CONVERT);
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
                // Ctrl+← / Ctrl+→: to the start or end of the word.
                v if v == VK_LEFT.0 || v == VK_RIGHT.0 => {
                    self.composer.cursor_to(v == VK_RIGHT.0);
                    self.invalidate();
                }
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
            // Editing mid-word: move the cursor, delete after it.
            v if (v == VK_LEFT.0 || v == VK_RIGHT.0) && composing => {
                self.composer
                    .move_cursor(if v == VK_LEFT.0 { -1 } else { 1 });
                self.invalidate();
            }
            v if (v == VK_HOME.0 || v == VK_END.0) && composing => {
                self.composer.cursor_to(v == VK_END.0);
                self.invalidate();
            }
            v if v == VK_DELETE.0 && composing => {
                self.composer.delete();
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

    /// The row showing the current word's learned pick (among the Tamil
    /// suggestions), if picks are remembered.
    fn learned_row(&self, options: &Options) -> Option<usize> {
        if !self.config.remember_picks {
            return None;
        }
        let word = self.learned.get(&self.composer.key()?)?;
        options.words[..options.tamil]
            .iter()
            .position(|o| o == word)
    }

    fn view(&self) -> View {
        let candidates = self.shown();
        let (hint, warn) = if let Some(f) = &self.flash {
            (f.clone(), true)
        } else {
            match &self.status {
                Status::Loading => ("Loading the model…".to_string(), false),
                Status::Failed(e) => (format!("Model failed to load: {e}"), true),
                // Empty: the popup shows its key tips.
                Status::Ready => (String::new(), false),
            }
        };
        View {
            input: self.composer.text().to_string(),
            cursor: self.composer.cursor(),
            preview: self.composer.render(Some(&candidates.words)),
            english: candidates.len().checked_sub(1),
            learned: self.learned_row(&candidates),
            predicted: candidates.tamil..candidates.tamil + candidates.predicted,
            candidates: candidates.words,
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
        // events must be counted as in flight before they are sent. Without
        // the hook (Tamil mode off, e.g. convert selection) none come back.
        let expected = if a.kb_hook.is_some() {
            insert::event_count(&items, mode)
        } else {
            0
        };
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
                    a.cancel_convert(None);
                    failed = Some(e);
                }
                Event::Done {
                    key,
                    candidates,
                    predictions,
                } => {
                    a.pending.remove(&key);
                    // Bounded: typed words should not pile up in memory.
                    if a.cache.len() >= MAX_CACHE {
                        a.cache.clear();
                        a.predicted.clear();
                    }
                    a.predicted.insert(key.clone(), predictions);
                    a.cache.insert(key, candidates);
                }
            }
        }
        a.note_shown();
        a.invalidate();
        a.convert_ready();
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
    const WELCOME: usize = 4;
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
        (WELCOME, "Welcome tour"),
        (0, "-"),
        (QUIT, "Quit"),
    ];
    // Modal loop: the app state must not be borrowed meanwhile.
    match tray::menu(main, &items, &[]) {
        TOGGLE => toggle(),
        SETTINGS => open_settings(),
        WELCOME => open_welcome(),
        QUIT => quit(),
        _ => {}
    }
}

/// Opens the Settings window. While it is open the global hotkey is
/// unregistered (so pressing it records it in the hotkey box instead of
/// toggling Tamil mode) and the keyboard hook ignores keys meant for it.
fn open_settings() {
    if settings::hwnd().is_none() {
        with_app(|a| {
            // SAFETY: our registrations; re-registered in `settings_closed`.
            unsafe {
                let _ = UnregisterHotKey(Some(a.main), HOTKEY_ID);
                let _ = UnregisterHotKey(Some(a.main), CONVERT_HOTKEY_ID);
            }
            a.discard();
            a.cancel_convert(None);
        });
    }
    let actions = settings::Actions {
        apply: apply_settings,
        suggest: suggest_words,
        clear_history,
        on_close: settings_closed,
    };
    settings::open(current_values(), actions);
}

fn current_values() -> settings::Values {
    let autostart = ezhuthu_install::autostart_enabled();
    let (config, learned) =
        with_app(|a| (a.config.clone(), a.learned.entries())).unwrap_or_default();
    settings::Values {
        hotkey: config.hotkey,
        convert_hotkey: config.convert_hotkey,
        insert_mode: config.insert_mode,
        beam: config.beam,
        remember_picks: config.remember_picks,
        predict: config.predict,
        theme: config.theme,
        autostart,
        learned,
    }
}

/// Settings "Clear typing history…": forgets it and deletes the file.
fn clear_history() -> Result<(), String> {
    let result = with_app(|a| {
        a.history.clear();
        a.history_dirty = false;
        match &a.history_path {
            Some(path) => a
                .history
                .save(path)
                .map_err(|e| format!("Couldn't delete the typing history: {e:#}")),
            None => Ok(()),
        }
    });
    result.unwrap_or_else(|| Err("Ezhuthu is busy; try again".into()))
}

/// The model's suggestions for `key` (Settings > Words); none until it loads.
fn suggest_words(key: &str) -> Vec<String> {
    let Some(xlit) = with_app(|a| a.xlit.clone()).flatten() else {
        return Vec::new();
    };
    xlit.transliterate(key)
        .map(|c| c.into_iter().map(|c| c.text).collect())
        .unwrap_or_default()
}

/// Opens the welcome tour, with the user's shortcuts and popup theme.
fn open_welcome() {
    let Some(info) = with_app(|a| welcome::Info {
        hotkey: a.config.hotkey.clone(),
        convert_hotkey: a.config.convert_hotkey.clone(),
        dark: a.config.theme.is_dark(),
    }) else {
        return;
    };
    welcome::open(info, welcome_closed);
}

/// The tour closed: it won't open by itself again; "Start typing" turns
/// Tamil mode on.
fn welcome_closed(start_typing: bool) {
    with_app(|a| {
        if !a.config.welcome_shown {
            a.config.welcome_shown = true;
            if let Err(e) = a.config.save(&a.config_path) {
                eprintln!("settings not saved: {e:#}");
            }
        }
        if start_typing {
            a.set_mode(true);
        }
    });
}

fn settings_closed() {
    with_app(|a| a.register_hotkeys());
}

/// Settings window OK: validate, apply live and save.
fn apply_settings(v: &settings::Values) -> Result<(), String> {
    let hotkey = Hotkey::parse(&v.hotkey).map_err(|e| format!("{e:#}"))?;
    let convert = match v.convert_hotkey.as_str() {
        "" => None,
        keys => Some(Hotkey::parse(keys).map_err(|e| format!("{e:#}"))?),
    };
    if convert == Some(hotkey) {
        return Err("Tamil mode and convert selection need different shortcuts.".into());
    }
    let result = with_app(|a| {
        // Both shortcuts are unregistered while Settings is open, so a trial
        // registration tells whether another app holds a new one.
        let taken = |keys: &str, h: Hotkey| {
            let mods = HOT_KEY_MODIFIERS(h.modifiers) | MOD_NOREPEAT;
            // SAFETY: a trial registration, removed straight away.
            let free = unsafe { RegisterHotKey(Some(a.main), HOTKEY_ID, mods, h.vk) }.is_ok();
            let _ = unsafe { UnregisterHotKey(Some(a.main), HOTKEY_ID) };
            (!free).then(|| format!("{keys} is already used by another app. Choose another."))
        };
        if v.hotkey != a.config.hotkey
            && let Some(e) = taken(&v.hotkey, hotkey)
        {
            return Err(e);
        }
        if let Some(c) = convert
            && v.convert_hotkey != a.config.convert_hotkey
            && let Some(e) = taken(&v.convert_hotkey, c)
        {
            return Err(e);
        }
        let exe = std::env::current_exe().map_err(|e| format!("can't find ezhuthu.exe: {e}"))?;
        ezhuthu_install::set_autostart(v.autostart.then_some(exe.as_path()))
            .map_err(|e| format!("Couldn't change Start with Windows: {e:#}"))?;

        let mut new = a.config.clone();
        new.hotkey = v.hotkey.clone();
        new.convert_hotkey = v.convert_hotkey.clone();
        new.insert_mode = v.insert_mode;
        new.beam = v.beam;
        new.remember_picks = v.remember_picks;
        new.predict = v.predict;
        new.theme = v.theme;
        new.save(&a.config_path)
            .map_err(|e| format!("Couldn't save the settings: {e:#}"))?;
        if v.learned != a.learned.entries() {
            a.learned.set(&v.learned);
            if let Some(path) = &a.learned_path {
                a.learned
                    .save(path)
                    .map_err(|e| format!("Couldn't save the learned words: {e:#}"))?;
            }
            a.learned_dirty = false;
        }
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
        a.save_learned();
        a.cancel_convert(None);
        a.tray.remove();
        // SAFETY: our registrations.
        unsafe {
            let _ = UnregisterHotKey(Some(a.main), HOTKEY_ID);
            let _ = UnregisterHotKey(Some(a.main), CONVERT_HOTKEY_ID);
        }
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
        WM_HOTKEY if wparam.0 as i32 == CONVERT_HOTKEY_ID => {
            with_app(|a| a.start_convert());
        }
        WM_APP_CONVERT => {
            with_app(|a| a.start_convert());
        }
        WM_TIMER if wparam.0 == CONVERT_TIMER => {
            with_app(|a| a.convert_tick());
        }
        WM_TIMER if wparam.0 == BADGE_TIMER => {
            with_app(|a| {
                if !a.badge.tick(a.converting.is_some()) {
                    // SAFETY: killing our own timer.
                    let _ = unsafe { KillTimer(Some(a.main), BADGE_TIMER) };
                }
            });
        }
        WM_APP_FLUSH => flush(),
        WM_APP_TRAY => on_tray((lparam.0 & 0xFFFF) as u32),
        WM_APP_ENGINE => on_engine_events(),
        // The installer asks a running copy to quit this way.
        WM_CLOSE => quit(),
        WM_APP_SETTINGS => open_settings(),
        WM_APP_SAVE_LEARNED => {
            with_app(|a| a.save_learned());
        }
        WM_APP_FOREGROUND => {
            let fg = HWND(wparam.0 as *mut _);
            with_app(|a| a.on_foreground(fg));
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

/// Another window came to the foreground (click, Alt+Tab): usually drops the
/// word (see `App::on_foreground`).
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
        // SAFETY: posting to our own window; the handle travels as a value.
        let _ = unsafe {
            PostMessageW(
                Some(main),
                WM_APP_FOREGROUND,
                WPARAM(hwnd.0 as usize),
                LPARAM(0),
            )
        };
    }
}

/// Shows a fatal startup error (no console in release builds).
pub fn report_fatal(e: &anyhow::Error) {
    error_box(None, &format!("Ezhuthu could not start:\n\n{e:#}"));
}
