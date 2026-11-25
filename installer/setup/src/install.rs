//! What the setup does: install for the current user (no administrator
//! rights), update in place, and uninstall.
//!
//! Installed state:
//! - the files of the app in `%LOCALAPPDATA%\Programs\<app>`, and
//!   `uninstall.exe` (a copy of this setup) next to them;
//! - a Start menu shortcut;
//! - the entry in Settings > Apps (`HKCU\...\Uninstall\<key>`).

use std::path::{Path, PathBuf};

use windows::core::{w, Error, Interface, Result, BOOL, HRESULT, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::Storage::FileSystem::{
    MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
};
use windows::Win32::System::ApplicationInstallationAndServicing::MsiEnumRelatedProductsW;
use windows::Win32::System::Com::{
    CoCreateInstance, CoTaskMemFree, IPersistFile, CLSCTX_INPROC_SERVER,
};
use windows::Win32::System::Registry::{
    RegDeleteTreeW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_DWORD, REG_SZ,
    RRF_RT_REG_SZ,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, WaitForSingleObject, INFINITE, PROCESS_NAME_FORMAT,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
};
use windows::Win32::UI::Shell::{
    FOLDERID_LocalAppData, FOLDERID_Programs, FOLDERID_RoamingAppData, IShellLinkW,
    SHGetKnownFolderPath, ShellExecuteExW, ShellLink, KF_FLAG_CREATE, SEE_MASK_NOCLOSEPROCESS,
    SHELLEXECUTEINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowThreadProcessId, IsWindowVisible, PostMessageW, SW_SHOWNORMAL, WM_CLOSE,
};

use crate::app::{
    APP_NAME, DESCRIPTION, EXE, MSI_UPGRADE_CODE, PUBLISHER, SETTINGS_FOLDER, UNINSTALL_KEY_NAME,
    URL,
};
use crate::payload;

const UNINSTALLER: &str = "uninstall.exe";

fn uninstall_key() -> String {
    format!(r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{UNINSTALL_KEY_NAME}")
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

fn os_error(err: std::io::Error) -> Error {
    match err.raw_os_error() {
        Some(code) => Error::from_hresult(HRESULT::from_win32(code as u32)),
        None => Error::new(HRESULT(-1), err.to_string()),
    }
}

fn known_folder(id: &windows::core::GUID) -> Result<PathBuf> {
    // SAFETY: the returned string is copied, then freed as required.
    unsafe {
        let path = SHGetKnownFolderPath(id, KF_FLAG_CREATE, None)?;
        let result = path.to_string().map(PathBuf::from);
        CoTaskMemFree(Some(path.0.cast_const().cast()));
        result.map_err(|_| Error::empty())
    }
}

/// Where the app is installed.
pub fn install_folder() -> Result<PathBuf> {
    Ok(known_folder(&FOLDERID_LocalAppData)?
        .join("Programs")
        .join(APP_NAME))
}

fn shortcut_path() -> Result<PathBuf> {
    Ok(known_folder(&FOLDERID_Programs)?.join(format!("{APP_NAME}.lnk")))
}

fn reg_string(key: &str, value: &str) -> Option<String> {
    let (key, value) = (wide(key), wide(value));
    let mut buffer = vec![0u16; 1024];
    let mut size = (buffer.len() * 2) as u32;
    // SAFETY: the buffer holds `size` bytes.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(key.as_ptr()),
            PCWSTR(value.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    status.is_ok().then(|| {
        let chars = (size as usize / 2).saturating_sub(1);
        String::from_utf16_lossy(&buffer[..chars])
    })
}

fn set_string(key: &str, value: &str, data: &str) -> Result<()> {
    let (key, value, data) = (wide(key), wide(value), wide(data));
    // SAFETY: null-terminated strings of the given byte size.
    unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            PCWSTR(key.as_ptr()),
            PCWSTR(value.as_ptr()),
            REG_SZ.0,
            Some(data.as_ptr().cast()),
            (data.len() * 2) as u32,
        )
        .ok()
    }
}

fn set_dword(key: &str, value: &str, data: u32) -> Result<()> {
    let (key, value) = (wide(key), wide(value));
    // SAFETY: a 4-byte value.
    unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            PCWSTR(key.as_ptr()),
            PCWSTR(value.as_ptr()),
            REG_DWORD.0,
            Some((&data as *const u32).cast()),
            4,
        )
        .ok()
    }
}

/// The installed version, if any.
pub fn installed_version() -> Option<String> {
    reg_string(&uninstall_key(), "DisplayVersion")
}

/// The top-level windows of a process whose executable is called `EXE`.
struct Windows {
    list: Vec<HWND>,
}

unsafe extern "system" fn collect_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: `lparam` is the `Windows` that `close_running_app` passed.
    let windows = unsafe { &mut *(lparam.0 as *mut Windows) };
    if unsafe { IsWindowVisible(hwnd) }.as_bool() {
        windows.list.push(hwnd);
    }
    BOOL::from(true)
}

