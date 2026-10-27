//! The popup: a non-activating, topmost window with what's being typed and
//! its suggestions.
//!
//! It never takes focus (the target app keeps it, which is what lets this
//! work over the Start/search box). It is drawn on a `Canvas` (smooth
//! rounded shapes; text by DrawTextW, which shapes Tamil through Uniscribe,
//! in "Nirmala UI"). On Windows 11 the system rounds its corners and draws
//! the border and shadow; elsewhere it has a 1 px border of its own.
//!
//! Layout, top to bottom: the typed text in an input box (labelled EN), the
//! Tamil preview, then the suggestions (numbered key caps, the selected one
//! tinted orange, the typed word last with an EN tag), then key tips or a
//! status line.

use anyhow::Result;
use windows::Win32::Foundation::{COLORREF, HWND, RECT};
use windows::Win32::Graphics::Dwm::{
    DWMWA_BORDER_COLOR, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW, DEFAULT_CHARSET, DT_CENTER, DT_LEFT,
    DT_RIGHT, DT_SINGLELINE, DT_VCENTER, DeleteObject, FW_NORMAL, FW_SEMIBOLD, GetDC,
    GetTextMetricsW, HDC, HFONT, HGDIOBJ, OUT_DEFAULT_PRECIS, ReleaseDC, SelectObject, TEXTMETRICW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CS_DROPSHADOW, CreateWindowExW, IDC_ARROW, LoadCursorW, RegisterClassExW, WNDCLASSEXW, WNDPROC,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::{PCWSTR, w};

use crate::canvas::{Canvas, Rgb, mix};
use crate::win::{instance, scale, wide};

/// What to draw, built from the app state on each paint.
pub struct View {
    pub input: String,
    /// The caret's place in `input`, in characters.
    pub cursor: usize,
    pub preview: String,
    pub candidates: Vec<String>,
    /// The row holding the word as typed (inserted as English), if shown.
    pub english: Option<usize>,
    /// The row holding a learned pick (marked with a sparkle), if any.
    pub learned: Option<usize>,
    /// The rest of the word as once typed, dimmed after the caret (empty: none).
    pub ghost: String,
    /// The row being completed, the whole word, and the text it is shown
    /// going on from (the rest is dimmed; in the preview too when selected).
    pub completion: Option<(usize, String, String)>,
    /// Tab inserts a completion (the key tips say so).
    pub can_complete: bool,
    pub selected: usize,
    /// A status line (loading, errors); empty shows the key tips instead.
    pub hint: String,
    pub warn: bool,
}

/// Ezhuthu's accent (the logo's cursor), and dark text on it.
const ORANGE: Rgb = 0xF5A524;
const ON_ORANGE: Rgb = 0x15171C;
/// Popup width (96-DPI units).
const WIDTH: i32 = 300;

struct Fonts {
    input: HFONT,
    tamil: HFONT,
    cand: HFONT,
    small: HFONT,
    key: HFONT,
    input_h: i32,
    tamil_h: i32,
    cand_h: i32,
    small_h: i32,
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
        let (input, tamil, cand, small, key) = (
            make(110, normal, "Segoe UI"),
            make(150, normal, "Nirmala UI"),
            make(125, normal, "Nirmala UI"),
            make(80, normal, "Segoe UI"),
            make(80, semibold, "Segoe UI"),
        );
        Self {
            input_h: line_height(input),
            tamil_h: line_height(tamil),
            cand_h: line_height(cand),
            small_h: line_height(small),
            input,
            tamil,
            cand,
            small,
            key,
        }
    }
}

impl Drop for Fonts {
    fn drop(&mut self) {
        for f in [self.input, self.tamil, self.cand, self.small, self.key] {
            // SAFETY: we created these fonts and they are not selected anywhere.
            let _ = unsafe { DeleteObject(HGDIOBJ(f.0)) };
        }
    }
}

fn line_height(font: HFONT) -> i32 {
    // SAFETY: the screen DC is released and the font deselected before returning.
    unsafe {
        let dc = GetDC(None);
        let old = SelectObject(dc, HGDIOBJ(font.0));
        let mut tm = TEXTMETRICW::default();
        let _ = GetTextMetricsW(dc, &mut tm);
        SelectObject(dc, old);
        ReleaseDC(None, dc);
        tm.tmHeight
    }
}

