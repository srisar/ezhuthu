//! The welcome tour: a window of cards, one per feature, each with an
//! animated example, then a card for Gravitide. Shown the first time
//! Ezhuthu runs, and from the tray menu.
//!
//! Everything is drawn on a `Canvas` about 30 times a second (a timer
//! invalidates the window), light or dark like the popup. Cards cross-fade.
//! Keys: Right / Enter / Space next, Left back, Esc closes.

use std::cell::RefCell;
use std::f32::consts::{PI, TAU};
use std::time::Instant;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, COLOR_HIGHLIGHT, CreateFontW,
    DEFAULT_CHARSET, DRAW_TEXT_FORMAT, DT_CENTER, DT_LEFT, DT_RIGHT, DT_SINGLELINE, DT_VCENTER,
    DT_WORDBREAK, DeleteObject, EndPaint, FW_NORMAL, FW_SEMIBOLD, GetMonitorInfoW, GetSysColor,
    HFONT, HGDIOBJ, InvalidateRect, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint,
    OUT_DEFAULT_PRECIS, PAINTSTRUCT,
};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForSystem};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_BACK, VK_ESCAPE, VK_LEFT, VK_RETURN, VK_RIGHT,
    VK_SPACE,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyWindow, GetClientRect, HICON, IDC_ARROW,
    IDC_HAND, IMAGE_ICON, KillTimer, LR_DEFAULTCOLOR, LoadCursorW, LoadIconW, LoadImageW,
    RegisterClassExW, SW_SHOW, SW_SHOWNORMAL, SetCursor, SetForegroundWindow, SetTimer, ShowWindow,
    WINDOW_EX_STYLE, WM_DESTROY, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_PAINT, WM_SETCURSOR, WM_TIMER, WNDCLASSEXW, WS_CAPTION, WS_MINIMIZEBOX,
    WS_OVERLAPPED, WS_SYSMENU,
};
use windows::core::{HSTRING, PCWSTR, w};

use crate::canvas::{Canvas, Image, Rgb, mix};
use crate::shape::ease_out;
use crate::win::{instance, wide};

const SITE: &str = "https://gravitide.dev";
const TIMER: usize = 1;
/// Client area (96-DPI units).
const WIDTH: i32 = 560;
const HEIGHT: i32 = 470;
/// The animation stage, and the card's title and text below it.
const STAGE: (f32, f32, f32, f32) = (24.0, 24.0, 512.0, 212.0);
const CROSSFADE: f32 = 0.22;
/// The Gravitide logo, as rendered by tools/icons/make_logo.py.
const LOGO_LIGHT: &[u8] = include_bytes!("../logo/gravitide-light.bgra");
const LOGO_DARK: &[u8] = include_bytes!("../logo/gravitide-dark.bgra");
const LOGO_WIDTH: f32 = 240.0;
const DEFAULT_CONVERT: &str = "Ctrl+Alt+Shift+T";

/// What the tour shows: the user's shortcuts and theme.
pub struct Info {
    pub hotkey: String,
    /// Empty when convert selection is off.
    pub convert_hotkey: String,
    pub dark: bool,
}

/// Runs when the window closes: true if the user chose "Start typing".
pub type OnClose = fn(bool);

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    Skip,
    Back,
    Next,
    Link,
}

struct Colors {
    bg: Rgb,
    stage: Rgb,
    text: Rgb,
    muted: Rgb,
    line: Rgb,
    field: Rgb,
    key: Rgb,
    key_line: Rgb,
    key_down: Rgb,
    key_down_text: Rgb,
    popup: Rgb,
    popup_line: Rgb,
    /// The example popup's input box, divider and number key caps, as
    /// in the real popup.
    popup_input: Rgb,
    popup_input_line: Rgb,
    popup_sep: Rgb,
    popup_key: Rgb,
    popup_key_text: Rgb,
    hover: Rgb,
}

const LIGHT: Colors = Colors {
    bg: 0xFFFFFF,
    stage: 0xF3F4F6,
    text: 0x1B1B1B,
    muted: 0x5F6368,
    line: 0xD3D6DB,
    field: 0xFFFFFF,
    key: 0xFFFFFF,
    key_line: 0xC3C7CD,
    key_down: 0xFDE7C2,
    key_down_text: 0x7A4A00,
    popup: 0xFFFFFF,
    popup_line: 0xC8C8C8,
    popup_input: 0xF4F5F7,
    popup_input_line: 0xE1E3E6,
    popup_sep: 0xECECEC,
    popup_key: 0xEEF0F2,
    popup_key_text: 0x555555,
    hover: 0xEBEDF0,
};

const DARK: Colors = Colors {
    bg: 0x202020,
    stage: 0x272727,
    text: 0xF2F2F2,
    muted: 0xA3A3A3,
    line: 0x454545,
    field: 0x1B1B1B,
    key: 0x353535,
    key_line: 0x5A5A5A,
    key_down: 0x5C4313,
    key_down_text: 0xFFD48A,
    popup: 0x2B2B2B,
    popup_line: 0x4A4A4A,
    popup_input: 0x1F1F1F,
    popup_input_line: 0x3D3D3D,
    popup_sep: 0x3A3A3A,
    popup_key: 0x3A3A3A,
    popup_key_text: 0xCFCFCF,
    hover: 0x333333,
};

/// The logo's colours, for the badge and the primary button.
const TILE: Rgb = 0x15171C;
const LETTER: Rgb = 0xF7F7F5;
const ORANGE: Rgb = 0xF5A524;

struct Fonts {
    title: HFONT,
    body: HFONT,
    small: HFONT,
    button: HFONT,
    roman: HFONT,
    tamil: HFONT,
    tamil_big: HFONT,
}

