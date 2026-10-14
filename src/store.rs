//! The app's camera library: the list stored in `%APPDATA%` (without passwords,
//! in grid order) and the logins from the Windows vault.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::credentials::Credentials;
use crate::model::Camera;
use crate::rtsp_url;

/// App folder inside `%APPDATA%`.
const APP_FOLDER: &str = "Simple RTSP Viewer";
/// File with the camera list.
const STORE_FILE: &str = "cameras.json";

/// Which login a camera uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CameraAuth {
    /// The cameras' default login.
    #[default]
    Shared,
    /// No login.
    None,
    /// A login for this camera only, stored in the vault under its `id`.
    Own,
}

/// Camera stored by the app, without a password.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredCamera {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    pub host: String,
    pub port: u16,
    /// Main stream path, starting with `/`.
    pub main_path: String,
    /// Substream path (lower resolution), if the camera has one.
    #[serde(default)]
    pub sub_path: Option<String>,
    #[serde(default)]
    pub auth: CameraAuth,
}

impl StoredCamera {
    /// Address to show the user: the host, plus the port if it is not the default.
    pub fn address(&self) -> String {
        if self.port == rtsp_url::DEFAULT_PORT {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }

    /// Same physical camera and same main stream.
    pub fn same_stream(&self, other: &StoredCamera) -> bool {
        self.host.eq_ignore_ascii_case(&other.host)
            && self.port == other.port
            && self.main_path == other.main_path
    }
}

/// Network scan defaults (and defaults of the new camera form fields).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanSettings {
    pub port: u16,
    pub main_path: String,
    pub sub_path: String,
}

/// Paths used by TP-Link Tapo cameras: `stream1` is the main stream
/// and `stream2` the substream.
impl Default for ScanSettings {
    fn default() -> Self {
        Self {
            port: rtsp_url::DEFAULT_PORT,
            main_path: "/stream1".to_owned(),
            sub_path: "/stream2".to_owned(),
        }
    }
}

/// Content of the library file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreData {
    /// Cameras in grid order.
    #[serde(default)]
    pub cameras: Vec<StoredCamera>,
    #[serde(default)]
    pub scan: ScanSettings,
}

/// Loaded library: the stored list and the logins from the vault.
#[derive(Debug, Clone)]
pub struct CameraLibrary {
    pub path: PathBuf,
    pub data: StoreData,
    /// Default login, if already set.
    pub shared: Option<Credentials>,
    /// Per-camera logins, by camera `id`.
    pub own: HashMap<String, Credentials>,
}

impl CameraLibrary {
    /// Cameras ready to display, with each one's login in the URLs.
    pub fn cameras(&self) -> Vec<Camera> {
        self.data
            .cameras
            .iter()
            .map(|camera| self.to_camera(camera))
            .collect()
    }

    fn to_camera(&self, camera: &StoredCamera) -> Camera {
        let credentials = self.credentials_for(camera);
        Camera {
            id: camera.id.clone(),
            rtsp_url: rtsp_url::build(&camera.host, camera.port, &camera.main_path, credentials),
            name: camera.name.clone(),
            substream_url: camera
                .sub_path
                .as_deref()
                .map(|path| rtsp_url::build(&camera.host, camera.port, path, credentials)),
        }
    }

    /// Login the camera uses, if any.
    pub fn credentials_for(&self, camera: &StoredCamera) -> Option<&Credentials> {
        match camera.auth {
            CameraAuth::Shared => self.shared.as_ref(),
            CameraAuth::None => None,
            CameraAuth::Own => self.own.get(&camera.id),
        }
    }

    /// Some camera depends on the default login, which is not set yet.
    pub fn missing_shared_login(&self) -> bool {
        self.shared.is_none()
            && self
                .data
                .cameras
                .iter()
                .any(|camera| camera.auth == CameraAuth::Shared)
    }
}

/// `%APPDATA%\Simple RTSP Viewer\cameras.json` (or the current folder if
/// `APPDATA` does not exist).
pub fn default_store_path() -> PathBuf {
    match std::env::var_os("APPDATA") {
        Some(app_data) => Path::new(&app_data).join(APP_FOLDER).join(STORE_FILE),
        None => PathBuf::from(STORE_FILE),
    }
}

