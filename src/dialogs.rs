//! Windows 11 style dialogs: add and edit camera, default login, network camera
//! scan and removal confirmation.
//!
//! The dialogs only collect and validate data; the app writes the library and
//! the credential vault, based on the returned `DialogAction`.

use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex, PoisonError};

use crate::credentials::Credentials;
use crate::discovery::{self, ScanRequest, ScanStage, SharedProgress, VideoInfo};
use crate::import;
use crate::rtsp_url;
use crate::store::{self, CameraAuth, ScanSettings, StoredCamera};
use crate::theme::Palette;

/// Width of the dialog content.
const DIALOG_WIDTH: f32 = 440.0;

/// App data the dialogs read.
pub struct DialogEnv<'a> {
    pub palette: &'a Palette,
    pub shared: Option<&'a Credentials>,
    pub cameras: &'a [StoredCamera],
}

/// What the app should do after drawing the dialog.
pub enum DialogAction {
    /// Nothing: the dialog stays open.
    None,
    /// Close without changes.
    Close,
    /// Save the camera (new or edited) and its own login, if any.
    SaveCamera {
        camera: StoredCamera,
        own: Option<Credentials>,
    },
    SaveSharedLogin(Credentials),
    /// Add the cameras picked in the scan, remembering the parameters used.
    AddCameras {
        cameras: Vec<StoredCamera>,
        scan: ScanSettings,
    },
    RemoveCamera(String),
}

pub enum Dialog {
    Camera(CameraForm),
    SharedLogin(LoginForm),
    Scan(ScanDialog),
    ConfirmRemove { id: String, label: String },
}

impl Dialog {
    pub fn show(&mut self, ctx: &egui::Context, env: &DialogEnv<'_>) -> DialogAction {
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            return DialogAction::Close;
        }
        match self {
            Dialog::Camera(form) => form.show(ctx, env),
            Dialog::SharedLogin(form) => form.show(ctx, env),
            Dialog::Scan(scan) => scan.show(ctx, env),
            Dialog::ConfirmRemove { id, label } => show_confirm_remove(ctx, env, id, label),
        }
    }
}

// ---------------------------------------------------------------------------
// Add / edit camera
// ---------------------------------------------------------------------------

type TestResult = Arc<Mutex<Option<Result<VideoInfo, String>>>>;

pub struct CameraForm {
    /// `id` of the camera being edited; `None` for a new camera.
    editing: Option<String>,
    name: String,
    host: String,
    port: String,
    main_path: String,
    sub_path: String,
    auth: CameraAuth,
    own_username: String,
    own_password: String,
    error: Option<String>,
    /// Result of the "Test connection" button (`None` inside = in progress).
    test: Option<TestResult>,
}

impl CameraForm {
    /// Form for a new camera, with the default port and paths.
    pub fn new(defaults: &ScanSettings) -> Self {
        Self {
            editing: None,
            name: String::new(),
            host: String::new(),
            port: defaults.port.to_string(),
            main_path: defaults.main_path.clone(),
            sub_path: defaults.sub_path.clone(),
            auth: CameraAuth::Shared,
            own_username: String::new(),
            own_password: String::new(),
            error: None,
            test: None,
        }
    }

    /// Form filled in with an existing camera.
    pub fn edit(camera: &StoredCamera, own: Option<&Credentials>) -> Self {
        Self {
            editing: Some(camera.id.clone()),
            name: camera.name.clone().unwrap_or_default(),
            host: camera.host.clone(),
            port: camera.port.to_string(),
            main_path: camera.main_path.clone(),
            sub_path: camera.sub_path.clone().unwrap_or_default(),
            auth: camera.auth,
            own_username: own.map(|login| login.username.clone()).unwrap_or_default(),
            own_password: own.map(|login| login.password.clone()).unwrap_or_default(),
            error: None,
            test: None,
        }
    }