impl Fonts {
    fn new(dpi: u32) -> Self {
        let make = |tenths: i32, weight: i32, face: &str| {
            let face = wide(face);
            // SAFETY: the face name is NUL-terminated and outlives the call.
            unsafe {
                CreateFontW(
                    -(tenths * dpi as i32 / 720),
                    0,
                    0,
                    0,
                    weight,
                    0,
                    0,
                    0,
                    DEFAULT_CHARSET,
                    OUT_DEFAULT_PRECIS,
                    CLIP_DEFAULT_PRECIS,
                    CLEARTYPE_QUALITY,
                    0,
                    PCWSTR(face.as_ptr()),
                )
            }
        };
        let (normal, semibold) = (FW_NORMAL.0 as i32, FW_SEMIBOLD.0 as i32);
        Self {
            title: make(150, semibold, "Segoe UI"),
            body: make(105, normal, "Segoe UI"),
            small: make(85, normal, "Segoe UI"),
            button: make(100, semibold, "Segoe UI"),
            roman: make(140, normal, "Segoe UI"),
            tamil: make(130, normal, "Nirmala UI"),
            tamil_big: make(260, normal, "Nirmala UI"),
        }
    }
}

impl Drop for Fonts {
    fn drop(&mut self) {
        let all = [
            self.title,
            self.body,
            self.small,
            self.button,
            self.roman,
            self.tamil,
            self.tamil_big,
        ];
        for f in all {
            // SAFETY: we created these fonts; none is selected anywhere.
            let _ = unsafe { DeleteObject(HGDIOBJ(f.0)) };
        }
    }
}

struct Card {
    title: &'static str,
    scene: fn(&mut Ctx, f32),
}

const CARDS: [Card; 7] = [
    Card {
        title: "Type Tamil in English letters",
        scene: scene_welcome,
    },
    Card {
        title: "Turn Tamil mode on and off",
        scene: scene_toggle,
    },
    Card {
        title: "Pick from suggestions",
        scene: scene_pick,
    },
    Card {
        title: "English when you need it",
        scene: scene_english,
    },
    Card {
        title: "Learns your words",
        scene: scene_learn,
    },
    Card {
        title: "Convert text you already typed",
        scene: scene_convert,
    },
    Card {
        title: "Made by Gravitide",
        scene: scene_gravitide,
    },
];
const LAST: usize = CARDS.len() - 1;

/// A card's text, which mentions the user's own shortcuts.
fn body(card: usize, info: &Info) -> String {
    match card {
        0 => "Type words the way they sound, in any app. Ezhuthu turns them into Tamil as you go."
            .into(),
        1 => format!(
            "Press {} anywhere. The tray icon shows the Ezhuthu logo while Tamil mode is on.",
            info.hotkey
        ),
        2 => "A popup at your cursor offers Tamil spellings. Press Space for the first, or a \
              number for another."
            .into(),
        3 => "The last suggestion is always the word as you typed it, so a few English words \
              need no switching."
            .into(),
        4 => "Pick a different suggestion once, and it comes first next time. See and edit them \
              in Settings › Words."
            .into(),
        5 if info.convert_hotkey.is_empty() => {
            "Select romanized text in any app and press a shortcut to turn it into Tamil. \
             Turn it on in Settings › Shortcuts."
                .into()
        }
        5 => format!(
            "Select romanized text in any app and press {} to turn it into Tamil.",
            info.convert_hotkey
        ),
        _ => "Ezhuthu is free and open source, built in Sri Lanka with love. Visit our website \
              for more products."
            .into(),
    }
}

struct Welcome {
    hwnd: HWND,
    dpi: u32,
    info: Info,
    card: usize,
    since: Instant,
    /// The frame shown before the card changed, faded out over CROSSFADE.
    fade: Option<(Instant, Vec<u32>)>,
    canvas: Option<Canvas>,
    fonts: Fonts,
    logo: Option<Image>,
    app_icon: Option<HICON>,
    tray_on: Option<HICON>,
    tray_off: Option<HICON>,
    hover: Option<Hit>,
    pressed: Option<Hit>,
    start_typing: bool,
    on_close: OnClose,
}

thread_local! {
    static WELCOME: RefCell<Option<Welcome>> = const { RefCell::new(None) };
}

/// The open welcome window, if any.
pub fn hwnd() -> Option<HWND> {
    WELCOME.with(|w| w.borrow().as_ref().map(|w| w.hwnd))
}

/// Opens the welcome tour (or brings it to the front).
pub fn open(info: Info, on_close: OnClose) {
    if let Some(h) = hwnd() {
        // SAFETY: bringing our own window to the front.
        let _ = unsafe { SetForegroundWindow(h) };
        return;
    }
    if let Err(e) = create(info, on_close) {
        crate::win::error_box(None, &format!("Couldn't open the welcome tour:\n\n{e:#}"));
    }
}

fn load_icon(id: u16, size: i32) -> Option<HICON> {
    // SAFETY: loading an icon resource linked into this exe at a given size;
    // destroyed in WM_DESTROY.
    unsafe {
        LoadImageW(
            Some(instance()),
            PCWSTR(id as usize as *const u16),
            IMAGE_ICON,
            size,
            size,
            LR_DEFAULTCOLOR,
        )
    }
    .ok()
    .map(|h| HICON(h.0))
}

