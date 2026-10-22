//! ezhuthu-setup: installs ezhuthu for the current user (no admin rights).
//!
//! The app and model files are embedded at build time (see build.rs). Setup
//! lets the user pick the install location (default
//! %LOCALAPPDATA%\Programs\ezhuthu), copies the files there, adds a Start Menu
//! shortcut and a Settings > Apps entry (uninstall runs
//! `ezhuthu.exe --uninstall`), asks about starting with Windows, and launches
//! the app. Running it again updates an existing install in place; user
//! settings in %APPDATA% are untouched.
//!
//! Unattended: `ezhuthu-setup.exe --silent [--dir <folder>] [--autostart]
//! [--no-launch]` shows no dialogs and reports failure through the exit code (1).

#![windows_subsystem = "windows"]

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use ezhuthu_install as install;
use windows::Win32::Foundation::ERROR_CANCELLED;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Shell::{
    FOS_FORCEFILESYSTEM, FOS_PATHMUSTEXIST, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog,
    IShellItem, SHCreateItemFromParsingName, SIGDN_FILESYSPATH,
};
use windows::Win32::UI::WindowsAndMessaging::{
    IDYES, MB_ICONERROR, MB_ICONINFORMATION, MB_ICONQUESTION, MB_ICONWARNING, MB_OK, MB_YESNO,
    MESSAGEBOX_RESULT, MESSAGEBOX_STYLE, MessageBoxW,
};
use windows::core::{HSTRING, w};

mod payload {
    include!(concat!(env!("OUT_DIR"), "/payload.rs"));
}

const TITLE: &str = "ezhuthu setup";
const VERSION: &str = env!("CARGO_PKG_VERSION");

struct Options {
    silent: bool,
    /// Exact install folder (`--dir`), instead of asking.
    dir: Option<PathBuf>,
    /// Silent mode only: turn on Start with Windows.
    autostart: bool,
    launch: bool,
}

impl Options {
    fn from_args() -> Result<Self> {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let has = |flag: &str| args.iter().any(|a| a == flag);
        let dir = match args.iter().position(|a| a == "--dir") {
            Some(i) => Some(PathBuf::from(
                args.get(i + 1).context("--dir needs a folder")?,
            )),
            None => None,
        };
        Ok(Self {
            silent: has("--silent"),
            dir,
            autostart: has("--autostart"),
            launch: !has("--no-launch"),
        })
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
    let silent = std::env::args().any(|a| a == "--silent");
    if let Err(e) = Options::from_args().and_then(|opts| run(&opts)) {
        if !silent {
            message(&format!("Setup failed:\n\n{e:#}"), MB_OK | MB_ICONERROR);
        }
        std::process::exit(1);
    }
}

fn run(opts: &Options) -> Result<()> {
    if payload::FILES.is_empty() {
        bail!("this setup was built without the app files; build it with tools/build-installer.sh");
    }
    let Some(dir) = choose_dir(opts)? else {
        return Ok(());
    };
    let exe = dir.join(install::EXE_NAME);

    if !install::close_running_app(Duration::from_secs(5)) {
        bail!("ezhuthu is running and didn't close; quit it from the tray and run setup again");
    }
    let bytes = write_files(&dir)?;
    let names: Vec<&str> = payload::FILES.iter().map(|(name, _)| *name).collect();
    install::record_files(&dir, &names)?;
    install::create_shortcut(
        &exe,
        &install::start_menu_shortcut()?,
        "ezhuthu: Tamil phonetic input",
    )?;
    install::register_uninstall(&dir, VERSION, (bytes / 1024) as u32)?;

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
    opts.tell(&format!(
        "ezhuthu is installed in {} and running in the tray (the எ icon).\n\n\
         Press Ctrl+Alt+T in any app to switch Tamil typing on or off.\n\
         To remove it, use Settings > Apps > ezhuthu.",
        dir.display()
    ));
    Ok(())
}

/// Decides the install folder: the existing install (updated in place),
/// `--dir`, the default (silent), or the user's pick. None = cancelled.
fn choose_dir(opts: &Options) -> Result<Option<PathBuf>> {
    if let Some(existing) = install::installed_dir() {
        if let Some(d) = &opts.dir
            && !install::same_dir(d, &existing)
        {
            bail!(
                "ezhuthu is already installed in {}; uninstall it first to install elsewhere",
                existing.display()
            );
        }
        let question = format!(
            "Update ezhuthu to {VERSION} in {}?\n\n\
             (To install it somewhere else, first uninstall it from Settings > Apps.)",
            existing.display()
        );
        return Ok(opts.ask(&question, true).then_some(existing));
    }
    if let Some(d) = &opts.dir {
        ensure_writable(d)?;
        return Ok(Some(d.clone()));
    }
    let default_parent = install::default_parent()?;
    if opts.silent {
        let d = install::dir_in(&default_parent);
        ensure_writable(&d)?;
        return Ok(Some(d));
    }

    let intro = format!(
        "Install ezhuthu {VERSION} (Tamil phonetic input) for this user?\n\n\
         Next, choose where to install it. A folder named \"ezhuthu\" is created in \
         the location you pick (the default is {}).\n\nNo administrator rights are needed.",
        default_parent.display()
    );
    if !opts.ask(&intro, true) {
        return Ok(None);
    }
    let _ = std::fs::create_dir_all(&default_parent); // so the picker can open there
    let mut start = default_parent;
    loop {
        let Some(picked) = pick_folder(&start)? else {
            return Ok(None);
        };
        let dir = install::dir_in(&picked);
        start = picked;
        if let Err(e) = ensure_writable(&dir) {
            message(
                &format!(
                    "ezhuthu can't be installed in {}:\n{e:#}\n\n\
                     Pick a folder you can write to, for example one inside your user \
                     folder. (Program Files needs administrator rights.)",
                    dir.display()
                ),
                MB_OK | MB_ICONWARNING,
            );
            continue;
        }
        if opts.ask(&format!("Install ezhuthu in:\n\n{}", dir.display()), true) {
            return Ok(Some(dir));
        }
    }
}

/// Creates `dir` if needed and checks that files can be written there.
fn ensure_writable(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let probe = dir.join(".ezhuthu-write-test");
    std::fs::write(&probe, b"").with_context(|| format!("writing in {}", dir.display()))?;
    let _ = std::fs::remove_file(probe);
    Ok(())
}

/// The standard Windows folder picker, opened at `start`. None = cancelled.
fn pick_folder(start: &Path) -> Result<Option<PathBuf>> {
    // SAFETY: COM is initialised for this thread; the dialog and shell items
    // are released when dropped; the returned path string is freed with
    // CoTaskMemFree as the API requires.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
        dialog.SetOptions(
            dialog.GetOptions()? | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST,
        )?;
        dialog.SetTitle(w!("Choose where to install ezhuthu"))?;
        dialog.SetOkButtonLabel(w!("Install here"))?;
        if let Ok(item) =
            SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(start.as_os_str()), None)
        {
            let _ = dialog.SetFolder(&item);
        }
        if let Err(e) = dialog.Show(None) {
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
