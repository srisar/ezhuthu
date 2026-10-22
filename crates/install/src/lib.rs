//! Per-user install state for ezhuthu, shared by the app and `ezhuthu-setup`:
//! install folder, "Start with Windows" (HKCU Run), the Settings > Apps
//! uninstall entry, the Start Menu shortcut, and closing a running instance.
//!
//! Everything lives under the current user (HKCU, %LOCALAPPDATA%, %APPDATA%),
//! so neither installing nor uninstalling needs administrator rights.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use windows::Win32::Foundation::{
    ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, LPARAM, WIN32_ERROR, WPARAM,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, IPersistFile,
};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ,
    RRF_RT_REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegDeleteValueW, RegGetValueW,
    RegSetValueExW,
};
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW, WM_CLOSE};
use windows::core::{HSTRING, Interface, PCWSTR, w};

pub const APP_NAME: &str = "ezhuthu";
pub const EXE_NAME: &str = "ezhuthu.exe";
/// Window class of the running app's hidden main window.
pub const MAIN_WINDOW_CLASS: &str = "ezhuthu.main";

const RUN_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const UNINSTALL_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Uninstall\ezhuthu");
const VALUE_NAME: PCWSTR = w!("ezhuthu");

/// `%LOCALAPPDATA%\Programs\ezhuthu`, the usual per-user program folder.
pub fn install_dir() -> Result<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA").context("LOCALAPPDATA is not set")?;
    Ok(PathBuf::from(local).join("Programs").join(APP_NAME))
}

/// `%APPDATA%\Microsoft\Windows\Start Menu\Programs\ezhuthu.lnk`.
pub fn start_menu_shortcut() -> Result<PathBuf> {
    let roaming = std::env::var_os("APPDATA").context("APPDATA is not set")?;
    Ok(PathBuf::from(roaming)
        .join(r"Microsoft\Windows\Start Menu\Programs")
        .join(format!("{APP_NAME}.lnk")))
}

// ---------------------------------------------------------------- registry

fn check(err: WIN32_ERROR, what: &str) -> Result<()> {
    if err == ERROR_SUCCESS {
        Ok(())
    } else {
        bail!("{what} failed (error {})", err.0)
    }
}

/// An HKCU key opened for writing; closed on drop.
struct Key(HKEY);

impl Key {
    fn create(path: PCWSTR) -> Result<Self> {
        let mut key = HKEY::default();
        // SAFETY: creating/opening a key under HKCU; the handle is closed on drop.
        let err = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                path,
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                None,
                &mut key,
                None,
            )
        };
        check(err, "opening a registry key")?;
        Ok(Self(key))
    }

    fn set_str(&self, name: &str, value: &str) -> Result<()> {
        let data: Vec<u8> = value
            .encode_utf16()
            .chain(std::iter::once(0))
            .flat_map(u16::to_le_bytes)
            .collect();
        // SAFETY: `data` is a NUL-terminated UTF-16 string as REG_SZ requires.
        let err =
            unsafe { RegSetValueExW(self.0, &HSTRING::from(name), None, REG_SZ, Some(&data)) };
        check(err, "writing a registry value")
    }

    fn set_dword(&self, name: &str, value: u32) -> Result<()> {
        // SAFETY: four bytes, as REG_DWORD requires.
        let err = unsafe {
            RegSetValueExW(
                self.0,
                &HSTRING::from(name),
                None,
                REG_DWORD,
                Some(&value.to_le_bytes()),
            )
        };
        check(err, "writing a registry value")
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: we own the handle.
        let _ = unsafe { RegCloseKey(self.0) };
    }
}

/// The command stored in HKCU Run, if "Start with Windows" is on.
pub fn autostart_command() -> Option<String> {
    let mut buf = vec![0u16; 1024];
    let mut len = (buf.len() * 2) as u32;
    // SAFETY: `buf` holds `len` bytes; RegGetValueW NUL-terminates REG_SZ.
    let err = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            VALUE_NAME,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut len),
        )
    };
    if err != ERROR_SUCCESS {
        return None;
    }
    let units = (len as usize / 2).saturating_sub(1);
    Some(String::from_utf16_lossy(&buf[..units]))
}

pub fn autostart_enabled() -> bool {
    autostart_command().is_some()
}

/// Turns "Start with Windows" on (starting `exe`) or off.
pub fn set_autostart(exe: Option<&Path>) -> Result<()> {
    match exe {
        Some(exe) => Key::create(RUN_KEY)?.set_str("ezhuthu", &format!("\"{}\"", exe.display())),
        None => {
            // SAFETY: deleting our own value; a missing value is fine.
            let err = unsafe {
                let mut key = HKEY::default();
                let open = RegCreateKeyExW(
                    HKEY_CURRENT_USER,
                    RUN_KEY,
                    None,
                    PCWSTR::null(),
                    REG_OPTION_NON_VOLATILE,
                    KEY_SET_VALUE,
                    None,
                    &mut key,
                    None,
                );
                check(open, "opening the Run key")?;
                let err = RegDeleteValueW(key, VALUE_NAME);
                let _ = RegCloseKey(key);
                err
            };
            if err == ERROR_SUCCESS || err == ERROR_FILE_NOT_FOUND {
                Ok(())
            } else {
                check(err, "removing the Run value")
            }
        }
    }
}

