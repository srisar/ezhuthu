//! The progress badge for convert selection: a small pill near the caret
//! with a spinner ("Converting to Tamil…"), then a check mark ("Converted"),
//! then it fades away.
//!
//! It is a layered window drawn in software into a premultiplied BGRA
//! bitmap (anti-aliased shapes from distance functions, text coverage from
//! GDI), so it has smooth edges and fades on Windows 10 and 11 alike. It
//! never takes focus and clicks go through it.

use std::f32::consts::{PI, TAU};
use std::time::{Duration, Instant};

use anyhow::Result;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, ANTIALIASED_QUALITY, BI_RGB, BITMAPINFO, BITMAPINFOHEADER,
    BLENDFUNCTION, CLIP_DEFAULT_PRECIS, CreateCompatibleDC, CreateDIBSection, CreateFontW,
    DEFAULT_CHARSET, DIB_RGB_COLORS, DT_CALCRECT, DT_LEFT, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER,
    DeleteDC, DeleteObject, DrawTextW, FW_SEMIBOLD, GetDC, HBITMAP, HDC, HFONT, HGDIOBJ,
    OUT_DEFAULT_PRECIS, ReleaseDC, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, HWND_TOPMOST, RegisterClassExW, SW_HIDE, SWP_NOACTIVATE,
    SWP_NOSIZE, SWP_SHOWWINDOW, SetWindowPos, ShowWindow, ULW_ALPHA, UpdateLayeredWindow,
    WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::{PCWSTR, w};

use crate::caret;
use crate::shape::{
    arc_distance, capsule, coverage_of, dist, ease_out, lerp2, ratio, rounded_rect, smoothstep,
};
use crate::win::{instance, scale, wide};

const WORKING_TEXT: &str = "Converting to Tamil…";
const DONE_TEXT: &str = "Converted";
/// The logo's colours: dark tile, light letter, orange cursor.
const TILE: [f32; 3] = [0x15 as f32, 0x17 as f32, 0x1C as f32];
const LETTER: [f32; 3] = [0xF7 as f32, 0xF7 as f32, 0xF5 as f32];
const ORANGE: [f32; 3] = [0xF5 as f32, 0xA5 as f32, 0x24 as f32];

const FADE_IN: Duration = Duration::from_millis(150);
const CHECK_DRAW: Duration = Duration::from_millis(220);
const HOLD: Duration = Duration::from_millis(750);
const FADE_OUT: Duration = Duration::from_millis(250);
/// The badge rises this far (96-DPI px) while fading in.
const RISE: f32 = 6.0;

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Hidden,
    Working,
    /// The conversion finished: check mark, hold, fade out.
    Done,
    /// Cancelled or failed: fade out from the current opacity.
    Leaving {
        from: f32,
    },
}

pub struct Badge {
    hwnd: HWND,
    phase: Phase,
    /// When the badge appeared, and when the current phase began.
    shown: Instant,
    since: Instant,
    dpi: u32,
    /// Window size and its top-left once fully risen.
    size: (i32, i32),
    pos: (i32, i32),
    /// Text coverage (0–255 per pixel) for each label, `size` sized.
    working: Vec<u8>,
    done: Vec<u8>,
    alpha: f32,
}

unsafe extern "system" fn proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    // SAFETY: default processing; the badge draws itself with UpdateLayeredWindow.
    unsafe { DefWindowProcW(h, m, w, l) }
}

