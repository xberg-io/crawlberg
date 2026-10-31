use crate::BrowserKind;

/// Host for downloading browsers and metadata.
pub struct BrowserHost {
    pub(crate) object: String,
    pub(crate) metadata: String,
}

impl BrowserHost {
    pub fn new(object: &str, metadata: &str) -> Self {
        Self {
            object: object.to_string(),
            metadata: metadata.to_string(),
        }
    }

    pub fn single(host: &str) -> Self {
        Self {
            object: host.to_string(),
            metadata: host.to_string(),
        }
    }

    #[doc(hidden)] // internal API
    pub fn current(kind: BrowserKind) -> Self {
        match kind {
            BrowserKind::Chromium => Self {
                object: "https://storage.googleapis.com".to_string(),
                metadata: "https://storage.googleapis.com".to_string(),
            },
            BrowserKind::Chrome | BrowserKind::ChromeHeadlessShell => Self {
                object: "https://storage.googleapis.com".to_string(),
                metadata: "https://googlechromelabs.github.io/".to_string(),
            },
        }
    }
}
