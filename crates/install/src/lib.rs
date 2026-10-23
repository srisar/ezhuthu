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
    CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, HANDLE, HLOCAL, LPARAM, LocalFree,
    WIN32_ERROR, WPARAM,
};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
    GetNamedSecurityInfoW, SDDL_REVISION_1, SE_FILE_OBJECT, SetNamedSecurityInfoW,
};
use windows::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, GetTokenInformation,
    OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
    TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, IPersistFile,
};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ,
    RRF_RT_REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegDeleteValueW, RegGetValueW,
    RegSetValueExW,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW, WM_CLOSE};
use windows::core::{BOOL, HSTRING, Interface, PCWSTR, PWSTR, w};

pub const APP_NAME: &str = "ezhuthu";
/// The name people see (Start Menu, Settings > Apps, windows).
pub const DISPLAY_NAME: &str = "Ezhuthu";
pub const EXE_NAME: &str = "ezhuthu.exe";
/// Window class of the running app's hidden main window.
pub const MAIN_WINDOW_CLASS: &str = "ezhuthu.main";

const RUN_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const UNINSTALL_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Uninstall\ezhuthu");
const VALUE_NAME: PCWSTR = w!("ezhuthu");

/// `%LOCALAPPDATA%\Programs`, the usual per-user program location and the
/// folder picker's starting point.
pub fn default_parent() -> Result<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA").context("LOCALAPPDATA is not set")?;
    Ok(PathBuf::from(local).join("Programs"))
}

/// Install folder for a location the user picked: `<parent>\ezhuthu`, or the
/// picked folder itself if it is already named "ezhuthu".
pub fn dir_in(parent: &Path) -> PathBuf {
    let named_ours = parent
        .file_name()
        .is_some_and(|n| n.eq_ignore_ascii_case(APP_NAME));
    if named_ours {
        parent.to_path_buf()
    } else {
        parent.join(APP_NAME)
    }
}

/// Where ezhuthu is installed, from its Settings > Apps entry, if the
/// program is still there.
pub fn installed_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(read_str(UNINSTALL_KEY, w!("InstallLocation"))?);
    dir.join(EXE_NAME).is_file().then_some(dir)
}

/// True if `a` and `b` name the same folder (case-insensitive, as on NTFS).
pub fn same_dir(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().trim_end_matches('\\').to_lowercase();
    norm(a) == norm(b)
}

/// `%APPDATA%\Microsoft\Windows\Start Menu\Programs\Ezhuthu.lnk` (the file
/// name is what the Start Menu shows).
pub fn start_menu_shortcut() -> Result<PathBuf> {
    let roaming = std::env::var_os("APPDATA").context("APPDATA is not set")?;
    Ok(PathBuf::from(roaming)
        .join(r"Microsoft\Windows\Start Menu\Programs")
        .join(format!("{DISPLAY_NAME}.lnk")))
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
    read_str(RUN_KEY, VALUE_NAME)
}

/// Reads a REG_SZ value under HKCU.
fn read_str(subkey: PCWSTR, value: PCWSTR) -> Option<String> {
    let mut buf = vec![0u16; 1024];
    let mut len = (buf.len() * 2) as u32;
    // SAFETY: `buf` holds `len` bytes; RegGetValueW NUL-terminates REG_SZ.
    let err = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey,
            value,
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
    key.set_str("DisplayName", "Ezhuthu (Tamil phonetic input)")?;
    key.set_str("DisplayVersion", version)?;
    key.set_str("Publisher", "Ezhuthu")?;
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
    // Overwriting keeps an existing file's letter case (e.g. an older
    // "ezhuthu.lnk"), so remove it first to get the new name.
    let _ = std::fs::remove_file(lnk);
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

// ---------------------------------------------------------------- install folder security

/// Restricts `dir` to the current user, SYSTEM and Administrators (a
/// protected DACL, inherited by everything inside). Otherwise a folder that
/// other users can write to would let them replace ezhuthu.exe, which runs
/// as this user at every sign-in. Fails on drives without permissions
/// (FAT32/exFAT), which can't be protected at all.
///
/// The folder must not be a link and must be owned by this user, SYSTEM or
/// Administrators: an owner can always rewrite the permissions, so a folder
/// another user created beforehand could be reopened by them later.
pub fn secure_dir(dir: &Path) -> Result<()> {
    if is_reparse_point(dir) {
        bail!(
            "{} is a link (junction or symlink); choose a real folder",
            dir.display()
        );
    }
    let sid = current_user_sid()?;
    check_owner(dir, &sid)?;
    let sddl = format!("D:P(A;OICI;FA;;;{sid})(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)");
    // SAFETY: the security descriptor allocated by the conversion is freed
    // with LocalFree; the DACL pointer points into it and is only used before.
    unsafe {
        let mut sd = PSECURITY_DESCRIPTOR::default();
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            &HSTRING::from(sddl),
            SDDL_REVISION_1,
            &mut sd,
            None,
        )
        .context("building the folder permissions")?;
        let (mut present, mut defaulted) = (BOOL(0), BOOL(0));
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let got = GetSecurityDescriptorDacl(sd, &mut present, &mut dacl, &mut defaulted);
        let err = got.map(|()| {
            SetNamedSecurityInfoW(
                &HSTRING::from(dir.as_os_str()),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                None,
                None,
                Some(dacl),
                None,
            )
        });
        let _ = LocalFree(Some(HLOCAL(sd.0)));
        let err = err.context("reading the folder permissions")?;
        if err != ERROR_SUCCESS {
            bail!(
                "couldn't restrict who can change {} (error {}); the drive may not \
                 support permissions (FAT32/exFAT), so choose a folder on an NTFS drive",
                dir.display(),
                err.0
            );
        }
    }
    Ok(())
}

