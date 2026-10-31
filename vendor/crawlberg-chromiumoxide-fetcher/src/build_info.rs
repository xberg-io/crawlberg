use std::fmt;

use crate::Revision;

/// Information about a build of a browser.
#[derive(Debug, Clone)]
pub struct BuildInfo {
    /// The revision of the browser.
    pub revision: Option<Revision>,
    /// The version of the browser.
    pub version: Option<String>,
    /// The ID will uniquely identify the build, it will be either the revision or the version.
    pub id: String,
}

impl fmt::Display for BuildInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ID: {}", self.id)?;
        if let Some(revision) = &self.revision {
            write!(f, ", Revision: {}", revision)?;
        }
        if let Some(version) = &self.version {
            write!(f, ", Version: {}", version)?;
        }
        Ok(())
    }
}

impl BuildInfo {
    #[doc(hidden)] // internal API
    pub fn revision(revision: Revision) -> Self {
        Self {
            revision: Some(revision),
            version: None,
            id: revision.to_string(),
        }
    }

    #[doc(hidden)] // internal API
    pub fn version(version: String) -> Self {
        Self {
            revision: None,
            version: Some(version.clone()),
            id: version,
        }
    }

    #[doc(hidden)] // internal API
    pub fn both(version: String, revision: Revision) -> Self {
        Self {
            revision: Some(revision),
            version: Some(version.clone()),
            id: version,
        }
    }
}