/// Whether the process is the app, by the file name of its executable.
fn is_app_process(process: HANDLE) -> bool {
    let mut buffer = [0u16; 1024];
    let mut size = buffer.len() as u32;
    // SAFETY: the buffer holds `size` characters.
    let queried = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_FORMAT(0),
            PWSTR(buffer.as_mut_ptr()),
            &mut size,
        )
    };
    queried.is_ok()
        && Path::new(&String::from_utf16_lossy(&buffer[..size as usize]))
            .file_name()
            .is_some_and(|name| name.eq_ignore_ascii_case(EXE))
}

/// Asks the running app to close and waits for it (up to 5 s).
pub fn close_running_app() {
    let mut windows = Windows { list: Vec::new() };
    // SAFETY: the callback only reads window handles into `windows`, which
    // outlives the call.
    unsafe {
        let _ = EnumWindows(
            Some(collect_window),
            LPARAM(&mut windows as *mut Windows as isize),
        );
    }
    let mut waiting = Vec::new();
    for hwnd in windows.list {
        let mut pid = 0;
        // SAFETY: plain queries; the process handles are closed below.
        unsafe {
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            let Ok(process) = OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                false,
                pid,
            ) else {
                continue;
            };
            if is_app_process(process) {
                let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                waiting.push(process);
            } else {
                let _ = CloseHandle(process);
            }
        }
    }
    for process in waiting {
        // SAFETY: a handle opened above.
        unsafe {
            WaitForSingleObject(process, 5000);
            let _ = CloseHandle(process);
        }
    }
}

/// Removes the per-machine MSI install, if present. Windows asks for
/// administrator approval for it.
fn remove_old_msi() -> Result<()> {
    let mut code = [0u16; 39];
    // SAFETY: the buffer holds a product code (38 characters and a null).
    let found =
        unsafe { MsiEnumRelatedProductsW(MSI_UPGRADE_CODE, None, 0, PWSTR(code.as_mut_ptr())) };
    if found != 0 {
        return Ok(());
    }
    let product = String::from_utf16_lossy(&code[..38]);
    let parameters = wide(&format!("/x {product} /passive /norestart"));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: w!("open"),
        lpFile: w!("msiexec.exe"),
        lpParameters: PCWSTR(parameters.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: the strings outlive the call; the process handle is closed.
    unsafe {
        ShellExecuteExW(&mut info)?;
        if !info.hProcess.is_invalid() {
            WaitForSingleObject(info.hProcess, INFINITE);
            let _ = CloseHandle(info.hProcess);
        }
    }
    Ok(())
}

fn create_shortcut(target: &Path, folder: &Path) -> Result<()> {
    let path = shortcut_path()?;
    let (target_w, folder_w, path_w) = (
        wide(&target.to_string_lossy()),
        wide(&folder.to_string_lossy()),
        wide(&path.to_string_lossy()),
    );
    let description = wide(DESCRIPTION);
    // SAFETY: COM calls with null-terminated strings that outlive them.
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        link.SetPath(PCWSTR(target_w.as_ptr()))?;
        link.SetWorkingDirectory(PCWSTR(folder_w.as_ptr()))?;
        link.SetDescription(PCWSTR(description.as_ptr()))?;
        link.SetIconLocation(PCWSTR(target_w.as_ptr()), 0)?;
        link.cast::<IPersistFile>()?
            .Save(PCWSTR(path_w.as_ptr()), true)
    }
}

