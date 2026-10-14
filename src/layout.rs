//! Layout_Persistence: reconciliation and persistence of the grid order.

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::model::Camera;

/// Core pure function of this component (basis of the "Layout reconciliation" Correctness
/// Property).
///
/// Reconciles the list of imported cameras with the previously saved order:
/// - Cameras whose `rtsp_url` appears in `saved_order` are placed following the
///   relative order in which those URLs appear in `saved_order`.
/// - Cameras in `imported` with no match in `saved_order` are placed after all the
///   previous ones, keeping among themselves the relative order in which they appear in `imported`.
/// - URLs in `saved_order` with no match in `imported` are ignored (orphan cameras).
/// - If `imported` contains duplicate `rtsp_url`s, each position of `saved_order` is
///   matched with at most one camera of `imported`, consuming positions to preserve
///   cardinality (no camera is lost or duplicated in the result).
pub fn restore_order(imported: &[Camera], saved_order: &[String]) -> Vec<Camera> {
    let mut consumed = vec![false; imported.len()];
    let mut result = Vec::with_capacity(imported.len());

    for url in saved_order {
        if let Some(idx) = imported
            .iter()
            .enumerate()
            .find(|(i, cam)| !consumed[*i] && &cam.rtsp_url == url)
            .map(|(i, _)| i)
        {
            consumed[idx] = true;
            result.push(imported[idx].clone());
        }
    }

    for (i, cam) in imported.iter().enumerate() {
        if !consumed[i] {
            result.push(cam.clone());
        }
    }

    result
}

/// Persisted format of the Layout_Config_File: only the order of the cameras'
/// `rtsp_url`s in the grid, used as the matching key by `restore_order`
/// (Requirement 5.1).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LayoutFile {
    pub camera_order: Vec<String>,
}

/// Pure inverse of `restore_order`: extracts the current in-memory order into
/// the persisted format.
pub fn build_layout_file(current_order: &[Camera]) -> LayoutFile {
    LayoutFile {
        camera_order: current_order
            .iter()
            .map(|cam| cam.rtsp_url.clone())
            .collect(),
    }
}

/// Possible errors when loading the Layout_Config_File (Requirements 5.2, 5.4, 8.2).
#[derive(Debug)]
pub enum LayoutLoadError {
    /// Failed to read the file from disk (includes the missing file case, Requirement
    /// 5.3).
    Io(std::io::Error),
    /// The file content is not syntactically valid JSON, or does not match the
    /// expected `LayoutFile` format (Requirement 5.4).
    InvalidJson(serde_json::Error),
}

impl std::fmt::Display for LayoutLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LayoutLoadError::Io(err) => write!(f, "failed to read the layout file: {err}"),
            LayoutLoadError::InvalidJson(err) => {
                write!(f, "the layout file is not valid JSON: {err}")
            }
        }
    }
}

impl std::error::Error for LayoutLoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LayoutLoadError::Io(err) => Some(err),
            LayoutLoadError::InvalidJson(err) => Some(err),
        }
    }
}

impl From<std::io::Error> for LayoutLoadError {
    fn from(err: std::io::Error) -> Self {
        LayoutLoadError::Io(err)
    }
}

/// Entry point with I/O: reads `path` and parses the JSON with serde into a
/// `LayoutFile` (Requirements 5.2, 8.2). A missing file or invalid JSON is
/// reported as `LayoutLoadError`; the caller decides whether to treat these
/// cases as an empty `saved_order` (Requirements 5.3, 5.4).
pub fn load_layout_file(path: &Path) -> Result<LayoutFile, LayoutLoadError> {
    let contents = fs::read_to_string(path)?;

    let layout: LayoutFile =
        serde_json::from_str(&contents).map_err(LayoutLoadError::InvalidJson)?;

    Ok(layout)
}

