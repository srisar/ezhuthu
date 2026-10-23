//! ezhuthu-setup: installs ezhuthu for the current user (no admin rights).
//!
//! The app and model files are embedded at build time (see build.rs). A small
//! window shows the install folder (default %LOCALAPPDATA%\Programs\ezhuthu)
//! with a Browse button and a "Start with Windows" checkbox. Setup then
//! restricts the folder to this user, copies the files, adds a Start Menu
//! shortcut and a Settings > Apps entry (uninstall runs
//! `ezhuthu.exe --uninstall`), and launches the app. Running it again updates
//! an existing install in place; user settings in %APPDATA% are untouched.
//!
//! Unattended: `ezhuthu-setup.exe --silent [--dir <folder>] [--autostart]
//! [--no-launch]` shows no window and reports failure through the exit code (1).

#![windows_subsystem = "windows"]

mod ui;

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use ezhuthu_install as install;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{
    MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MESSAGEBOX_STYLE, MessageBoxW,
};
use windows::core::HSTRING;

mod payload {
    include!(concat!(env!("OUT_DIR"), "/payload.rs"));
}

const TITLE: &str = "ezhuthu setup";
const VERSION: &str = env!("CARGO_PKG_VERSION");

struct Options {
    silent: bool,
    /// Exact install folder (`--dir`), silent mode.
    dir: Option<PathBuf>,
    /// Silent mode: turn on Start with Windows.
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
}

fn message(text: &str, style: MESSAGEBOX_STYLE) {
    // SAFETY: both strings are valid for the duration of the call.
    unsafe { MessageBoxW(None, &HSTRING::from(text), &HSTRING::from(TITLE), style) };
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
    // Uninstall deletes only the files on this list, so never install others.
    let allowed = install::installable_files();
    if let Some((name, _)) = payload::FILES
        .iter()
        .find(|(n, _)| !allowed.iter().any(|a| a == n))
    {
        bail!("this setup contains an unexpected file ({name})");
    }

    let existing = install::installed_dir();
    let (dir, autostart) = if opts.silent {
        (silent_dir(opts, existing.as_deref())?, opts.autostart)
    } else {
        let updating = existing.is_some();
        let initial = match &existing {
            Some(d) => d.clone(),
            None => install::dir_in(&install::default_parent()?),
        };
        let autostart = if updating {
            install::autostart_enabled()
        } else {
            true
        };
        match ui::run(&initial, updating, autostart)? {
            Some(choice) => (choice.dir, choice.autostart),
            None => return Ok(()),
        }
    };
    let exe = dir.join(install::EXE_NAME);

    if !install::close_running_app(Duration::from_secs(5)) {
        bail!("ezhuthu is running and didn't close; quit it from the tray and run setup again");
    }
    install::check_install_dir(&dir)?;
    ensure_writable(&dir)?;
    install::secure_dir(&dir)?;
    // Again, now that only this user can add files: anything that slipped in
    // between the first check and the lock-down is caught here.
    install::check_install_dir(&dir)?;
    let bytes = write_files(&dir)?;
    install::create_shortcut(
        &exe,
        &install::start_menu_shortcut()?,
        "ezhuthu: Tamil phonetic input",
    )?;
    install::register_uninstall(&dir, VERSION, (bytes / 1024) as u32)?;
    install::set_autostart(autostart.then_some(exe.as_path()))?;

    if opts.launch {
        std::process::Command::new(&exe)
            .current_dir(&dir)
            .spawn()
            .with_context(|| format!("starting {}", exe.display()))?;
    }
    if !opts.silent {
        message(
            &format!(
                "ezhuthu is installed in {} and running in the tray (the எ icon).\n\n\
                 Press Ctrl+Alt+T in any app to switch Tamil typing on or off.\n\
                 To remove it, use Settings > Apps > ezhuthu.",
                dir.display()
            ),
            MB_OK | MB_ICONINFORMATION,
        );
    }
    Ok(())
}

/// Silent mode: the existing install, `--dir`, or the default folder.
fn silent_dir(opts: &Options, existing: Option<&Path>) -> Result<PathBuf> {
    match (existing, &opts.dir) {
        (Some(e), Some(d)) if !install::same_dir(d, e) => bail!(
            "ezhuthu is already installed in {}; uninstall it first to install elsewhere",
            e.display()
        ),
        (Some(e), _) => Ok(e.to_path_buf()),
        (None, Some(d)) => Ok(d.clone()),
        (None, None) => Ok(install::dir_in(&install::default_parent()?)),
    }
}

/// Creates `dir` if needed and checks that files can be written there.
pub(crate) fn ensure_writable(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let probe = dir.join(".ezhuthu-write-test");
    std::fs::write(&probe, b"").with_context(|| format!("writing in {}", dir.display()))?;
    let _ = std::fs::remove_file(probe);
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
