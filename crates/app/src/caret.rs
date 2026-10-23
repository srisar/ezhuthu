//! Where to put the popup: under the target's text caret, else the mouse.

use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    ClientToScreen, GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    GUITHREADINFO, GetCursorPos, GetGUIThreadInfo, GetWindowThreadProcessId,
};

/// Screen position to anchor the popup: left edge, caret bottom and top.
#[derive(Debug, Clone, Copy)]
pub struct Anchor {
    pub x: i32,
    pub below: i32,
    pub above: i32,
}

pub fn find(target: HWND) -> Anchor {
    caret_of(target).unwrap_or_else(|| {
        let mut p = POINT::default();
        // SAFETY: GetCursorPos writes one POINT.
        let _ = unsafe { GetCursorPos(&mut p) };
        Anchor {
            x: p.x,
            below: p.y + 20,
            above: p.y - 4,
        }
    })
}

fn caret_of(target: HWND) -> Option<Anchor> {
    // SAFETY: plain queries on another thread's GUI state; GUITHREADINFO is
    // initialised with its size as the API requires.
    unsafe {
        let tid = GetWindowThreadProcessId(target, None);
        let mut gti = GUITHREADINFO {
            cbSize: size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        GetGUIThreadInfo(tid, &mut gti).ok()?;
        let r = gti.rcCaret;
        // Many apps (Chromium, Electron, XAML) report no caret.
        if gti.hwndCaret.0.is_null() || r.bottom <= r.top {
            return None;
        }
        let mut top = POINT {
            x: r.left,
            y: r.top,
        };
        let mut bottom = POINT {
            x: r.left,
            y: r.bottom,
        };
        if !ClientToScreen(gti.hwndCaret, &mut top).as_bool()
            || !ClientToScreen(gti.hwndCaret, &mut bottom).as_bool()
        {
            return None;
        }
        Some(Anchor {
            x: bottom.x,
            below: bottom.y,
            above: top.y,
        })
    }
}

/// Effective DPI of the monitor containing the anchor.
pub fn dpi_at(a: Anchor) -> u32 {
    // SAFETY: MonitorFromPoint never fails with DEFAULTTONEAREST.
    unsafe {
        let mon = MonitorFromPoint(POINT { x: a.x, y: a.below }, MONITOR_DEFAULTTONEAREST);
        let (mut x, mut y) = (96, 96);
        let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut x, &mut y);
        x
    }
}

/// Top-left for a `w` x `h` popup: below the caret, or above it if there is
/// no room, kept inside the monitor's work area.
pub fn place(a: Anchor, w: i32, h: i32, gap: i32) -> (i32, i32) {
    let work = work_area(a);
    let x = a.x.clamp(work.left, (work.right - w).max(work.left));
    let mut y = a.below + gap;
    if y + h > work.bottom {
        y = a.above - gap - h;
    }
    (x, y.clamp(work.top, (work.bottom - h).max(work.top)))
}

fn work_area(a: Anchor) -> RECT {
    // SAFETY: MONITORINFO is initialised with its size.
    unsafe {
        let mon = MonitorFromPoint(POINT { x: a.x, y: a.below }, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let _ = GetMonitorInfoW(mon, &mut mi);
        mi.rcWork
    }
}
