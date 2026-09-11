//! Config_Importer: reading and validation of the Import_File.

use std::fs;
use std::path::Path;

use crate::model::Camera;

/// Maximum number of characters accepted for the RTSP URL.
const MAX_URL_LEN: usize = 2048;

/// Maximum number of characters accepted for the camera's display name.
const MAX_NAME_LEN: usize = 100;

/// Maximum number of valid cameras loaded from a single Import_File.
const MAX_CAMERAS: usize = 64;

/// Raw entry read from the Import_File, before validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedEntry {
    pub raw_url: String,
    pub raw_name: Option<String>,
    pub raw_substream_url: Option<String>,
}

/// Result of validating/importing a list of `ImportedEntry`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportOutcome {
    /// Valid cameras, in the original order, at most `MAX_CAMERAS`.
    pub cameras: Vec<Camera>,
    /// Number of entries dropped for not containing a valid RTSP_URL.
    pub rejected_count: usize,
    /// `true` if there were more than `MAX_CAMERAS` valid entries.
    pub truncated: bool,
}

/// Checks whether `url` matches the pattern
/// `rtsp://[user:password@]host[:port]/path` and respects the `MAX_URL_LEN`
/// character limit.
fn is_valid_rtsp_url(url: &str) -> bool {
    if url.chars().count() > MAX_URL_LEN {
        return false;
    }

    let Some(rest) = url.strip_prefix("rtsp://") else {
        return false;
    };

    // Splits the path (required, must start with '/') from the rest `[user:password@]host[:port]`.
    let Some(slash_idx) = rest.find('/') else {
        return false;
    };
    let authority = &rest[..slash_idx];
    let path = &rest[slash_idx..];

    if authority.is_empty() || path.len() <= 1 {
        // path == "/" alone has no path after the slash.
        return false;
    }

    // Splits the optional credentials (user:password@) from host[:port].
    let host_port = if let Some(at_idx) = authority.rfind('@') {
        let userinfo = &authority[..at_idx];
        let host_port = &authority[at_idx + 1..];
        // If there is an '@', there must be non-empty userinfo in the
        // user:password format.
        if userinfo.is_empty() {
            return false;
        }
        let mut parts = userinfo.splitn(2, ':');
        let user = parts.next().unwrap_or("");
        if user.is_empty() {
            return false;
        }
        // The password (if present) may be empty or not; only the username is
        // required.
        host_port
    } else {
        authority
    };

    if host_port.is_empty() {
        return false;
    }

    // Splits the host from the optional port, taking care not to confuse it with IPv6 (out of
    // scope here; we only handle the simple host[:port] format).
    let (host, port) = match host_port.rsplit_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (host_port, None),
    };

    if host.is_empty() {
        return false;
    }
    // The host must contain only characters typical of a hostname/IP.
    if !host
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
    {
        return false;
    }

    if let Some(port) = port {
        if port.is_empty() || !port.chars().all(|c| c.is_ascii_digit()) {
            return false;
        }
    }

    true
}

/// Validates and converts a list of `ImportedEntry` into an `ImportOutcome`,
/// applying the URL/name validation rules, the valid camera limit and the
/// computation of `truncated`/`rejected_count` (Requirements 1.1, 1.2, 1.4, 1.6,
/// 1.7).
pub fn validate_and_import(entries: &[ImportedEntry]) -> ImportOutcome {
    use std::collections::HashSet;

    let mut cameras = Vec::new();
    let mut rejected_count = 0usize;
    let mut valid_count = 0usize;
    let mut seen_urls: HashSet<&str> = HashSet::new();

    for entry in entries {
        if !is_valid_rtsp_url(&entry.raw_url) {
            rejected_count += 1;
            continue;
        }

        // Duplicate URLs are dropped: the System keys cameras, streams and
        // textures by `rtsp_url`, so two entries with the same URL cannot be
        // decoded or shown independently. For a simple viewer, importing the
        // same camera twice makes no sense; the repeat is counted as a rejected
        // entry.
        if !seen_urls.insert(entry.raw_url.as_str()) {
            rejected_count += 1;
            continue;
        }

        valid_count += 1;

        // A name that is too long is treated as "no name", without dropping the
        // camera.
        let name = match &entry.raw_name {
            Some(name) if name.chars().count() <= MAX_NAME_LEN => Some(name.clone()),
            _ => None,
        };

        // An invalid substream (or one equal to the main URL) is ignored
        // without dropping the camera: it then uses only the main URL.
        let substream_url = match &entry.raw_substream_url {
            Some(sub) if is_valid_rtsp_url(sub) && sub != &entry.raw_url => Some(sub.clone()),
            Some(sub) if sub != &entry.raw_url => {
                tracing::warn!(
                    rtsp_url = %crate::model::redact_rtsp_credentials(&entry.raw_url),
                    "invalid substream_url ignored; the camera will use only the main URL"
                );
                None
            }
            _ => None,
        };

        if cameras.len() < MAX_CAMERAS {
            cameras.push(Camera {
                rtsp_url: entry.raw_url.clone(),
                name,
                substream_url,
            });
        }
    }

    let truncated = valid_count > MAX_CAMERAS;

    ImportOutcome {
        cameras,
        rejected_count,
        truncated,
    }
}