/// Fails unless `path` is owned by the user `sid`, SYSTEM or Administrators.
fn check_owner(path: &Path, sid: &str) -> Result<()> {
    const SYSTEM: &str = "S-1-5-18";
    const ADMINISTRATORS: &str = "S-1-5-32-544";
    let owner = owner_sid(path)?;
    if owner != sid && owner != SYSTEM && owner != ADMINISTRATORS {
        bail!(
            "{} belongs to another account ({owner}); choose a folder you created yourself",
            path.display()
        );
    }
    Ok(())
}

/// Checks a folder before installing into it:
/// - no folder on the path is a link (junction or symlink);
/// - it holds nothing but ezhuthu's own files (or leftovers of an interrupted
///   update), none of them links, all owned by this user, SYSTEM or
///   Administrators.
///
/// So setup never adopts, or re-permissions, files someone else put there.
pub fn check_install_dir(dir: &Path) -> Result<()> {
    if let Some(link) = dir.ancestors().find(|a| is_reparse_point(a)) {
        bail!(
            "{} is a link (junction or symlink); choose a real folder",
            link.display()
        );
    }
    if !dir.exists() {
        return Ok(());
    }
    let sid = current_user_sid()?;
    check_owner(dir, &sid)?;
    let known = |name: &str, list: &[&str]| {
        let name = name.to_ascii_lowercase();
        list.iter().any(|k| {
            let k = k.to_ascii_lowercase();
            // Interrupted updates leave "<stem>.partial" files behind.
            let partial = Path::new(&k).with_extension("partial");
            name == k || Path::new(&name) == partial
        })
    };
    let top: Vec<&str> = [EXE_NAME, "assets"]
        .into_iter()
        .chain(LEGACY_FILES.iter().copied())
        .collect();
    let mut entries: Vec<(PathBuf, bool)> = Vec::new();
    for e in std::fs::read_dir(dir)?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        entries.push((e.path(), known(&name, &top)));
    }
    let assets = dir.join("assets");
    if assets.is_dir() && !is_reparse_point(&assets) {
        for e in std::fs::read_dir(&assets)?.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            entries.push((e.path(), known(&name, ASSET_FILES)));
        }
    }
    for (path, ours) in entries {
        if !ours {
            bail!(
                "{} already contains other files ({}); choose an empty folder",
                dir.display(),
                path.display()
            );
        }
        if is_reparse_point(&path) {
            bail!("{} is a link; choose another folder", path.display());
        }
        check_owner(&path, &sid)?;
    }
    Ok(())
}

/// The owner of `path` as a SID string.
fn owner_sid(path: &Path) -> Result<String> {
    // SAFETY: the owner SID points into the returned security descriptor,
    // which is freed with LocalFree after the SID has been converted.
    unsafe {
        let mut owner = PSID::default();
        let mut sd = PSECURITY_DESCRIPTOR::default();
        let err = GetNamedSecurityInfoW(
            &HSTRING::from(path.as_os_str()),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            Some(&mut owner),
            None,
            None,
            None,
            &mut sd,
        );
        if err != ERROR_SUCCESS {
            bail!(
                "couldn't read who owns {} (error {}); the drive may not support \
                 permissions (FAT32/exFAT), so choose a folder on an NTFS drive",
                path.display(),
                err.0
            );
        }
        let mut s = PWSTR::null();
        let converted = ConvertSidToStringSidW(owner, &mut s);
        let sid = converted.map(|()| s.to_string());
        if !s.is_null() {
            let _ = LocalFree(Some(HLOCAL(s.0.cast())));
        }
        let _ = LocalFree(Some(HLOCAL(sd.0)));
        Ok(sid??)
    }
}

