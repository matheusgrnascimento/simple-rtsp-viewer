// Release builds use the GUI subsystem, so Windows does not open a console
// window next to the app. Debug builds keep the console for the logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod credentials;
mod dialogs;
mod discovery;
mod grid;
mod import;
mod layout;
mod model;
mod rtsp_url;
mod store;
mod streaming;
mod theme;
mod ui;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use store::{CameraAuth, CameraLibrary, StoreData};
use streaming::StreamManagerHandle;
use ui::SimpleRtspViewerApp;

/// Maximum time the shutdown waits for the camera threads (and a write that may
/// be in progress) to finish before abandoning them.
const SHUTDOWN_GRACE_PERIOD: Duration = Duration::from_secs(2);

/// Files from the previous version, which kept the cameras (with the password
/// in the URLs) and the grid order in the current folder. They are imported
/// only once, on the first run with the app's library.
const LEGACY_CAMERAS_FILE: &str = "cameras.json";
const LEGACY_LAYOUT_FILE: &str = "layout.json";

/// Camera file to import on startup (Requirement 1.1): the one passed on the
/// command line (`simple-rtsp-viewer path/cameras.json`) or, on the first run,
/// the previous version's `cameras.json` in the current folder.
fn import_source(first_run: bool) -> Option<PathBuf> {
    if let Some(arg) = std::env::args_os().nth(1) {
        return Some(PathBuf::from(arg));
    }
    let legacy = PathBuf::from(LEGACY_CAMERAS_FILE);
    (first_run && legacy.is_file()).then_some(legacy)
}

/// Shows a native error message with the specific cause of the import failure,
/// without interrupting the System startup (Requirement 1.3): the previously
/// loaded camera list (empty at this startup stage) stays unchanged.
fn show_import_error(err: &import::ImportError) {
    rfd::MessageDialog::new()
        .set_title("Error importing cameras")
        .set_description(err.to_string())
        .set_level(rfd::MessageLevel::Error)
        .show();
}

/// Warns the user when the import had problems: no valid camera, rejected
/// entries or the maximum number of cameras reached (Requirements 1.5, 1.6,
/// 1.7). A clean import opens no warning; the summary only goes to the log.
fn show_import_summary(outcome: &import::ImportOutcome) {
    if outcome.cameras.is_empty() {
        rfd::MessageDialog::new()
            .set_title("No cameras loaded")
            .set_description("No entry with a valid RTSP URL was found in the import file.")
            .set_level(rfd::MessageLevel::Warning)
            .show();
        return;
    }

    if outcome.rejected_count == 0 && !outcome.truncated {
        return;
    }

    let mut description = format!(
        "{} camera(s) loaded.\n{} entry(ies) rejected.",
        outcome.cameras.len(),
        outcome.rejected_count
    );
    if outcome.truncated {
        description.push_str("\nMaximum of 64 cameras reached: extra valid entries were ignored.");
    }

    rfd::MessageDialog::new()
        .set_title("Import finished with warnings")
        .set_description(&description)
        .set_level(rfd::MessageLevel::Warning)
        .show();
}

/// Loads the app's camera library (`%APPDATA%`) and the logins from the Windows
/// vault, first importing the camera file given by `import_source`, if any.
fn load_library() -> CameraLibrary {
    let path = store::default_store_path();
    let (mut data, first_run) = match store::load(&path) {
        Ok(Some(data)) => (data, false),
        Ok(None) => (StoreData::default(), true),
        Err(err) => {
            // An unreadable file is not overwritten: it is set aside and the
            // app starts with an empty list.
            let backup = path.with_extension("json.unreadable");
            let kept = std::fs::rename(&path, &backup).is_ok();
            tracing::error!(error = %err, "unreadable camera list");
            let mut description = format!("The camera list could not be read: {err}");
            if kept {
                description.push_str(&format!("\n\nThe file was saved to {}.", backup.display()));
            }
            rfd::MessageDialog::new()
                .set_title("Unreadable camera list")
                .set_description(description)
                .set_level(rfd::MessageLevel::Error)
                .show();
            (StoreData::default(), false)
        }
    };

    let mut shared = credentials::load_shared().unwrap_or_else(|err| {
        tracing::warn!(error = %err, "the default login could not be read from the vault");
        None
    });

    if let Some(source) = import_source(first_run) {
        import_into_library(&mut data, &mut shared, &source, first_run, &path);
    }

    let own: HashMap<String, credentials::Credentials> = data
        .cameras
        .iter()
        .filter(|camera| camera.auth == CameraAuth::Own)
        .filter_map(|camera| match credentials::load_for_camera(&camera.id) {
            Ok(login) => login.map(|login| (camera.id.clone(), login)),
            Err(err) => {
                tracing::warn!(error = %err, "a camera's own login could not be read from the vault");
                None
            }
        })
        .collect();

    CameraLibrary {
        path,
        data,
        shared,
        own,
    }
}

