//! Notification-area icon: tooltip, balloon notices and the context menu.

use anyhow::{Result, bail};
use windows::Win32::Foundation::COLORREF;
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateBitmap,
    CreateCompatibleDC, CreateDIBSection, CreateFontW, CreateSolidBrush, DEFAULT_CHARSET,
    DIB_RGB_COLORS, DT_CENTER, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, DeleteDC, DeleteObject,
    DrawTextW, FW_SEMIBOLD, FillRect, GetDC, HGDIOBJ, OUT_DEFAULT_PRECIS, ReleaseDC, SelectObject,
    SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIIF_INFO, NIIF_WARNING, NIM_ADD,
    NIM_DELETE, NIM_MODIFY, NIM_SETVERSION, NOTIFYICON_VERSION_4, NOTIFYICONDATAW,
    Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconIndirect, CreatePopupMenu, DestroyIcon, DestroyMenu, GetCursorPos,
    GetSystemMetrics, HICON, ICONINFO, MF_CHECKED, MF_GRAYED, MF_SEPARATOR, MF_STRING,
    PostMessageW, SM_CXSMICON, SetForegroundWindow, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON,
    TrackPopupMenu, WM_NULL,
};
use windows::core::PCWSTR;

use crate::win::{copy_into, wide};

const ICON_ID: u32 = 1;
/// Icon backgrounds (BGR): blue while Tamil mode is on, grey while off.
const ON_BG: COLORREF = COLORREF(0x00B0_5A1F); // #1F5AB0
const OFF_BG: COLORREF = COLORREF(0x0080_8080); // #808080

pub struct Tray {
    hwnd: HWND,
    icon_on: HICON,
    icon_off: HICON,
    active: bool,
    callback: u32,
    tip: String,
}

impl Tray {
    pub fn add(hwnd: HWND, callback: u32, tip: &str) -> Result<Self> {
        let tray = Self {
            hwnd,
            icon_on: make_icon(ON_BG)?,
            icon_off: make_icon(OFF_BG)?,
            active: false,
            callback,
            tip: tip.to_string(),
        };
        tray.register()?;
        Ok(tray)
    }

    fn data(&self) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: ICON_ID,
            ..Default::default()
        }
    }

    fn icon(&self) -> HICON {
        if self.active {
            self.icon_on
        } else {
            self.icon_off
        }
    }

    /// Adds the icon; also used after Explorer restarts ("TaskbarCreated").
    pub fn register(&self) -> Result<()> {
        let mut d = self.data();
        d.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP;
        d.uCallbackMessage = self.callback;
        d.hIcon = self.icon();
        copy_into(&mut d.szTip, &self.tip);
        // SAFETY: `d` is a fully initialised NOTIFYICONDATAW.
        unsafe {
            if !Shell_NotifyIconW(NIM_ADD, &d).as_bool() {
                bail!("could not add the tray icon");
            }
            d.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            let _ = Shell_NotifyIconW(NIM_SETVERSION, &d);
        }
        Ok(())
    }

    /// Switches the icon (on/off) and tooltip.
    pub fn set_state(&mut self, active: bool, tip: &str) {
        self.active = active;
        self.tip = tip.to_string();
        let mut d = self.data();
        d.uFlags = NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        d.hIcon = self.icon();
        copy_into(&mut d.szTip, tip);
        // SAFETY: as in `register`.
        let _ = unsafe { Shell_NotifyIconW(NIM_MODIFY, &d) };
    }

    pub fn notify(&self, title: &str, text: &str, warning: bool) {
        let mut d = self.data();
        d.uFlags = NIF_INFO;
        d.dwInfoFlags = if warning { NIIF_WARNING } else { NIIF_INFO };
        copy_into(&mut d.szInfoTitle, title);
        copy_into(&mut d.szInfo, text);
        // SAFETY: as in `register`.
        let _ = unsafe { Shell_NotifyIconW(NIM_MODIFY, &d) };
    }

    pub fn remove(&self) {
        // SAFETY: as in `register`.
        let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &self.data()) };
    }
}

/// Shows a context menu for `owner` at the cursor; returns the chosen item id
/// (0 = none). Items: (id, label); id 0 is a disabled header, "-" a
/// separator; ids in `checked` get a check mark. Runs a modal loop, so call
/// it without holding the app state.
pub fn menu(owner: HWND, items: &[(usize, &str)], checked: &[usize]) -> usize {
    // SAFETY: the menu is destroyed before returning; label buffers
    // outlive AppendMenuW, which copies them.
    unsafe {
        let Ok(menu) = CreatePopupMenu() else {
            return 0;
        };
        for &(id, label) in items {
            let text = wide(label);
            let flags = match (id, label) {
                (_, "-") => MF_SEPARATOR,
                (0, _) => MF_STRING | MF_GRAYED,
                _ if checked.contains(&id) => MF_STRING | MF_CHECKED,
                _ => MF_STRING,
            };
            let _ = AppendMenuW(menu, flags, id, PCWSTR(text.as_ptr()));
        }
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        // Required so the menu closes when the user clicks elsewhere.
        let _ = SetForegroundWindow(owner);
        let id = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
            pt.x,
            pt.y,
            None,
            owner,
            None,
        );
        let _ = PostMessageW(Some(owner), WM_NULL, Default::default(), Default::default());
        let _ = DestroyMenu(menu);
        id.0 as usize
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        // SAFETY: we created the icons.
        unsafe {
            let _ = DestroyIcon(self.icon_on);
            let _ = DestroyIcon(self.icon_off);
        }
    }
}

/// Draws the tray icon at runtime: a white "எ" on a `bg` square, so the
/// exe needs no resource file.
fn make_icon(bg_color: COLORREF) -> Result<HICON> {
    // SAFETY: every GDI object created here is selected out and deleted
    // before returning; the DIB is 32 bpp, top-down, size x size.
    unsafe {
        let size = GetSystemMetrics(SM_CXSMICON).max(16) * 2;
        let screen = GetDC(None);
        let dc = CreateCompatibleDC(Some(screen));
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size,
                biHeight: -size,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let color = CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0)?;
        // All-zero mask = fully opaque (the DIB's alpha is left at 0).
        let mask = CreateBitmap(size, size, 1, 1, None);
        let old = SelectObject(dc, HGDIOBJ(color.0));
        let rect = RECT {
            left: 0,
            top: 0,
            right: size,
            bottom: size,
        };
        let bg = CreateSolidBrush(bg_color);
        FillRect(dc, &rect, bg);
        let font = CreateFontW(
            -(size * 7 / 8),
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
            CLEARTYPE_QUALITY,
            0,
            PCWSTR(wide("Nirmala UI").as_ptr()),
        );
        let old_font = SelectObject(dc, HGDIOBJ(font.0));
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, COLORREF(0x00FF_FFFF));
        let mut glyph: Vec<u16> = "எ".encode_utf16().collect();
        let mut r = rect;
        DrawTextW(
            dc,
            &mut glyph,
            &mut r,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );
        SelectObject(dc, old_font);
        SelectObject(dc, old);
        let info = ICONINFO {
            fIcon: true.into(),
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        };
        let icon = CreateIconIndirect(&info);
        let _ = DeleteObject(HGDIOBJ(font.0));
        let _ = DeleteObject(HGDIOBJ(bg.0));
        let _ = DeleteObject(HGDIOBJ(color.0));
        let _ = DeleteObject(HGDIOBJ(mask.0));
        let _ = DeleteDC(dc);
        ReleaseDC(None, screen);
        Ok(icon?)
    }
}
