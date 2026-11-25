//! Simple RTSP Viewer Setup: installs the app for the current user, with no
//! administrator rights, in one Windows 11 style window; also its
//! uninstaller (`uninstall.exe --uninstall`).
//!
//! Options: `--silent` (no window), and for `--uninstall`,
//! `--remove-settings`.

#![windows_subsystem = "windows"]

mod app;
mod install;
mod payload;
mod ui;

use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};

const VERSION: &str = env!("APP_VERSION");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Install,
    Uninstall,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |flag: &str| args.iter().any(|a| a == flag);
    let mode = if has("--uninstall") {
        Mode::Uninstall
    } else {
        Mode::Install
    };
    let payload = payload::embedded();
    // SAFETY: COM for this thread (shortcuts, images), before any COM call.
    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    if mode == Mode::Uninstall && install::relaunch_from_temp(&args) {
        return;
    }
    let code = if has("--silent") {
        let result = match mode {
            Mode::Install => install::install(payload, VERSION, &mut |_, _| {}).map(|_| ()),
            Mode::Uninstall => install::uninstall(!has("--remove-settings"), &mut |_, _| {}),
        };
        i32::from(result.is_err())
    } else {
        ui::run(mode, payload, VERSION)
    };
    if mode == Mode::Uninstall {
        install::delete_self_later();
    }
    std::process::exit(code);
}
