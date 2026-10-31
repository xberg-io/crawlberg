use std::{fmt, str::FromStr};

use super::metadata::LastKnownGoodVersions;
use super::{Result, VersionError};
use crate::{BrowserHost, BrowserKind, BuildInfo, Platform, Runtime};

/// The channel of the browser to download.
///
/// For Chrome, you can check the corresponding version of the [chrome-for-testing](https://googlechromelabs.github.io/chrome-for-testing/).
/// For Chromium, only the `Canary` channel is supported.
#[derive(Clone, Copy, Debug)]
pub enum Channel {
    /// The canary version of the browser.
    Canary,
    /// The dev version of the browser.
    Dev,
    /// The beta version of the browser.
    Beta,
    /// The stable version of the browser.
    Stable,
}

impl Channel {
    fn as_key(&self) -> &str {
        match self {
            Self::Canary => "Canary",
            Self::Dev => "Dev",
            Self::Beta => "Beta",
            Self::Stable => "Stable",
        }
    }

    pub(crate) async fn resolve(&self, kind: BrowserKind, platform: Platform, host: &BrowserHost) -> Result<BuildInfo> {
        match kind {
            BrowserKind::Chromium => match self {
                Channel::Canary => {
                    let url = format!(
                        "{host}/chromium-browser-snapshots/{folder}/LAST_CHANGE",
                        host = host.metadata,
                        folder = kind.folder(platform)
                    );
                    let last_change = Runtime::download_text(&url)
                        .await
                        .map_err(VersionError::ResolveFailed)?;
                    Ok(BuildInfo::revision(last_change.parse()?))
                }
                _ => Err(VersionError::ResolveFailed(anyhow::anyhow!(
                    "Invalid channel for chromium"
                ))),
            },
            BrowserKind::Chrome | BrowserKind::ChromeHeadlessShell => {
                let url = format!(
                    "{host}/chrome-for-testing/last-known-good-versions.json",
                    host = host.metadata
                );
                let last_known_good_versions = Runtime::download_json::<LastKnownGoodVersions>(&url)
                    .await
                    .map_err(VersionError::ResolveFailed)?;
                let Some(version) = last_known_good_versions.channels.get(self.as_key()) else {
                    return Err(VersionError::InvalidChannel(self.to_string()));
                };
                Ok(BuildInfo::both(version.version.clone(), version.revision.parse()?))
            }
        }
    }
}

impl fmt::Display for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Canary => write!(f, "canary"),
            Self::Dev => write!(f, "dev"),
            Self::Beta => write!(f, "beta"),
            Self::Stable => write!(f, "stable"),
        }
    }
}

impl FromStr for Channel {
    type Err = VersionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "canary" => Ok(Self::Canary),
            "Canary" => Ok(Self::Canary),
            "dev" => Ok(Self::Dev),
            "Dev" => Ok(Self::Dev),
            "beta" => Ok(Self::Beta),
            "Beta" => Ok(Self::Beta),
            "stable" => Ok(Self::Stable),
            "Stable" => Ok(Self::Stable),
            _ => Err(VersionError::InvalidChannel(s.to_string())),
        }
    }
}

impl TryFrom<String> for Channel {
    type Error = VersionError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::from_str(&value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_channel_resolve_chrome() {
        let host = BrowserHost::current(BrowserKind::Chrome);
        let build_info = Channel::Stable
            .resolve(BrowserKind::Chrome, Platform::Linux, &host)
            .await;
        assert!(build_info.is_ok());
    }

    #[tokio::test]
    async fn test_channel_resolve_chromium() {
        let host = BrowserHost::current(BrowserKind::Chromium);
        let build_info = Channel::Canary
            .resolve(BrowserKind::Chromium, Platform::Linux, &host)
            .await;
        assert!(build_info.is_ok());
    }
}
