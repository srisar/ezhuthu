//! Notification-area icon: tooltip, balloon notices and the context menu.

use anyhow::{Result, bail};
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::UI::Controls::{LIM_SMALL, LoadIconMetric};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIIF_INFO, NIIF_WARNING, NIM_ADD,
    NIM_DELETE, NIM_MODIFY, NIM_SETVERSION, NOTIFYICON_VERSION_4, NOTIFYICONDATAW,
    Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyIcon, DestroyMenu, GetCursorPos, HICON, MF_CHECKED,
    MF_GRAYED, MF_SEPARATOR, MF_STRING, PostMessageW, SetForegroundWindow, TPM_NONOTIFY,
    TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, WM_NULL,
};
use windows::core::PCWSTR;

use crate::win::{copy_into, instance, wide};

const ICON_ID: u32 = 1;
/// Icon resources linked in by build.rs (crates/app/icons): the letter with
/// a cursor while Tamil mode is on, the letter on grey while off.
const IDI_TRAY_ON: u16 = 2;
const IDI_TRAY_OFF: u16 = 3;

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
            icon_on: load_icon(IDI_TRAY_ON)?,
            icon_off: load_icon(IDI_TRAY_OFF)?,
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

/// Loads a tray icon at the small-icon size for the system DPI, picking the
/// best of the sizes drawn in the .ico.
fn load_icon(id: u16) -> Result<HICON> {
    // SAFETY: `id` is an integer resource (MAKEINTRESOURCE) naming an icon
    // group that build.rs links into this exe; Tray's drop destroys it.
    Ok(unsafe {
        LoadIconMetric(
            Some(instance()),
            PCWSTR(id as usize as *const u16),
            LIM_SMALL,
        )?
    })
}
