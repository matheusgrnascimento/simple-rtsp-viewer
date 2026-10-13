//! Building and parsing RTSP URLs, with the login kept apart from the address.
//!
//! The app stores cameras without passwords (address, port and paths) and
//! builds the full URL only when opening the stream, with the login taken from
//! the Windows vault.

use crate::credentials::Credentials;

/// Default RTSP port.
pub const DEFAULT_PORT: u16 = 554;

/// Parts of an `rtsp://[user:password@]host[:port]/path` URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtspUrl {
    pub credentials: Option<Credentials>,
    pub host: String,
    pub port: u16,
    /// Path, with the query if any, always starting with `/`.
    pub path: String,
}

/// Parses an RTSP URL. Follows the same rules as the import validation: the
/// login ends at the last `@` before the first `/`.
pub fn parse(url: &str) -> Option<RtspUrl> {
    let rest = url.strip_prefix("rtsp://")?;
    let (authority, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };
    let (userinfo, host_port) = match authority.rfind('@') {
        Some(index) => (Some(&authority[..index]), &authority[index + 1..]),
        None => (None, authority),
    };
    let credentials = userinfo.map(|info| {
        let (username, password) = info.split_once(':').unwrap_or((info, ""));
        Credentials {
            username: username.to_owned(),
            password: password.to_owned(),
        }
    });
    let (host, port) = match host_port.rsplit_once(':') {
        Some((host, port)) => (host, port.parse().ok()?),
        None => (host_port, DEFAULT_PORT),
    };
    if host.is_empty() {
        return None;
    }
    Some(RtspUrl {
        credentials,
        host: host.to_owned(),
        port,
        path: normalize_path(path),
    })
}

/// Builds the RTSP URL with the given login (or without a login, with `None`).
pub fn build(host: &str, port: u16, path: &str, credentials: Option<&Credentials>) -> String {
    let login = credentials
        .map(|credentials| format!("{}:{}@", credentials.username, credentials.password))
        .unwrap_or_default();
    format!("rtsp://{login}{host}:{port}{}", normalize_path(path))
}

/// Path without surrounding spaces and always starting with `/`.
pub fn normalize_path(path: &str) -> String {
    let path = path.trim();
    if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("/{path}")
    }
}

/// Host accepted by the app: letters, digits, `.` and `-` (names and IPv4).
pub fn is_valid_host(host: &str) -> bool {
    !host.is_empty()
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn login(username: &str, password: &str) -> Credentials {
        Credentials {
            username: username.to_owned(),
            password: password.to_owned(),
        }
    }

    #[test]
    fn parses_url_with_login_port_and_path() {
        let parsed = parse("rtsp://admin:secret@192.168.1.10:8554/stream1").unwrap();
        assert_eq!(parsed.credentials, Some(login("admin", "secret")));
        assert_eq!(parsed.host, "192.168.1.10");
        assert_eq!(parsed.port, 8554);
        assert_eq!(parsed.path, "/stream1");
    }

    #[test]
    fn parses_url_without_login_using_default_port_and_keeps_query() {
        let parsed = parse("rtsp://camera.local/cam/realmonitor?channel=1&subtype=0").unwrap();
        assert_eq!(parsed.credentials, None);
        assert_eq!(parsed.host, "camera.local");
        assert_eq!(parsed.port, DEFAULT_PORT);
        assert_eq!(parsed.path, "/cam/realmonitor?channel=1&subtype=0");
    }

    #[test]
    fn password_may_contain_at_sign() {
        let parsed = parse("rtsp://admin:p@ss@host/s").unwrap();
        assert_eq!(parsed.credentials, Some(login("admin", "p@ss")));
        assert_eq!(parsed.host, "host");
    }

    #[test]
    fn rejects_non_rtsp_and_bad_port() {
        assert_eq!(parse("http://host/s"), None);
        assert_eq!(parse("rtsp://host:port/s"), None);
        assert_eq!(parse("rtsp://:554/s"), None);
    }

    #[test]
    fn builds_url_with_and_without_login() {
        assert_eq!(
            build(
                "192.168.1.10",
                554,
                "stream1",
                Some(&login("admin", "secret"))
            ),
            "rtsp://admin:secret@192.168.1.10:554/stream1"
        );
        assert_eq!(
            build("192.168.1.10", 8554, "/stream2", None),
            "rtsp://192.168.1.10:8554/stream2"
        );
    }

    #[test]
    fn build_then_parse_roundtrips() {
        let url = build("cam-1.local", 554, "/stream1", Some(&login("u", "p")));
        let parsed = parse(&url).unwrap();
        assert_eq!(parsed.host, "cam-1.local");
        assert_eq!(parsed.port, 554);
        assert_eq!(parsed.path, "/stream1");
        assert_eq!(parsed.credentials, Some(login("u", "p")));
    }

    #[test]
    fn host_validation() {
        assert!(is_valid_host("192.168.50.122"));
        assert!(is_valid_host("garage-cam.local"));
        assert!(!is_valid_host(""));
        assert!(!is_valid_host("host with space"));
        assert!(!is_valid_host("user@host"));
    }
}
