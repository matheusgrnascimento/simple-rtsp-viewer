mod credentials;
mod discovery;
mod grid;
mod import;
mod layout;
mod model;
mod rtsp_url;
mod streaming;
mod theme;
mod ui;

use std::path::PathBuf;
use std::time::Duration;

use streaming::StreamManagerHandle;
use ui::SimpleRtspViewerApp;

/// Maximum time the shutdown waits for the camera threads (and a layout write
/// that may be in progress) to finish before abandoning them.
const SHUTDOWN_GRACE_PERIOD: Duration = Duration::from_secs(2);

/// Path of the Layout_Config_File kept by the System (Requirement 5).
///
/// Kept in the current working directory for simplicity at this stage of the
/// project; it can move to a platform-specific configuration directory in a
/// future iteration without affecting the pure functions of `layout` (which
/// take the path as a parameter).
fn layout_file_path() -> PathBuf {
    PathBuf::from("layout.json")
}

/// Determines the Import_File to load (Requirement 1.1).
///
/// Prefers the first command-line argument, when given (`simple-rtsp-viewer
/// path/cameras.json`): this allows using the System in environments without a
/// native file picker (headless, SSH, or without `xdg-desktop-portal`/`zenity`
/// installed). Without the argument, it falls back to the native file dialog;
/// returns `None` if the dialog is not available or the user cancels the
/// selection.
fn pick_import_file() -> Option<PathBuf> {
    if let Some(arg) = std::env::args_os().nth(1) {
        return Some(PathBuf::from(arg));
    }

    rfd::FileDialog::new()
        .set_title("Select the camera import file")
        .add_filter("JSON", &["json"])
        .pick_file()
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

/// Loads the Import_File selected by `pick_import_file`, showing an error
/// message with the specific cause on failure (Requirement 1.3) and a warning
/// when entries were rejected or ignored (Requirements 1.5, 1.6, 1.7). If the
/// user cancels the file selection, or if the import fails, returns an empty
/// camera list (equivalent to keeping the previously loaded list unchanged,
/// which is still empty at this startup stage).
fn load_cameras() -> Vec<model::Camera> {
    let Some(path) = pick_import_file() else {
        tracing::warn!(
            "no import file selected; starting without cameras. \
             Pass the path as an argument: `simple-rtsp-viewer cameras.json`"
        );
        return Vec::new();
    };

    match import::load_import_file(&path) {
        Ok(outcome) => {
            tracing::info!(
                path = %path.display(),
                loaded = outcome.cameras.len(),
                rejected = outcome.rejected_count,
                truncated = outcome.truncated,
                "camera import finished"
            );
            show_import_summary(&outcome);
            outcome.cameras
        }
        Err(err) => {
            tracing::error!(path = %path.display(), error = %err, "failed to import the camera file");
            show_import_error(&err);
            Vec::new()
        }
    }
}

/// Loads the saved order from the Layout_Config_File (if it exists and is
/// valid) and reconciles it with the imported camera list, returning the final
/// camera order (Requirements 5.2, 5.3, 5.4). A missing or corrupted layout
/// file is handled the same way: the Import_File order is used as the default.
fn load_camera_order(
    imported: Vec<model::Camera>,
    layout_path: &std::path::Path,
) -> Vec<model::Camera> {
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

    let imported_cameras = load_cameras();
    let layout_path = layout_file_path();
    let cameras = load_camera_order(imported_cameras, &layout_path);

    // `StreamManagerHandle::new` and `SimpleRtspViewerApp::new` call
    // `tokio::spawn` internally (Requirement 3.1), so a tokio runtime must be
    // active (through `enter`) for the whole run of `eframe::run_native`, which
    // runs the UI event loop blocking on the main thread.
    let runtime = tokio::runtime::Runtime::new().expect("failed to initialize the tokio runtime");
    let runtime_guard = runtime.enter();

    let stream_manager_handle = StreamManagerHandle::new();
    let shutdown_handle = stream_manager_handle.clone();

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Simple RTSP Viewer")
            .with_inner_size([1280.0, 760.0])
            .with_min_inner_size([560.0, 360.0])
            // Transparent window so the Windows 11 Mica backdrop shows through.
            .with_transparent(theme::mica_requested()),
        ..Default::default()
    };

    let run_result = eframe::run_native(
        "Simple RTSP Viewer",
        native_options,
        Box::new(move |cc| {
            Ok(Box::new(
                SimpleRtspViewerApp::new(cameras, stream_manager_handle, layout_path)
                    .with_windows_look(cc),
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
