//! What this setup knows about the app it installs.

use windows::core::{w, PCWSTR};

pub const APP_NAME: &str = "Simple RTSP Viewer";
/// The app's executable inside the payload, and in the install folder.
pub const EXE: &str = "Simple RTSP Viewer.exe";
pub const PUBLISHER: &str = "Matheus Nascimento";
pub const URL: &str = "https://github.com/matheusgrnascimento/simple-rtsp-viewer";
/// The Start menu shortcut's tooltip.
pub const DESCRIPTION: &str = "View multiple RTSP cameras in a grid";
/// The name of the key in `Uninstall` (Settings > Apps).
pub const UNINSTALL_KEY_NAME: &str = "SimpleRtspViewer";
/// The upgrade code of the MSI, to remove a copy installed by it.
pub const MSI_UPGRADE_CODE: PCWSTR = w!("{6F1B3C52-7E0A-4C7D-9B1E-3A9D2F4C8E15}");
/// The folder under `%APPDATA%` with what the user created, if the app has one.
pub const SETTINGS_FOLDER: Option<&str> = Some("Simple RTSP Viewer");
pub const KEEP_SETTINGS: &str = "Keep my camera list";
/// Window class of the setup, and the variable that forces `light` or `dark`.
pub const WINDOW_CLASS: PCWSTR = w!("SimpleRtspViewer.Setup");
pub const THEME_VARIABLE: &str = "SIMPLE_RTSP_VIEWER_THEME";

pub const SETUP_TITLE: PCWSTR = w!("Simple RTSP Viewer Setup");
pub const UNINSTALL_TITLE: PCWSTR = w!("Uninstall Simple RTSP Viewer");

pub const INSTALL_INTRO: &str = "View many RTSP cameras in one grid. Installs for your account only, with no administrator rights.";
pub const UPDATE_INTRO: &str = "Updates the app in place. Your camera list stays.";
pub const UNINSTALL_INTRO: &str =
    "Removes the app and its Start menu shortcut. Saved camera logins stay in the Windows Credential Manager.";
pub const INSTALLED: &str = "Installed. Find Simple RTSP Viewer in the Start menu.";
pub const REMOVED: &str = "Simple RTSP Viewer was removed.";