/// Writes `value` to `path` as JSON, atomically (Requirements 5.7, 8.1, 8.3).
///
/// The write goes to a temporary file in the same directory as `path` (so the
/// final `rename` is atomic on the same file system), with the JSON content
/// indented by `serde_json::to_writer_pretty`. The temporary file is synced to
/// disk before the `rename`, so a process failure between the write and the
/// rename never leaves `path` truncated or corrupted: if any step before the
/// rename fails, the temporary file is removed and the content previously saved
/// in `path` stays intact.
pub fn save_json_atomically<T: serde::Serialize>(
    path: &Path,
    value: &T,
) -> Result<(), std::io::Error> {
    // Global counter that makes the temporary file name unique across
    // concurrent writes from the same process. Without it, two writes fired in
    // quick succession (e.g. two reorders in a row, each on its own
    // `spawn_blocking` thread) would use the same temporary path and could step
    // on each other in `create`/`rename`, risking a corrupted write
    // (Requirement 8.3).
    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    let dir = path.parent().unwrap_or_else(|| Path::new("."));

    let temp_file_name = format!(
        "{}.tmp.{}.{}",
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "layout".to_string()),
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let temp_path = dir.join(temp_file_name);

    let write_result = (|| -> Result<(), std::io::Error> {
        let file = fs::File::create(&temp_path)?;
        let mut writer = std::io::BufWriter::new(file);
        serde_json::to_writer_pretty(&mut writer, value).map_err(std::io::Error::from)?;
        let file = writer.into_inner().map_err(|err| err.into_error())?;
        file.sync_all()?;
        Ok(())
    })();

    if let Err(err) = write_result {
        let _ = fs::remove_file(&temp_path);
        return Err(err);
    }

    if let Err(err) = fs::rename(&temp_path, path) {
        let _ = fs::remove_file(&temp_path);
        return Err(err);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::collections::HashMap;

    fn cam(url: &str) -> Camera {
        Camera {
            id: url.to_string(),
            rtsp_url: url.to_string(),
            name: None,
            substream_url: None,
        }
    }

    fn multiset(cams: &[Camera]) -> HashMap<String, usize> {
        let mut m = HashMap::new();
        for c in cams {
            *m.entry(c.rtsp_url.clone()).or_insert(0) += 1;
        }
        m
    }

    /// Independent reference of `restore_order` for the unique URL case: matched ones
    /// first (order of `saved`), then the others (order of `imported`).
    fn expected_unique(imported: &[Camera], saved: &[String]) -> Vec<Camera> {
        let mut result = Vec::new();
        let mut added: std::collections::HashSet<String> = std::collections::HashSet::new();
        for url in saved {
            if added.contains(url) {
                continue;
            }
            if let Some(c) = imported.iter().find(|c| &c.rtsp_url == url) {
                result.push(c.clone());
                added.insert(url.clone());
            }
        }
        for c in imported {
            if !added.contains(&c.rtsp_url) {
                result.push(c.clone());
            }
        }
        result
    }

    fn unique_temp_path(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "layout_test_{}_{}_{}.json",
            tag,
            std::process::id(),
            nanos
        ))
    }

    proptest! {
        // Feature: simple-rtsp-viewer, Property 5: Layout order reconciliation (unique URLs)
        #[test]
        fn property_restore_order_unique(
            imported_raw in proptest::collection::vec(
                proptest::sample::select(vec!['a', 'b', 'c', 'd', 'e', 'f']),
                0..8,
            ),
            saved_raw in proptest::collection::vec(
                proptest::sample::select(vec!['a', 'b', 'c', 'd', 'e', 'f', 'g', 'h']),
                0..10,
            ),
        ) {
            let mut seen = std::collections::HashSet::new();
            let imported: Vec<Camera> = imported_raw
                .iter()
                .filter(|c| seen.insert(**c))
                .map(|c| cam(&c.to_string()))
                .collect();
            let saved: Vec<String> = saved_raw.iter().map(|c| c.to_string()).collect();

            let result = restore_order(&imported, &saved);
            prop_assert_eq!(result, expected_unique(&imported, &saved));
        }

        // Feature: simple-rtsp-viewer, Property 5: cardinality preserved even with duplicate URLs
        #[test]
        fn property_restore_order_cardinality(
            imported_raw in proptest::collection::vec(
                proptest::sample::select(vec!['a', 'b', 'c']),
                0..10,
            ),
            saved_raw in proptest::collection::vec(
                proptest::sample::select(vec!['a', 'b', 'c', 'z']),
                0..10,
            ),
        ) {
            let imported: Vec<Camera> = imported_raw.iter().map(|c| cam(&c.to_string())).collect();
            let saved: Vec<String> = saved_raw.iter().map(|c| c.to_string()).collect();
            let result = restore_order(&imported, &saved);
            prop_assert_eq!(result.len(), imported.len());
            prop_assert_eq!(multiset(&result), multiset(&imported));
        }

        // Feature: simple-rtsp-viewer, Property 8: Serialization round-trip (LayoutFile)
        #[test]
        fn property_layout_roundtrip(
            urls in proptest::collection::vec("[a-z0-9:/@._-]{1,20}", 0..12),
        ) {
            let layout = LayoutFile { camera_order: urls };
            let json = serde_json::to_string(&layout).unwrap();
            let back: LayoutFile = serde_json::from_str(&json).unwrap();
            prop_assert_eq!(layout.camera_order, back.camera_order);
        }

        // Feature: simple-rtsp-viewer, Property 8: Serialization round-trip (Camera)
        #[test]
        fn property_camera_roundtrip(
            url in "[^\"\\\\]{1,40}",
            name in proptest::option::of("[^\"\\\\]{0,30}"),
            substream_url in proptest::option::of("[^\"\\\\]{1,40}"),
        ) {
            let camera = Camera { id: url.clone(), rtsp_url: url, name, substream_url };
            let json = serde_json::to_string(&camera).unwrap();
            let back: Camera = serde_json::from_str(&json).unwrap();
            prop_assert_eq!(camera, back);
        }
    }

    #[test]
    fn restore_order_appends_unmatched_at_end() {
        let imported = vec![cam("rtsp://a"), cam("rtsp://b"), cam("rtsp://c")];
        let saved = vec!["rtsp://c".to_string(), "rtsp://a".to_string()];
        let result = restore_order(&imported, &saved);
        assert_eq!(
            result,
            vec![cam("rtsp://c"), cam("rtsp://a"), cam("rtsp://b")]
        );
    }

    #[test]
    fn restore_order_ignores_orphan_saved_urls() {
        let imported = vec![cam("rtsp://a")];
        let saved = vec!["rtsp://x".to_string(), "rtsp://a".to_string()];
        assert_eq!(restore_order(&imported, &saved), vec![cam("rtsp://a")]);
    }

    #[test]
    fn build_layout_file_extracts_urls_in_order() {
        let cameras = vec![cam("rtsp://a"), cam("rtsp://b")];
        let layout = build_layout_file(&cameras);
        assert_eq!(layout.camera_order, vec!["rtsp://a", "rtsp://b"]);
    }

    #[test]
    fn save_then_load_roundtrips_and_is_indented() {
        let path = unique_temp_path("roundtrip");
        let layout = LayoutFile {
            camera_order: vec!["rtsp://a".to_string(), "rtsp://b".to_string()],
        };
        save_json_atomically(&path, &layout).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        // JSON indented over multiple lines (Requirement 8.1).
        assert!(text.contains('\n'));
        assert!(text.contains("  "));

        let loaded = load_layout_file(&path).unwrap();
        assert_eq!(loaded.camera_order, layout.camera_order);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn load_missing_file_returns_io_error() {
        let path = std::env::temp_dir().join("layout_does_not_exist_84213.json");
        assert!(matches!(
            load_layout_file(&path),
            Err(LayoutLoadError::Io(_))
        ));
    }

    #[test]
    fn load_corrupted_file_returns_invalid_json_error() {
        let path = unique_temp_path("corrupt");
        std::fs::write(&path, "{ not valid json").unwrap();
        assert!(matches!(
            load_layout_file(&path),
            Err(LayoutLoadError::InvalidJson(_))
        ));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn save_preserves_previous_content_on_overwrite() {
        let path = unique_temp_path("overwrite");
        let first = LayoutFile {
            camera_order: vec!["rtsp://first".to_string()],
        };
        save_json_atomically(&path, &first).unwrap();
        let second = LayoutFile {
            camera_order: vec!["rtsp://second".to_string()],
        };
        save_json_atomically(&path, &second).unwrap();
        assert_eq!(
            load_layout_file(&path).unwrap().camera_order,
            second.camera_order
        );
        std::fs::remove_file(&path).ok();
    }
}