/// The popup's colours, light or dark.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Palette {
    background: Rgb,
    /// Drawn only where Windows doesn't draw the border (before Windows 11).
    border: Rgb,
    input: Rgb,
    input_line: Rgb,
    separator: Rgb,
    text: Rgb,
    muted: Rgb,
    /// Key caps (numbers, key tips) and the EN tags.
    key: Rgb,
    key_text: Rgb,
    warning: Rgb,
}

impl Palette {
    const LIGHT: Self = Self {
        background: 0xFFFFFF,
        border: 0xC8C8C8,
        input: 0xF4F5F7,
        input_line: 0xE1E3E6,
        separator: 0xECECEC,
        text: 0x1B1B1B,
        muted: 0x6E6E6E,
        key: 0xEEF0F2,
        key_text: 0x555555,
        warning: 0xC02000,
    };
    /// Close to Windows 11's dark menus and flyouts.
    const DARK: Self = Self {
        background: 0x2B2B2B,
        border: 0x454545,
        input: 0x1F1F1F,
        input_line: 0x3D3D3D,
        separator: 0x3A3A3A,
        text: 0xF2F2F2,
        muted: 0xA3A3A3,
        key: 0x3A3A3A,
        key_text: 0xCFCFCF,
        warning: 0xFF7B6B,
    };

    /// The selected row's tint: orange washed into the background.
    fn selected(&self) -> Rgb {
        mix(self.background, ORANGE, 0.18)
    }
}

struct Layout {
    w: i32,
    h: i32,
    input: RECT,
    preview: RECT,
    sep: RECT,
    rows: Vec<RECT>,
    hint: RECT,
}

pub struct Popup {
    pub hwnd: HWND,
    dpi: u32,
    rows: usize,
    fonts: Fonts,
    palette: Palette,
    /// Windows rounds the corners and draws the border (Windows 11).
    system_frame: bool,
}