/// Registers ezhuthu in Settings > Apps (per user), uninstalled via
/// `ezhuthu.exe --uninstall`.
pub fn register_uninstall(dir: &Path, version: &str, size_kb: u32) -> Result<()> {
    let exe = dir.join(EXE_NAME);
    let key = Key::create(UNINSTALL_KEY)?;
    key.set_str("DisplayName", "ezhuthu (Tamil phonetic input)")?;
    key.set_str("DisplayVersion", version)?;
    key.set_str("Publisher", "ezhuthu")?;
    key.set_str("InstallLocation", &dir.display().to_string())?;
    key.set_str("DisplayIcon", &exe.display().to_string())?;
    key.set_str(
        "UninstallString",
        &format!("\"{}\" --uninstall", exe.display()),
    )?;
    key.set_dword("EstimatedSize", size_kb)?;
    key.set_dword("NoModify", 1)?;
    key.set_dword("NoRepair", 1)?;
    Ok(())
}

pub fn unregister_uninstall() -> Result<()> {
    // SAFETY: deleting our own key tree under HKCU; a missing key is fine.
    let err = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, UNINSTALL_KEY) };
    if err == ERROR_SUCCESS || err == ERROR_FILE_NOT_FOUND {
        Ok(())
    } else {
        check(err, "removing the uninstall entry")
    }
}

// ---------------------------------------------------------------- shortcut

/// Creates (or replaces) a shell shortcut at `lnk` pointing to `target`.
pub fn create_shortcut(target: &Path, lnk: &Path, description: &str) -> Result<()> {
    if let Some(dir) = lnk.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    // SAFETY: COM is initialised for this thread (repeat calls are harmless);
    // the shell link object is released when the interfaces drop.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        link.SetPath(&HSTRING::from(target.as_os_str()))?;
        if let Some(dir) = target.parent() {
            link.SetWorkingDirectory(&HSTRING::from(dir.as_os_str()))?;
        }
        link.SetDescription(&HSTRING::from(description))?;
        link.SetIconLocation(&HSTRING::from(target.as_os_str()), 0)?;
        link.cast::<IPersistFile>()?
            .Save(&HSTRING::from(lnk.as_os_str()), true)
            .with_context(|| format!("saving {}", lnk.display()))?;
    }
    Ok(())
}

// ---------------------------------------------------------------- running instance

fn main_window() -> Option<windows::Win32::Foundation::HWND> {
    // SAFETY: plain lookup of a top-level window by class.
    unsafe { FindWindowW(&HSTRING::from(MAIN_WINDOW_CLASS), PCWSTR::null()) }.ok()
}

pub fn app_running() -> bool {
    main_window().is_some()
}

/// Asks a running ezhuthu to quit (it removes its tray icon) and waits for it.
/// Returns false if it is still running after `timeout`.
pub fn close_running_app(timeout: Duration) -> bool {
    let Some(hwnd) = main_window() else {
        return true;
    };
    // SAFETY: posting WM_CLOSE to the app's own hidden window.
    let _ = unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) };
    let start = Instant::now();
    while start.elapsed() < timeout {
        if !app_running() {
            // The window is gone; give the process a moment to exit and
            // release its files.
            std::thread::sleep(Duration::from_millis(300));
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

// ---------------------------------------------------------------- uninstall

/// Removes ezhuthu for this user: stops it, deletes the Run value, uninstall
/// entry and shortcut, then deletes `dir` once this process has exited (it
/// may be running from that folder). User settings in %APPDATA% are kept.
pub fn uninstall(dir: &Path) -> Result<()> {
    if !close_running_app(Duration::from_secs(5)) {
        bail!("ezhuthu is still running; quit it from the tray and try again");
    }
    set_autostart(None)?;
    unregister_uninstall()?;
    if let Ok(lnk) = start_menu_shortcut() {
        let _ = std::fs::remove_file(lnk);
    }
    delete_after_exit(dir)
}

/// Deletes `dir` from a hidden `cmd` that first waits ~2 s for us to exit.
fn delete_after_exit(dir: &Path) -> Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    // Only ever delete the folder this crate installs to.
    if dir != install_dir()? {
        bail!(
            "refusing to delete {}: not the ezhuthu install folder",
            dir.display()
        );
    }
    std::process::Command::new("cmd.exe")
        .raw_arg(format!(
            "/d /c ping -n 3 127.0.0.1 >nul & rd /s /q \"{}\"",
            dir.display()
        ))
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .context("starting the cleanup process")?;
    Ok(())
}