impl Badge {
    pub fn create() -> Result<Self> {
        let class = w!("ezhuthu.badge");
        // SAFETY: the class struct is fully initialised; the window is owned
        // by this thread and lives as long as the app.
        let hwnd = unsafe {
            let wc = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(proc),
                hInstance: instance(),
                lpszClassName: class,
                ..Default::default()
            };
            RegisterClassExW(&wc);
            CreateWindowExW(
                WS_EX_LAYERED
                    | WS_EX_TRANSPARENT
                    | WS_EX_TOPMOST
                    | WS_EX_TOOLWINDOW
                    | WS_EX_NOACTIVATE,
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
            phase: Phase::Hidden,
            shown: Instant::now(),
            since: Instant::now(),
            dpi: 96,
            size: (0, 0),
            pos: (0, 0),
            working: Vec::new(),
            done: Vec::new(),
            alpha: 0.0,
        })
    }

    fn s(&self, v: f32) -> f32 {
        v * self.dpi as f32 / 96.0
    }

    /// Shadow room around the pill, in pixels.
    fn margin(&self) -> i32 {
        scale(10, self.dpi)
    }

    /// Shows the spinner near `target`'s caret.
    pub fn start(&mut self, target: HWND) {
        let anchor = caret::find(target);
        let dpi = caret::dpi_at(anchor);
        if dpi != self.dpi || self.working.is_empty() {
            self.dpi = dpi;
            self.layout();
        }
        let m = self.margin();
        let (w, h) = self.size;
        let (x, y) = caret::place(anchor, w - 2 * m, h - 2 * m, scale(8, dpi));
        self.pos = (x - m, y - m);
        self.phase = Phase::Working;
        self.shown = Instant::now();
        self.since = self.shown;
        self.alpha = 0.0;
        // SAFETY: our own window; shown without activation.
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                self.pos.0,
                self.pos.1,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOSIZE | SWP_SHOWWINDOW,
            );
        }
        self.tick(true);
    }

    /// The conversion finished: switch to the check mark.
    pub fn done(&mut self) {
        if self.phase == Phase::Working {
            self.phase = Phase::Done;
            self.since = Instant::now();
        }
    }

    /// Advances the animation. `working` says whether the conversion is
    /// still running (if it stopped without `done`, the badge fades out).
    /// Returns false once hidden (the caller stops its timer).
    pub fn tick(&mut self, working: bool) -> bool {
        let now = Instant::now();
        if self.phase == Phase::Working && !working {
            self.phase = Phase::Leaving { from: self.alpha };
            self.since = now;
        }
        let fade_in = ease_out(ratio(now - self.shown, FADE_IN));
        let in_phase = now - self.since;
        self.alpha = match self.phase {
            Phase::Hidden => return false,
            Phase::Working => fade_in,
            Phase::Done => {
                let out = in_phase.saturating_sub(CHECK_DRAW + HOLD);
                fade_in * (1.0 - ratio(out, FADE_OUT))
            }
            Phase::Leaving { from } => from * (1.0 - ratio(in_phase, FADE_OUT)),
        };
        if self.alpha <= 0.0 && !matches!(self.phase, Phase::Working) {
            self.phase = Phase::Hidden;
            // SAFETY: hiding our own window.
            let _ = unsafe { ShowWindow(self.hwnd, SW_HIDE) };
            return false;
        }
        let rise = self.s(RISE) * (1.0 - fade_in);
        self.present(rise.round() as i32);
        true
    }

    /// Sizes the window for the longer label and renders both labels.
    fn layout(&mut self) {
        let font = self.font();
        let text_w = [WORKING_TEXT, DONE_TEXT]
            .iter()
            .map(|t| measure(font, t))
            .max()
            .unwrap_or(0);
        let m = self.margin();
        let pill_w = scale(12 + 16 + 9 + 14, self.dpi) + text_w;
        let pill_h = scale(34, self.dpi);
        self.size = (pill_w + 2 * m, pill_h + 2 * m);
        let text_x = m + scale(12 + 16 + 9, self.dpi);
        self.working = coverage(font, WORKING_TEXT, self.size, text_x);
        self.done = coverage(font, DONE_TEXT, self.size, text_x);
        // SAFETY: we created the font; it isn't selected anywhere.
        let _ = unsafe { DeleteObject(HGDIOBJ(font.0)) };
    }

    fn font(&self) -> HFONT {
        let face = wide("Segoe UI");
        // SAFETY: the face name is NUL-terminated and outlives the call.
        unsafe {
            CreateFontW(
                -(10 * self.dpi as i32 / 72),
                0,
                0,
                0,
                FW_SEMIBOLD.0 as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                ANTIALIASED_QUALITY,
                0,
                PCWSTR(face.as_ptr()),
            )
        }
    }

    /// Renders the current frame and puts it on screen, `rise` px lower
    /// than the resting position.
    fn present(&self, rise: i32) {
        let (w, h) = self.size;
        let pixels = self.render();
        // SAFETY: a top-down 32-bit DIB section sized w x h receives exactly
        // w * h pixels; every GDI object is released before returning.
        unsafe {
            let screen = GetDC(None);
            let mem = CreateCompatibleDC(Some(screen));
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            if let Ok(bitmap) =
                CreateDIBSection(Some(mem), &info, DIB_RGB_COLORS, &mut bits, None, 0)
                && !bits.is_null()
            {
                std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits as *mut u32, pixels.len());
                let old = SelectObject(mem, HGDIOBJ(bitmap.0));
                let blend = BLENDFUNCTION {
                    BlendOp: AC_SRC_OVER as u8,
                    BlendFlags: 0,
                    SourceConstantAlpha: (self.alpha * 255.0).round().clamp(0.0, 255.0) as u8,
                    AlphaFormat: AC_SRC_ALPHA as u8,
                };
                let dst = POINT {
                    x: self.pos.0,
                    y: self.pos.1 + rise,
                };
                let size = SIZE { cx: w, cy: h };
                let src = POINT::default();
                let _ = UpdateLayeredWindow(
                    self.hwnd,
                    Some(screen),
                    Some(&dst),
                    Some(&size),
                    Some(mem),
                    Some(&src),
                    COLORREF(0),
                    Some(&blend),
                    ULW_ALPHA,
                );
                SelectObject(mem, old);
                let _ = DeleteObject(HGDIOBJ(HBITMAP(bitmap.0).0));
            }
            let _ = DeleteDC(mem);
            ReleaseDC(None, screen);
        }
    }

    /// One frame as premultiplied BGRA.
    fn render(&self) -> Vec<u32> {
        let (w, h) = self.size;
        let m = self.margin() as f32;
        let (pw, ph) = (w as f32 - 2.0 * m, h as f32 - 2.0 * m);
        let radius = ph / 2.0;
        let (cx, cy) = (m + self.s(12.0 + 8.0), h as f32 / 2.0);
        let ring = self.s(7.0);
        let stroke = self.s(2.2);
        let blur = self.s(6.0);
        let elapsed = self.shown.elapsed().as_secs_f32();
        let in_phase = self.since.elapsed();

        // Spinner arc: turns once per 0.9 s while its length breathes.
        let start = elapsed * TAU / 0.9;
        let sweep = PI * (0.5 + 1.0 * (0.5 - 0.5 * (elapsed * TAU / 1.4).cos()));
        let done = matches!(self.phase, Phase::Done);
        let check = ease_out(ratio(in_phase, CHECK_DRAW));
        // Check mark: two strokes, drawn in over CHECK_DRAW.
        let a = (cx - ring * 0.55, cy + ring * 0.02);
        let b = (cx - ring * 0.12, cy + ring * 0.45);
        let c = (cx + ring * 0.6, cy - ring * 0.42);
        let (l1, l2) = (dist(a, b), dist(b, c));
        let drawn = check * (l1 + l2);
        let b_end = lerp2(a, b, (drawn / l1).min(1.0));
        let c_end = lerp2(b, c, ((drawn - l1) / l2).clamp(0.0, 1.0));
        let text = if done { &self.done } else { &self.working };

        let mut out = vec![0u32; (w * h) as usize];
        for y in 0..h {
            for x in 0..w {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                // Soft shadow below the pill.
                let shadow = rounded_rect(px, py - self.s(2.0), m, m, pw, ph, radius);
                let mut acc = [0.0f32; 4]; // premultiplied r, g, b, a
                over(
                    &mut acc,
                    [0.0; 3],
                    0.28 * (1.0 - smoothstep(-blur, blur, shadow)),
                );
                let pill = coverage_of(rounded_rect(px, py, m, m, pw, ph, radius));
                if pill > 0.0 {
                    over(&mut acc, TILE, pill);
                    let t = text[(y * w + x) as usize] as f32 / 255.0;
                    over(&mut acc, LETTER, t * pill);
                    let d = (dist((px, py), (cx, cy)) - ring).abs() - stroke / 2.0;
                    if done {
                        let mut seg = capsule((px, py), a, b_end) - stroke / 2.0;
                        if drawn > l1 {
                            seg = seg.min(capsule((px, py), b, c_end) - stroke / 2.0);
                        }
                        if drawn > 0.0 {
                            over(&mut acc, ORANGE, coverage_of(seg) * pill);
                        }
                    } else {
                        // Faint track, then the arc with round ends.
                        over(&mut acc, ORANGE, 0.22 * coverage_of(d) * pill);
                        let arc = arc_distance((px, py), (cx, cy), ring, start, sweep);
                        over(&mut acc, ORANGE, coverage_of(arc - stroke / 2.0) * pill);
                    }
                }
                out[(y * w + x) as usize] = pack(acc);
            }
        }
        out
    }
}