/// Writes `data` as `path` through a temporary file, replacing it.
fn write_replacing(path: &Path, data: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(os_error)?;
    }
    let mut temp = path.as_os_str().to_os_string();
    temp.push(".new");
    let temp = PathBuf::from(temp);
    std::fs::write(&temp, data).map_err(os_error)?;
    let (from, to) = (wide(&temp.to_string_lossy()), wide(&path.to_string_lossy()));
    // The app may need a moment to release its files after it exits (or a
    // virus scanner holds them), so try again for a few seconds.
    let mut moved = Ok(());
    for _ in 0..20 {
        // SAFETY: null-terminated paths.
        moved = unsafe {
            MoveFileExW(
                PCWSTR(from.as_ptr()),
                PCWSTR(to.as_ptr()),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if moved.is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    if moved.is_err() {
        let _ = std::fs::remove_file(&temp);
        return Err(Error::new(
            HRESULT(-1),
            format!("{APP_NAME} is still running. Close it and try again."),
        ));
    }
    Ok(())
}

/// Installs or updates the app. `progress` gets a fraction and a status.
/// Returns the path of the app's executable.
pub fn install(
    payload: &[u8],
    version: &str,
    progress: &mut dyn FnMut(f32, &str),
) -> Result<PathBuf> {
    if payload.is_empty() {
        return Err(Error::new(
            HRESULT(-1),
            "This setup was built without the app.",
        ));
    }
    progress(0.05, &format!("Closing {APP_NAME}"));
    close_running_app();
    progress(0.1, "Removing the earlier version");
    remove_old_msi()?;
    progress(0.2, "Unpacking files");
    let table = payload::unpack(payload)?;
    let files = payload::files(&table)?;
    let folder = install_folder()?;
    std::fs::create_dir_all(&folder).map_err(os_error)?;
    let mut size_kb = 0u64;
    for (index, (name, data)) in files.iter().enumerate() {
        progress(
            0.3 + 0.4 * index as f32 / files.len() as f32,
            "Copying files",
        );
        write_replacing(&folder.join(name), data)?;
        size_kb += data.len() as u64 / 1024;
    }
    let exe = folder.join(EXE);
    if !exe.is_file() {
        return Err(Error::new(
            HRESULT(-1),
            "This setup was built without the app.",
        ));
    }
    let me = std::env::current_exe().map_err(os_error)?;
    let uninstaller = folder.join(UNINSTALLER);
    if me != uninstaller {
        let bytes = std::fs::read(&me).map_err(os_error)?;
        write_replacing(&uninstaller, &bytes)?;
        // The uninstaller does not need the files: keep the copy small.
        payload::strip(&uninstaller);
    }
    size_kb += std::fs::metadata(&uninstaller).map_or(0, |m| m.len() / 1024);
    progress(0.8, "Adding the shortcut");
    create_shortcut(&exe, &folder)?;
    progress(0.9, "Registering the app");
    let key = uninstall_key();
    let uninstall = format!("\"{}\" --uninstall", uninstaller.display());
    for (value, data) in [
        ("DisplayName", APP_NAME.to_string()),
        ("DisplayVersion", version.to_string()),
        ("Publisher", PUBLISHER.to_string()),
        ("DisplayIcon", format!("{},0", exe.display())),
        ("InstallLocation", folder.display().to_string()),
        ("UninstallString", uninstall.clone()),
        ("QuietUninstallString", format!("{uninstall} --silent")),
        ("URLInfoAbout", URL.to_string()),
    ] {
        set_string(&key, value, &data)?;
    }
    set_dword(&key, "NoModify", 1)?;
    set_dword(&key, "NoRepair", 1)?;
    set_dword(&key, "EstimatedSize", size_kb as u32)?;
    progress(1.0, "Done");
    Ok(exe)
}

/// Starts the installed app, as the user who runs the setup.
pub fn launch(exe: &Path) {
    let _ = std::process::Command::new(exe)
        .current_dir(exe.parent().unwrap_or(Path::new(".")))
        .spawn();
}

/// Removes the app. What the user created stays if `keep_settings`.
pub fn uninstall(keep_settings: bool, progress: &mut dyn FnMut(f32, &str)) -> Result<()> {
    progress(0.2, &format!("Closing {APP_NAME}"));
    close_running_app();
    progress(0.5, "Removing files");
    if let Ok(shortcut) = shortcut_path() {
        let _ = std::fs::remove_file(shortcut);
    }
    let folder = install_folder()?;
    if folder.exists() {
        std::fs::remove_dir_all(&folder).map_err(os_error)?;
    }
    let key = wide(&uninstall_key());
    // SAFETY: deletes this app's own key.
    let _ = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(key.as_ptr())) };
    if let (false, Some(settings)) = (keep_settings, SETTINGS_FOLDER) {
        if let Ok(appdata) = known_folder(&FOLDERID_RoamingAppData) {
            let _ = std::fs::remove_dir_all(appdata.join(settings));
        }
    }
    progress(1.0, "Done");
    Ok(())
}

/// The uninstaller lives in the folder it removes: it runs from a copy in
/// the temporary folder instead. Returns true when this process should exit
/// because the copy took over.
pub fn relaunch_from_temp(args: &[String]) -> bool {
    let Ok(me) = std::env::current_exe() else {
        return false;
    };
    let Ok(folder) = install_folder() else {
        return false;
    };
    if !me.starts_with(&folder) {
        return false;
    }
    let copy = std::env::temp_dir().join(format!(
        "{}-uninstall-{}.exe",
        UNINSTALL_KEY_NAME,
        std::process::id()
    ));
    if std::fs::copy(&me, &copy).is_err() {
        return false;
    }
    std::process::Command::new(&copy).args(args).spawn().is_ok()
}

/// Deletes this temporary uninstaller copy once it exits.
pub fn delete_self_later() {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let Ok(me) = std::env::current_exe() else {
        return;
    };
    if !me.starts_with(std::env::temp_dir()) {
        return;
    }
    let _ = std::process::Command::new("cmd.exe")
        .args(["/c", "ping -n 3 127.0.0.1 >nul & del /f /q"])
        .arg(&me)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
}

/// An HWND the install thread may post to.
pub struct PostTarget(pub HWND);
// SAFETY: PostMessageW accepts a window of another thread.
unsafe impl Send for PostTarget {}