fn create(info: Info, on_close: OnClose) -> windows::core::Result<()> {
    // SAFETY: standard window creation on the UI thread; strings outlive the
    // calls; the icons are destroyed in WM_DESTROY.
    unsafe {
        let class = w!("ezhuthu.welcome");
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(window_proc),
            hInstance: instance(),
            hIcon: LoadIconW(Some(instance()), PCWSTR(1 as _)).unwrap_or_default(),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassExW(&wc);

        let dpi = GetDpiForSystem();
        let s = |v: i32| v * dpi as i32 / 96;
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        let mut r = RECT {
            left: 0,
            top: 0,
            right: s(WIDTH),
            bottom: s(HEIGHT),
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
            w!("Welcome to Ezhuthu"),
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
        if info.dark {
            let on: u32 = 1;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE,
                &on as *const u32 as *const _,
                size_of::<u32>() as u32,
            );
        }
        let logo_data = if info.dark { LOGO_DARK } else { LOGO_LIGHT };
        let logo = Image::from_bgra(logo_data)
            .map(|img| img.scaled((LOGO_WIDTH * dpi as f32 / 96.0).round() as usize));
        WELCOME.with(|cell| {
            *cell.borrow_mut() = Some(Welcome {
                hwnd,
                dpi,
                info,
                card: 0,
                since: Instant::now(),
                fade: None,
                canvas: None,
                fonts: Fonts::new(dpi),
                logo,
                app_icon: load_icon(1, s(44)),
                tray_on: load_icon(2, s(24)),
                tray_off: load_icon(3, s(24)),
                hover: None,
                pressed: None,
                start_typing: false,
                on_close,
            });
        });
        SetTimer(Some(hwnd), TIMER, 33, None);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
    Ok(())
}

impl Welcome {
    fn s(&self, v: f32) -> f32 {
        v * self.dpi as f32 / 96.0
    }

    fn rect(&self, x: f32, y: f32, w: f32, h: f32) -> RECT {
        RECT {
            left: self.s(x).round() as i32,
            top: self.s(y).round() as i32,
            right: self.s(x + w).round() as i32,
            bottom: self.s(y + h).round() as i32,
        }
    }

    fn colors(&self) -> &'static Colors {
        if self.info.dark { &DARK } else { &LIGHT }
    }

    /// The buttons shown on the current card, with their rectangles.
    fn buttons(&self) -> Vec<(Hit, RECT)> {
        let y = 414.0;
        let mut out = Vec::new();
        if self.card < LAST {
            out.push((Hit::Skip, self.rect(20.0, y, 70.0, 34.0)));
        }
        if self.card > 0 {
            out.push((
                Hit::Back,
                self.rect(WIDTH as f32 - 24.0 - 132.0 - 98.0, y, 90.0, 34.0),
            ));
        }
        out.push((
            Hit::Next,
            self.rect(WIDTH as f32 - 24.0 - 132.0, y, 132.0, 34.0),
        ));
        if self.card == LAST {
            out.push((Hit::Link, self.link_rect()));
        }
        out
    }

    /// The "Visit gravitide.dev" button on the last card.
    fn link_rect(&self) -> RECT {
        let (sx, sy, sw, _) = STAGE;
        self.rect(sx + sw / 2.0 - 90.0, sy + 150.0, 180.0, 34.0)
    }

    fn hit_at(&self, x: i32, y: i32) -> Option<Hit> {
        self.buttons()
            .into_iter()
            .find(|(_, r)| x >= r.left && x < r.right && y >= r.top && y < r.bottom)
            .map(|(hit, _)| hit)
    }

    fn go(&mut self, card: usize) {
        if card == self.card {
            return;
        }
        let previous = self.canvas.as_mut().map(|c| c.pixels().to_vec());
        self.fade = previous.map(|p| (Instant::now(), p));
        self.card = card;
        self.since = Instant::now();
        self.pressed = None;
    }

    /// Renders the frame onto the canvas (resized to the client area).
    fn render(&mut self) {
        let mut r = RECT::default();
        // SAFETY: plain query of our own window.
        let _ = unsafe { GetClientRect(self.hwnd, &mut r) };
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        let mut canvas = match self.canvas.take() {
            Some(c) if c.w == w && c.h == h => c,
            _ => match Canvas::new(w, h) {
                Some(c) => c,
                None => return,
            },
        };
        let c = self.colors();
        canvas.clear(c.bg);
        let (sx, sy, sw, sh) = STAGE;
        canvas.round_rect(
            self.s(sx),
            self.s(sy),
            self.s(sw),
            self.s(sh),
            self.s(12.0),
            c.stage,
            1.0,
        );
        {
            let mut ctx = Ctx {
                cv: &mut canvas,
                c,
                f: &self.fonts,
                k: self.dpi as f32 / 96.0,
                ox: self.s(sx),
                oy: self.s(sy),
                // SAFETY: plain query.
                highlight: rgb_of(unsafe { GetSysColor(COLOR_HIGHLIGHT) }),
                info: &self.info,
                logo: self.logo.as_ref(),
                app_icon: self.app_icon,
                tray: (self.tray_on, self.tray_off),
                link_hover: self.hover == Some(Hit::Link),
            };
            (CARDS[self.card].scene)(&mut ctx, self.since.elapsed().as_secs_f32());
        }

        let center = DT_CENTER | DT_SINGLELINE | DT_VCENTER;
        canvas.text(
            self.fonts.title,
            CARDS[self.card].title,
            self.rect(24.0, 248.0, 512.0, 34.0),
            c.text,
            center,
        );
        canvas.text(
            self.fonts.body,
            &body(self.card, &self.info),
            self.rect(56.0, 288.0, 448.0, 56.0),
            c.muted,
            DT_CENTER | DT_WORDBREAK,
        );
        // Page dots.
        let n = CARDS.len() as f32;
        let gap = 14.0;
        let x0 = WIDTH as f32 / 2.0 - (n - 1.0) * gap / 2.0;
        for i in 0..CARDS.len() {
            let on = i == self.card;
            let color = if on { ORANGE } else { c.line };
            canvas.circle(
                self.s(x0 + i as f32 * gap),
                self.s(372.0),
                self.s(if on { 4.0 } else { 3.5 }),
                color,
                1.0,
            );
        }
        // Buttons.
        for (hit, r) in self.buttons() {
            let hover = self.hover == Some(hit);
            let (x, y) = (r.left as f32, r.top as f32);
            let (bw, bh) = ((r.right - r.left) as f32, (r.bottom - r.top) as f32);
            let radius = self.s(8.0);
            match hit {
                Hit::Link => continue, // drawn by the scene
                Hit::Next => {
                    let fill = if hover {
                        mix(ORANGE, 0xFFFFFF, 0.15)
                    } else {
                        ORANGE
                    };
                    canvas.round_rect(x, y, bw, bh, radius, fill, 1.0);
                    let label = if self.card == LAST {
                        "Start typing"
                    } else {
                        "Next"
                    };
                    canvas.text(self.fonts.button, label, r, TILE, center);
                }
                Hit::Back => {
                    if hover {
                        canvas.round_rect(x, y, bw, bh, radius, c.hover, 1.0);
                    }
                    canvas.round_rect_line((x, y, bw, bh), radius, self.s(1.0), c.line, 1.0);
                    canvas.text(self.fonts.button, "Back", r, c.text, center);
                }
                Hit::Skip => {
                    if hover {
                        canvas.round_rect(x, y, bw, bh, radius, c.hover, 1.0);
                    }
                    canvas.text(self.fonts.button, "Skip", r, c.muted, center);
                }
            }
        }
        // Cross-fade from the previous card.
        if let Some((start, previous)) = &self.fade {
            let k = ease_out((start.elapsed().as_secs_f32() / CROSSFADE).min(1.0));
            if k < 1.0 {
                canvas.blend_over(previous, 1.0 - k);
            } else {
                self.fade = None;
            }
        }
        self.canvas = Some(canvas);
    }
}