/// Serde representation of an Import_File entry, with the JSON key names
/// (`url`/`name`) used in the input format (Requirement 8.2).
#[derive(Debug, Clone, serde::Deserialize)]
struct RawImportEntry {
    url: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    substream_url: Option<String>,
}

impl From<RawImportEntry> for ImportedEntry {
    fn from(raw: RawImportEntry) -> Self {
        ImportedEntry {
            raw_url: raw.url,
            raw_name: raw.name,
            raw_substream_url: raw.substream_url,
        }
    }
}

/// Possible errors when loading the Import_File (Requirement 1.3).
#[derive(Debug)]
pub enum ImportError {
    /// Failed to read the file from disk.
    Io(std::io::Error),
    /// The file content is not syntactically valid JSON.
    InvalidJson(serde_json::Error),
    /// The JSON is valid, but its top-level value is not a list.
    NotAList,
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImportError::Io(err) => write!(f, "failed to read the import file: {err}"),
            ImportError::InvalidJson(err) => {
                write!(f, "the import file is not valid JSON: {err}")
            }
            ImportError::NotAList => {
                write!(f, "the import file does not contain a list of cameras")
            }
        }
    }
}

impl std::error::Error for ImportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ImportError::Io(err) => Some(err),
            ImportError::InvalidJson(err) => Some(err),
            ImportError::NotAList => None,
        }
    }
}

impl From<std::io::Error> for ImportError {
    fn from(err: std::io::Error) -> Self {
        ImportError::Io(err)
    }
}

