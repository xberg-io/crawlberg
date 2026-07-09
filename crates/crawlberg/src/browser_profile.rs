//! Persistent browser profiles for preserving cookies and localStorage across sessions.
#![allow(dead_code)]

use std::path::PathBuf;

use crate::error::CrawlError;

/// A named browser profile whose user-data directory persists across crawl sessions.
///
/// Profile names are sanitized to prevent path-traversal attacks. The underlying
/// directory lives under `<data_dir>/crawlberg/profiles/<name>`.
#[derive(Debug, Clone)]
pub struct BrowserProfile {
    /// Sanitized profile name.
    pub name: Box<str>,
    /// Absolute path to the Chrome/Chromium `--user-data-dir`.
    pub user_data_dir: PathBuf,
}

impl BrowserProfile {
    /// Create a new `BrowserProfile` handle (does **not** create the directory on disk).
    ///
    /// # Errors
    ///
    /// Returns `CrawlError::InvalidConfig` if the name is empty, too long, or contains
    /// non-ASCII or forbidden characters.
    /// Returns `CrawlError::Other` if the system data directory cannot be determined.
    pub fn new(name: &str) -> Result<Self, CrawlError> {
        validate_profile_name(name)?;
        let user_data_dir = profiles_base_dir()?.join(name);
        Ok(Self {
            name: Box::from(name),
            user_data_dir,
        })
    }

    /// Returns `true` when the profile directory already exists on disk.
    pub fn exists(&self) -> bool {
        self.user_data_dir.is_dir()
    }

    /// Create the profile directory (and any missing parents).
    ///
    /// On Unix, the directory permissions are set to `0o700` (owner-only access).
    ///
    /// # Errors
    ///
    /// Returns `CrawlError::Other` on I/O failure.
    pub fn create(&self) -> Result<(), CrawlError> {
        std::fs::create_dir_all(&self.user_data_dir)
            .map_err(|e| CrawlError::Other(format!("failed to create profile directory: {e}")))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o700);
            std::fs::set_permissions(&self.user_data_dir, perms)
                .map_err(|e| CrawlError::Other(format!("failed to set profile permissions: {e}")))?;
        }
        Ok(())
    }

    /// Recursively delete the profile directory.
    ///
    /// Refuses to follow symlinks — the profile directory must be a real directory.
    ///
    /// # Errors
    ///
    /// Returns `CrawlError::Other` if the path is a symlink or on I/O failure.
    pub fn delete(&self) -> Result<(), CrawlError> {
        let metadata = std::fs::symlink_metadata(&self.user_data_dir)
            .map_err(|e| CrawlError::Other(format!("failed to read profile metadata: {e}")))?;
        if metadata.is_symlink() {
            return Err(CrawlError::Other(
                "refusing to delete symlinked profile directory".into(),
            ));
        }
        std::fs::remove_dir_all(&self.user_data_dir)
            .map_err(|e| CrawlError::Other(format!("failed to delete profile directory: {e}")))
    }

    /// List every profile name found in the base profiles directory.
    ///
    /// Only directories with valid UTF-8 names are returned.
    ///
    /// # Errors
    ///
    /// Returns `CrawlError::Other` if the base directory cannot be read.
    pub fn list_all() -> Result<Vec<Box<str>>, CrawlError> {
        let base = profiles_base_dir()?;
        list_profiles_in(&base)
    }

    /// Return Chrome/Chromium CLI arguments that point to this profile's data directory.
    pub fn chrome_args(&self) -> Vec<String> {
        vec![format!("--user-data-dir={}", self.user_data_dir.display())]
    }
}

/// Base directory for all browser profiles: `<data_dir>/crawlberg/profiles`.
///
/// # Errors
///
/// Returns `CrawlError::Other` if the system data directory cannot be determined.
fn profiles_base_dir() -> Result<PathBuf, CrawlError> {
    let base = dirs::data_dir().ok_or_else(|| CrawlError::Other("unable to determine data directory".into()))?;
    Ok(base.join("crawlberg").join("profiles"))
}

/// List every profile name found in the given base directory.
///
/// Only directories with valid UTF-8 names are returned, sorted alphabetically.
///
/// # Errors
///
/// Returns `CrawlError::Other` if the directory cannot be read.
pub fn list_profiles_in(base: &std::path::Path) -> Result<Vec<Box<str>>, CrawlError> {
    if !base.is_dir() {
        return Ok(Vec::new());
    }

    let entries =
        std::fs::read_dir(base).map_err(|e| CrawlError::Other(format!("failed to read profiles directory: {e}")))?;

    let mut names: Vec<Box<str>> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| CrawlError::Other(format!("failed to read profile entry: {e}")))?;
        if entry.path().is_dir()
            && let Some(name) = entry.file_name().to_str()
        {
            names.push(Box::from(name));
        }
    }
    names.sort();
    Ok(names)
}

