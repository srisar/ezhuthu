//! The popup: a non-activating, topmost window painted with GDI.
//!
//! It never takes focus (the target app keeps it, which is what lets this
//! work over the Start/search box). Text is drawn with DrawTextW, which
//! shapes Tamil through Uniscribe, in "Nirmala UI" (ships with Windows).

use anyhow::Result;
use windows::Win32::Foundation::{COLORREF, HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, COLOR_3DLIGHT, COLOR_GRAYTEXT, COLOR_HIGHLIGHT,
    COLOR_HIGHLIGHTTEXT, COLOR_WINDOW, COLOR_WINDOWTEXT, CreateCompatibleBitmap,
    CreateCompatibleDC, CreateFontW, DEFAULT_CHARSET, DRAW_TEXT_FORMAT, DT_CALCRECT, DT_LEFT,
    DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DT_VCENTER, DeleteDC, DeleteObject, DrawTextW, FW_NORMAL,
    FillRect, GetDC, GetSysColor, GetSysColorBrush, GetTextMetricsW, HDC, HFONT, HGDIOBJ,
    OUT_DEFAULT_PRECIS, ReleaseDC, SRCCOPY, SelectObject, SetBkMode, SetTextColor, TEXTMETRICW,
    TRANSPARENT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CS_DROPSHADOW, CreateWindowExW, IDC_ARROW, LoadCursorW, RegisterClassExW, WNDCLASSEXW, WNDPROC,
    WS_BORDER, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::{PCWSTR, w};

use crate::win::{instance, scale, wide};

/// What to draw, built from the app state on each paint.
pub struct View {
    pub input: String,
    pub preview: String,
    pub candidates: Vec<String>,
    pub selected: usize,
    pub hint: String,
    pub warn: bool,
}

struct Fonts {
    input: HFONT,
    tamil: HFONT,
    cand: HFONT,
    small: HFONT,
    input_h: i32,
    tamil_h: i32,
    cand_h: i32,
    small_h: i32,
}

impl Fonts {
    fn new(dpi: u32) -> Self {
        let make = |pt: i32, face: &str| {
            let face = wide(face);
            // SAFETY: the face name is NUL-terminated and outlives the call.
            unsafe {
                CreateFontW(
                    -(pt * dpi as i32 / 72),
                    0,
                    0,
                    0,
                    FW_NORMAL.0 as i32,
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
        let (input, tamil, cand, small) = (
            make(11, "Segoe UI"),
            make(15, "Nirmala UI"),
            make(13, "Nirmala UI"),
            make(9, "Segoe UI"),
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
        }
    }
}

impl Drop for Fonts {
    fn drop(&mut self) {
        for f in [self.input, self.tamil, self.cand, self.small] {
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
                WS_POPUP | WS_BORDER,
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
        Ok(Self {
            hwnd,
            dpi: 96,
            rows,
            fonts: Fonts::new(96),
        })
    }

    pub fn set_dpi(&mut self, dpi: u32) {
        if dpi != self.dpi {
            self.dpi = dpi;
            self.fonts = Fonts::new(dpi);
        }
    }

    pub fn rows(&self) -> usize {
        self.rows
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
        let (pad, w) = (s(10), s(440));
        let inner = |top, h| RECT {
            left: pad,
            top,
            right: w - pad,
            bottom: top + h,
        };
        let input = inner(pad, f.input_h + s(6));
        let preview = inner(input.bottom + s(2), f.tamil_h + s(8));
        let sep = inner(preview.bottom + s(4), s(1).max(1));
        let row_h = f.cand_h + s(4);
        let mut top = sep.bottom + s(4);
        let rows = (0..self.rows)
            .map(|_| {
                let r = RECT {
                    left: pad / 2,
                    top,
                    right: w - pad / 2,
                    bottom: top + row_h,
                };
                top += row_h;
                r
            })
            .collect();
        let hint = inner(top + s(4), f.small_h + s(4));
        // +2 for WS_BORDER, which is outside the client area.
        Layout {
            w: w + 2,
            h: hint.bottom + pad + 2,
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

    /// Paints `view` into `hdc` through an off-screen buffer (no flicker).
    pub fn paint(&self, hdc: HDC, view: &View) {
        let l = self.layout();
        let f = &self.fonts;
        // SAFETY: the memory DC and bitmap are created, used and deleted here;
        // every selected object is restored before deletion.
        unsafe {
            let mem = CreateCompatibleDC(Some(hdc));
            let bmp = CreateCompatibleBitmap(hdc, l.w, l.h);
            let old_bmp = SelectObject(mem, HGDIOBJ(bmp.0));
            let old_font = SelectObject(mem, HGDIOBJ(f.input.0));
            SetBkMode(mem, TRANSPARENT);
            let full = RECT {
                left: 0,
                top: 0,
                right: l.w,
                bottom: l.h,
            };
            FillRect(mem, &full, GetSysColorBrush(COLOR_WINDOW));

            if view.input.is_empty() {
                text(
                    mem,
                    f.input,
                    COLOR_GRAYTEXT,
                    l.input,
                    "type Tamil in English letters…",
                    DT_LEFT,
                );
            } else {
                let mut shown = view.input.clone();
                shown.push('▏'); // caret
                text_tail(mem, f.input, COLOR_WINDOWTEXT, l.input, &shown);
            }
            text_tail(mem, f.tamil, COLOR_WINDOWTEXT, l.preview, &view.preview);
            FillRect(mem, &l.sep, GetSysColorBrush(COLOR_3DLIGHT));

            for (i, (row, cand)) in l.rows.iter().zip(&view.candidates).enumerate() {
                let selected = i == view.selected;
                if selected {
                    FillRect(mem, row, GetSysColorBrush(COLOR_HIGHLIGHT));
                }
                let fg = if selected {
                    COLOR_HIGHLIGHTTEXT
                } else {
                    COLOR_WINDOWTEXT
                };
                let num_w = scale(26, self.dpi);
                let num = RECT {
                    left: row.left + scale(6, self.dpi),
                    right: row.left + num_w,
                    ..*row
                };
                let body = RECT {
                    left: row.left + num_w,
                    ..*row
                };
                let num_color = if selected {
                    COLOR_HIGHLIGHTTEXT
                } else {
                    COLOR_GRAYTEXT
                };
                text(mem, f.small, num_color, num, &(i + 1).to_string(), DT_LEFT);
                text(mem, f.cand, fg, body, cand, DT_LEFT);
            }

            let hint_color = if view.warn {
                None
            } else {
                Some(COLOR_GRAYTEXT)
            };
            match hint_color {
                Some(c) => text(mem, f.small, c, l.hint, &view.hint, DT_LEFT),
                None => text_rgb(
                    mem,
                    f.small,
                    COLORREF(0x0000_20C0),
                    l.hint,
                    &view.hint,
                    DT_LEFT,
                ),
            }

            let _ = BitBlt(hdc, 0, 0, l.w, l.h, Some(mem), 0, 0, SRCCOPY);
            SelectObject(mem, old_font);
            SelectObject(mem, old_bmp);
            let _ = DeleteObject(HGDIOBJ(bmp.0));
            let _ = DeleteDC(mem);
        }
    }
}

type SysColor = windows::Win32::Graphics::Gdi::SYS_COLOR_INDEX;

fn text(dc: HDC, font: HFONT, color: SysColor, r: RECT, s: &str, align: DRAW_TEXT_FORMAT) {
    // SAFETY: plain query.
    let rgb = COLORREF(unsafe { GetSysColor(color) });
    text_rgb(dc, font, rgb, r, s, align);
}

fn text_rgb(dc: HDC, font: HFONT, color: COLORREF, mut r: RECT, s: &str, align: DRAW_TEXT_FORMAT) {
    // An empty slice has a dangling pointer, and DrawTextW reads it even
    // with a length of 0 (it crashed user32), so never pass one.
    if s.is_empty() {
        return;
    }
    let mut buf: Vec<u16> = s.encode_utf16().collect();
    // SAFETY: `dc` is a valid DC; the font stays alive while selected.
    unsafe {
        SelectObject(dc, HGDIOBJ(font.0));
        SetTextColor(dc, color);
        DrawTextW(
            dc,
            &mut buf,
            &mut r,
            align | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
        );
    }
}

/// Left-aligned text, but right-aligned when too long, so the end
/// (where the user is typing) stays visible.
fn text_tail(dc: HDC, font: HFONT, color: SysColor, r: RECT, s: &str) {
    if s.is_empty() {
        return; // see text_rgb
    }
    let mut buf: Vec<u16> = s.encode_utf16().collect();
    let mut calc = r;
    // SAFETY: DT_CALCRECT only measures.
    unsafe {
        SelectObject(dc, HGDIOBJ(font.0));
        DrawTextW(
            dc,
            &mut buf,
            &mut calc,
            DT_CALCRECT | DT_SINGLELINE | DT_NOPREFIX,
        );
    }
    let align = if calc.right - calc.left > r.right - r.left {
        DT_RIGHT
    } else {
        DT_LEFT
    };
    text(dc, font, color, r, s, align);
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::DefWindowProcW;

    unsafe extern "system" fn proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        // SAFETY: default processing for a test window.
        unsafe { DefWindowProcW(h, m, w, l) }
    }

    fn view(input: &str, preview: &str, candidates: &[&str]) -> View {
        View {
            input: input.into(),
            preview: preview.into(),
            candidates: candidates.iter().map(|s| s.to_string()).collect(),
            selected: 0,
            hint: String::new(),
            warn: false,
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
            popup.paint(dc, &view("amma", "அம்மா", &["அம்மா", "ஆம்மா"]));
        }
        // SAFETY: we created it.
        let _ = unsafe { DeleteDC(dc) };
    }
}