/// The current user's SID as a string ("S-1-5-21-...").
fn current_user_sid() -> Result<String> {
    // SAFETY: the token handle is closed below; the TOKEN_USER buffer is sized
    // by the first call; the SID string is freed with LocalFree.
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)?;
        let mut len = 0;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
        let mut buf = vec![0u8; len as usize];
        let got = GetTokenInformation(
            token,
            TokenUser,
            Some(buf.as_mut_ptr().cast()),
            len,
            &mut len,
        );
        let _ = CloseHandle(token);
        got.context("reading the current user")?;
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut s = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut s)?;
        let sid = s.to_string();
        let _ = LocalFree(Some(HLOCAL(s.0.cast())));
        Ok(sid?)
    }
}

// ---------------------------------------------------------------- uninstall

/// Every asset setup may install (under `assets\`). Uninstall deletes only
/// these names plus ezhuthu.exe, so nothing else in a folder the user chose
/// is ever touched. Setup refuses to install anything not listed here.
pub const ASSET_FILES: &[&str] = &[
    "vocab_src.json",
    "vocab_tgt.json",
    "xlit_encoder.onnx",
    "xlit_decoder.onnx",
    "ta_word_freq.bin",
];
/// Left by earlier setup builds; removed if present.
const LEGACY_FILES: &[&str] = &["install-files.txt"];

/// Paths (relative, `/`-separated) of everything setup installs.
pub fn installable_files() -> Vec<String> {
    std::iter::once(EXE_NAME.to_string())
        .chain(ASSET_FILES.iter().map(|f| format!("assets/{f}")))
        .collect()
}

/// Removes ezhuthu for this user: stops it, deletes the Run value, uninstall
/// entry, shortcut and the installed files. The running uninstaller
/// (ezhuthu.exe itself) is deleted once this process has exited. User
/// settings in %APPDATA% are kept.
pub fn uninstall(dir: &Path) -> Result<()> {
    if !close_running_app(Duration::from_secs(5)) {
        bail!("Ezhuthu is still running; quit it from the tray and try again");
    }
    set_autostart(None)?;
    unregister_uninstall()?;
    if let Ok(lnk) = start_menu_shortcut() {
        let _ = std::fs::remove_file(lnk);
    }
    remove_files(dir)?;
    delete_exe_after_exit(dir)
}

const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