impl Popup {
    pub fn create(wndproc: WNDPROC, rows: usize) -> Result<Self> {
        let class = w!("ezhuthu.popup");
        // SAFETY: the class struct is fully initialised; the window is owned by
        // this thread and destroyed with the process.
        let hwnd = unsafe {
            let wc = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                style: CS_DROPSHADOW,
                lpfnWndProc: wndproc,
                hInstance: instance(),
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                lpszClassName: class,
                ..Default::default()
            };
            RegisterClassExW(&wc);
            CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class,
                w!("ezhuthu"),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance()),
                None,
            )?
        };
        // Rounded corners with the system border and shadow on Windows 11;
        // the call fails on older Windows, where we draw a border instead.
        let round = DWMWCP_ROUND;
        // SAFETY: the attribute value is a DWM_WINDOW_CORNER_PREFERENCE of
        // the size passed.
        let system_frame = unsafe {
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &round as *const _ as *const _,
                size_of_val(&round) as u32,
            )
        }
        .is_ok();
        let mut popup = Self {
            hwnd,
            dpi: 96,
            rows,
            fonts: Fonts::new(96),
            palette: Palette::LIGHT,
            system_frame,
        };
        popup.set_dark(false);
        Ok(popup)
    }

    pub fn set_dpi(&mut self, dpi: u32) {
        if dpi != self.dpi {
            self.dpi = dpi;
            self.fonts = Fonts::new(dpi);
        }
    }

    /// Light or dark colours (takes effect at the next paint).
    pub fn set_dark(&mut self, dark: bool) {
        self.palette = if dark { Palette::DARK } else { Palette::LIGHT };
        if self.system_frame {
            // The system border in the palette's colour (0x00BBGGRR).
            let c = self.palette.border;
            let color = COLORREF(((c & 0xFF) << 16) | (c & 0xFF00) | ((c >> 16) & 0xFF));
            // SAFETY: the attribute value is a COLORREF of the size passed.
            let _ = unsafe {
                DwmSetWindowAttribute(
                    self.hwnd,
                    DWMWA_BORDER_COLOR,
                    &color as *const _ as *const _,
                    size_of::<COLORREF>() as u32,
                )
            };
        }
    }

    pub fn set_rows(&mut self, rows: usize) {
        self.rows = rows;
    }

    /// Sizes the popup for `rows` rows; the caller resizes the window to
    /// `size()`.
    pub fn set_list(&mut self, rows: usize) {
        self.rows = rows;
    }

    pub fn size(&self) -> (i32, i32) {
        let l = self.layout();
        (l.w, l.h)
    }

    pub fn gap(&self) -> i32 {
        scale(6, self.dpi)
    }

    fn layout(&self) -> Layout {
        let s = |v| scale(v, self.dpi);
        let f = &self.fonts;
        let (pad, w) = (s(8), s(WIDTH));
        let input = RECT {
            left: pad,
            top: pad + s(2),
            right: w - pad,
            bottom: pad + s(2) + f.input_h + s(14),
        };
        let inner = |top, h| RECT {
            left: pad + s(4),
            top,
            right: w - pad - s(4),
            bottom: top + h,
        };
        let preview = inner(input.bottom + s(4), f.tamil_h + s(6));
        let sep = inner(preview.bottom + s(4), s(1).max(1));
        let row_h = f.cand_h + s(8);
        let mut top = sep.bottom + s(5);
        let rows = (0..self.rows)
            .map(|_| {
                let r = RECT {
                    left: s(5),
                    top,
                    right: w - s(5),
                    bottom: top + row_h,
                };
                top += row_h + s(1);
                r
            })
            .collect();
        let hint = inner(top + s(3), f.small_h + s(8));
        Layout {
            w,
            h: hint.bottom + pad,
            input,
            preview,
            sep,
            rows,
            hint,
        }
    }

    pub fn row_at(&self, x: i32, y: i32) -> Option<usize> {
        self.layout()
            .rows
            .iter()
            .position(|r| x >= r.left && x < r.right && y >= r.top && y < r.bottom)
    }

    /// Paints `view` into `hdc` (through a canvas, so there is no flicker).
    pub fn paint(&self, hdc: HDC, view: &View) {
        let l = self.layout();
        let Some(mut cv) = Canvas::new(l.w, l.h) else {
            return;
        };
        let (f, p) = (&self.fonts, &self.palette);
        let s = |v: i32| scale(v, self.dpi) as f32;
        let fr = |r: &RECT| {
            (
                r.left as f32,
                r.top as f32,
                (r.right - r.left) as f32,
                (r.bottom - r.top) as f32,
            )
        };
        let one_line = DT_SINGLELINE | DT_VCENTER;

        cv.clear(p.background);
        if !self.system_frame {
            cv.round_rect_line((0.0, 0.0, l.w as f32, l.h as f32), 0.0, 1.0, p.border, 1.0);
        }

        // The typed text, in its input box with an orange caret and "EN".
        let (x, y, bw, bh) = fr(&l.input);
        cv.round_rect(x, y, bw, bh, s(7), p.input, 1.0);
        cv.round_rect_line((x, y, bw, bh), s(7), s(1).max(1.0), p.input_line, 1.0);
        let en_w = cv.measure(f.small, "EN").0;
        let text_box = RECT {
            left: l.input.left + scale(10, self.dpi),
            right: l.input.right - scale(14, self.dpi) - en_w,
            ..l.input
        };
        cv.text(
            f.small,
            "EN",
            RECT {
                right: l.input.right - scale(10, self.dpi),
                ..l.input
            },
            p.muted,
            DT_RIGHT | one_line,
        );
        if view.input.is_empty() {
            cv.text(
                f.input,
                "type Tamil in English letters…",
                text_box,
                p.muted,
                DT_LEFT | one_line,
            );
        } else {
            let end = text_tail(&mut cv, f.input, &view.input, text_box, p.text);
            // The caret sits after the first `cursor` characters: measured
            // from the left when the text fits, else from its right end.
            let before: String = view.input.chars().take(view.cursor).collect();
            let after: String = view.input.chars().skip(view.cursor).collect();
            let caret = if end < text_box.right {
                text_box.left + cv.measure(f.input, &before).0
            } else {
                end - cv.measure(f.input, &after).0
            };
            let caret_x = (caret + scale(1, self.dpi)) as f32;
            let (cy, ch) = (y + bh / 2.0, f.input_h as f32 * 0.42);
            cv.line(
                (caret_x, cy - ch),
                (caret_x, cy + ch),
                s(2).max(1.5) * 0.75,
                ORANGE,
                1.0,
            );
            // The rest of the word as once typed, dimmed after the caret.
            if !view.ghost.is_empty() && end < text_box.right {
                let ghost_box = RECT {
                    left: end + scale(3, self.dpi),
                    ..text_box
                };
                cv.text(f.input, &view.ghost, ghost_box, p.muted, DT_LEFT | one_line);
            }
        }

        // The Tamil preview of the chosen suggestion, with the completion's
        // rest dimmed when that's the suggestion being completed.
        match &view.completion {
            Some((row, word, from))
                if *row == view.selected && view.candidates.get(*row) == Some(&view.preview) =>
            {
                completed(&mut cv, f.tamil, l.preview, word, from, (p.text, p.muted));
            }
            _ => {
                text_tail(&mut cv, f.tamil, &view.preview, l.preview, p.text);
            }
        }
        let (sx, sy, sw, _) = fr(&l.sep);
        cv.round_rect(sx, sy, sw, s(1).max(1.0), 0.0, p.separator, 1.0);

        // The suggestions.
        for (i, (row, word)) in l.rows.iter().zip(&view.candidates).enumerate() {
            let selected = i == view.selected;
            let (rx, ry, rw, rh) = fr(row);
            if selected {
                cv.round_rect(rx, ry, rw, rh, s(7), p.selected(), 1.0);
            }
            // The number's key cap (the tenth row is picked with 0).
            let k = s(20);
            let (kx, ky) = (rx + s(8), ry + (rh - k) / 2.0);
            let (key_fill, key_text) = if selected {
                (ORANGE, ON_ORANGE)
            } else {
                (p.key, p.key_text)
            };
            cv.round_rect(kx, ky, k, k, s(5), key_fill, 1.0);
            let key_rect = RECT {
                left: kx as i32,
                top: ky as i32,
                right: (kx + k) as i32,
                bottom: (ky + k) as i32,
            };
            let number = ((i + 1) % 10).to_string();
            cv.text(f.key, &number, key_rect, key_text, DT_CENTER | one_line);
            let body = RECT {
                left: row.left + scale(38, self.dpi),
                right: row.right - scale(8, self.dpi),
                ..*row
            };
            // A chip at the right edge: "EN" on the English row, "Tab" on the
            // suggestion being completed.
            let chip = |cv: &mut Canvas, text: &str| -> f32 {
                let tw = cv.measure(f.small, text).0 as f32;
                let (cw, ch) = (tw + s(12), s(16));
                let cx = rx + rw - s(10) - cw;
                let cy = ry + (rh - ch) / 2.0;
                cv.round_rect(cx, cy, cw, ch, ch / 2.0, p.key, 1.0);
                let r = RECT {
                    left: cx as i32,
                    top: cy as i32,
                    right: (cx + cw) as i32,
                    bottom: (cy + ch) as i32,
                };
                cv.text(f.small, text, r, p.key_text, DT_CENTER | one_line);
                cw + s(6)
            };
            let mut used_right = 0.0;
            match &view.completion {
                Some((done_row, done, from)) if *done_row == i => {
                    used_right = chip(&mut cv, "Tab");
                    let room = RECT {
                        right: body.right - used_right as i32,
                        ..body
                    };
                    completed(&mut cv, f.cand, room, done, from, (p.text, p.muted));
                }
                _ => cv.text(f.cand, word, body, p.text, DT_LEFT | one_line),
            }
            if view.english == Some(i) {
                chip(&mut cv, "EN");
            }
            if view.learned == Some(i) {
                // A sparkle: a big four-pointed star and a small one above it.
                let (cx, cy) = (rx + rw - s(18) - used_right, ry + rh / 2.0 + s(1));
                cv.star(cx, cy, s(6), ORANGE, 1.0);
                cv.star(cx + s(7), cy - s(6), s(3), ORANGE, 1.0);
            }
        }

        // Key tips, or a status line.
        if view.hint.is_empty() {
            self.key_tips(&mut cv, l.hint, view.can_complete);
        } else {
            let color = if view.warn { p.warning } else { p.muted };
            cv.text(f.small, &view.hint, l.hint, color, DT_CENTER | one_line);
        }

        cv.blit(hdc);
    }

    /// "↑↓ choose  Space insert  Esc cancel" (or "Tab complete" first when a
    /// completion is showing), each key on a small key cap.
    fn key_tips(&self, cv: &mut Canvas, r: RECT, can_complete: bool) {
        let (f, p) = (&self.fonts, &self.palette);
        let s = |v: i32| scale(v, self.dpi);
        let tips = if can_complete {
            [("Tab", "complete"), ("Space", "insert"), ("Esc", "cancel")]
        } else {
            [("↑↓", "choose"), ("Space", "insert"), ("Esc", "cancel")]
        };
        let parts: Vec<(i32, i32)> = tips
            .iter()
            .map(|(key, label)| {
                (
                    cv.measure(f.small, key).0 + s(8),
                    cv.measure(f.small, label).0,
                )
            })
            .collect();
        let gap = s(4);
        let space = s(12);
        let total: i32 =
            parts.iter().map(|(k, t)| k + gap + t).sum::<i32>() + space * (tips.len() as i32 - 1);
        let mut x = r.left + ((r.right - r.left) - total) / 2;
        let cap_h = s(16);
        let cy = r.top + ((r.bottom - r.top) - cap_h) / 2;
        let one_line = DT_SINGLELINE | DT_VCENTER;
        for ((key, label), (kw, tw)) in tips.iter().zip(parts) {
            cv.round_rect(
                x as f32,
                cy as f32,
                kw as f32,
                cap_h as f32,
                s(4) as f32,
                p.key,
                1.0,
            );
            let cap = RECT {
                left: x,
                top: cy,
                right: x + kw,
                bottom: cy + cap_h,
            };
            cv.text(f.small, key, cap, p.key_text, DT_CENTER | one_line);
            x += kw + gap;
            let text = RECT {
                left: x,
                right: x + tw + s(2),
                ..r
            };
            cv.text(f.small, label, text, p.muted, DT_LEFT | one_line);
            x += tw + space;
        }
    }
}

