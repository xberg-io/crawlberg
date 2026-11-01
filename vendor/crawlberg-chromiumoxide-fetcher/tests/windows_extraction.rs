#![cfg(windows)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use tempfile::TempDir;
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

#[path = "../src/runtime/mod.rs"]
#[allow(dead_code, reason = "This test-side module import also includes unused network helpers.")]
mod runtime;

struct Fixture(TempDir);

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("create fixture");
        fs::create_dir(directory.path().join("outside")).expect("create outside destination");
        fs::write(directory.path().join("outside/sentinel"), b"unchanged").expect("write sentinel");
        Self(directory)
    }

    fn archive(&self, entries: &[(&str, &[u8], bool)]) -> PathBuf {
        let path = self.0.path().join("chromium.zip");
        let mut archive = ZipWriter::new(fs::File::create(&path).expect("create real ZIP"));
        for (name, contents, symlink) in entries {
            let options = SimpleFileOptions::default();
            if *symlink {
                archive
                    .add_symlink(*name, std::str::from_utf8(contents).expect("UTF-8 target"), options)
                    .expect("add symlink");
            } else {
                archive.start_file(*name, options).expect("add entry");
                archive.write_all(contents).expect("write entry");
            }
        }
        archive.finish().expect("finish archive");
        path
    }

    fn destination(&self) -> PathBuf {
        self.0.path().join("install")
    }

    fn assert_outside_unchanged(&self) {
        assert_eq!(
            fs::read(self.0.path().join("outside/sentinel")).expect("read sentinel"),
            b"unchanged"
        );
        assert!(!self.0.path().join("outside/payload").exists());
    }
}

async fn extract(archive: PathBuf, destination: &Path) -> anyhow::Result<()> {
    runtime::Runtime::unzip(archive, destination.to_path_buf()).await
}

#[tokio::test]
async fn preserves_windows_executable_and_in_root_file_symlink() {
    let fixture = Fixture::new();
    let archive = fixture.archive(&[
        ("chrome.exe", b"chromium", false),
        ("chrome-link.exe", b"chrome.exe", true),
    ]);
    extract(archive, &fixture.destination())
        .await
        .expect("extract Windows layout");
    assert_eq!(
        fs::read(fixture.destination().join("chrome-link.exe")).expect("follow in-root symlink"),
        b"chromium"
    );
    fixture.assert_outside_unchanged();
}

#[tokio::test]
async fn rejects_escaping_directory_link_before_a_file() {
    let fixture = Fixture::new();
    let archive = fixture.archive(&[("pivot", b"../outside", true), ("pivot/payload", b"escape", false)]);
    let result = extract(archive, &fixture.destination()).await;
    fixture.assert_outside_unchanged();
    assert!(result.is_err());
}

#[tokio::test]
async fn rejects_escaping_file_symlink_even_as_last_entry() {
    let fixture = Fixture::new();
    let archive = fixture.archive(&[
        ("chrome.exe", b"chromium", false),
        ("pivot", b"../outside/sentinel", true),
    ]);
    let result = extract(archive, &fixture.destination()).await;
    fixture.assert_outside_unchanged();
    assert!(result.is_err());
}

#[tokio::test]
async fn rejects_file_through_a_preexisting_windows_directory_symlink() {
    let fixture = Fixture::new();
    let destination = fixture.destination();
    fs::create_dir(&destination).expect("create destination");
    std::os::windows::fs::symlink_dir("../outside", destination.join("pivot")).expect("create real directory symlink");
    let archive = fixture.archive(&[("pivot/sentinel", b"overwritten", false)]);
    let result = extract(archive, &destination).await;
    fixture.assert_outside_unchanged();
    assert!(result.is_err());
}

#[tokio::test]
async fn rejects_backslash_parent_traversal() {
    let fixture = Fixture::new();
    let archive = fixture.archive(&[("..\\outside\\sentinel", b"overwritten", false)]);
    let result = extract(archive, &fixture.destination()).await;
    fixture.assert_outside_unchanged();
    assert!(result.is_err());
}
