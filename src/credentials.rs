//! Camera logins, kept in the Windows Credential Manager.
//!
//! There is a default login, used by most cameras, and per-camera logins for
//! the cameras that need one. Each one is stored in a generic Windows
//! credential (service "Simple RTSP Viewer"), with the username and password as
//! JSON.

use serde::{Deserialize, Serialize};

/// Service name of the credentials in the Credential Manager.
const SERVICE: &str = "Simple RTSP Viewer";

/// Credential of the cameras' default login.
const SHARED_ENTRY: &str = "default-login";

/// Username and password of a camera.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

/// Never shows the password in logs or error messages.
impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("username", &self.username)
            .field("password", &"***")
            .finish()
    }
}

impl Credentials {
    /// Checks whether the login can go inside an RTSP URL: `/`, `?` and `#`
    /// would end the login part of the URL, spaces would break it, and a `:` in
    /// the username would be read as the start of the password.
    pub fn validate(&self) -> Result<(), String> {
        if self.username.trim().is_empty() {
            return Err("Enter the username.".to_owned());
        }
        if self.username.contains(':') {
            return Err("The username cannot contain \":\".".to_owned());
        }
        let breaks_url = |text: &str| {
            text.chars()
                .any(|c| matches!(c, '/' | '?' | '#') || c.is_whitespace())
        };
        if breaks_url(&self.username) || breaks_url(&self.password) {
            return Err(
                "Username and password cannot contain spaces or the characters / ? #.".to_owned(),
            );
        }
        Ok(())
    }
}

/// Stored default login, if any.
pub fn load_shared() -> Result<Option<Credentials>, String> {
    read(SHARED_ENTRY)
}

pub fn save_shared(credentials: &Credentials) -> Result<(), String> {
    write(SHARED_ENTRY, credentials)
}

/// Per-camera login, if any.
pub fn load_for_camera(camera_id: &str) -> Result<Option<Credentials>, String> {
    read(&camera_entry(camera_id))
}

pub fn save_for_camera(camera_id: &str, credentials: &Credentials) -> Result<(), String> {
    write(&camera_entry(camera_id), credentials)
}

/// Deletes a camera's own login (no error if it does not exist).
pub fn delete_for_camera(camera_id: &str) -> Result<(), String> {
    let entry = keyring::Entry::new(SERVICE, &camera_entry(camera_id)).map_err(describe)?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(err) => Err(describe(err)),
    }
}

fn camera_entry(camera_id: &str) -> String {
    format!("camera-{camera_id}")
}

fn read(entry_name: &str) -> Result<Option<Credentials>, String> {
    let entry = keyring::Entry::new(SERVICE, entry_name).map_err(describe)?;
    match entry.get_password() {
        Ok(json) => serde_json::from_str(&json)
            .map(Some)
            .map_err(|err| format!("unreadable credential \"{entry_name}\": {err}")),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(err) => Err(describe(err)),
    }
}

fn write(entry_name: &str, credentials: &Credentials) -> Result<(), String> {
    let entry = keyring::Entry::new(SERVICE, entry_name).map_err(describe)?;
    let json = serde_json::to_string(credentials).map_err(|err| err.to_string())?;
    entry.set_password(&json).map_err(describe)
}

fn describe(err: keyring::Error) -> String {
    format!("Windows Credential Manager: {err}")
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
    fn debug_hides_the_password() {
        let text = format!("{:?}", login("admin", "secret"));
        assert!(text.contains("admin"));
        assert!(!text.contains("secret"));
    }

    #[test]
    fn accepts_regular_login_including_at_sign_in_password() {
        assert!(login("camuser", "p4ss@-strong!").validate().is_ok());
    }

    #[test]
    fn rejects_logins_that_would_break_the_url() {
        assert!(login("", "x").validate().is_err());
        assert!(login("user:name", "x").validate().is_err());
        assert!(login("user", "a/b").validate().is_err());
        assert!(login("user", "a?b").validate().is_err());
        assert!(login("user", "a#b").validate().is_err());
        assert!(login("user", "a b").validate().is_err());
    }
}
