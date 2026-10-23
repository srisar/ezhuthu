//! An opaque drawing surface over a 32-bit DIB section: anti-aliased shapes
//! drawn in Rust (distance functions from `shape`) and text drawn by GDI into
//! the same pixels, then blitted to a window in one go (no flicker).
//!
//! Colours are 0x00RRGGBB (the DIB's B, G, R, X bytes).

use windows::Win32::Foundation::{COLORREF, RECT, SIZE};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CreateCompatibleDC, CreateDIBSection,
    DIB_RGB_COLORS, DRAW_TEXT_FORMAT, DT_NOPREFIX, DeleteDC, DeleteObject, DrawTextW, GdiFlush,
    GetTextExtentPoint32W, HBITMAP, HDC, HFONT, HGDIOBJ, SRCCOPY, SelectObject, SetBkMode,
    SetTextColor, TRANSPARENT,
};
use windows::Win32::UI::WindowsAndMessaging::{DI_NORMAL, DrawIconEx, HICON};

use crate::shape::{arc_distance, capsule, coverage_of, dist, rounded_rect};

pub type Rgb = u32;

/// `a` blended towards `b` by `t` (0 = a, 1 = b).
pub fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    let ch = |shift: u32| {
        let (x, y) = (((a >> shift) & 0xFF) as f32, ((b >> shift) & 0xFF) as f32);
        ((x + (y - x) * t).round() as u32) << shift
    };
    ch(16) | ch(8) | ch(0)
}

/// A COLORREF (0x00BBGGRR) for GDI from an Rgb.
fn colorref(c: Rgb) -> COLORREF {
    COLORREF(((c & 0xFF) << 16) | (c & 0xFF00) | ((c >> 16) & 0xFF))
}

/// A premultiplied BGRA image (see tools/icons/make_logo.py).
pub struct Image {
    pub w: usize,
    pub h: usize,
    pub pixels: Vec<u32>,
}

impl Image {
    /// Reads the `.bgra` format: u32 width, u32 height, then pixels.
    pub fn from_bgra(data: &[u8]) -> Option<Self> {
        let w = u32::from_le_bytes(data.get(0..4)?.try_into().ok()?) as usize;
        let h = u32::from_le_bytes(data.get(4..8)?.try_into().ok()?) as usize;
        let body = data.get(8..8 + w * h * 4)?;
        let pixels = body
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| u32::from_le_bytes(*p))
            .collect();
        Some(Self { w, h, pixels })
    }

    /// Scaled to `w` pixels wide (area-averaged, for a crisp downscale).
    pub fn scaled(&self, w: usize) -> Self {
        let w = w.clamp(1, self.w.max(1) * 4);
        let h = (self.h * w).div_ceil(self.w).max(1);
        let (sx, sy) = (self.w as f32 / w as f32, self.h as f32 / h as f32);
        let mut pixels = Vec::with_capacity(w * h);
        for y in 0..h {
            let (y0, y1) = (y as f32 * sy, (y as f32 + 1.0) * sy);
            for x in 0..w {
                let (x0, x1) = (x as f32 * sx, (x as f32 + 1.0) * sx);
                let mut acc = [0.0f32; 4];
                let mut area = 0.0;
                for iy in y0.floor() as usize..(y1.ceil() as usize).min(self.h) {
                    let wy = (y1.min(iy as f32 + 1.0) - y0.max(iy as f32)).max(0.0);
                    for ix in x0.floor() as usize..(x1.ceil() as usize).min(self.w) {
                        let wx = (x1.min(ix as f32 + 1.0) - x0.max(ix as f32)).max(0.0);
                        let p = self.pixels[iy * self.w + ix];
                        for (c, a) in acc.iter_mut().enumerate() {
                            *a += ((p >> (8 * c)) & 0xFF) as f32 * wx * wy;
                        }
                        area += wx * wy;
                    }
                }
                let px = acc.iter().enumerate().fold(0u32, |px, (c, a)| {
                    px | (((a / area.max(1e-6)).round() as u32).min(255) << (8 * c))
                });
                pixels.push(px);
            }
        }
        Self { w, h, pixels }
    }
}

pub struct Canvas {
    dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u32,
    pub w: i32,
    pub h: i32,
}