/// Width of `text` in `font`, in pixels.
fn measure(font: HFONT, text: &str) -> i32 {
    let mut buf: Vec<u16> = text.encode_utf16().collect();
    let mut r = RECT::default();
    // SAFETY: the screen DC is released and the font deselected before returning.
    unsafe {
        let dc = GetDC(None);
        let old = SelectObject(dc, HGDIOBJ(font.0));
        DrawTextW(
            dc,
            &mut buf,
            &mut r,
            DT_CALCRECT | DT_SINGLELINE | DT_NOPREFIX,
        );
        SelectObject(dc, old);
        ReleaseDC(None, dc);
    }
    r.right - r.left
}

/// Grey-scale coverage of `text` drawn at `x`, vertically centred, on a
/// `size` canvas: GDI draws white on black, the red channel is the coverage.
fn coverage(font: HFONT, text: &str, size: (i32, i32), x: i32) -> Vec<u8> {
    let (w, h) = size;
    let mut out = vec![0u8; (w * h) as usize];
    let mut buf: Vec<u16> = text.encode_utf16().collect();
    // SAFETY: a top-down 32-bit DIB section of w x h pixels is read within
    // its bounds; every GDI object is released before returning.
    unsafe {
        let screen = GetDC(None);
        let dc: HDC = CreateCompatibleDC(Some(screen));
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        if let Ok(bitmap) = CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0)
            && !bits.is_null()
        {
            let old_bmp = SelectObject(dc, HGDIOBJ(bitmap.0));
            let old_font = SelectObject(dc, HGDIOBJ(font.0));
            SetBkMode(dc, TRANSPARENT);
            SetTextColor(dc, COLORREF(0x00FF_FFFF));
            let mut r = RECT {
                left: x,
                top: 0,
                right: w,
                bottom: h,
            };
            DrawTextW(
                dc,
                &mut buf,
                &mut r,
                DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
            );
            let px = std::slice::from_raw_parts(bits as *const u32, (w * h) as usize);
            for (o, p) in out.iter_mut().zip(px) {
                *o = ((p >> 16) & 0xFF) as u8;
            }
            SelectObject(dc, old_font);
            SelectObject(dc, old_bmp);
            let _ = DeleteObject(HGDIOBJ(bitmap.0));
        }
        let _ = DeleteDC(dc);
        ReleaseDC(None, screen);
    }
    out
}

