//! ezhuthu: Tamil phonetic input for Windows. A tray app: press the hotkey
//! in any app, type romanized Tamil, press Enter to insert the Tamil text.

// No console window in release builds; debug builds keep it for logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod caret;
mod compose;
mod config;
mod engine;
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