/// Imports a camera file into the library (Requirements 1.1–1.7), moving the
/// logins from the URLs to the Windows vault and saving the library. When
/// migrating from the previous version (`first_run`), keeps the order of the
/// old `layout.json`. Shows the specific error if the file cannot be read
/// (Requirement 1.3) and a warning if entries were rejected.
fn import_into_library(
    data: &mut StoreData,
    shared: &mut Option<credentials::Credentials>,
    source: &Path,
    first_run: bool,
    store_path: &Path,
) {
    let outcome = match import::load_import_file(source) {
        Ok(outcome) => outcome,
        Err(err) => {
            tracing::error!(path = %source.display(), error = %err, "failed to import the camera file");
            show_import_error(&err);
            return;
        }
    };
    show_import_summary(&outcome);

    let cameras = if first_run {
        load_camera_order(outcome.cameras, Path::new(LEGACY_LAYOUT_FILE))
    } else {
        outcome.cameras
    };
    let merge = store::merge_imported(data, &cameras, shared);

    if merge.shared_defined {
        if let Some(login) = shared.as_ref() {
            if let Err(err) = credentials::save_shared(login) {
                tracing::error!(error = %err, "the imported default login was not saved to the vault");
            }
        }
    }
    for (id, login) in &merge.own {
        if let Err(err) = credentials::save_for_camera(id, login) {
            tracing::error!(error = %err, "an imported camera login was not saved to the vault");
        }
    }
    if let Err(err) = store::save(store_path, data) {
        tracing::error!(error = %err, "failed to save the camera list");
    }

    tracing::info!(
        path = %source.display(),
        adicionadas = merge.added,
        repetidas = merge.duplicates,
        rejected = outcome.rejected_count,
        "cameras imported into the app's list"
    );
}

/// Loads the saved order from the Layout_Config_File (if it exists and is
/// valid) and reconciles it with the imported camera list, returning the final
/// camera order (Requirements 5.2, 5.3, 5.4). A missing or corrupted layout
/// file is handled the same way: the Import_File order is used as the default.
fn load_camera_order(imported: Vec<model::Camera>, layout_path: &Path) -> Vec<model::Camera> {
    let saved_order = match layout::load_layout_file(layout_path) {
        Ok(layout_file) => layout_file.camera_order,
        Err(err) => {
            tracing::warn!(
                error = %err,
                "layout file missing or invalid; using the import file order"
            );
            Vec::new()
        }
    };

    layout::restore_order(&imported, &saved_order)
}

/// Window icon, shown in the title bar, the taskbar and Alt+Tab. Without it,
/// eframe shows its default icon instead of the one embedded in the
/// executable.
fn app_icon() -> Option<egui::IconData> {
    match eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png")) {
        Ok(icon) => Some(icon),
        Err(err) => {
            tracing::warn!(error = %err, "failed to load the window icon");
            None
        }
    }
}

fn main() -> eframe::Result {
    // Installs the `tracing` subscriber so the log events emitted by the other
    // modules (import, layout, streaming) are actually recorded (Requirements
    // 1.4, 3.3, 5.4, 5.7, 8.3). The level can be changed at runtime with the
    // `RUST_LOG` environment variable; without it, logs from the `info` level
    // up.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let library = load_library();

    // `StreamManagerHandle::new` and `SimpleRtspViewerApp::new` call
    // `tokio::spawn` internally (Requirement 3.1), so a tokio runtime must be
    // active (through `enter`) for the whole run of `eframe::run_native`, which
    // runs the UI event loop blocking on the main thread.
    let runtime = tokio::runtime::Runtime::new().expect("failed to initialize the tokio runtime");
    let runtime_guard = runtime.enter();

    let stream_manager_handle = StreamManagerHandle::new();
    let shutdown_handle = stream_manager_handle.clone();

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Simple RTSP Viewer")
        .with_inner_size([1280.0, 760.0])
        .with_min_inner_size([560.0, 360.0])
        // Transparent window so the Windows 11 Mica backdrop shows through.
        .with_transparent(theme::mica_requested());
    if let Some(icon) = app_icon() {
        viewport = viewport.with_icon(icon);
    }
    let native_options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    let run_result = eframe::run_native(
        "Simple RTSP Viewer",
        native_options,
        Box::new(move |cc| {
            Ok(Box::new(
                SimpleRtspViewerApp::new(library, stream_manager_handle).with_windows_look(cc),
            ) as Box<dyn eframe::App>)
        }),
    );

    // System shutdown: tells the Stream_Manager to stop (without reconnecting)
    // every running StreamTask and release its resources before the process
    // ends (Requirements 3.6, 6.6). `shutdown` only returns after every
    // cancellation has been signaled.
    runtime.block_on(shutdown_handle.shutdown());

    // Shuts the runtime down with a deadline. Dropping the runtime would wait
    // without limit for the `spawn_blocking` threads, and a StreamTask stuck in
    // network I/O (a connection hung until FFmpeg's socket timeout) would hold
    // up closing the app. After the deadline, the remaining threads are
    // abandoned and the process exits normally.
    drop(runtime_guard);
    runtime.shutdown_timeout(SHUTDOWN_GRACE_PERIOD);

    run_result
}
