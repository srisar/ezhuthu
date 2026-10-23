//! The setup window: install folder (with Browse…), "Start with Windows",
//! and Install / Cancel. Native controls in the system message font.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use anyhow::Result;
use ezhuthu_install as install;
use windows::Win32::Foundation::{ERROR_CANCELLED, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    COLOR_WINDOW, CreateFontIndirectW, DeleteObject, GetMonitorInfoW, GetSysColorBrush, HBRUSH,
    HDC, HGDIOBJ, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint, SetBkMode, TRANSPARENT,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{
    BST_CHECKED, BST_UNCHECKED, CheckDlgButton, EM_SETREADONLY, IsDlgButtonChecked,
};
use windows::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, GetDpiForSystem, SystemParametersInfoForDpi,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
use windows::Win32::UI::Shell::{
    FOS_FORCEFILESYSTEM, FOS_PATHMUSTEXIST, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog,
    IShellItem, SHCreateItemFromParsingName, SIGDN_FILESYSPATH,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BS_AUTOCHECKBOX, BS_DEFPUSHBUTTON, BS_PUSHBUTTON, CreateWindowExW, DefWindowProcW,
    DestroyWindow, DispatchMessageW, ES_AUTOHSCROLL, GetDlgItem, GetMessageW, GetWindowTextLengthW,
    GetWindowTextW, HMENU, IDCANCEL, IDOK, IDYES, IsDialogMessageW, MB_ICONQUESTION,
    MB_ICONWARNING, MB_OK, MB_YESNO, MSG, MessageBoxW, NONCLIENTMETRICSW, PostQuitMessage,
    RegisterClassExW, SPI_GETNONCLIENTMETRICS, SW_SHOW, SendMessageW, SetWindowTextW, ShowWindow,
    TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_CTLCOLORSTATIC,
    WM_DESTROY, WM_SETFONT, WNDCLASSEXW, WS_BORDER, WS_CAPTION, WS_CHILD, WS_EX_CLIENTEDGE,
    WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE,
};
use windows::core::{HSTRING, PCWSTR, w};

const ID_BROWSE: i32 = 3;
const ID_PATH: i32 = 4;
const ID_AUTOSTART: i32 = 5;

/// What the user chose.
pub struct Choice {
    pub dir: PathBuf,
    pub autostart: bool,
}

struct State {
    updating: bool,
    choice: Option<Choice>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn message(
    owner: HWND,
    text: &str,
    style: windows::Win32::UI::WindowsAndMessaging::MESSAGEBOX_STYLE,
) -> bool {
    // SAFETY: both strings are valid for the duration of the call.
    let r = unsafe {
        MessageBoxW(
            Some(owner),
            &HSTRING::from(text),
            &HSTRING::from(crate::TITLE),
            style,
        )
    };
    r == IDYES
}

/// Shows the setup window. `path` is prefilled (the default folder, or the
/// existing install when `updating`, which can't be changed). None = cancelled.
pub fn run(path: &Path, updating: bool, autostart: bool) -> Result<Option<Choice>> {
    // SAFETY: standard window creation and message loop on this thread; all
    // strings outlive the calls that use them; the font is deleted at the end.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let instance = GetModuleHandleW(None)?;
        let class = w!("ezhuthu.setup");
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(window_proc),
            hInstance: instance.into(),
            hbrBackground: GetSysColorBrush(COLOR_WINDOW),
            hCursor: windows::Win32::UI::WindowsAndMessaging::LoadCursorW(
                None,
                windows::Win32::UI::WindowsAndMessaging::IDC_ARROW,
            )?,
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassExW(&wc);

        let dpi = GetDpiForSystem();
        let s = |v: i32| v * dpi as i32 / 96;
        let mut ncm = NONCLIENTMETRICSW {
            cbSize: size_of::<NONCLIENTMETRICSW>() as u32,
            ..Default::default()
        };
        let _ = SystemParametersInfoForDpi(
            SPI_GETNONCLIENTMETRICS.0,
            ncm.cbSize,
            Some(&mut ncm as *mut _ as *mut _),
            0,
            dpi,
        );
        let font = CreateFontIndirectW(&ncm.lfMessageFont);

        // Client area 480 x 232 (96-DPI units), centred on the primary monitor.
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        let mut r = RECT {
            left: 0,
            top: 0,
            right: s(480),
            bottom: s(232),
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
        let (x, y) = (
            work.left + (work.right - work.left - w) / 2,
            work.top + (work.bottom - work.top - h) / 2,
        );
        let title = HSTRING::from(crate::TITLE);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            &title,
            style,
            x,
            y,
            w,
            h,
            None,
            None,
            Some(instance.into()),
            None,
        )?;

        let child = |class: PCWSTR,
                     text: &str,
                     style: u32,
                     ex: WINDOW_EX_STYLE,
                     id: i32,
                     rect: (i32, i32, i32, i32)| {
            let ctl = CreateWindowExW(
                ex,
                class,
                &HSTRING::from(text),
                WS_CHILD | WS_VISIBLE | WINDOW_STYLE(style),
                s(rect.0),
                s(rect.1),
                s(rect.2),
                s(rect.3),
                Some(hwnd),
                Some(HMENU(id as isize as *mut _)),
                Some(instance.into()),
                None,
            );
            if let Ok(ctl) = ctl {
                SendMessageW(
                    ctl,
                    WM_SETFONT,
                    Some(WPARAM(font.0 as usize)),
                    Some(LPARAM(1)),
                );
            }
            ctl
        };
        let none = WINDOW_EX_STYLE(0);
        let heading = format!("ezhuthu {} — Tamil phonetic input", crate::VERSION);
        child(w!("STATIC"), &heading, 0, none, -1, (16, 16, 448, 20))?;
        child(
            w!("STATIC"),
            "Install folder:",
            0,
            none,
            -1,
            (16, 48, 448, 18),
        )?;
        let edit = child(
            w!("EDIT"),
            &path.display().to_string(),
            (WS_TABSTOP.0 | ES_AUTOHSCROLL as u32) & !WS_BORDER.0,
            WS_EX_CLIENTEDGE,
            ID_PATH,
            (16, 68, 360, 24),
        )?;
        let browse = child(
            w!("BUTTON"),
            "Browse…",
            WS_TABSTOP.0 | BS_PUSHBUTTON as u32,
            none,
            ID_BROWSE,
            (384, 67, 80, 26),
        )?;
        let note = if updating {
            "ezhuthu is already installed here and will be updated. To install it \
             somewhere else, uninstall it first (Settings > Apps)."
        } else {
            "Browse to choose another location; a folder named \"ezhuthu\" is created \
             there. No administrator rights are needed."
        };
        child(w!("STATIC"), note, 0, none, -1, (16, 100, 448, 36))?;
        child(
            w!("BUTTON"),
            "Start ezhuthu when I sign in to Windows",
            WS_TABSTOP.0 | BS_AUTOCHECKBOX as u32,
            none,
            ID_AUTOSTART,
            (16, 144, 448, 22),
        )?;
        let ok_label = if updating { "Update" } else { "Install" };
        child(
            w!("BUTTON"),
            ok_label,
            WS_TABSTOP.0 | BS_DEFPUSHBUTTON as u32,
            none,
            IDOK.0,
            (296, 188, 80, 28),
        )?;
        child(
            w!("BUTTON"),
            "Cancel",
            WS_TABSTOP.0 | BS_PUSHBUTTON as u32,
            none,
            IDCANCEL.0,
            (384, 188, 80, 28),
        )?;
        let _ = CheckDlgButton(
            hwnd,
            ID_AUTOSTART,
            if autostart {
                BST_CHECKED
            } else {
                BST_UNCHECKED
            },
        );
        if updating {
            SendMessageW(edit, EM_SETREADONLY, Some(WPARAM(1)), None);
            let _ = EnableWindow(browse, false);
        }

        STATE.with(|st| {
            *st.borrow_mut() = Some(State {
                updating,
                choice: None,
            });
        });
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetFocus(GetDlgItem(Some(hwnd), IDOK.0).ok());

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            // Tab, Enter (Install) and Esc (Cancel) like a dialog.
            if !IsDialogMessageW(hwnd, &msg).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        let state = STATE.with(|st| st.borrow_mut().take());
        let _ = DeleteObject(HGDIOBJ(font.0));
        Ok(state.and_then(|s| s.choice))
    }
}

fn text_of(hwnd: HWND, id: i32) -> String {
    // SAFETY: the buffer is sized from GetWindowTextLengthW plus the NUL.
    unsafe {
        let Ok(ctl) = GetDlgItem(Some(hwnd), id) else {
            return String::new();
        };
        let mut buf = vec![0u16; GetWindowTextLengthW(ctl) as usize + 1];
        let n = GetWindowTextW(ctl, &mut buf);
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }
}

fn on_browse(hwnd: HWND) {
    let current = PathBuf::from(text_of(hwnd, ID_PATH).trim());
    // Open the picker at the nearest folder that exists.
    let start = current
        .ancestors()
        .find(|p| p.is_dir())
        .map(Path::to_path_buf)
        .or_else(|| install::default_parent().ok())
        .unwrap_or_default();
    match pick_folder(hwnd, &start) {
        Ok(Some(picked)) => {
            let dir = install::dir_in(&picked);
            // SAFETY: setting the text of our own edit control.
            unsafe {
                if let Ok(edit) = GetDlgItem(Some(hwnd), ID_PATH) {
                    let _ = SetWindowTextW(edit, &HSTRING::from(dir.as_os_str()));
                }
            }
        }
        Ok(None) => {}
        Err(e) => {
            message(
                hwnd,
                &format!("Couldn't open the folder picker:\n{e:#}"),
                MB_OK | MB_ICONWARNING,
            );
        }
    }
}

/// Validates the folder; on success stores the choice and closes the window.
fn on_install(hwnd: HWND, updating: bool) {
    let text = text_of(hwnd, ID_PATH);
    let mut dir = PathBuf::from(text.trim());
    if !dir.is_absolute() {
        message(
            hwnd,
            "Enter the full path of a folder, like C:\\Users\\you\\Apps\\ezhuthu, or use Browse.",
            MB_OK | MB_ICONWARNING,
        );
        return;
    }
    // A folder that already holds other things (e.g. "D:\Tools") would get its
    // permissions tightened and be shared with ezhuthu; suggest a subfolder.
    if !updating && has_foreign_files(&dir) {
        let sub = install::dir_in(&dir);
        if sub != dir
            && message(
                hwnd,
                &format!(
                    "{} already contains other files.\n\nInstall ezhuthu in its own folder, {}?",
                    dir.display(),
                    sub.display()
                ),
                MB_YESNO | MB_ICONQUESTION,
            )
        {
            dir = sub;
        } else {
            return;
        }
    }
    if let Err(e) = install::check_install_dir(&dir) {
        message(hwnd, &format!("{e:#}"), MB_OK | MB_ICONWARNING);
        return;
    }
    // Outside the user's own folder, other accounts on this PC may be able to
    // move the folder aside and plant a look-alike ezhuthu.exe.
    let in_profile = std::env::var_os("USERPROFILE").is_some_and(|home| {
        let (d, h) = (
            dir.to_string_lossy().to_lowercase(),
            home.to_string_lossy().to_lowercase(),
        );
        d.starts_with(&(h.trim_end_matches('\\').to_string() + "\\"))
    });
    if !updating
        && !in_profile
        && !message(
            hwnd,
            &format!(
                "{} is outside your user folder.\n\nDepending on how this PC is set up, other \
                 accounts may be able to replace ezhuthu's files there, and ezhuthu runs as \
                 you. The default location inside your user folder is safer.\n\n\
                 Install here anyway?",
                dir.display()
            ),
            MB_YESNO | MB_ICONWARNING,
        )
    {
        return;
    }
    if let Err(e) = crate::ensure_writable(&dir) {
        message(
            hwnd,
            &format!(
                "ezhuthu can't be installed in {}:\n{e:#}\n\nChoose a folder you can write to, \
                 for example one inside your user folder. (Program Files needs \
                 administrator rights.)",
                dir.display()
            ),
            MB_OK | MB_ICONWARNING,
        );
        return;
    }
    // SAFETY: reading our own checkbox.
    let autostart = unsafe { IsDlgButtonChecked(hwnd, ID_AUTOSTART) } == BST_CHECKED.0;
    STATE.with(|st| {
        if let Some(s) = st.borrow_mut().as_mut() {
            s.choice = Some(Choice { dir, autostart });
        }
    });
    // SAFETY: destroying our own window ends the message loop (WM_DESTROY).
    let _ = unsafe { DestroyWindow(hwnd) };
}

/// True if `dir` exists and contains anything that isn't ezhuthu's.
fn has_foreign_files(dir: &Path) -> bool {
    let ours: Vec<String> = std::iter::once(install::EXE_NAME.to_string())
        .chain(["assets".to_string(), "install-files.txt".to_string()])
        .collect();
    std::fs::read_dir(dir).is_ok_and(|entries| {
        entries
            .flatten()
            .any(|e| !ours.iter().any(|o| e.file_name().eq_ignore_ascii_case(o)))
    })
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as i32;
            let updating = STATE.with(|st| st.borrow().as_ref().is_some_and(|s| s.updating));
            match id {
                ID_BROWSE => on_browse(hwnd),
                i if i == IDOK.0 => on_install(hwnd, updating),
                i if i == IDCANCEL.0 => {
                    // SAFETY: destroying our own window.
                    let _ = unsafe { DestroyWindow(hwnd) };
                }
                _ => {}
            }
            LRESULT(0)
        }
        // White background for labels and the checkbox, matching the window.
        WM_CTLCOLORSTATIC => {
            // SAFETY: wparam is the control's DC for this message.
            unsafe { SetBkMode(HDC(wparam.0 as *mut _), TRANSPARENT) };
            let brush: HBRUSH = unsafe { GetSysColorBrush(COLOR_WINDOW) };
            LRESULT(brush.0 as isize)
        }
        WM_CLOSE => {
            // SAFETY: destroying our own window.
            let _ = unsafe { DestroyWindow(hwnd) };
            LRESULT(0)
        }
        WM_DESTROY => {
            // SAFETY: ends our message loop.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        // SAFETY: default processing for everything else.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// The standard Windows folder picker, owned by `owner`, opened at `start`.
/// None = cancelled.
pub fn pick_folder(owner: HWND, start: &Path) -> Result<Option<PathBuf>> {
    // SAFETY: COM is initialised for this thread; the dialog and shell items
    // are released when dropped; the returned path string is freed with
    // CoTaskMemFree as the API requires.
    unsafe {
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
        dialog.SetOptions(
            dialog.GetOptions()? | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST,
        )?;
        dialog.SetTitle(w!("Choose where to install ezhuthu"))?;
        dialog.SetOkButtonLabel(w!("Select folder"))?;
        if let Ok(item) =
            SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(start.as_os_str()), None)
        {
            let _ = dialog.SetFolder(&item);
        }
        if let Err(e) = dialog.Show(Some(owner)) {
            if e.code() == ERROR_CANCELLED.to_hresult() {
                return Ok(None);
            }
            return Err(e.into());
        }
        let name = dialog.GetResult()?.GetDisplayName(SIGDN_FILESYSPATH)?;
        let path = name.to_string();
        CoTaskMemFree(Some(name.0 as *const _));
        Ok(Some(PathBuf::from(path?)))
    }
}
