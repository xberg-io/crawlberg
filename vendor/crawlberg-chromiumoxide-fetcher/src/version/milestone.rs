use std::{fmt, str::FromStr};

use super::metadata::LatestVersionsPerMilestone;
use super::{Result, VersionError};
use crate::{BrowserHost, BrowserKind, BuildInfo, Runtime};

/// Represents a milestone of a browser (e.g. 113).
#[derive(Clone, Copy, Debug)]
pub struct Milestone(u32);

impl Milestone {
    pub const fn new(milestone: u32) -> Self {
        Self(milestone)
    }

    pub(crate) async fn resolve(&self, kind: BrowserKind, host: &BrowserHost) -> Result<BuildInfo> {
        match kind {
            BrowserKind::Chromium => Err(VersionError::ResolveFailed(anyhow::anyhow!(
                "Milestone not supported for chromium"
            ))),
            BrowserKind::Chrome | BrowserKind::ChromeHeadlessShell => {
                let url = format!(
                    "{host}/chrome-for-testing/latest-versions-per-milestone.json",
                    host = host.metadata
                );
                let latest_versions_per_milestone = Runtime::download_json::<LatestVersionsPerMilestone>(&url)
                    .await
                    .map_err(VersionError::ResolveFailed)?;
                let Some(version) = latest_versions_per_milestone.milestones.get(&self.to_string()) else {
                    return Err(VersionError::InvalidMilestone(self.to_string()));
                };
                Ok(BuildInfo::both(version.version.clone(), version.revision.parse()?))
            }
        }
    }
}

impl fmt::Display for Milestone {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for Milestone {
    type Err = VersionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let milestone = s
            .parse::<u32>()
            .map_err(|_| VersionError::InvalidMilestone(s.to_string()))?;
        Ok(Milestone(milestone))
    }
}

impl TryFrom<String> for Milestone {
    type Error = VersionError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::from_str(&value)
    }
}

impl From<u32> for Milestone {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_milestone_resolve_chrome() {
        let host = BrowserHost::current(BrowserKind::Chrome);
        let build_info = Milestone::new(113).resolve(BrowserKind::Chrome, &host).await.unwrap();
        assert_eq!(build_info.id, "113.0.5672.63");
    }
}