/// An Rgb from a GetSysColor COLORREF (0x00BBGGRR).
fn rgb_of(c: u32) -> Rgb {
    ((c & 0xFF) << 16) | (c & 0xFF00) | ((c >> 16) & 0xFF)
}

// ---------------------------------------------------------------- scenes

/// Drawing helpers for the scenes, in 96-DPI units from the stage's corner.
struct Ctx<'a> {
    cv: &'a mut Canvas,
    c: &'static Colors,
    f: &'a Fonts,
    k: f32,
    ox: f32,
    oy: f32,
    highlight: Rgb,
    info: &'a Info,
    logo: Option<&'a Image>,
    app_icon: Option<HICON>,
    tray: (Option<HICON>, Option<HICON>),
    link_hover: bool,
}

impl Ctx<'_> {
    fn x(&self, v: f32) -> f32 {
        self.ox + v * self.k
    }
    fn y(&self, v: f32) -> f32 {
        self.oy + v * self.k
    }
    fn n(&self, v: f32) -> f32 {
        v * self.k
    }
    fn rect(&self, x: f32, y: f32, w: f32, h: f32) -> RECT {
        RECT {
            left: self.x(x).round() as i32,
            top: self.y(y).round() as i32,
            right: self.x(x + w).round() as i32,
            bottom: self.y(y + h).round() as i32,
        }
    }
    fn text(
        &mut self,
        font: HFONT,
        s: &str,
        r: (f32, f32, f32, f32),
        color: Rgb,
        fmt: DRAW_TEXT_FORMAT,
    ) {
        let rect = self.rect(r.0, r.1, r.2, r.3);
        self.cv
            .text(font, s, rect, color, fmt | DT_SINGLELINE | DT_VCENTER);
    }
    /// Text width in 96-DPI units.
    fn width(&self, font: HFONT, s: &str) -> f32 {
        if s.is_empty() {
            return 0.0;
        }
        self.cv.measure(font, s).0 as f32 / self.k
    }

    /// A text box with `text` (Tamil font), a blinking caret, and the text
    /// optionally shown selected.
    #[allow(clippy::too_many_arguments)]
    fn field(&mut self, x: f32, y: f32, w: f32, text: &str, t: f32, caret: bool, selected: bool) {
        let h = 36.0;
        let (c, line) = (self.c, self.c.line);
        self.cv.round_rect(
            self.x(x),
            self.y(y),
            self.n(w),
            self.n(h),
            self.n(6.0),
            c.field,
            1.0,
        );
        self.cv.round_rect_line(
            (self.x(x), self.y(y), self.n(w), self.n(h)),
            self.n(6.0),
            self.n(1.0),
            line,
            1.0,
        );
        let tw = self.width(self.f.tamil, text);
        if selected && !text.is_empty() {
            let hl = self.highlight;
            self.cv.round_rect(
                self.x(x + 10.0),
                self.y(y + 7.0),
                self.n(tw),
                self.n(22.0),
                self.n(2.0),
                hl,
                0.35,
            );
        }
        self.text(
            self.f.tamil,
            text,
            (x + 10.0, y, w - 20.0, h),
            c.text,
            DT_LEFT,
        );
        if caret && (t * 2.0).fract() < 0.6 {
            let cx = self.x(x + 11.0 + tw);
            let text_color = c.text;
            self.cv.line(
                (cx, self.y(y + 9.0)),
                (cx, self.y(y + 27.0)),
                self.n(1.2),
                text_color,
                1.0,
            );
        }
    }

    /// A small copy of the real popup: the typed text in its input box,
    /// the Tamil preview, then numbered suggestions (the selected one
    /// tinted orange, EN on the typed word, a sparkle on a learned pick).
    fn popup(&mut self, x: f32, y: f32, p: &Mini, t: f32) {
        let (w, pad, input_h, preview_h, row_h) = (240.0, 6.0, 26.0, 26.0, 24.0);
        let h = pad + input_h + 3.0 + preview_h + 7.0 + p.rows.len() as f32 * row_h + pad;
        let c = self.c;
        let frame = (self.x(x), self.y(y), self.n(w), self.n(h));
        self.cv.round_rect(
            frame.0,
            frame.1,
            frame.2,
            frame.3,
            self.n(8.0),
            c.popup,
            1.0,
        );
        self.cv
            .round_rect_line(frame, self.n(8.0), self.n(1.0), c.popup_line, 1.0);

        // The input box: typed text, an orange caret, "EN".
        let (bx, by, bw) = (x + pad, y + pad, w - 2.0 * pad);
        let input = (self.x(bx), self.y(by), self.n(bw), self.n(input_h));
        self.cv.round_rect(
            input.0,
            input.1,
            input.2,
            input.3,
            self.n(6.0),
            c.popup_input,
            1.0,
        );
        self.cv
            .round_rect_line(input, self.n(6.0), self.n(1.0), c.popup_input_line, 1.0);
        self.text(
            self.f.small,
            "EN",
            (bx, by, bw - 8.0, input_h),
            c.muted,
            DT_RIGHT,
        );
        self.text(
            self.f.body,
            p.typed,
            (bx + 8.0, by, bw - 40.0, input_h),
            c.text,
            DT_LEFT,
        );
        if (t * 2.0).fract() < 0.6 {
            let cx = self.x(bx + 9.0 + self.width(self.f.body, p.typed));
            let (top, bottom) = (self.y(by + 6.0), self.y(by + input_h - 6.0));
            self.cv
                .line((cx, top), (cx, bottom), self.n(1.4), ORANGE, 1.0);
        }

        // The preview and a divider.
        let py = by + input_h + 3.0;
        self.text(
            self.f.tamil,
            p.preview,
            (bx + 4.0, py, bw - 8.0, preview_h),
            c.text,
            DT_LEFT,
        );
        let sep_y = py + preview_h + 2.0;
        self.cv.round_rect(
            self.x(bx + 4.0),
            self.y(sep_y),
            self.n(bw - 8.0),
            self.n(1.0).max(1.0),
            0.0,
            c.popup_sep,
            1.0,
        );

        // The suggestions.
        let mut ry = sep_y + 5.0;
        for (i, word) in p.rows.iter().enumerate() {
            let on = i == p.selected;
            if on {
                let tint = mix(c.popup, ORANGE, 0.18);
                self.cv.round_rect(
                    self.x(x + 4.0),
                    self.y(ry),
                    self.n(w - 8.0),
                    self.n(row_h),
                    self.n(6.0),
                    tint,
                    1.0,
                );
            }
            let (key_fill, key_text) = if on {
                (ORANGE, TILE)
            } else {
                (c.popup_key, c.popup_key_text)
            };
            let (kx, ky, k) = (x + 10.0, ry + 4.0, 16.0);
            self.cv.round_rect(
                self.x(kx),
                self.y(ky),
                self.n(k),
                self.n(k),
                self.n(4.0),
                key_fill,
                1.0,
            );
            self.text(
                self.f.small,
                &(i + 1).to_string(),
                (kx, ky, k, k),
                key_text,
                DT_CENTER,
            );
            self.text(
                self.f.tamil,
                word,
                (x + 32.0, ry, w - 60.0, row_h),
                c.text,
                DT_LEFT,
            );
            if p.english == Some(i) {
                let cw = self.width(self.f.small, "EN") + 10.0;
                let (cx, cy, ch) = (x + w - 12.0 - cw, ry + 5.0, 14.0);
                self.cv.round_rect(
                    self.x(cx),
                    self.y(cy),
                    self.n(cw),
                    self.n(ch),
                    self.n(7.0),
                    c.popup_key,
                    1.0,
                );
                self.text(
                    self.f.small,
                    "EN",
                    (cx, cy, cw, ch),
                    c.popup_key_text,
                    DT_CENTER,
                );
            }
            if p.learned == Some(i) {
                let (cx, cy) = (self.x(x + w - 20.0), self.y(ry + row_h / 2.0 + 1.0));
                self.cv.star(cx, cy, self.n(5.0), ORANGE, 1.0);
                self.cv
                    .star(cx + self.n(6.0), cy - self.n(5.0), self.n(2.5), ORANGE, 1.0);
            }
            ry += row_h;
        }
    }

    /// Key caps centred on `cx`, joined by "+"; `down(i)` presses key i.
    fn keys(&mut self, cx: f32, y: f32, keys: &[&str], down: impl Fn(usize) -> bool) {
        let h = 32.0;
        let plus = 18.0;
        let widths: Vec<f32> = keys
            .iter()
            .map(|k| (self.width(self.f.button, k) + 22.0).max(36.0))
            .collect();
        let total: f32 = widths.iter().sum::<f32>() + plus * (keys.len().saturating_sub(1)) as f32;
        let mut x = cx - total / 2.0;
        let c = self.c;
        for (i, key) in keys.iter().enumerate() {
            let w = widths[i];
            let pressed = down(i);
            let dy = if pressed { 2.0 } else { 0.0 };
            if !pressed {
                // A thin "edge" under the key cap.
                self.cv.round_rect(
                    self.x(x),
                    self.y(y + 2.0),
                    self.n(w),
                    self.n(h),
                    self.n(7.0),
                    c.key_line,
                    1.0,
                );
            }
            let (fill, text) = if pressed {
                (c.key_down, c.key_down_text)
            } else {
                (c.key, c.text)
            };
            self.cv.round_rect(
                self.x(x),
                self.y(y + dy),
                self.n(w),
                self.n(h),
                self.n(7.0),
                fill,
                1.0,
            );
            self.cv.round_rect_line(
                (self.x(x), self.y(y + dy), self.n(w), self.n(h)),
                self.n(7.0),
                self.n(1.0),
                c.key_line,
                1.0,
            );
            self.text(self.f.button, key, (x, y + dy, w, h), text, DT_CENTER);
            x += w;
            if i + 1 < keys.len() {
                self.text(self.f.button, "+", (x, y, plus, h), c.muted, DT_CENTER);
                x += plus;
            }
        }
    }

    /// The convert badge: spinner (or check) and its label.
    fn badge(&mut self, x: f32, y: f32, done: bool, t: f32) {
        let label = if done {
            "Converted"
        } else {
            "Converting to Tamil…"
        };
        let w = 34.0 + self.width(self.f.button, label) + 14.0;
        let h = 30.0;
        self.cv.round_rect(
            self.x(x),
            self.y(y),
            self.n(w),
            self.n(h),
            self.n(15.0),
            TILE,
            1.0,
        );
        let (cx, cy, r) = (self.x(x + 17.0), self.y(y + 15.0), self.n(6.0));
        if done {
            let p = |dx: f32, dy: f32| (cx + dx * r, cy + dy * r);
            self.cv
                .line(p(-0.55, 0.02), p(-0.12, 0.45), self.n(2.0), ORANGE, 1.0);
            self.cv
                .line(p(-0.12, 0.45), p(0.6, -0.42), self.n(2.0), ORANGE, 1.0);
        } else {
            self.cv.arc(cx, cy, r, self.n(2.0), 0.0, TAU, ORANGE, 0.22);
            let sweep = PI * (0.5 + (0.5 - 0.5 * (t * TAU / 1.4).cos()));
            self.cv
                .arc(cx, cy, r, self.n(2.0), t * TAU / 0.9, sweep, ORANGE, 1.0);
        }
        self.text(
            self.f.button,
            label,
            (x + 32.0, y, w - 32.0, h),
            LETTER,
            DT_LEFT,
        );
    }

    fn icon(&mut self, icon: Option<HICON>, x: f32, y: f32, size: f32) {
        if let Some(icon) = icon {
            let (px, py, ps) = (
                self.x(x).round() as i32,
                self.y(y).round() as i32,
                self.n(size).round() as i32,
            );
            self.cv.icon(icon, px, py, ps);
        }
    }
}