/// Draws `word`, which goes on from `shown` (a suggestion), as `shown`'s
/// part in `colors.0` and the rest in `colors.1` (dimmed). The split falls
/// where a letter is whole, so no vowel sign or pulli is cut off from it.
fn completed(cv: &mut Canvas, font: HFONT, r: RECT, word: &str, shown: &str, colors: (Rgb, Rgb)) {
    let is_mark = |c: char| matches!(c, '\u{0B82}' | '\u{0BBE}'..='\u{0BCD}' | '\u{0BD7}');
    let stem = shown.strip_suffix('\u{0BCD}').unwrap_or(shown);
    let mut split = if word.starts_with(stem) {
        stem.len()
    } else {
        0
    };
    while split > 0 && word[split..].chars().next().is_some_and(is_mark) {
        split = word[..split].char_indices().last().map_or(0, |(i, _)| i);
    }
    let (head, rest) = word.split_at(split);
    let one_line = DT_SINGLELINE | DT_VCENTER;
    cv.text(font, head, r, colors.0, DT_LEFT | one_line);
    let rest_box = RECT {
        left: r.left + cv.measure(font, head).0,
        ..r
    };
    cv.text(font, rest, rest_box, colors.1, DT_LEFT | one_line);
}

/// Draws `s` left-aligned in `r`, or right-aligned when too long so the end
/// (where the user is typing) stays visible. Returns the x where it ends.
fn text_tail(cv: &mut Canvas, font: HFONT, s: &str, r: RECT, color: Rgb) -> i32 {
    if s.is_empty() {
        return r.left;
    }
    let width = cv.measure(font, s).0;
    let fits = width <= r.right - r.left;
    let align = if fits { DT_LEFT } else { DT_RIGHT };
    cv.text(font, s, r, color, align | DT_SINGLELINE | DT_VCENTER);
    if fits { r.left + width } else { r.right }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows::Win32::Graphics::Gdi::{CreateCompatibleDC, DeleteDC};
    use windows::Win32::UI::WindowsAndMessaging::DefWindowProcW;

    unsafe extern "system" fn proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        // SAFETY: default processing for a test window.
        unsafe { DefWindowProcW(h, m, w, l) }
    }

    fn view(input: &str, preview: &str, candidates: &[&str]) -> View {
        View {
            input: input.into(),
            cursor: input.chars().count(),
            preview: preview.into(),
            candidates: candidates.iter().map(|s| s.to_string()).collect(),
            english: candidates.len().checked_sub(1),
            learned: None,
            ghost: String::new(),
            completion: None,
            can_complete: false,
            selected: 0,
            hint: String::new(),
            warn: false,
        }
    }

    /// Paints a sample popup in both themes to `popup-light.bmp` and
    /// `popup-dark.bmp` under target/ (for screenshots). Run with:
    /// cargo test -p ezhuthu popup_preview -- --ignored
    #[test]
    #[ignore]
    fn popup_preview() {
        use windows::Win32::Graphics::Gdi::{
            BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleBitmap, DIB_RGB_COLORS, GetDIBits,
        };
        let mut popup = Popup::create(Some(proc), 6).unwrap();
        popup.set_dpi(144);
        let mut v = view(
            "vanakkam",
            "வணக்கம்",
            &["வணக்கம்", "வனக்கம்", "வணக்காம்", "வானக்கம்", "வனாக்கம்", "vanakkam"],
        );
        v.learned = Some(0);
        // Typing "vanakk", with a completion from the typing history.
        let mut complete = view(
            "vanakk",
            "வணக்க்",
            &["வணக்க்", "வனக்க்", "வனக்", "வானக்க்", "வனாக்க்", "vanakk"],
        );
        complete.ghost = "am".into();
        complete.completion = Some((0, "வணக்கம்".into(), "வணக்க்".into()));
        complete.can_complete = true;
        let samples = [
            (false, "light", &v),
            (true, "dark", &v),
            (false, "complete-light", &complete),
            (true, "complete-dark", &complete),
        ];
        for (dark, name, v) in samples {
            popup.set_list(v.candidates.len());
            let (w, h) = popup.size();
            popup.set_dark(dark);
            // SAFETY: a screen-compatible bitmap painted, read back within its
            // size and deleted, with every selection restored.
            unsafe {
                let screen = GetDC(None);
                let dc = CreateCompatibleDC(Some(screen));
                let bmp = CreateCompatibleBitmap(screen, w, h);
                let old = SelectObject(dc, HGDIOBJ(bmp.0));
                popup.paint(dc, v);
                SelectObject(dc, old);
                let mut info = BITMAPINFO {
                    bmiHeader: BITMAPINFOHEADER {
                        biSize: size_of::<BITMAPINFOHEADER>() as u32,
                        biWidth: w,
                        biHeight: h,
                        biPlanes: 1,
                        biBitCount: 32,
                        biCompression: BI_RGB.0,
                        ..Default::default()
                    },
                    ..Default::default()
                };
                let mut px = vec![0u8; (w * h * 4) as usize];
                GetDIBits(
                    dc,
                    bmp,
                    0,
                    h as u32,
                    Some(px.as_mut_ptr() as *mut _),
                    &mut info,
                    DIB_RGB_COLORS,
                );
                let _ = DeleteObject(HGDIOBJ(bmp.0));
                let _ = DeleteDC(dc);
                ReleaseDC(None, screen);
                let mut file = Vec::new();
                file.extend(b"BM");
                file.extend((54 + px.len() as u32).to_le_bytes());
                file.extend([0u8; 4]);
                file.extend(54u32.to_le_bytes());
                file.extend(40u32.to_le_bytes());
                file.extend(w.to_le_bytes());
                file.extend(h.to_le_bytes());
                file.extend(1u16.to_le_bytes());
                file.extend(32u16.to_le_bytes());
                file.extend([0u8; 24]);
                file.extend(&px);
                let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join(format!("../../target/popup-{name}.bmp"));
                std::fs::write(path, file).unwrap();
            }
        }
    }

    /// Regression: painting the just-opened popup (empty preview and hint)
    /// crashed inside DrawTextW.
    #[test]
    fn paints_empty_and_filled_views() {
        let mut popup = Popup::create(Some(proc), 5).unwrap();
        // SAFETY: a memory DC, deleted below.
        let dc = unsafe { CreateCompatibleDC(None) };
        for dpi in [96, 144] {
            popup.set_dpi(dpi);
            popup.paint(dc, &view("", "", &[]));
            popup.paint(dc, &view("amma", "அம்மா", &["அம்மா", "ஆம்மா", "amma"]));
            let mut learned = view("amma", "அம்மா", &["அம்மா", "ஆம்மா", "amma"]);
            learned.learned = Some(0);
            popup.paint(dc, &learned);
            let mut status = view("amma", "", &[]);
            status.hint = "Loading the model…".into();
            popup.paint(dc, &status);
        }
        // SAFETY: we created it.
        let _ = unsafe { DeleteDC(dc) };
    }

    #[test]
    fn rows_are_clickable_where_drawn() {
        let popup = Popup::create(Some(proc), 3).unwrap();
        let l = popup.layout();
        let r = l.rows[1];
        assert_eq!(popup.row_at(r.left + 4, r.top + 2), Some(1));
        assert_eq!(popup.row_at(r.left + 4, l.input.top), None);
    }
}
