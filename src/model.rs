//! Domain types shared by the other modules.

/// A camera loaded from the Import_File.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Camera {
    /// Stable camera id, which does not change when the address or the login
    /// change. The UI keeps each camera's state by it.
    #[serde(default)]
    pub id: String,
    pub rtsp_url: String,     // up to 2048 characters, validated on import
    pub name: Option<String>, // up to 100 characters, validated on import
    /// The camera's secondary stream (lower resolution), shown in the grid. The
    /// main `rtsp_url` is only opened when the camera is enlarged, or always if
    /// there is no substream. Validated on import like `rtsp_url`.
    #[serde(default)]
    pub substream_url: Option<String>,
}

impl Camera {
    /// URL to open to show the camera: the main one when it is enlarged (or
    /// when there is no substream), the substream in the grid.
    pub fn stream_url(&self, enlarged: bool) -> &str {
        match (&self.substream_url, enlarged) {
            (Some(substream), false) => substream,
            _ => &self.rtsp_url,
        }
    }
}

/// Maximum size, in physical pixels, at which the UI shows a camera's video.
/// The StreamTask scales each frame down to fit it, keeping the aspect ratio,
/// before converting it to RGBA, so it does not convert and send pixels the
/// screen would not show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplaySize {
    pub width: u32,
    pub height: u32,
}

/// Connection status of a camera, used by the UI to pick the visual indicator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionStatus {
    Connected,
    Disconnected,
    Reconnecting,
}

/// Decoded video frame, already at display size and in egui's image format
/// (RGBA, alpha 255). The pixel converter writes straight into this image's
/// buffer, which becomes a texture without any copy or conversion on the UI
/// thread.
pub type CameraFrame = egui::ColorImage;

/// Removes the embedded credentials (`user:password@`) from an RTSP URL,
/// replacing them with `***`, for safe use in logs and messages.
///
/// An RTSP_URL may contain embedded basic authentication
/// (`rtsp://user:password@host/path`); logging the raw URL would leak the
/// password in plain text. This function keeps the host and path (useful for
/// diagnostics) and hides only the `userinfo`. URLs without credentials are
/// returned unchanged.
pub fn redact_rtsp_credentials(url: &str) -> String {
    let Some(scheme_end) = url.find("://") else {
        return url.to_string();
    };
    let authority_start = scheme_end + 3;

    // The `authority` runs up to the first '/' after the scheme (or the end of
    // the string).
    let authority_end = url[authority_start..]
        .find('/')
        .map(|idx| authority_start + idx)
        .unwrap_or(url.len());
    let authority = &url[authority_start..authority_end];

    match authority.rfind('@') {
        Some(at_idx) => {
            let host_port = &authority[at_idx + 1..];
            format!(
                "{}***@{}{}",
                &url[..authority_start],
                host_port,
                &url[authority_end..]
            )
        }
        None => url.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_userinfo_preserving_host_and_path() {
        assert_eq!(
            redact_rtsp_credentials("rtsp://user:pass@192.168.1.10:554/stream1"),
            "rtsp://***@192.168.1.10:554/stream1"
        );
    }

    #[test]
    fn leaves_url_without_credentials_unchanged() {
        assert_eq!(
            redact_rtsp_credentials("rtsp://192.168.1.20/stream1"),
            "rtsp://192.168.1.20/stream1"
        );
    }

    #[test]
    fn at_sign_in_path_is_not_treated_as_credentials() {
        assert_eq!(
            redact_rtsp_credentials("rtsp://host/path@x"),
            "rtsp://host/path@x"
        );
    }

    #[test]
    fn redacts_password_containing_at_sign() {
        assert_eq!(
            redact_rtsp_credentials("rtsp://user:p@ss@host/s"),
            "rtsp://***@host/s"
        );
    }

    #[test]
    fn non_url_string_is_unchanged() {
        assert_eq!(redact_rtsp_credentials("not a url"), "not a url");
    }

    #[test]
    fn stream_url_uses_substream_in_grid_and_main_when_enlarged() {
        let camera = Camera {
            id: "a".to_string(),
            rtsp_url: "rtsp://host/main".to_string(),
            name: None,
            substream_url: Some("rtsp://host/sub".to_string()),
        };
        assert_eq!(camera.stream_url(false), "rtsp://host/sub");
        assert_eq!(camera.stream_url(true), "rtsp://host/main");
    }

    #[test]
    fn stream_url_without_substream_is_always_main() {
        let camera = Camera {
            id: "b".to_string(),
            rtsp_url: "rtsp://host/main".to_string(),
            name: None,
            substream_url: None,
        };
        assert_eq!(camera.stream_url(false), "rtsp://host/main");
        assert_eq!(camera.stream_url(true), "rtsp://host/main");
    }
}
