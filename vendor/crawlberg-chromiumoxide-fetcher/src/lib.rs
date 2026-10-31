//! A library for downloading and installing chromium and chrome for testing.
//!
//! You can either use the tags or a specific revision.
//! The version you download must be at least as recent as the current CDP revision
//! otherwise chromiumoxide will likely not be able to communicate with it.
//!
//! We provide good defaults for the most common use cases.
//!
//! # Example
//! ```no_run
//! use chromiumoxide_fetcher::{BrowserFetcher, BrowserFetcherOptions};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let fetcher = BrowserFetcher::new(BrowserFetcherOptions::default()?);
//!     let revision_info = fetcher.fetch().await?;
//!     Ok(())
//! }
//! ```

pub use self::build_info::BuildInfo;
pub use self::error::FetcherError;
pub use self::fetcher::{BrowserFetcher, BrowserFetcherInstallation, BrowserFetcherOptions};
pub use self::host::BrowserHost;
pub use self::kind::BrowserKind;
pub use self::platform::Platform;
use self::runtime::Runtime;
pub use self::version::{BrowserVersion, Channel, Revision, Version, VersionError};

mod build_info;
mod error;
mod fetcher;
mod host;
mod kind;
mod platform;
mod runtime;
mod version;