    /// Validates the fields and builds the camera to save.
    fn validated(
        &self,
        cameras: &[StoredCamera],
    ) -> Result<(StoredCamera, Option<Credentials>), String> {
        let name = self.name.trim();
        if name.chars().count() > 100 {
            return Err("The name can have at most 100 characters.".to_owned());
        }
        let host = self.host.trim();
        if !rtsp_url::is_valid_host(host) {
            return Err(
                "Enter the camera's IP or name (only letters, digits, \".\" and \"-\").".to_owned(),
            );
        }
        let port: u16 = self
            .port
            .trim()
            .parse()
            .ok()
            .filter(|port| *port > 0)
            .ok_or_else(|| "Invalid port.".to_owned())?;
        let main_path = rtsp_url::normalize_path(&self.main_path);
        if main_path.len() <= 1 {
            return Err("Enter the main stream path.".to_owned());
        }
        let sub_path = Some(self.sub_path.trim())
            .filter(|path| !path.is_empty())
            .map(rtsp_url::normalize_path);
        if sub_path.as_deref() == Some(main_path.as_str()) {
            return Err("The substream needs a path different from the main one.".to_owned());
        }
        if !import::is_valid_rtsp_url(&rtsp_url::build(host, port, &main_path, None)) {
            return Err("Invalid RTSP address.".to_owned());
        }

        let own = (self.auth == CameraAuth::Own).then(|| Credentials {
            username: self.own_username.trim().to_owned(),
            password: self.own_password.clone(),
        });
        if let Some(login) = &own {
            login.validate()?;
        }

        let camera = StoredCamera {
            id: self.editing.clone().unwrap_or_else(store::new_camera_id),
            name: (!name.is_empty()).then(|| name.to_owned()),
            host: host.to_owned(),
            port,
            main_path,
            sub_path,
            auth: self.auth,
        };
        if cameras
            .iter()
            .any(|other| other.id != camera.id && other.same_stream(&camera))
        {
            return Err("This camera is already in the list.".to_owned());
        }
        Ok((camera, own))
    }

    fn start_test(&mut self, ctx: &egui::Context, env: &DialogEnv<'_>) {
        let (camera, own) = match self.validated(env.cameras) {
            Ok(valid) => valid,
            Err(err) => {
                self.error = Some(err);
                return;
            }
        };
        self.error = None;
        let login = match camera.auth {
            CameraAuth::Shared => env.shared.cloned(),
            CameraAuth::None => None,
            CameraAuth::Own => own,
        };
        let url = rtsp_url::build(&camera.host, camera.port, &camera.main_path, login.as_ref());

        let result: TestResult = Arc::new(Mutex::new(None));
        self.test = Some(result.clone());
        let ctx = ctx.clone();
        tokio::task::spawn_blocking(move || {
            let outcome = discovery::probe(&url);
            *result.lock().unwrap_or_else(PoisonError::into_inner) = Some(outcome);
            ctx.request_repaint();
        });
    }

    fn show(&mut self, ctx: &egui::Context, env: &DialogEnv<'_>) -> DialogAction {
        let title = if self.editing.is_some() {
            "Edit camera"
        } else {
            "Add camera"
        };
        let response = dialog_window(title).show(ctx, |ui| {
            ui.set_width(DIALOG_WIDTH);
            egui::Grid::new("camera_form")
                .num_columns(2)
                .spacing([12.0, 10.0])
                .show(ui, |ui| {
                    ui.label("Name");
                    text_field(ui, &mut self.name, "E.g. Garage");
                    ui.end_row();

                    ui.label("Address");
                    text_field(ui, &mut self.host, "IP or name, e.g. 192.168.50.124");
                    ui.end_row();

                    ui.label("Port");
                    ui.add(egui::TextEdit::singleline(&mut self.port).desired_width(80.0));
                    ui.end_row();

                    ui.label("Main stream");
                    text_field(ui, &mut self.main_path, "/stream1");
                    ui.end_row();

                    ui.label("Substream");
                    text_field(ui, &mut self.sub_path, "Optional, e.g. /stream2");
                    ui.end_row();

                    ui.label("Login");
                    ui.vertical(|ui| {
                        let shared_label = if env.shared.is_some() {
                            "Default login"
                        } else {
                            "Default login (not set yet)"
                        };
                        ui.radio_value(&mut self.auth, CameraAuth::Shared, shared_label);
                        ui.radio_value(
                            &mut self.auth,
                            CameraAuth::Own,
                            "Login for this camera only",
                        );
                        ui.radio_value(&mut self.auth, CameraAuth::None, "No login");
                    });
                    ui.end_row();

                    if self.auth == CameraAuth::Own {
                        ui.label("Username");
                        text_field(ui, &mut self.own_username, "");
                        ui.end_row();

                        ui.label("Password");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.own_password)
                                .password(true)
                                .desired_width(f32::INFINITY),
                        );
                        ui.end_row();
                    }
                });

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let testing = self.test.as_ref().is_some_and(|test| {
                    test.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .is_none()
                });
                if ui
                    .add_enabled(!testing, secondary_button("Test connection"))
                    .clicked()
                {
                    self.start_test(ctx, env);
                }
                if let Some(test) = &self.test {
                    match &*test.lock().unwrap_or_else(PoisonError::into_inner) {
                        None => {
                            ui.spinner();
                            ui.label("Connecting…");
                        }
                        Some(Ok(info)) => {
                            ui.colored_label(
                                env.palette.success_text,
                                format!("Connected: {info}"),
                            );
                        }
                        Some(Err(err)) => {
                            ui.colored_label(env.palette.critical_text, format!("Failed: {err}"));
                        }
                    }
                }
            });

            show_error(ui, env, self.error.as_deref());
            let (save, cancel) = footer(ui, env.palette, "Save", true);
            if cancel {
                return DialogAction::Close;
            }
            if save {
                match self.validated(env.cameras) {
                    Ok((camera, own)) => return DialogAction::SaveCamera { camera, own },
                    Err(err) => self.error = Some(err),
                }
            }
            DialogAction::None
        });
        inner_action(response)
    }
}

