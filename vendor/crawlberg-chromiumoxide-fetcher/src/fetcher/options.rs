use std::path::PathBuf;

use directories::BaseDirs;

use crate::error::{FetcherError, Result};
use crate::{BrowserHost, BrowserKind, BrowserVersion, Platform, Revision};

const CACHE_NAME: &str = "chromiumoxide";

/// Options for the fetcher
pub struct BrowserFetcherOptions {
    /// The host that will be used for downloading browsers and metadata.
    ///
    /// defaults to something sensible for the given browser kind
    pub(crate) host: BrowserHost,

    /// The path to download browsers to.
    ///
    /// defaults to $HOME/.cache/chromiumoxide
    pub(crate) path: PathBuf,

    /// The platform to download the browser for.
    ///
    /// defaults to the currently used platform
    pub(crate) platform: Platform,

    /// The kind of browser to download.
    ///
    /// defaults to Chromium
    pub(crate) kind: BrowserKind,

    /// The desired browser revision.
    ///
    /// defaults to something sensible for the given browser kind
    pub(crate) version: BrowserVersion,
}

impl BrowserFetcherOptions {
    pub fn builder() -> BrowserFetcherOptionsBuilder {
        BrowserFetcherOptionsBuilder::default()
    }

    #[allow(clippy::should_implement_trait)]
    pub fn default() -> Result<Self> {
        Self::builder().build()
    }
}

#[derive(Default)]
pub struct BrowserFetcherOptionsBuilder {
    host: Option<BrowserHost>,
    path: Option<PathBuf>,
    platform: Option<Platform>,
    kind: Option<BrowserKind>,
    version: Option<BrowserVersion>,
}

impl BrowserFetcherOptionsBuilder {
    #[deprecated(since = "0.8.0", note = "Use with_version instead")]
    pub fn with_revision<T: Into<Revision>>(mut self, revision: T) -> Self {
        self.version = Some(BrowserVersion::Revision(revision.into()));
        self
    }

    pub fn with_host<T: Into<BrowserHost>>(mut self, host: T) -> Self {
        self.host = Some(host.into());
        self
    }

    pub fn with_path<T: Into<PathBuf>>(mut self, path: T) -> Self {
        self.path = Some(path.into());
        self
    }

    pub fn with_platform<T: Into<Platform>>(mut self, platform: T) -> Self {
        self.platform = Some(platform.into());
        self
    }

    pub fn with_kind<T: Into<BrowserKind>>(mut self, kind: T) -> Self {
        self.kind = Some(kind.into());
        self
    }

    pub fn with_version<T: Into<BrowserVersion>>(mut self, version: T) -> Self {
        self.version = Some(version.into());
        self
    }

    pub fn build(self) -> Result<BrowserFetcherOptions> {
        let path = self
            .path
            .or_else(|| {
                BaseDirs::new().map(|dirs| {
                    let mut path = dirs.cache_dir().to_path_buf();
                    path.push(CACHE_NAME);
                    path
                })
            })
            .ok_or(FetcherError::NoPathAvailable)?;

        let platform = self
            .platform
            .or_else(Platform::current)
            .ok_or(FetcherError::UnsupportedOs(
                std::env::consts::OS,
                std::env::consts::ARCH,
            ))?;

        let kind = self.kind.unwrap_or_default();

        let version = self.version.unwrap_or_else(|| BrowserVersion::current(kind));

        let host = self.host.unwrap_or_else(|| BrowserHost::current(kind));

        Ok(BrowserFetcherOptions {
            host,
            path,
            platform,
            kind,
            version,
        })
    }
}