/// `count` characters of `s` (it may hold Tamil).
fn prefix(s: &str, count: usize) -> String {
    s.chars().take(count).collect()
}

/// How many of `len` characters are typed `t` seconds in, starting at
/// `start`, one every `every` seconds.
fn typed(t: f32, start: f32, every: f32, len: usize) -> usize {
    if t < start {
        0
    } else {
        (((t - start) / every) as usize + 1).min(len)
    }
}

fn split_keys(hotkey: &str) -> Vec<&str> {
    hotkey
        .split('+')
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .collect()
}

fn scene_welcome(ctx: &mut Ctx, t: f32) {
    let t = t % 3.8;
    let (w, c) = (STAGE.2, ctx.c);
    ctx.icon(ctx.app_icon, w / 2.0 - 22.0, 22.0, 44.0);
    let word = "vanakkam";
    let n = typed(t, 0.3, 0.09, word.len());
    ctx.text(
        ctx.f.roman,
        &prefix(word, n),
        (0.0, 78.0, w, 30.0),
        c.muted,
        DT_CENTER,
    );
    let a = ((t - 1.3) / 0.45).clamp(0.0, 1.0);
    ctx.text(
        ctx.f.tamil_big,
        "வணக்கம்",
        (0.0, 112.0, w, 70.0),
        mix(c.stage, c.text, a),
        DT_CENTER,
    );
}