// ---------------------------------------------------------------------------
// Default login
// ---------------------------------------------------------------------------

pub struct LoginForm {
    username: String,
    password: String,
    show_password: bool,
    error: Option<String>,
}

impl LoginForm {
    pub fn new(current: Option<&Credentials>) -> Self {
        Self {
            username: current
                .map(|login| login.username.clone())
                .unwrap_or_default(),
            password: current
                .map(|login| login.password.clone())
                .unwrap_or_default(),
            show_password: false,
            error: None,
        }
    }

    fn show(&mut self, ctx: &egui::Context, env: &DialogEnv<'_>) -> DialogAction {
        let response = dialog_window("Default camera login").show(ctx, |ui| {
            ui.set_width(DIALOG_WIDTH);
            secondary_text(
                ui,
                env,
                "Used by cameras with the \"Default login\" option and by the network scan. \
                 It is kept in the Windows Credential Manager.",
            );
            ui.add_space(8.0);
            egui::Grid::new("login_form")
                .num_columns(2)
                .spacing([12.0, 10.0])
                .show(ui, |ui| {
                    ui.label("Username");
                    text_field(ui, &mut self.username, "");
                    ui.end_row();

                    ui.label("Password");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.password)
                            .password(!self.show_password)
                            .desired_width(f32::INFINITY),
                    );
                    ui.end_row();

                    ui.label("");
                    ui.checkbox(&mut self.show_password, "Show password");
                    ui.end_row();
                });

            show_error(ui, env, self.error.as_deref());
            let (save, cancel) = footer(ui, env.palette, "Save", true);
            if cancel {
                return DialogAction::Close;
            }
            if save {
                let login = Credentials {
                    username: self.username.trim().to_owned(),
                    password: self.password.clone(),
                };
                match login.validate() {
                    Ok(()) => return DialogAction::SaveSharedLogin(login),
                    Err(err) => self.error = Some(err),
                }
            }
            DialogAction::None
        });
        inner_action(response)
    }
}

// ---------------------------------------------------------------------------
// Network scan
// ---------------------------------------------------------------------------

/// Camera found and checked (or not) to be added.
struct Selection {
    checked: bool,
    name: String,
}

pub struct ScanDialog {
    port: String,
    main_path: String,
    sub_path: String,
    /// Scan in progress or finished.
    progress: Option<SharedProgress>,
    /// Parameters of the scan in progress or finished.
    scanned_with: Option<ScanSettings>,
    selection: BTreeMap<Ipv4Addr, Selection>,
    error: Option<String>,
}

impl ScanDialog {
    pub fn new(defaults: &ScanSettings) -> Self {
        Self {
            port: defaults.port.to_string(),
            main_path: defaults.main_path.clone(),
            sub_path: defaults.sub_path.clone(),
            progress: None,
            scanned_with: None,
            selection: BTreeMap::new(),
            error: None,
        }
    }

    fn settings(&self) -> Result<ScanSettings, String> {
        let port: u16 = self
            .port
            .trim()
            .parse()
            .ok()
            .filter(|port| *port > 0)
            .ok_or_else(|| "Invalid port.".to_owned())?;
        let main_path = rtsp_url::normalize_path(&self.main_path);
        if main_path.len() <= 1 {
            return Err("Enter the main stream path.".to_owned());
        }
        let sub_path = self.sub_path.trim();
        Ok(ScanSettings {
            port,
            main_path,
            sub_path: if sub_path.is_empty() {
                String::new()
            } else {
                rtsp_url::normalize_path(sub_path)
            },
        })
    }