impl Canvas {
    pub fn new(w: i32, h: i32) -> Option<Self> {
        let (w, h) = (w.max(1), h.max(1));
        // SAFETY: a memory DC with a top-down 32-bit DIB section of w x h
        // pixels selected into it; both are released in Drop.
        unsafe {
            let dc = CreateCompatibleDC(None);
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
            let Ok(bitmap) = CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0)
            else {
                let _ = DeleteDC(dc);
                return None;
            };
            if bits.is_null() {
                let _ = DeleteObject(HGDIOBJ(bitmap.0));
                let _ = DeleteDC(dc);
                return None;
            }
            let old = SelectObject(dc, HGDIOBJ(bitmap.0));
            SetBkMode(dc, TRANSPARENT);
            Some(Self {
                dc,
                bitmap,
                old,
                bits: bits as *mut u32,
                w,
                h,
            })
        }
    }

    /// The pixels, after any pending GDI drawing has landed.
    pub fn pixels(&mut self) -> &mut [u32] {
        // SAFETY: GDI may batch drawing; flush before touching the bits. The
        // DIB holds exactly w * h pixels and lives as long as self.
        unsafe {
            let _ = GdiFlush();
            std::slice::from_raw_parts_mut(self.bits, (self.w * self.h) as usize)
        }
    }

    pub fn clear(&mut self, color: Rgb) {
        self.pixels().fill(color);
    }

    /// Blends `color` at coverage `alpha` into the pixels of `[x0, x1) x
    /// [y0, y1)` where `coverage(px, py)` (pixel centre) is positive.
    fn paint(
        &mut self,
        (x0, y0, x1, y1): (f32, f32, f32, f32),
        color: Rgb,
        alpha: f32,
        coverage: impl Fn(f32, f32) -> f32,
    ) {
        if alpha <= 0.0 {
            return;
        }
        let (w, h) = (self.w, self.h);
        let xs = (x0.floor() as i32).max(0)..(x1.ceil() as i32).min(w);
        let ys = (y0.floor() as i32).max(0)..(y1.ceil() as i32).min(h);
        let px = self.pixels();
        for y in ys {
            for x in xs.clone() {
                let c = coverage(x as f32 + 0.5, y as f32 + 0.5) * alpha;
                if c > 0.0 {
                    let i = (y * w + x) as usize;
                    px[i] = mix(px[i], color, c);
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn round_rect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, color: Rgb, alpha: f32) {
        self.paint(
            (x - 1.0, y - 1.0, x + w + 1.0, y + h + 1.0),
            color,
            alpha,
            |px, py| coverage_of(rounded_rect(px, py, x, y, w, h, r)),
        );
    }

    /// A rounded rectangle's outline, `width` thick, inside its bounds.
    pub fn round_rect_line(
        &mut self,
        (x, y, w, h): (f32, f32, f32, f32),
        r: f32,
        width: f32,
        color: Rgb,
        alpha: f32,
    ) {
        self.paint(
            (x - 1.0, y - 1.0, x + w + 1.0, y + h + 1.0),
            color,
            alpha,
            |px, py| {
                let d = rounded_rect(px, py, x, y, w, h, r);
                coverage_of(d) * (1.0 - coverage_of(d + width))
            },
        );
    }

    pub fn circle(&mut self, cx: f32, cy: f32, r: f32, color: Rgb, alpha: f32) {
        self.paint(
            (cx - r - 1.0, cy - r - 1.0, cx + r + 1.0, cy + r + 1.0),
            color,
            alpha,
            |px, py| coverage_of(dist((px, py), (cx, cy)) - r),
        );
    }

    /// An arc of radius `r` (stroke `width`) from `start` through `sweep`.
    #[allow(clippy::too_many_arguments)]
    pub fn arc(
        &mut self,
        cx: f32,
        cy: f32,
        r: f32,
        width: f32,
        start: f32,
        sweep: f32,
        color: Rgb,
        alpha: f32,
    ) {
        let m = r + width;
        self.paint((cx - m, cy - m, cx + m, cy + m), color, alpha, |px, py| {
            coverage_of(arc_distance((px, py), (cx, cy), r, start, sweep) - width / 2.0)
        });
    }

    /// A line from `a` to `b` with round ends.
    pub fn line(&mut self, a: (f32, f32), b: (f32, f32), width: f32, color: Rgb, alpha: f32) {
        let m = width;
        let bounds = (
            a.0.min(b.0) - m,
            a.1.min(b.1) - m,
            a.0.max(b.0) + m,
            a.1.max(b.1) + m,
        );
        self.paint(bounds, color, alpha, |px, py| {
            coverage_of(capsule((px, py), a, b) - width / 2.0)
        });
    }

    /// Text in `font` and `color` inside `rect`, with DrawTextW `format`.
    pub fn text(
        &mut self,
        font: HFONT,
        s: &str,
        mut rect: RECT,
        color: Rgb,
        format: DRAW_TEXT_FORMAT,
    ) {
        // An empty slice has a dangling pointer, and DrawTextW reads it.
        if s.is_empty() {
            return;
        }
        let mut buf: Vec<u16> = s.encode_utf16().collect();
        // SAFETY: drawing into our own DC; the font is the caller's and stays
        // alive for the call, and is deselected afterwards.
        unsafe {
            let old = SelectObject(self.dc, HGDIOBJ(font.0));
            SetTextColor(self.dc, colorref(color));
            DrawTextW(self.dc, &mut buf, &mut rect, format | DT_NOPREFIX);
            SelectObject(self.dc, old);
        }
    }

    /// Width and height of `s` in `font`.
    pub fn measure(&self, font: HFONT, s: &str) -> (i32, i32) {
        let units: Vec<u16> = s.encode_utf16().collect();
        let mut size = SIZE::default();
        // SAFETY: measuring in our own DC; the font is deselected afterwards.
        unsafe {
            let old = SelectObject(self.dc, HGDIOBJ(font.0));
            let _ = GetTextExtentPoint32W(self.dc, &units, &mut size);
            SelectObject(self.dc, old);
        }
        (size.cx, size.cy)
    }

    /// Draws an icon at (x, y), `size` pixels square.
    pub fn icon(&mut self, icon: HICON, x: i32, y: i32, size: i32) {
        // SAFETY: drawing a valid icon into our own DC.
        let _ = unsafe { DrawIconEx(self.dc, x, y, icon, size, size, 0, None, DI_NORMAL) };
    }

    /// Draws a premultiplied image with its top-left at (x, y).
    #[cfg(test)]
    pub fn image(&mut self, img: &Image, x: i32, y: i32) {
        self.image_faded(img, x, y, 1.0);
    }

    /// Draws a premultiplied image at (x, y), at opacity `alpha`.
    pub fn image_faded(&mut self, img: &Image, x: i32, y: i32, alpha: f32) {
        let fade = (alpha.clamp(0.0, 1.0) * 256.0) as u32;
        let (w, h) = (self.w, self.h);
        let px = self.pixels();
        for iy in 0..img.h as i32 {
            let ty = y + iy;
            if ty < 0 || ty >= h {
                continue;
            }
            for ix in 0..img.w as i32 {
                let tx = x + ix;
                if tx < 0 || tx >= w {
                    continue;
                }
                let p = img.pixels[(iy as usize) * img.w + ix as usize];
                // Scale all four premultiplied channels by the fade.
                let s = (0..4).fold(0u32, |s, c| {
                    s | ((((p >> (8 * c)) & 0xFF) * fade / 256) << (8 * c))
                });
                let a = s >> 24;
                if a == 0 {
                    continue;
                }
                let i = (ty * w + tx) as usize;
                let d = px[i];
                let ch = |shift: u32| {
                    let sc = (s >> shift) & 0xFF;
                    let dc = (d >> shift) & 0xFF;
                    ((sc + dc * (255 - a) / 255).min(255)) << shift
                };
                px[i] = ch(16) | ch(8) | ch(0);
            }
        }
    }

    /// Blends `other` (a frame of the same size) over the pixels by `t`.
    pub fn blend_over(&mut self, other: &[u32], t: f32) {
        let px = self.pixels();
        if other.len() != px.len() {
            return;
        }
        for (p, &o) in px.iter_mut().zip(other) {
            *p = mix(*p, o, t);
        }
    }

    /// Copies the canvas to `hdc` at (0, 0).
    pub fn blit(&mut self, hdc: HDC) {
        // SAFETY: copying from our own DC; GDI work is flushed first.
        unsafe {
            let _ = GdiFlush();
            let _ = BitBlt(hdc, 0, 0, self.w, self.h, Some(self.dc), 0, 0, SRCCOPY);
        }
    }
}

impl Drop for Canvas {
    fn drop(&mut self) {
        // SAFETY: we created both; the bitmap is deselected before deletion.
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(HGDIOBJ(self.bitmap.0));
            let _ = DeleteDC(self.dc);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixes_colours() {
        assert_eq!(mix(0x000000, 0xFFFFFF, 0.5), 0x808080);
        assert_eq!(mix(0x102030, 0x102030, 0.7), 0x102030);
        assert_eq!(mix(0xFF0000, 0x0000FF, 1.0), 0x0000FF);
    }

    #[test]
    fn draws_shapes_text_and_images() {
        let mut cv = Canvas::new(40, 20).unwrap();
        cv.clear(0xFFFFFF);
        cv.round_rect(2.0, 2.0, 36.0, 16.0, 8.0, 0x000000, 1.0);
        assert_eq!(cv.pixels()[10 * 40 + 20], 0x000000, "inside is filled");
        assert_eq!(cv.pixels()[0], 0xFFFFFF, "the corner is clear");
        // an opaque red pixel, scaled up, lands in place
        let img = Image {
            w: 1,
            h: 1,
            pixels: vec![0xFFFF_0000],
        };
        cv.image(&img.scaled(2), 0, 0);
        assert_eq!(cv.pixels()[0] & 0xFFFFFF, 0xFF0000);
        let bgra = [1u8, 0, 0, 0, 1, 0, 0, 0, 0, 0, 255, 255];
        assert_eq!(Image::from_bgra(&bgra).unwrap().pixels, [0xFFFF_0000]);
    }
}