/// Validate that a profile name is safe for use as a directory component.
///
/// # Rules
///
/// - Must not be empty
/// - Must not exceed 255 characters
/// - Must contain only ASCII alphanumeric characters, hyphens, underscores, and dots
fn validate_profile_name(name: &str) -> Result<(), CrawlError> {
    if name.is_empty() {
        return Err(CrawlError::InvalidConfig("profile name must not be empty".into()));
    }
    if name.len() > 255 {
        return Err(CrawlError::InvalidConfig(
            "profile name must not exceed 255 characters".into(),
        ));
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
    {
        return Err(CrawlError::InvalidConfig(
            "profile name must contain only ASCII alphanumeric characters, hyphens, underscores, and dots".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_profile_path_deterministic() {
        let a = BrowserProfile::new("my-profile").unwrap();
        let b = BrowserProfile::new("my-profile").unwrap();
        assert_eq!(a.user_data_dir, b.user_data_dir);
        assert_eq!(&*a.name, "my-profile");
    }

    #[test]
    fn test_profile_name_validation_rejects_forward_slash() {
        let result = BrowserProfile::new("bad/name");
        assert!(result.is_err());
    }

    #[test]
    fn test_profile_name_validation_rejects_backslash() {
        let result = BrowserProfile::new("bad\\name");
        assert!(result.is_err());
    }

    #[test]
    fn test_profile_name_validation_rejects_space() {
        let result = BrowserProfile::new("bad name");
        assert!(result.is_err());
    }

    #[test]
    fn test_profile_name_validation_rejects_empty() {
        let result = BrowserProfile::new("");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("empty"), "error should mention empty: {err}");
    }

    #[test]
    fn test_profile_name_validation_rejects_nul() {
        let result = BrowserProfile::new("bad\0name");
        assert!(result.is_err());
    }

    #[test]
    fn test_profile_name_validation_rejects_too_long() {
        let long_name = "a".repeat(256);
        let result = BrowserProfile::new(&long_name);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("255"), "error should mention limit: {err}");
    }

    #[test]
    fn test_profile_name_validation_rejects_unicode() {
        assert!(BrowserProfile::new("calf\u{00e9}").is_err());
        assert!(BrowserProfile::new("\u{0430}dmin").is_err());
        assert!(BrowserProfile::new("profile\u{200b}name").is_err());
    }

    #[test]
    fn test_profile_name_validation_accepts_valid_names() {
        assert!(BrowserProfile::new("default").is_ok());
        assert!(BrowserProfile::new("my-profile").is_ok());
        assert!(BrowserProfile::new("profile_v2").is_ok());
        assert!(BrowserProfile::new("profile.bak").is_ok());
        assert!(BrowserProfile::new("a").is_ok());
        assert!(BrowserProfile::new(&"x".repeat(255)).is_ok());
    }

    #[test]
    fn test_chrome_args_contains_user_data_dir() {
        let profile = BrowserProfile::new("test-chrome").unwrap();
        let args = profile.chrome_args();
        assert_eq!(args.len(), 1);
        assert!(
            args[0].starts_with("--user-data-dir="),
            "arg should start with flag: {}",
            args[0]
        );
        assert!(
            args[0].contains("test-chrome"),
            "arg should contain profile name: {}",
            args[0]
        );
    }

    #[test]
    fn test_profiles_base_dir_structure() {
        let base = profiles_base_dir().unwrap();
        assert!(
            base.ends_with("crawlberg/profiles") || base.ends_with("crawlberg\\profiles"),
            "base dir should end with crawlberg/profiles: {base:?}"
        );
    }

    #[test]
    fn test_list_profiles_in_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let result = list_profiles_in(dir.path()).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_list_profiles_in_nonexistent_dir() {
        let result = list_profiles_in(std::path::Path::new("/nonexistent/path")).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_list_profiles_in_with_entries() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("alpha")).unwrap();
        std::fs::create_dir(dir.path().join("beta")).unwrap();
        std::fs::write(dir.path().join("not-a-dir"), b"").unwrap();

        let names = list_profiles_in(dir.path()).unwrap();
        assert_eq!(names, vec![Box::from("alpha"), Box::from("beta")]);
    }
}
