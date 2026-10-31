use std::{fmt, str::FromStr};

use super::metadata::LatestPatchVersionsPerBuild;
use super::{Result, VersionError};
use crate::{BrowserHost, BrowserKind, BuildInfo, Runtime};

/// Represents a version of a browser (e.g. 113.0.5672).
/// The patch
#[derive(Clone, Copy, Debug, PartialOrd, Ord, PartialEq, Eq)]
pub struct Version {
    major: u32,
    minor: u32,
    build: u32,
    patch: Option<u32>,
}

impl Version {
    pub const fn new(major: u32, minor: u32, build: u32) -> Self {
        Self {
            major,
            minor,
            build,
            patch: None,
        }
    }

    pub const fn exact(major: u32, minor: u32, build: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            build,
            patch: Some(patch),
        }
    }

    pub(crate) async fn resolve(&self, kind: BrowserKind, host: &BrowserHost) -> Result<BuildInfo> {
        match kind {
            BrowserKind::Chromium => Err(VersionError::ResolveFailed(anyhow::anyhow!(
                "Build not supported for chromium"
            ))),
            BrowserKind::Chrome | BrowserKind::ChromeHeadlessShell => {
                if self.patch.is_some() {
                    return Ok(BuildInfo::version(self.to_string()));
                }

                let url = format!(
                    "{host}/chrome-for-testing/latest-patch-versions-per-build.json",
                    host = host.metadata
                );
                let latest_patch_versions_per_build = Runtime::download_json::<LatestPatchVersionsPerBuild>(&url)
                    .await
                    .map_err(VersionError::ResolveFailed)?;
                let Some(version) = latest_patch_versions_per_build.builds.get(&self.to_string()) else {
                    return Err(VersionError::InvalidBuild(self.to_string()));
                };
                Ok(BuildInfo::both(version.version.clone(), version.revision.parse()?))
            }
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.build)?;
        if let Some(patch) = self.patch {
            write!(f, ".{}", patch)?;
        }
        Ok(())
    }
}

impl FromStr for Version {
    type Err = VersionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts = s.split('.').collect::<Vec<&str>>();

        if parts.len() < 3 || parts.len() > 4 {
            return Err(VersionError::InvalidBuild(s.to_string()));
        }

        Ok(Version {
            major: parts[0]
                .parse()
                .map_err(|_| VersionError::InvalidBuild(s.to_string()))?,
            minor: parts[1]
                .parse()
                .map_err(|_| VersionError::InvalidBuild(s.to_string()))?,
            build: parts[2]
                .parse()
                .map_err(|_| VersionError::InvalidBuild(s.to_string()))?,
            patch: if parts.len() == 4 {
                Some(
                    parts[3]
                        .parse()
                        .map_err(|_| VersionError::InvalidBuild(s.to_string()))?,
                )
            } else {
                None
            },
        })
    }
}

impl TryFrom<String> for Version {
    type Error = VersionError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::from_str(&value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_version_resolve_chrome() {
        let host = BrowserHost::current(BrowserKind::Chrome);
        let build_info = Version::new(113, 0, 5672)
            .resolve(BrowserKind::Chrome, &host)
            .await
            .unwrap();
        assert_eq!(build_info.id, "113.0.5672.63");
    }

    #[tokio::test]
    async fn test_version_resolve_chrome_patch() {
        let host = BrowserHost::current(BrowserKind::Chrome);
        let build_info = Version::exact(113, 0, 5672, 62)
            .resolve(BrowserKind::Chrome, &host)
            .await
            .unwrap();
        assert_eq!(build_info.id, "113.0.5672.62");
    }
}