/// True for symlinks, junctions and other reparse points.
fn is_reparse_point(path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    std::fs::symlink_metadata(path)
        .is_ok_and(|m| m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
}

/// Deletes the known asset files (and legacy leftovers) directly, after
/// checking that neither the folders nor the files are reparse points and
/// that each file really lies inside `dir`.
fn remove_files(dir: &Path) -> Result<()> {
    let assets = dir.join("assets");
    for d in [dir, assets.as_path()] {
        if is_reparse_point(d) {
            bail!(
                "refusing to uninstall: {} is a link, not a folder",
                d.display()
            );
        }
    }
    let root = dir
        .canonicalize()
        .with_context(|| format!("resolving {}", dir.display()))?;
    let files = ASSET_FILES
        .iter()
        .map(|f| assets.join(f))
        .chain(LEGACY_FILES.iter().map(|f| dir.join(f)));
    for path in files {
        if std::fs::symlink_metadata(&path).is_err() {
            continue; // not there
        }
        if is_reparse_point(&path) {
            bail!("refusing to delete {}: it is a link", path.display());
        }
        let real = path.canonicalize()?;
        if !real.starts_with(&root) {
            bail!(
                "refusing to delete {}: it is outside {}",
                real.display(),
                root.display()
            );
        }
        std::fs::remove_file(&real).with_context(|| format!("deleting {}", real.display()))?;
    }
    let _ = std::fs::remove_dir(&assets); // only if empty
    Ok(())
}

/// Deletes ezhuthu.exe (this running process) and then the folder, if empty,
/// from a hidden `cmd` that first waits ~2 s for us to exit.
fn delete_exe_after_exit(dir: &Path) -> Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let exe = dir.join(EXE_NAME);
    let (exe_s, dir_s) = (exe.display().to_string(), dir.display().to_string());
    // Inside cmd quotes, a quote would end the string and '%' would expand.
    if exe_s.contains(['"', '%']) || is_reparse_point(&exe) {
        bail!("delete {} yourself to finish the uninstall", dir.display());
    }
    std::process::Command::new("cmd.exe")
        .raw_arg(format!(
            "/d /c ping -n 3 127.0.0.1 >nul & del /f /q \"{exe_s}\" & rd \"{dir_s}\" 2>nul"
        ))
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .context("starting the cleanup process")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_folder_is_a_subfolder_unless_already_named_ezhuthu() {
        assert_eq!(
            dir_in(Path::new(r"D:\Tools")),
            Path::new(r"D:\Tools\ezhuthu")
        );
        assert_eq!(
            dir_in(Path::new(r"D:\Apps\Ezhuthu")),
            Path::new(r"D:\Apps\Ezhuthu")
        );
        assert_eq!(dir_in(Path::new(r"D:\")), Path::new(r"D:\ezhuthu"));
    }

    #[test]
    fn compares_folders_like_ntfs() {
        assert!(same_dir(
            Path::new(r"C:\Apps\ezhuthu"),
            Path::new(r"c:\apps\EZHUTHU\")
        ));
        assert!(!same_dir(
            Path::new(r"C:\Apps\ezhuthu"),
            Path::new(r"C:\Apps\other")
        ));
    }

    /// The allowlist must match the assets the release ships (tools/assets.sha256).
    #[test]
    fn allowlist_matches_the_shipped_assets() {
        let sums = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/assets.sha256");
        let mut shipped: Vec<String> = std::fs::read_to_string(sums)
            .unwrap()
            .lines()
            .filter_map(|l| l.split_whitespace().nth(1))
            .map(|n| n.trim_start_matches('*').to_string())
            .collect();
        let mut listed: Vec<String> = ASSET_FILES.iter().map(|s| s.to_string()).collect();
        shipped.sort();
        listed.sort();
        assert_eq!(shipped, listed);
    }

    #[test]
    fn removes_only_known_files_and_keeps_the_rest() {
        let dir = std::env::temp_dir().join(format!("ezhuthu-test-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("assets")).unwrap();
        for f in installable_files() {
            std::fs::write(dir.join(&f), b"x").unwrap();
        }
        std::fs::write(dir.join("keep.txt"), b"user file").unwrap();
        std::fs::write(dir.join("assets/mine.txt"), b"user file").unwrap();
        remove_files(&dir).unwrap();
        for f in ASSET_FILES {
            assert!(!dir.join("assets").join(f).exists(), "{f} should be gone");
        }
        assert!(dir.join("keep.txt").exists());
        assert!(
            dir.join("assets/mine.txt").exists(),
            "folder with user files is kept"
        );
        assert!(
            dir.join(EXE_NAME).exists(),
            "the exe is deleted after exit, not here"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn secures_a_folder_and_keeps_it_usable() {
        let dir = std::env::temp_dir().join(format!("ezhuthu-acl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        secure_dir(&dir).unwrap();
        std::fs::write(dir.join("f"), b"x").unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn install_dir_must_be_new_empty_or_ours() {
        let dir = std::env::temp_dir().join(format!("ezhuthu-check-{}", std::process::id()));
        assert!(
            check_install_dir(&dir).is_ok(),
            "a folder that doesn't exist yet"
        );
        std::fs::create_dir_all(dir.join("assets")).unwrap();
        assert!(check_install_dir(&dir).is_ok(), "an empty folder");
        for f in installable_files() {
            std::fs::write(dir.join(&f), b"x").unwrap();
        }
        std::fs::write(dir.join("assets/vocab_src.partial"), b"x").unwrap();
        assert!(
            check_install_dir(&dir).is_ok(),
            "an earlier install of ours"
        );
        std::fs::write(dir.join("assets/other.dll"), b"x").unwrap();
        assert!(check_install_dir(&dir).is_err(), "a stranger in assets");
        std::fs::remove_file(dir.join("assets/other.dll")).unwrap();
        std::fs::write(dir.join("notes.txt"), b"x").unwrap();
        assert!(check_install_dir(&dir).is_err(), "a stranger at the top");
        std::fs::remove_dir_all(&dir).unwrap();
    }
    #[test]
    fn refuses_to_secure_a_junction() {
        let base = std::env::temp_dir().join(format!("ezhuthu-junction-{}", std::process::id()));
        let (real, link) = (base.join("real"), base.join("link"));
        std::fs::create_dir_all(&real).unwrap();
        // Junctions need no special privilege (unlike symlinks).
        let made = std::process::Command::new("cmd")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(&link)
            .arg(&real)
            .output()
            .unwrap();
        assert!(made.status.success(), "mklink /J failed");
        let err = secure_dir(&link).unwrap_err().to_string();
        assert!(err.contains("is a link"), "{err}");
        std::fs::remove_dir(&link).unwrap(); // removes the junction only
        std::fs::remove_dir_all(&base).unwrap();
    }
}