fn scene_toggle(ctx: &mut Ctx, t: f32) {
    let t = t % 3.8;
    let (w, c) = (STAGE.2, ctx.c);
    let hotkey = ctx.info.hotkey.clone();
    let keys = split_keys(&hotkey);
    ctx.keys(w / 2.0, 52.0, &keys, |i| {
        t > 0.7 + 0.16 * i as f32 && t < 2.1
    });
    let on = t > 1.25;
    let label = if on {
        "Tamil mode on"
    } else {
        "Tamil mode off"
    };
    let lw = ctx.width(ctx.f.body, label);
    let x = w / 2.0 - (24.0 + 10.0 + lw) / 2.0;
    let icon = if on { ctx.tray.0 } else { ctx.tray.1 };
    ctx.icon(icon, x, 128.0, 24.0);
    let color = if on { c.text } else { c.muted };
    ctx.text(
        ctx.f.body,
        label,
        (x + 34.0, 126.0, lw + 4.0, 28.0),
        color,
        DT_LEFT,
    );
}

/// What the example popup shows (see `Ctx::popup`).
struct Mini<'a> {
    typed: &'a str,
    preview: &'a str,
    rows: &'a [&'a str],
    selected: usize,
    english: Option<usize>,
    learned: Option<usize>,
}

fn scene_pick(ctx: &mut Ctx, t: f32) {
    let t = t % 4.6;
    let word = "natchathiram";
    let n = typed(t, 0.4, 0.08, word.len());
    let done = t >= 2.6;
    let text = if done {
        "நட்சத்திரம் ".to_string()
    } else {
        prefix(word, n)
    };
    ctx.field(20.0, 40.0, 220.0, &text, t, true, false);
    if n >= 3 && !done {
        let typed_word = prefix(word, n);
        let rows = ["நட்சத்திரம்", "நட்சதிரம்", "நட்சாத்திரம்", typed_word.as_str()];
        let mini = Mini {
            typed: &typed_word,
            preview: rows[0],
            rows: &rows,
            selected: 0,
            english: Some(3),
            learned: None,
        };
        ctx.popup(256.0, 8.0, &mini, t);
    }
    ctx.keys(130.0, 118.0, &["Space"], |_| (2.25..2.6).contains(&t));
}

fn scene_english(ctx: &mut Ctx, t: f32) {
    let t = t % 4.4;
    let word = "hello";
    let n = typed(t, 0.5, 0.09, word.len());
    let done = t >= 2.5;
    let text = if done {
        "வணக்கம் hello ".to_string()
    } else {
        format!("வணக்கம் {}", prefix(word, n))
    };
    ctx.field(20.0, 40.0, 220.0, &text, t, true, false);
    if n >= 1 && !done {
        let typed_word = prefix(word, n);
        let rows = ["ஹலோ", "ஹெல்லோ", typed_word.as_str()];
        let selected = if t > 1.6 { 2 } else { 0 };
        let mini = Mini {
            typed: &typed_word,
            preview: rows[selected],
            rows: &rows,
            selected,
            english: Some(2),
            learned: None,
        };
        ctx.popup(256.0, 20.0, &mini, t);
    }
    ctx.keys(130.0, 118.0, &["3"], |_| (2.05..2.5).contains(&t));
}