    fn start(&mut self, ctx: &egui::Context, env: &DialogEnv<'_>) {
        let settings = match self.settings() {
            Ok(settings) => settings,
            Err(err) => {
                self.error = Some(err);
                return;
            }
        };
        self.error = None;
        self.selection.clear();

        let request = ScanRequest {
            port: settings.port,
            main_path: settings.main_path.clone(),
            sub_path: Some(settings.sub_path.clone()).filter(|path| !path.is_empty()),
            credentials: env.shared.cloned(),
        };
        let ctx = ctx.clone();
        self.progress = Some(discovery::start_scan(request, move || {
            ctx.request_repaint()
        }));
        self.scanned_with = Some(settings);
    }

    fn show(&mut self, ctx: &egui::Context, env: &DialogEnv<'_>) -> DialogAction {
        let snapshot = self.progress.as_ref().map(|progress| {
            progress
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        });
        let running = snapshot
            .as_ref()
            .is_some_and(|progress| progress.stage != ScanStage::Done);

        let response = dialog_window("Scan the network for cameras").show(ctx, |ui| {
            ui.set_width(DIALOG_WIDTH + 80.0);
            secondary_text(
                ui,
                env,
                "Looks for the RTSP port on every address of your local network and tests \
                 the video paths with the default login.",
            );
            if env.shared.is_none() {
                ui.add_space(4.0);
                ui.colored_label(
                    env.palette.critical_text,
                    "Default login not set: only cameras without a password will be found.",
                );
            }
            ui.add_space(8.0);

            ui.add_enabled_ui(!running, |ui| {
                egui::Grid::new("scan_form")
                    .num_columns(6)
                    .spacing([8.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("Port");
                        ui.add(egui::TextEdit::singleline(&mut self.port).desired_width(52.0));
                        ui.label("Main");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.main_path).desired_width(110.0),
                        );
                        ui.label("Substream");
                        ui.add(egui::TextEdit::singleline(&mut self.sub_path).desired_width(110.0));
                        ui.end_row();
                    });
            });
            ui.add_space(8.0);

            ui.horizontal(|ui| {
                let label = if snapshot.is_some() {
                    "Scan again"
                } else {
                    "Scan"
                };
                if ui
                    .add_enabled(!running, accent_button(label, env.palette))
                    .clicked()
                {
                    self.start(ctx, env);
                }
                if let Some(progress) = &snapshot {
                    if !progress.networks.is_empty() {
                        secondary_text(
                            ui,
                            env,
                            &format!("Network: {}", progress.networks.join(", ")),
                        );
                    }
                }
            });

            if let Some(progress) = &snapshot {
                ui.add_space(8.0);
                show_scan_progress(ui, progress, self.scanned_with.as_ref());
                ui.add_space(4.0);
                self.show_results(ui, env, progress);
            }

            show_error(ui, env, self.error.as_deref());

            let to_add = self.selected_cameras(snapshot.as_ref());
            let label = match to_add.len() {
                0 => "Add".to_owned(),
                count => format!("Add ({count})"),
            };
            let (add, close) = footer(ui, env.palette, &label, !to_add.is_empty() && !running);
            if close {
                return DialogAction::Close;
            }
            if add {
                if let Some(scan) = self.scanned_with.clone() {
                    return DialogAction::AddCameras {
                        cameras: to_add,
                        scan,
                    };
                }
            }
            DialogAction::None
        });
        inner_action(response)
    }

    fn show_results(
        &mut self,
        ui: &mut egui::Ui,
        env: &DialogEnv<'_>,
        progress: &discovery::ScanProgress,
    ) {
        let port = self
            .scanned_with
            .as_ref()
            .map_or(rtsp_url::DEFAULT_PORT, |scan| scan.port);
        if progress.stage == ScanStage::Done && progress.found.is_empty() {
            secondary_text(ui, env, "No device with this port open was found.");
            return;
        }

        egui::ScrollArea::vertical()
            .max_height(260.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for device in &progress.found {
                    let ip = device.ip.to_string();
                    let already_added = env
                        .cameras
                        .iter()
                        .any(|camera| camera.host == ip && camera.port == port);

                    match &device.main {
                        Ok(info) if already_added => {
                            secondary_text(
                                ui,
                                env,
                                &format!("{ip} · {info} · already in the list"),
                            );
                        }
                        Ok(info) => {
                            let entry =
                                self.selection
                                    .entry(device.ip)
                                    .or_insert_with(|| Selection {
                                        checked: true,
                                        name: format!("Camera {}", device.ip.octets()[3]),
                                    });
                            ui.horizontal(|ui| {
                                ui.checkbox(&mut entry.checked, "");
                                ui.add(
                                    egui::TextEdit::singleline(&mut entry.name)
                                        .desired_width(140.0),
                                );
                                let sub = device
                                    .sub
                                    .as_ref()
                                    .map(|sub| format!(" · substream {}×{}", sub.width, sub.height))
                                    .unwrap_or_else(|| " · no substream".to_owned());
                                ui.label(format!("{ip} · {info}{sub}"));
                            });
                        }
                        Err(err) => {
                            secondary_text(ui, env, &format!("{ip} · port open, but {err}"));
                        }
                    }
                }
            });
    }

    /// Cameras checked to be added, ready to save.
    fn selected_cameras(&self, progress: Option<&discovery::ScanProgress>) -> Vec<StoredCamera> {
        let (Some(progress), Some(scan)) = (progress, &self.scanned_with) else {
            return Vec::new();
        };
        progress
            .found
            .iter()
            .filter(|device| device.main.is_ok())
            .filter_map(|device| {
                let selection = self.selection.get(&device.ip)?;
                selection.checked.then(|| StoredCamera {
                    id: store::new_camera_id(),
                    name: Some(selection.name.trim().to_owned()).filter(|name| !name.is_empty()),
                    host: device.ip.to_string(),
                    port: scan.port,
                    main_path: scan.main_path.clone(),
                    sub_path: device
                        .sub
                        .as_ref()
                        .map(|_| scan.sub_path.clone())
                        .filter(|path| !path.is_empty()),
                    auth: CameraAuth::Shared,
                })
            })
            .collect()
    }
}