/// Reads the library; `Ok(None)` if the file does not exist yet.
pub fn load(path: &Path) -> Result<Option<StoreData>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|err| format!("{} is not valid JSON: {err}", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(format!("failed to read {}: {err}", path.display())),
    }
}

/// Writes the library atomically, creating the app folder if needed.
pub fn save(path: &Path, data: &StoreData) -> std::io::Result<()> {
    if let Some(folder) = path.parent() {
        if !folder.as_os_str().is_empty() {
            std::fs::create_dir_all(folder)?;
        }
    }
    crate::layout::save_json_atomically(path, data)
}

/// New unique id for a camera.
pub fn new_camera_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    format!("{nanos:x}{:04x}", COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// Result of merging imported cameras (URLs with login) into the library.
#[derive(Debug, Default)]
pub struct ImportMerge {
    /// New cameras added.
    pub added: usize,
    /// Cameras skipped because they are already in the library.
    pub duplicates: usize,
    /// The default login was set from the imported URLs.
    pub shared_defined: bool,
    /// Per-camera logins to save to the vault, by camera `id`.
    pub own: Vec<(String, Credentials)>,
}

/// Merges cameras imported from a `cameras.json` (URLs with the embedded login)
/// into the library, separating the logins:
/// - with no default login set, the first login found becomes the default;
/// - cameras with the default login use it, cameras without a login stay
///   without one, and cameras with another login get their own login
///   (returned in `own`);
/// - cameras already in the library (same host, port and path) are skipped;
/// - the substream is only kept if it is on the same host and port.
pub fn merge_imported(
    data: &mut StoreData,
    imported: &[Camera],
    shared: &mut Option<Credentials>,
) -> ImportMerge {
    let mut merge = ImportMerge::default();

    for camera in imported {
        let Some(main) = rtsp_url::parse(&camera.rtsp_url) else {
            continue;
        };

        let sub_path = camera
            .substream_url
            .as_deref()
            .and_then(rtsp_url::parse)
            .filter(|sub| sub.host.eq_ignore_ascii_case(&main.host) && sub.port == main.port)
            .map(|sub| sub.path)
            .filter(|path| path != &main.path);

        let mut stored = StoredCamera {
            id: new_camera_id(),
            name: camera.name.clone(),
            host: main.host,
            port: main.port,
            main_path: main.path,
            sub_path,
            auth: CameraAuth::None,
        };
        if data
            .cameras
            .iter()
            .any(|existing| existing.same_stream(&stored))
        {
            merge.duplicates += 1;
            continue;
        }

        if let Some(login) = main.credentials {
            if shared.is_none() {
                *shared = Some(login.clone());
                merge.shared_defined = true;
            }
            if shared.as_ref() == Some(&login) {
                stored.auth = CameraAuth::Shared;
            } else {
                stored.auth = CameraAuth::Own;
                merge.own.push((stored.id.clone(), login));
            }
        }

        data.cameras.push(stored);
        merge.added += 1;
    }

    merge
}

#[cfg(test)]
mod tests {
    use super::*;

    fn imported(url: &str, sub: Option<&str>, name: Option<&str>) -> Camera {
        Camera {
            id: url.to_owned(),
            rtsp_url: url.to_owned(),
            name: name.map(str::to_owned),
            substream_url: sub.map(str::to_owned),
        }
    }

    fn login(username: &str, password: &str) -> Credentials {
        Credentials {
            username: username.to_owned(),
            password: password.to_owned(),
        }
    }

    #[test]
    fn first_login_becomes_shared_and_is_removed_from_the_cameras() {
        let mut data = StoreData::default();
        let mut shared = None;
        let merge = merge_imported(
            &mut data,
            &[
                imported(
                    "rtsp://camuser:pw@192.168.50.122:554/stream1",
                    Some("rtsp://camuser:pw@192.168.50.122:554/stream2"),
                    Some("Camera 122"),
                ),
                imported("rtsp://camuser:pw@192.168.50.123:554/stream1", None, None),
            ],
            &mut shared,
        );

        assert_eq!(merge.added, 2);
        assert!(merge.shared_defined);
        assert!(merge.own.is_empty());
        assert_eq!(shared, Some(login("camuser", "pw")));

        let first = &data.cameras[0];
        assert_eq!(first.host, "192.168.50.122");
        assert_eq!(first.port, 554);
        assert_eq!(first.main_path, "/stream1");
        assert_eq!(first.sub_path.as_deref(), Some("/stream2"));
        assert_eq!(first.auth, CameraAuth::Shared);
        assert_eq!(first.name.as_deref(), Some("Camera 122"));
        assert_eq!(data.cameras[1].sub_path, None);

        // No password in what goes to the file.
        let json = serde_json::to_string(&data).unwrap();
        assert!(!json.contains("pw"));
        assert!(!json.contains("camuser"));
    }

    #[test]
    fn different_login_becomes_own_and_no_login_stays_none() {
        let mut data = StoreData::default();
        let mut shared = Some(login("camuser", "pw"));
        let merge = merge_imported(
            &mut data,
            &[
                imported("rtsp://other:x@10.0.0.5/live", None, None),
                imported("rtsp://10.0.0.6/live", None, None),
            ],
            &mut shared,
        );

        assert!(!merge.shared_defined);
        assert_eq!(data.cameras[0].auth, CameraAuth::Own);
        assert_eq!(
            merge.own,
            vec![(data.cameras[0].id.clone(), login("other", "x"))]
        );
        assert_eq!(data.cameras[1].auth, CameraAuth::None);
    }

    #[test]
    fn duplicates_and_substreams_on_other_hosts_are_skipped() {
        let mut data = StoreData::default();
        let mut shared = None;
        merge_imported(
            &mut data,
            &[imported(
                "rtsp://u:p@10.0.0.5/s1",
                Some("rtsp://u:p@10.0.0.9/s2"),
                None,
            )],
            &mut shared,
        );
        assert_eq!(data.cameras[0].sub_path, None);

        let merge = merge_imported(
            &mut data,
            &[imported("rtsp://10.0.0.5:554/s1", None, None)],
            &mut shared,
        );
        assert_eq!(merge.added, 0);
        assert_eq!(merge.duplicates, 1);
        assert_eq!(data.cameras.len(), 1);
    }

    #[test]
    fn library_builds_urls_with_the_right_login() {
        let camera = |id: &str, auth| StoredCamera {
            id: id.to_owned(),
            name: None,
            host: "10.0.0.5".to_owned(),
            port: 554,
            main_path: "/stream1".to_owned(),
            sub_path: Some("/stream2".to_owned()),
            auth,
        };
        let library = CameraLibrary {
            path: PathBuf::from("x.json"),
            data: StoreData {
                cameras: vec![
                    camera("a", CameraAuth::Shared),
                    camera("b", CameraAuth::None),
                    camera("c", CameraAuth::Own),
                ],
                scan: ScanSettings::default(),
            },
            shared: Some(login("default", "1")),
            own: HashMap::from([("c".to_owned(), login("own", "2"))]),
        };

        let cameras = library.cameras();
        assert_eq!(cameras[0].rtsp_url, "rtsp://default:1@10.0.0.5:554/stream1");
        assert_eq!(
            cameras[0].substream_url.as_deref(),
            Some("rtsp://default:1@10.0.0.5:554/stream2")
        );
        assert_eq!(cameras[1].rtsp_url, "rtsp://10.0.0.5:554/stream1");
        assert_eq!(cameras[2].rtsp_url, "rtsp://own:2@10.0.0.5:554/stream1");
        assert_eq!(cameras[2].id, "c");
        assert!(!library.missing_shared_login());
    }

    #[test]
    fn store_roundtrips_through_disk() {
        let path = std::env::temp_dir().join(format!(
            "store_test_{}_{}.json",
            std::process::id(),
            new_camera_id()
        ));
        assert_eq!(load(&path).unwrap(), None);

        let mut data = StoreData::default();
        let mut shared = None;
        merge_imported(
            &mut data,
            &[imported(
                "rtsp://u:p@10.0.0.5/s1",
                None,
                Some("Living room"),
            )],
            &mut shared,
        );
        save(&path, &data).unwrap();
        assert_eq!(load(&path).unwrap(), Some(data));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn camera_ids_are_unique() {
        let ids: std::collections::HashSet<String> = (0..1000).map(|_| new_camera_id()).collect();
        assert_eq!(ids.len(), 1000);
    }
}