/// Composites `color` at `alpha` over the premultiplied accumulator.
fn over(acc: &mut [f32; 4], color: [f32; 3], alpha: f32) {
    if alpha <= 0.0 {
        return;
    }
    let keep = 1.0 - alpha;
    for i in 0..3 {
        acc[i] = color[i] * alpha + acc[i] * keep;
    }
    acc[3] = alpha + acc[3] * keep;
}

/// Premultiplied BGRA, as UpdateLayeredWindow wants it.
fn pack(acc: [f32; 4]) -> u32 {
    let c = |v: f32| v.round().clamp(0.0, 255.0) as u32;
    (c(acc[3] * 255.0) << 24) | (c(acc[0]) << 16) | (c(acc[1]) << 8) | c(acc[2])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compositing_is_premultiplied() {
        let mut acc = [0.0; 4];
        over(&mut acc, [255.0, 0.0, 0.0], 0.5);
        assert_eq!(pack(acc), 0x8080_0000);
        over(&mut acc, [0.0, 0.0, 255.0], 1.0);
        assert_eq!(pack(acc), 0xFF00_00FF);
    }

    /// Shows the badge at the mouse for a few seconds (for screenshots): the
    /// spinner for 2 s, then the check mark until it fades. Run with:
    /// cargo test -p ezhuthu badge_preview -- --ignored
    #[test]
    #[ignore]
    fn badge_preview() {
        let mut badge = Badge::create().unwrap();
        badge.start(HWND::default());
        let start = Instant::now();
        while badge.tick(start.elapsed() < Duration::from_secs(4)) {
            if start.elapsed() > Duration::from_secs(2) {
                badge.done();
            }
            std::thread::sleep(Duration::from_millis(16));
        }
    }

    #[test]
    fn badge_renders_every_phase() {
        let mut badge = Badge::create().unwrap();
        badge.layout();
        for phase in [Phase::Working, Phase::Done] {
            badge.phase = phase;
            let frame = badge.render();
            assert_eq!(frame.len(), (badge.size.0 * badge.size.1) as usize);
            // the pill's centre is opaque, the corner is clear
            let (w, h) = badge.size;
            assert_eq!(frame[(h / 2 * w + w / 2) as usize] >> 24, 0xFF);
            assert_eq!(frame[0] >> 24, 0);
        }
    }
}
