//! ezhuthu-setup: installs ezhuthu for the current user (no admin rights).
//!
//! The app and model files are embedded at build time (see build.rs). Setup
//! copies them to %LOCALAPPDATA%\Programs\ezhuthu, adds a Start Menu shortcut
//! and a Settings > Apps entry (uninstall runs `ezhuthu.exe --uninstall`),
//! asks about starting with Windows, and launches the app. Running it again
//! updates an existing install; user settings in %APPDATA% are untouched.
//!
//! Unattended: `ezhuthu-setup.exe --silent [--autostart] [--no-launch]`
//! shows no dialogs and reports failure through the exit code (1).

#![windows_subsystem = "windows"]

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use ezhuthu_install as install;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{
    IDYES, MB_ICONERROR, MB_ICONINFORMATION, MB_ICONQUESTION, MB_OK, MB_YESNO, MESSAGEBOX_RESULT,
    MESSAGEBOX_STYLE, MessageBoxW,
};
use windows::core::HSTRING;

mod payload {
    include!(concat!(env!("OUT_DIR"), "/payload.rs"));
}

const TITLE: &str = "ezhuthu setup";

struct Options {
    silent: bool,
    /// Silent mode only: turn on Start with Windows.
    autostart: bool,
    launch: bool,
}

impl Options {
    fn from_args() -> Self {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let has = |flag: &str| args.iter().any(|a| a == flag);
        Self {
            silent: has("--silent"),
            autostart: has("--autostart"),
            launch: !has("--no-launch"),
        }
    }

    /// Asks a Yes/No question, or answers `silent_answer` without asking.
    fn ask(&self, text: &str, silent_answer: bool) -> bool {
        if self.silent {
            return silent_answer;
        }
        message(text, MB_YESNO | MB_ICONQUESTION) == IDYES
    }

    fn tell(&self, text: &str) {
        if !self.silent {
            message(text, MB_OK | MB_ICONINFORMATION);
        }
    }
}

fn message(text: &str, style: MESSAGEBOX_STYLE) -> MESSAGEBOX_RESULT {
    // SAFETY: both strings are valid for the duration of the call.
    unsafe { MessageBoxW(None, &HSTRING::from(text), &HSTRING::from(TITLE), style) }
}

fn main() {
    // SAFETY: process-wide setting with a valid argument (fallback for the manifest).
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    let opts = Options::from_args();
    if let Err(e) = run(&opts) {
        if !opts.silent {
            message(&format!("Setup failed:\n\n{e:#}"), MB_OK | MB_ICONERROR);
        }
        std::process::exit(1);
    }
}

fn run(opts: &Options) -> Result<()> {
    if payload::FILES.is_empty() {
        bail!("this setup was built without the app files; build it with tools/build-installer.sh");
    }
    let version = env!("CARGO_PKG_VERSION");
    let dir = install::install_dir()?;
    let exe = dir.join(install::EXE_NAME);
    let action = if exe.exists() { "Update" } else { "Install" };
    let question = format!(
        "{action} ezhuthu {version} (Tamil phonetic input) for this user?\n\n\
         Folder: {}\n\nNo administrator rights are needed.",
        dir.display()
    );
    if !opts.ask(&question, true) {
        return Ok(());
    }

    if !install::close_running_app(Duration::from_secs(5)) {
        bail!("ezhuthu is running and didn't close; quit it from the tray and run setup again");
    }
    let bytes = write_files(&dir)?;
    install::create_shortcut(
        &exe,
        &install::start_menu_shortcut()?,
        "ezhuthu: Tamil phonetic input",
    )?;
    install::register_uninstall(&dir, version, (bytes / 1024) as u32)?;

    let autostart = opts.ask(
        "Start ezhuthu automatically when you sign in to Windows?\n\n\
         You can change this later from the tray menu (Start with Windows).",
        opts.autostart,
    );
    install::set_autostart(autostart.then_some(exe.as_path()))?;

    if opts.launch {
        std::process::Command::new(&exe)
            .current_dir(&dir)
            .spawn()
            .with_context(|| format!("starting {}", exe.display()))?;
    }
    opts.tell(
        "ezhuthu is installed and running in the tray (the எ icon).\n\n\
         Press Ctrl+Alt+T in any app to switch Tamil typing on or off.\n\
         To remove it, use Settings > Apps > ezhuthu.",
    );
    Ok(())
}

/// Writes every embedded file under `dir`; returns the total size in bytes.
/// Each file is written next to its target and then renamed over it, so an
/// interrupted update never leaves a half-written file in place.
fn write_files(dir: &Path) -> Result<u64> {
    let mut total = 0;
    for (name, bytes) in payload::FILES {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let tmp = path.with_extension("partial");
        std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &path).with_context(|| format!("replacing {}", path.display()))?;
        total += bytes.len() as u64;
    }
    Ok(total)
}