fn show_scan_progress(
    ui: &mut egui::Ui,
    progress: &discovery::ScanProgress,
    scan: Option<&ScanSettings>,
) {
    let port = scan.map_or(rtsp_url::DEFAULT_PORT, |scan| scan.port);
    let fraction = if progress.total == 0 {
        1.0
    } else {
        progress.done as f32 / progress.total as f32
    };
    let text = match progress.stage {
        ScanStage::Ports => format!(
            "Looking for port {port} on {} addresses… ({}/{})",
            progress.total, progress.done, progress.total
        ),
        ScanStage::Probing => format!(
            "Testing {} device(s) with the port open… ({}/{})",
            progress.total, progress.done, progress.total
        ),
        ScanStage::Done => {
            let cameras = progress
                .found
                .iter()
                .filter(|device| device.main.is_ok())
                .count();
            format!("Scan finished: {cameras} camera(s) responding.")
        }
    };
    ui.add(egui::ProgressBar::new(fraction).text(text));
}

// ---------------------------------------------------------------------------
// Remove camera
// ---------------------------------------------------------------------------

fn show_confirm_remove(
    ctx: &egui::Context,
    env: &DialogEnv<'_>,
    id: &str,
    label: &str,
) -> DialogAction {
    let response = dialog_window("Remove camera").show(ctx, |ui| {
        ui.set_width(DIALOG_WIDTH - 80.0);
        ui.label(format!("Remove \"{label}\" from the list?"));
        secondary_text(ui, env, "It can be added again later.");
        let (remove, cancel) = footer(ui, env.palette, "Remove", true);
        if remove {
            DialogAction::RemoveCamera(id.to_owned())
        } else if cancel {
            DialogAction::Close
        } else {
            DialogAction::None
        }
    });
    inner_action(response)
}

// ---------------------------------------------------------------------------
// Shared pieces
// ---------------------------------------------------------------------------

fn dialog_window(title: &str) -> egui::Window<'static> {
    egui::Window::new(title.to_owned())
        .id(egui::Id::new((
            "simple_rtsp_viewer_dialog",
            title.to_owned(),
        )))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
}

fn inner_action(response: Option<egui::InnerResponse<Option<DialogAction>>>) -> DialogAction {
    response
        .and_then(|response| response.inner)
        .unwrap_or(DialogAction::None)
}

