use std::fmt;
use std::path::PathBuf;

use crate::BuildInfo;

/// Details of an installed version of chromium
#[derive(Clone, Debug)]
pub struct BrowserFetcherInstallation {
    pub folder_path: PathBuf,
    pub executable_path: PathBuf,
    pub build_info: BuildInfo,
}

impl fmt::Display for BrowserFetcherInstallation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ID: {}", self.build_info.id)?;
        if let Some(revision) = &self.build_info.revision {
            write!(f, ", Revision: {}", revision)?;
        }
        if let Some(version) = &self.build_info.version {
            write!(f, ", Version: {}", version)?;
        }
        write!(f, ", Path: {}", self.executable_path.display())?;
        Ok(())
    }
}