/// Entry point with I/O: reads `path`, parses the JSON with serde and hands
/// validation to `validate_and_import` (Requirements 1.3, 8.2).
pub fn load_import_file(path: &Path) -> Result<ImportOutcome, ImportError> {
    let contents = fs::read_to_string(path)?;

    // First parses into a generic `serde_json::Value` so "invalid JSON" can be told
    // apart from "valid JSON that is not a list" with a specific message.
    let value: serde_json::Value =
        serde_json::from_str(&contents).map_err(ImportError::InvalidJson)?;

    if !value.is_array() {
        return Err(ImportError::NotAList);
    }

    let raw_entries: Vec<RawImportEntry> =
        serde_json::from_value(value).map_err(ImportError::InvalidJson)?;

    let entries: Vec<ImportedEntry> = raw_entries.into_iter().map(ImportedEntry::from).collect();

    Ok(validate_and_import(&entries))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn entry(url: &str, name: Option<&str>) -> ImportedEntry {
        ImportedEntry {
            raw_url: url.to_string(),
            raw_name: name.map(str::to_string),
            raw_substream_url: None,
        }
    }

    fn entry_with_substream(url: &str, substream: &str) -> ImportedEntry {
        ImportedEntry {
            raw_substream_url: Some(substream.to_string()),
            ..entry(url, None)
        }
    }

    #[test]
    fn valid_substream_is_kept() {
        let outcome = validate_and_import(&[entry_with_substream(
            "rtsp://192.168.1.10/stream1",
            "rtsp://192.168.1.10/stream2",
        )]);
        assert_eq!(
            outcome.cameras[0].substream_url,
            Some("rtsp://192.168.1.10/stream2".to_string())
        );
    }

    #[test]
    fn invalid_substream_is_dropped_but_camera_kept() {
        let outcome = validate_and_import(&[entry_with_substream(
            "rtsp://192.168.1.10/stream1",
            "http://192.168.1.10/stream2",
        )]);
        assert_eq!(outcome.cameras.len(), 1);
        assert_eq!(outcome.rejected_count, 0);
        assert_eq!(outcome.cameras[0].substream_url, None);
    }

    #[test]
    fn substream_equal_to_main_url_is_dropped() {
        let outcome = validate_and_import(&[entry_with_substream(
            "rtsp://192.168.1.10/stream1",
            "rtsp://192.168.1.10/stream1",
        )]);
        assert_eq!(outcome.cameras[0].substream_url, None);
    }

    #[test]
    fn load_import_file_reads_substream_url() {
        let path = write_temp_file(
            r#"[{"url": "rtsp://192.168.1.10/stream1", "substream_url": "rtsp://192.168.1.10/stream2"}]"#,
        );
        let outcome = load_import_file(&path).unwrap();
        assert_eq!(
            outcome.cameras[0].substream_url,
            Some("rtsp://192.168.1.10/stream2".to_string())
        );
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn accepts_simple_valid_url_without_name() {
        let outcome = validate_and_import(&[entry("rtsp://192.168.1.20/stream1", None)]);
        assert_eq!(outcome.cameras.len(), 1);
        assert_eq!(outcome.rejected_count, 0);
        assert!(!outcome.truncated);
        assert_eq!(outcome.cameras[0].rtsp_url, "rtsp://192.168.1.20/stream1");
        assert_eq!(outcome.cameras[0].name, None);
    }

    #[test]
    fn accepts_url_with_credentials_and_port() {
        let outcome = validate_and_import(&[entry(
            "rtsp://user:password@192.168.1.10:554/stream1",
            Some("Front door"),
        )]);
        assert_eq!(outcome.cameras.len(), 1);
        assert_eq!(outcome.cameras[0].name, Some("Front door".to_string()));
    }

    #[test]
    fn rejects_entry_without_valid_url() {
        let outcome = validate_and_import(&[
            entry("http://192.168.1.20/stream1", None),
            entry("rtsp://192.168.1.20/stream1", None),
        ]);
        assert_eq!(outcome.cameras.len(), 1);
        assert_eq!(outcome.rejected_count, 1);
    }

    #[test]
    fn rejects_url_over_2048_chars() {
        let long_path = "a".repeat(MAX_URL_LEN);
        let url = format!("rtsp://host/{}", long_path);
        assert!(url.chars().count() > MAX_URL_LEN);
        let outcome = validate_and_import(&[entry(&url, None)]);
        assert_eq!(outcome.cameras.len(), 0);
        assert_eq!(outcome.rejected_count, 1);
    }

    #[test]
    fn name_over_100_chars_is_dropped_but_camera_kept() {
        let long_name = "n".repeat(MAX_NAME_LEN + 1);
        let outcome =
            validate_and_import(&[entry("rtsp://192.168.1.20/stream1", Some(&long_name))]);
        assert_eq!(outcome.cameras.len(), 1);
        assert_eq!(outcome.rejected_count, 0);
        assert_eq!(outcome.cameras[0].name, None);
    }

    #[test]
    fn truncates_after_64_valid_cameras_preserving_order() {
        let entries: Vec<ImportedEntry> = (0..70)
            .map(|i| entry(&format!("rtsp://host{}/stream", i), None))
            .collect();
        let outcome = validate_and_import(&entries);
        assert_eq!(outcome.cameras.len(), MAX_CAMERAS);
        assert!(outcome.truncated);
        assert_eq!(outcome.rejected_count, 0);
        // Keeps the original order: the first camera of the result is the first entry.
        assert_eq!(outcome.cameras[0].rtsp_url, "rtsp://host0/stream");
        assert_eq!(
            outcome.cameras[MAX_CAMERAS - 1].rtsp_url,
            format!("rtsp://host{}/stream", MAX_CAMERAS - 1)
        );
    }

    #[test]
    fn empty_entries_yield_empty_outcome() {
        let outcome = validate_and_import(&[]);
        assert_eq!(outcome.cameras.len(), 0);
        assert_eq!(outcome.rejected_count, 0);
        assert!(!outcome.truncated);
    }

    fn write_temp_file(contents: &str) -> std::path::PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "import_test_{}_{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn load_import_file_missing_file_returns_io_error() {
        let path = std::env::temp_dir().join("this_file_does_not_exist_12345.json");
        let result = load_import_file(&path);
        assert!(matches!(result, Err(ImportError::Io(_))));
    }

    #[test]
    fn load_import_file_invalid_json_returns_invalid_json_error() {
        let path = write_temp_file("{ not valid json");
        let result = load_import_file(&path);
        assert!(matches!(result, Err(ImportError::InvalidJson(_))));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn load_import_file_object_instead_of_list_returns_not_a_list_error() {
        let path = write_temp_file(r#"{"url": "rtsp://host/stream"}"#);
        let result = load_import_file(&path);
        assert!(matches!(result, Err(ImportError::NotAList)));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn load_import_file_empty_list_yields_empty_outcome() {
        let path = write_temp_file("[]");
        let outcome = load_import_file(&path).unwrap();
        assert_eq!(outcome.cameras.len(), 0);
        assert_eq!(outcome.rejected_count, 0);
        assert!(!outcome.truncated);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn load_import_file_valid_list_delegates_to_validate_and_import() {
        let path = write_temp_file(
            r#"[
                {"url": "rtsp://192.168.1.10/stream1", "name": "Front door"},
                {"url": "rtsp://192.168.1.20/stream1"},
                {"url": "not-a-valid-url"}
            ]"#,
        );
        let outcome = load_import_file(&path).unwrap();
        assert_eq!(outcome.cameras.len(), 2);
        assert_eq!(outcome.rejected_count, 1);
        assert_eq!(outcome.cameras[0].name, Some("Front door".to_string()));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn duplicate_urls_are_rejected_keeping_first() {
        let outcome = validate_and_import(&[
            entry("rtsp://192.168.1.10/s", Some("A")),
            entry("rtsp://192.168.1.10/s", Some("B")),
            entry("rtsp://192.168.1.20/s", None),
        ]);
        assert_eq!(outcome.cameras.len(), 2);
        assert_eq!(outcome.rejected_count, 1);
        assert!(!outcome.truncated);
        assert_eq!(outcome.cameras[0].name, Some("A".to_string()));
        assert_eq!(outcome.cameras[0].rtsp_url, "rtsp://192.168.1.10/s");
        assert_eq!(outcome.cameras[1].rtsp_url, "rtsp://192.168.1.20/s");
    }

    proptest! {
        // Feature: simple-rtsp-viewer, Property 1: Camera validation and import
        #[test]
        fn property_validate_and_import(
            raw in proptest::collection::vec(
                (
                    "(rtsp://|http://|)[a-z0-9.:/@_-]{0,30}",
                    proptest::option::of("[a-z]{0,120}"),
                ),
                0..80,
            ),
        ) {
            let entries: Vec<ImportedEntry> = raw
                .iter()
                .map(|(u, n)| ImportedEntry {
                    raw_url: u.clone(),
                    raw_name: n.clone(),
                    raw_substream_url: None,
                })
                .collect();
            let outcome = validate_and_import(&entries);

            // (a) every resulting camera is valid and within the limits
            for c in &outcome.cameras {
                prop_assert!(is_valid_rtsp_url(&c.rtsp_url));
                prop_assert!(c.rtsp_url.chars().count() <= MAX_URL_LEN);
                if let Some(n) = &c.name {
                    prop_assert!(n.chars().count() <= MAX_NAME_LEN);
                }
            }

            // result without duplicate URLs
            let mut result_seen = std::collections::HashSet::new();
            for c in &outcome.cameras {
                prop_assert!(result_seen.insert(c.rtsp_url.clone()));
            }

            // expected sequence: first <=64 unique valid URLs in input order
            let mut seen = std::collections::HashSet::new();
            let mut unique_valid = 0usize;
            let mut expected_urls: Vec<String> = Vec::new();
            for (u, _) in &raw {
                if is_valid_rtsp_url(u) && seen.insert(u.clone()) {
                    unique_valid += 1;
                    if expected_urls.len() < MAX_CAMERAS {
                        expected_urls.push(u.clone());
                    }
                }
            }

            let result_urls: Vec<String> =
                outcome.cameras.iter().map(|c| c.rtsp_url.clone()).collect();
            // (b) relative order preserved + dedupe + (c) 64 camera limit
            prop_assert_eq!(result_urls, expected_urls);
            // (c) `truncated` iff there were more than 64 unique valid entries
            prop_assert_eq!(outcome.truncated, unique_valid > MAX_CAMERAS);
            // (d) rejected = invalid + duplicates = total − unique valid
            prop_assert_eq!(outcome.rejected_count, raw.len() - unique_valid);
        }
    }
}