fn text_field(ui: &mut egui::Ui, value: &mut String, hint: &str) {
    ui.add(
        egui::TextEdit::singleline(value)
            .hint_text(hint)
            .desired_width(f32::INFINITY),
    );
}

fn secondary_text(ui: &mut egui::Ui, env: &DialogEnv<'_>, text: &str) {
    ui.add(egui::Label::new(egui::RichText::new(text).color(env.palette.text_secondary)).wrap());
}

fn show_error(ui: &mut egui::Ui, env: &DialogEnv<'_>, error: Option<&str>) {
    if let Some(error) = error {
        ui.add_space(6.0);
        ui.colored_label(env.palette.critical_text, error);
    }
}

/// Primary button, in the Windows accent color.
pub fn accent_button(text: &str, palette: &Palette) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(text.to_owned()).color(palette.on_accent))
        .fill(palette.accent)
        .min_size(egui::vec2(96.0, 32.0))
}

pub fn secondary_button(text: &str) -> egui::Button<'static> {
    egui::Button::new(text.to_owned()).min_size(egui::vec2(96.0, 32.0))
}

/// Footer with the primary and cancel buttons, aligned to the right as in
/// Windows 11 dialogs. Returns (primary clicked, cancel clicked).
fn footer(
    ui: &mut egui::Ui,
    palette: &Palette,
    primary: &str,
    primary_enabled: bool,
) -> (bool, bool) {
    ui.add_space(16.0);
    let mut clicked = (false, false);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        clicked.1 = ui.add(secondary_button("Cancel")).clicked();
        clicked.0 = ui
            .add_enabled(primary_enabled, accent_button(primary, palette))
            .clicked();
    });
    clicked
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form() -> CameraForm {
        let mut form = CameraForm::new(&ScanSettings::default());
        form.host = "192.168.50.124".to_owned();
        form
    }

    #[test]
    fn new_camera_form_uses_default_paths_and_shared_login() {
        let (camera, own) = form().validated(&[]).unwrap();
        assert_eq!(camera.host, "192.168.50.124");
        assert_eq!(camera.port, 554);
        assert_eq!(camera.main_path, "/stream1");
        assert_eq!(camera.sub_path.as_deref(), Some("/stream2"));
        assert_eq!(camera.auth, CameraAuth::Shared);
        assert_eq!(camera.name, None);
        assert!(own.is_none());
    }

    #[test]
    fn paths_are_normalized_and_empty_substream_is_none() {
        let mut form = form();
        form.main_path = "live/main".to_owned();
        form.sub_path = "  ".to_owned();
        let (camera, _) = form.validated(&[]).unwrap();
        assert_eq!(camera.main_path, "/live/main");
        assert_eq!(camera.sub_path, None);
    }

    #[test]
    fn invalid_fields_are_reported() {
        let mut bad_host = form();
        bad_host.host = "living room camera".to_owned();
        assert!(bad_host.validated(&[]).is_err());

        let mut bad_port = form();
        bad_port.port = "0".to_owned();
        assert!(bad_port.validated(&[]).is_err());

        let mut same_paths = form();
        same_paths.sub_path = "/stream1".to_owned();
        assert!(same_paths.validated(&[]).is_err());

        let mut bad_login = form();
        bad_login.auth = CameraAuth::Own;
        bad_login.own_username = String::new();
        assert!(bad_login.validated(&[]).is_err());
    }

    #[test]
    fn own_login_is_returned_separately() {
        let mut form = form();
        form.auth = CameraAuth::Own;
        form.own_username = " admin ".to_owned();
        form.own_password = "secret".to_owned();
        let (camera, own) = form.validated(&[]).unwrap();
        assert_eq!(camera.auth, CameraAuth::Own);
        let own = own.unwrap();
        assert_eq!(own.username, "admin");
        assert_eq!(own.password, "secret");
    }

    #[test]
    fn duplicate_camera_is_rejected_but_editing_itself_is_fine() {
        let (existing, _) = form().validated(&[]).unwrap();
        assert!(form().validated(std::slice::from_ref(&existing)).is_err());

        let mut editing = CameraForm::edit(&existing, None);
        editing.name = "Garage".to_owned();
        let (edited, _) = editing.validated(std::slice::from_ref(&existing)).unwrap();
        assert_eq!(edited.id, existing.id);
        assert_eq!(edited.name.as_deref(), Some("Garage"));
    }
}
