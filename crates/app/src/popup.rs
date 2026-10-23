//! The popup: a non-activating, topmost window painted with GDI.
//!
//! It never takes focus (the target app keeps it, which is what lets this
//! work over the Start/search box). Text is drawn with DrawTextW, which
//! shapes Tamil through Uniscribe, in "Nirmala UI" (ships with Windows).

use anyhow::Result;
use windows::Win32::Foundation::{COLORREF, HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, COLOR_HIGHLIGHT, CreateCompatibleBitmap,
    CreateCompatibleDC, CreateFontW, CreateSolidBrush, DEFAULT_CHARSET, DRAW_TEXT_FORMAT,
    DT_CALCRECT, DT_LEFT, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DT_VCENTER, DeleteDC, DeleteObject,
    DrawTextW, FW_NORMAL, FillRect, GetDC, GetSysColor, GetTextMetricsW, HDC, HFONT, HGDIOBJ,
    OUT_DEFAULT_PRECIS, ReleaseDC, SRCCOPY, SelectObject, SetBkMode, SetTextColor, TEXTMETRICW,
    TRANSPARENT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CS_DROPSHADOW, CreateWindowExW, IDC_ARROW, LoadCursorW, RegisterClassExW, WNDCLASSEXW, WNDPROC,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::{PCWSTR, w};

use crate::win::{instance, scale, wide};

/// What to draw, built from the app state on each paint.
pub struct View {
    pub input: String,
    pub preview: String,
    pub candidates: Vec<String>,
    /// The row holding the word as typed (inserted as English), if shown.
    pub english: Option<usize>,
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
    palette: Palette,
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
        Ok(Self {
            hwnd,
            dpi: 96,
            rows,
            fonts: Fonts::new(96),
            palette: Palette::LIGHT,
        })
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
    }

    pub fn set_rows(&mut self, rows: usize) {
        self.rows = rows;
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
        // +2 for the 1 px border drawn around the edge.
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
        let p = &self.palette;
        // SAFETY: plain query.
        let highlight = COLORREF(unsafe { GetSysColor(COLOR_HIGHLIGHT) });
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
            fill(mem, &full, p.border);
            let inside = RECT {
                left: 1,
                top: 1,
                right: l.w - 1,
                bottom: l.h - 1,
            };
            fill(mem, &inside, p.background);

            if view.input.is_empty() {
                text(
                    mem,
                    f.input,
                    p.muted,
                    l.input,
                    "type Tamil in English letters…",
                    DT_LEFT,
                );
            } else {
                let mut shown = view.input.clone();
                shown.push('▏'); // caret
                text_tail(mem, f.input, p.text, l.input, &shown);
            }
            text_tail(mem, f.tamil, p.text, l.preview, &view.preview);
            fill(mem, &l.sep, p.separator);

            for (i, (row, cand)) in l.rows.iter().zip(&view.candidates).enumerate() {
                let selected = i == view.selected;
                if selected {
                    fill(mem, row, highlight);
                }
                let (fg, num_color) = if selected {
                    (p.selected_text, p.selected_text)
                } else {
                    (p.text, p.muted)
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
                // The tenth row is picked with 0.
                let key = ((i + 1) % 10).to_string();
                text(mem, f.small, num_color, num, &key, DT_LEFT);
                text(mem, f.cand, fg, body, cand, DT_LEFT);
                if view.english == Some(i) {
                    let tag = RECT {
                        right: row.right - scale(8, self.dpi),
                        ..*row
                    };
                    text(mem, f.small, num_color, tag, "English", DT_RIGHT);
                }
            }

            let hint_color = if view.warn { p.warning } else { p.muted };
            text(mem, f.small, hint_color, l.hint, &view.hint, DT_LEFT);

            let _ = BitBlt(hdc, 0, 0, l.w, l.h, Some(mem), 0, 0, SRCCOPY);
            SelectObject(mem, old_font);
            SelectObject(mem, old_bmp);
            let _ = DeleteObject(HGDIOBJ(bmp.0));
            let _ = DeleteDC(mem);
        }
    }
}

/// The popup's colours, light or dark.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Palette {
    background: COLORREF,
    border: COLORREF,
    separator: COLORREF,
    text: COLORREF,
    muted: COLORREF,
    /// Text on the selected row (its background is the system highlight).
    selected_text: COLORREF,
    warning: COLORREF,
}

impl Palette {
    const LIGHT: Self = Self {
        background: rgb(0xFF, 0xFF, 0xFF),
        border: rgb(0xC8, 0xC8, 0xC8),
        separator: rgb(0xE6, 0xE6, 0xE6),
        text: rgb(0x1B, 0x1B, 0x1B),
        muted: rgb(0x6E, 0x6E, 0x6E),
        selected_text: rgb(0xFF, 0xFF, 0xFF),
        warning: rgb(0xC0, 0x20, 0x00),
    };
    /// Close to Windows 11's dark menus and flyouts.
    const DARK: Self = Self {
        background: rgb(0x2B, 0x2B, 0x2B),
        border: rgb(0x48, 0x48, 0x48),
        separator: rgb(0x3D, 0x3D, 0x3D),
        text: rgb(0xF2, 0xF2, 0xF2),
        muted: rgb(0xA3, 0xA3, 0xA3),
        selected_text: rgb(0xFF, 0xFF, 0xFF),
        warning: rgb(0xFF, 0x7B, 0x6B),
    };
}

/// A COLORREF (which is 0x00BBGGRR) from red, green and blue.
const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}

fn fill(dc: HDC, r: &RECT, color: COLORREF) {
    // SAFETY: the brush is created, used and deleted here.
    unsafe {
        let brush = CreateSolidBrush(color);
        FillRect(dc, r, brush);
        let _ = DeleteObject(HGDIOBJ(brush.0));
    }
}

fn text(dc: HDC, font: HFONT, color: COLORREF, mut r: RECT, s: &str, align: DRAW_TEXT_FORMAT) {
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
fn text_tail(dc: HDC, font: HFONT, color: COLORREF, r: RECT, s: &str) {
    if s.is_empty() {
        return; // see text
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
            english: candidates.len().checked_sub(1),
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
            BI_RGB, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, GetDIBits, SelectObject,
        };
        let mut popup = Popup::create(Some(proc), 6).unwrap();
        popup.set_dpi(144);
        let mut v = view(
            "natchathiram",
            "நட்சத்திரம்",
            &[
                "நட்சத்திரம்",
                "நட்சதிரம்",
                "நட்சாத்திரம்",
                "நட்சாதிரம்",
                "நாட்சத்திரம்",
                "natchathiram",
            ],
        );
        v.hint = "↑↓ or 1–6 choose · Space / Enter insert · Esc discard".into();
        let (w, h) = popup.size();
        for (dark, name) in [(false, "light"), (true, "dark")] {
            popup.set_dark(dark);
            // SAFETY: a screen-compatible bitmap painted, read back within its
            // size and deleted, with every selection restored.
            unsafe {
                let screen = GetDC(None);
                let dc = CreateCompatibleDC(Some(screen));
                let bmp = CreateCompatibleBitmap(screen, w, h);
                let old = SelectObject(dc, HGDIOBJ(bmp.0));
                popup.paint(dc, &v);
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
        }
        // SAFETY: we created it.
        let _ = unsafe { DeleteDC(dc) };
    }
}
