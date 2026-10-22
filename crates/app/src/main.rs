//! ezhuthu: Tamil phonetic input for Windows. A tray app: the hotkey toggles
//! Tamil mode, in which romanized words typed in any app become Tamil.
//!
//! `ezhuthu.exe --uninstall` removes an installed copy (Settings > Apps runs it).

// No console window in release builds; debug builds keep it for logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod caret;
mod compose;
mod config;
mod engine;
mod focus;
mod hotkey;
mod insert;
mod popup;
mod tray;
mod win;

use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::core::w;

fn main() {
    if std::env::args().skip(1).any(|a| a == "--uninstall") {
        uninstall();
        return;
    }
    // SAFETY: process-wide setup calls with valid arguments. The manifest
    // already declares per-monitor-v2 DPI awareness; this is a fallback.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        // One instance per user session. The handle lives until exit.
        let _mutex = CreateMutexW(None, true, w!(r"Local\ezhuthu-single-instance"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            win::error_box(
                None,
                "ezhuthu is already running. Look for its icon in the tray.",
            );
            return;
        }
    }
    if let Err(e) = app::run() {
        app::report_fatal(&e);
    }
}

/// Settings > Apps > Uninstall runs `ezhuthu.exe --uninstall`;
/// add `--silent` for no dialogs (failure is reported as exit code 1).
fn uninstall() {
    let silent = std::env::args().skip(1).any(|a| a == "--silent");
    let fail = |msg: &str| {
        if !silent {
            win::error_box(None, msg);
        }
        std::process::exit(1);
    };
    let here = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.to_path_buf()));
    let installed = ezhuthu_install::installed_dir();
    let Some(dir) = here.filter(|d| {
        installed
            .as_deref()
            .is_some_and(|i| ezhuthu_install::same_dir(d, i))
    }) else {
        fail("This copy of ezhuthu isn't the installed one, so there is nothing to uninstall.");
        return;
    };
    if !silent
        && !win::ask(
            "Remove ezhuthu from this computer?\n\nYour settings in %APPDATA%\\ezhuthu are kept.",
        )
    {
        return;
    }
    match ezhuthu_install::uninstall(&dir) {
        Ok(()) if !silent => win::info_box("ezhuthu was removed."),
        Ok(()) => {}
        Err(e) => fail(&format!("Uninstall failed:\n\n{e:#}")),
    }
}
