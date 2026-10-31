use std::path::{Path, PathBuf};

use crate::{BrowserHost, BuildInfo, Platform, Revision};

/// The kind of browser to download.
#[derive(Default, Clone, Copy, Debug)]
pub enum BrowserKind {
    #[default]
    Chromium,
    Chrome,
    ChromeHeadlessShell,
}

impl BrowserKind {
    #[doc(hidden)] // internal API
    pub fn download_url(&self, platform: Platform, build_info: &BuildInfo, host: &BrowserHost) -> String {
        let folder = self.folder(platform);
        let archive = self.archive(platform, build_info.revision);
        match self {
            Self::Chromium => {
                format!(
                    "{host}/chromium-browser-snapshots/{folder}/{build_id}/{archive}.zip",
                    host = host.object,
                    folder = folder,
                    build_id = build_info.id,
                )
            }
            Self::Chrome => {
                format!(
                    "{host}/chrome-for-testing-public/{build_id}/{folder}/{archive}.zip",
                    host = host.object,
                    build_id = build_info.id,
                    folder = folder,
                    archive = archive,
                )
            }
            Self::ChromeHeadlessShell => {
                format!(
                    "{host}/chrome-for-testing-public/{build_id}/{folder}/{archive}.zip",
                    host = host.object,
                    build_id = build_info.id,
                    folder = folder,
                    archive = archive,
                )
            }
        }
    }

    pub(crate) fn executable(&self, platform: Platform, build_info: &BuildInfo, folder_path: &Path) -> PathBuf {
        let mut path = folder_path.to_path_buf();
        path.push(self.archive(platform, build_info.revision));
        match self {
            Self::Chromium => match platform {
                Platform::Linux => path.push("chrome"),
                Platform::Mac | Platform::MacArm => {
                    path.push("Chromium.app");
                    path.push("Contents");
                    path.push("MacOS");
                    path.push("Chromium")
                }
                Platform::Win32 | Platform::Win64 => path.push("chrome.exe"),
            },
            Self::Chrome => match platform {
                Platform::Linux => path.push("chrome"),
                Platform::Mac | Platform::MacArm => {
                    path.push("Google Chrome for Testing.app");
                    path.push("Contents");
                    path.push("MacOS");
                    path.push("Google Chrome for Testing")
                }
                Platform::Win32 | Platform::Win64 => path.push("chrome.exe"),
            },
            Self::ChromeHeadlessShell => match platform {
                Platform::Linux => path.push("chrome-headless-shell"),
                Platform::Mac | Platform::MacArm => path.push("chrome-headless-shell"),
                Platform::Win32 | Platform::Win64 => path.push("chrome-headless-shell.exe"),
            },
        }
        path
    }

    fn archive(&self, platform: Platform, revision: Option<Revision>) -> &'static str {
        const CHROMIUM_REVISION_WIN32: Revision = Revision::new(591_479);
        match self {
            Self::Chromium => match platform {
                Platform::Linux => "chrome-linux",
                Platform::Mac | Platform::MacArm => "chrome-mac",
                Platform::Win32 | Platform::Win64 => {
                    if let Some(revision) = revision {
                        if revision > CHROMIUM_REVISION_WIN32 {
                            "chrome-win"
                        } else {
                            "chrome-win32"
                        }
                    } else {
                        "chrome-win"
                    }
                }
            },
            Self::Chrome => match platform {
                Platform::Linux => "chrome-linux64",
                Platform::Mac => "chrome-mac-x64",
                Platform::MacArm => "chrome-mac-arm64",
                Platform::Win32 => "chrome-win32",
                Platform::Win64 => "chrome-win64",
            },
            Self::ChromeHeadlessShell => match platform {
                Platform::Linux => "chrome-headless-shell-linux64",
                Platform::Mac => "chrome-headless-shell-mac-x64",
                Platform::MacArm => "chrome-headless-shell-mac-arm64",
                Platform::Win32 => "chrome-headless-shell-win32",
                Platform::Win64 => "chrome-headless-shell-win64",
            },
        }
    }

    pub fn folder(&self, platform: Platform) -> &'static str {
        match self {
            Self::Chromium => match platform {
                Platform::Linux => "Linux_x64",
                Platform::Mac => "Mac",
                Platform::MacArm => "Mac_Arm",
                Platform::Win32 => "Win",
                Platform::Win64 => "Win_x64",
            },
            Self::Chrome | Self::ChromeHeadlessShell => match platform {
                Platform::Linux => "linux64",
                Platform::Mac => "mac-x64",
                Platform::MacArm => "mac-arm64",
                Platform::Win32 => "win32",
                Platform::Win64 => "win64",
            },
        }
    }
}