fn scene_learn(ctx: &mut Ctx, t: f32) {
    let t = t % 5.2;
    let c = ctx.c;
    let first = t < 2.6;
    let (label, note) = if first {
        ("First time", "You pick the second one")
    } else {
        ("Next time", "It comes first, with a sparkle")
    };
    ctx.text(
        ctx.f.title,
        label,
        (20.0, 66.0, 220.0, 32.0),
        c.text,
        DT_LEFT,
    );
    ctx.text(
        ctx.f.body,
        note,
        (20.0, 100.0, 230.0, 26.0),
        c.muted,
        DT_LEFT,
    );
    let (rows, selected, learned) = if first {
        (
            ["தமிழ்", "தமில்", "தாமில்", "thamil"],
            usize::from(t > 1.1),
            None,
        )
    } else {
        (["தமில்", "தமிழ்", "தாமில்", "thamil"], 0, Some(0))
    };
    let mini = Mini {
        typed: "thamil",
        preview: rows[selected],
        rows: &rows,
        selected,
        english: Some(3),
        learned,
    };
    ctx.popup(256.0, 8.0, &mini, t);
}

fn scene_convert(ctx: &mut Ctx, t: f32) {
    let t = t % 5.0;
    let w = STAGE.2;
    let done = t >= 2.3;
    let text = if done {
        "வணக்கம் அம்மா, நன்றி"
    } else {
        "vanakkam amma, nanri"
    };
    ctx.field(76.0, 26.0, 360.0, text, t, false, !done);
    let hotkey = if ctx.info.convert_hotkey.is_empty() {
        DEFAULT_CONVERT.to_string()
    } else {
        ctx.info.convert_hotkey.clone()
    };
    let keys = split_keys(&hotkey);
    ctx.keys(w / 2.0, 84.0, &keys, |i| {
        t > 0.8 + 0.14 * i as f32 && t < 1.7
    });
    if (1.35..3.9).contains(&t) {
        let label = if done {
            "Converted"
        } else {
            "Converting to Tamil…"
        };
        let bw = 34.0 + ctx.width(ctx.f.button, label) + 14.0;
        ctx.badge(w / 2.0 - bw / 2.0, 146.0, done, t);
    }
}

fn scene_gravitide(ctx: &mut Ctx, t: f32) {
    let w = STAGE.2;
    let a = ease_out((t / 0.6).min(1.0));
    if let Some(logo) = ctx.logo {
        let x = (ctx.x(w / 2.0) - logo.w as f32 / 2.0).round() as i32;
        let y = (ctx.y(40.0 + 8.0 * (1.0 - a))).round() as i32;
        ctx.cv.image_faded(logo, x, y, a);
    }
    let (bw, bh) = (180.0, 34.0);
    let (bx, by) = (w / 2.0 - bw / 2.0, 150.0);
    let fill = if ctx.link_hover {
        mix(ORANGE, ctx.c.stage, 0.75)
    } else {
        ctx.c.stage
    };
    ctx.cv.round_rect(
        ctx.x(bx),
        ctx.y(by),
        ctx.n(bw),
        ctx.n(bh),
        ctx.n(17.0),
        fill,
        1.0,
    );
    ctx.cv.round_rect_line(
        (ctx.x(bx), ctx.y(by), ctx.n(bw), ctx.n(bh)),
        ctx.n(17.0),
        ctx.n(1.2),
        ORANGE,
        1.0,
    );
    let text = ctx.c.text;
    ctx.text(
        ctx.f.button,
        "Visit gravitide.dev  ↗",
        (bx, by, bw, bh),
        text,
        DT_CENTER,
    );
}

// ---------------------------------------------------------------- window

fn track_leave(hwnd: HWND) {
    let mut tme = TRACKMOUSEEVENT {
        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
        dwFlags: TME_LEAVE,
        hwndTrack: hwnd,
        dwHoverTime: 0,
    };
    // SAFETY: asking for WM_MOUSELEAVE on our own window.
    let _ = unsafe { TrackMouseEvent(&mut tme) };
}

/// Acts on a click or key (Next on the last card closes with "Start typing").
fn act(hit: Hit) {
    let action = WELCOME.with(|w| {
        let mut w = w.borrow_mut();
        let w = w.as_mut()?;
        match hit {
            Hit::Next if w.card == LAST => {
                w.start_typing = true;
                Some(w.hwnd)
            }
            Hit::Next => {
                let next = w.card + 1;
                w.go(next);
                None
            }
            Hit::Back if w.card > 0 => {
                let back = w.card - 1;
                w.go(back);
                None
            }
            Hit::Skip => {
                w.go(LAST);
                None
            }
            Hit::Link => {
                // SAFETY: plain shell open of our website.
                unsafe {
                    ShellExecuteW(
                        None,
                        w!("open"),
                        &HSTRING::from(SITE),
                        None,
                        None,
                        SW_SHOWNORMAL,
                    );
                }
                None
            }
            Hit::Back => None,
        }
    });
    if let Some(hwnd) = action {
        // SAFETY: closing our own window (outside the state borrow).
        let _ = unsafe { DestroyWindow(hwnd) };
    }
}

fn mouse_at(lparam: LPARAM) -> POINT {
    POINT {
        x: (lparam.0 & 0xFFFF) as i16 as i32,
        y: ((lparam.0 >> 16) & 0xFFFF) as i16 as i32,
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_TIMER if wparam.0 == TIMER => {
            // SAFETY: our own window; painting happens in WM_PAINT.
            let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            // SAFETY: Begin/EndPaint pair on our own window.
            unsafe {
                let hdc = BeginPaint(hwnd, &mut ps);
                WELCOME.with(|w| {
                    if let Some(w) = w.borrow_mut().as_mut() {
                        w.render();
                        if let Some(canvas) = w.canvas.as_mut() {
                            canvas.blit(hdc);
                        }
                    }
                });
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let p = mouse_at(lparam);
            track_leave(hwnd);
            WELCOME.with(|w| {
                if let Some(w) = w.borrow_mut().as_mut() {
                    w.hover = w.hit_at(p.x, p.y);
                }
            });
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            WELCOME.with(|w| {
                if let Some(w) = w.borrow_mut().as_mut() {
                    w.hover = None;
                    w.pressed = None;
                }
            });
            LRESULT(0)
        }
        WM_SETCURSOR => {
            let over = WELCOME.with(|w| w.borrow().as_ref().is_some_and(|w| w.hover.is_some()));
            if over && (lparam.0 & 0xFFFF) as u32 == 1 {
                // SAFETY: the system hand cursor.
                unsafe {
                    if let Ok(hand) = LoadCursorW(None, IDC_HAND) {
                        SetCursor(Some(hand));
                    }
                }
                return LRESULT(1);
            }
            // SAFETY: default processing.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_LBUTTONDOWN => {
            let p = mouse_at(lparam);
            WELCOME.with(|w| {
                if let Some(w) = w.borrow_mut().as_mut() {
                    w.pressed = w.hit_at(p.x, p.y);
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let p = mouse_at(lparam);
            let hit = WELCOME.with(|w| {
                let mut w = w.borrow_mut();
                let w = w.as_mut()?;
                let hit = w.hit_at(p.x, p.y);
                let pressed = w.pressed.take();
                (hit.is_some() && hit == pressed).then_some(hit).flatten()
            });
            if let Some(hit) = hit {
                act(hit);
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let vk = wparam.0 as u16;
            if vk == VK_RIGHT.0 || vk == VK_RETURN.0 || vk == VK_SPACE.0 {
                act(Hit::Next);
            } else if vk == VK_LEFT.0 || vk == VK_BACK.0 {
                act(Hit::Back);
            } else if vk == VK_ESCAPE.0 {
                // SAFETY: closing our own window.
                let _ = unsafe { DestroyWindow(hwnd) };
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            if let Some(w) = WELCOME.with(|w| w.borrow_mut().take()) {
                // SAFETY: our timer and icons; nothing draws with them now.
                unsafe {
                    let _ = KillTimer(Some(hwnd), TIMER);
                    for icon in [w.app_icon, w.tray_on, w.tray_off].into_iter().flatten() {
                        let _ = DestroyIcon(icon);
                    }
                }
                let (on_close, start) = (w.on_close, w.start_typing);
                drop(w);
                on_close(start);
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
        DispatchMessageW, GetMessageW, MSG, PostQuitMessage, TranslateMessage,
    };

    #[test]
    fn typing_runs_through_the_word() {
        assert_eq!(typed(0.0, 0.3, 0.1, 8), 0);
        assert_eq!(typed(0.3, 0.3, 0.1, 8), 1);
        assert_eq!(typed(0.55, 0.3, 0.1, 8), 3);
        assert_eq!(typed(9.0, 0.3, 0.1, 8), 8);
        assert_eq!(prefix("வணக்கம்", 2), "வண");
        assert_eq!(split_keys("Ctrl + Alt+T"), ["Ctrl", "Alt", "T"]);
    }

    #[test]
    fn cards_mention_the_users_shortcuts() {
        let info = Info {
            hotkey: "Ctrl+Space".into(),
            convert_hotkey: String::new(),
            dark: false,
        };
        assert!(body(1, &info).contains("Ctrl+Space"));
        assert!(body(5, &info).contains("Settings › Shortcuts"));
    }

    /// Every card renders, in both themes, without panicking.
    #[test]
    fn renders_every_card() {
        fn closed(_: bool) {}
        for dark in [false, true] {
            let info = Info {
                hotkey: "Ctrl+Alt+T".into(),
                convert_hotkey: "Ctrl+Alt+Shift+T".into(),
                dark,
            };
            open(info, closed);
            let h = hwnd().expect("welcome window");
            for card in 0..CARDS.len() {
                WELCOME.with(|w| {
                    let mut w = w.borrow_mut();
                    let w = w.as_mut().unwrap();
                    w.go(card);
                    w.render();
                    assert!(w.canvas.is_some());
                });
            }
            // SAFETY: closing our own window.
            let _ = unsafe { DestroyWindow(h) };
            assert!(hwnd().is_none());
        }
    }

    /// Opens the tour for a while (for screenshots). Run with:
    /// cargo test -p ezhuthu welcome_preview -- --ignored
    /// (EZHUTHU_PREVIEW_CARD=n starts on card n; EZHUTHU_PREVIEW_DARK=1 for dark.)
    #[test]
    #[ignore]
    fn welcome_preview() {
        fn closed(_: bool) {
            // SAFETY: ends the test's message loop.
            unsafe { PostQuitMessage(0) };
        }
        let dark = std::env::var("EZHUTHU_PREVIEW_DARK").as_deref() == Ok("1");
        let info = Info {
            hotkey: "Ctrl+Alt+T".into(),
            convert_hotkey: "Ctrl+Alt+Shift+T".into(),
            dark,
        };
        open(info, closed);
        if let Some(card) = std::env::var("EZHUTHU_PREVIEW_CARD")
            .ok()
            .and_then(|c| c.parse::<usize>().ok())
        {
            WELCOME.with(|w| {
                if let Some(w) = w.borrow_mut().as_mut() {
                    w.card = card.min(LAST);
                    w.since = Instant::now();
                }
            });
        }
        let start = Instant::now();
        // SAFETY: a plain message loop; the window closes itself after 15 s.
        unsafe {
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                if start.elapsed().as_secs() >= 15
                    && let Some(h) = hwnd()
                {
                    let _ = DestroyWindow(h);
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}
