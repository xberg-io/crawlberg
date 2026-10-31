use std::fs;
use std::io::Write;
use std::path::PathBuf;

use tempfile::TempDir;
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

use super::do_unzip;

struct Fixture(TempDir);

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("create owned fixture directory");
        let path = directory.path();
        fs::create_dir(path.join("outside")).expect("create outside destination");
        fs::write(path.join("outside/sentinel"), b"unchanged").expect("create sentinel");
        Self(directory)
    }

    fn archive(&self, entries: &[(&str, &[u8], bool)]) -> PathBuf {
        let path = self.0.path().join("chromium.zip");
        let mut writer = ZipWriter::new(fs::File::create(&path).expect("create archive"));
        for (name, contents, symlink) in entries {
            let options = SimpleFileOptions::default().unix_permissions(0o755);
            if *symlink {
                writer
                    .add_symlink(
                        *name,
                        std::str::from_utf8(contents).expect("UTF-8 link target"),
                        options,
                    )
                    .expect("add symlink");
            } else {
                writer.start_file(*name, options).expect("add regular entry");
                writer.write_all(contents).expect("write entry contents");
            }
        }
        writer.finish().expect("finish real ZIP archive");
        path
    }

    fn assert_outside_unchanged(&self) {
        assert_eq!(
            fs::read(self.0.path().join("outside/sentinel")).expect("read sentinel"),
            b"unchanged"
        );
        assert!(!self.0.path().join("outside/payload").exists());
    }
}

#[test]
fn preserves_chromium_executable_and_in_root_framework_symlink() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new();
    let archive = fixture.archive(&[
        ("Chrome.app/Contents/Versions/current/framework", b"chromium", false),
        ("Chrome.app/Contents/Framework", b"Versions/current/framework", true),
    ]);
    let destination = fixture.0.path().join("install");
    do_unzip(&archive, &destination).expect("extract valid Chromium layout");
    let executable = destination.join("Chrome.app/Contents/Framework");
    assert_eq!(fs::read(&executable).expect("follow in-root symlink"), b"chromium");
    assert_ne!(
        fs::metadata(&executable)
            .expect("executable metadata")
            .permissions()
            .mode()
            & 0o111,
        0
    );
    fixture.assert_outside_unchanged();
}

#[test]
fn rejects_symlink_before_file_escape() {
    let fixture = Fixture::new();
    let archive = fixture.archive(&[("pivot", b"../outside", true), ("pivot/payload", b"escape", false)]);
    let result = do_unzip(&archive, &fixture.0.path().join("install"));
    fixture.assert_outside_unchanged();
    assert!(result.is_err());
}

#[test]
fn rejects_escaping_symlink_even_as_last_entry() {
    let fixture = Fixture::new();
    let archive = fixture.archive(&[("chrome", b"chromium", false), ("pivot", b"../outside", true)]);
    let result = do_unzip(&archive, &fixture.0.path().join("install"));
    fixture.assert_outside_unchanged();
    assert!(result.is_err());
}

#[test]
fn rejects_file_through_preexisting_escaping_symlink() {
    let fixture = Fixture::new();
    let destination = fixture.0.path().join("install");
    fs::create_dir(&destination).expect("create destination");
    std::os::unix::fs::symlink("../outside", destination.join("pivot")).expect("create preexisting symlink");
    let archive = fixture.archive(&[("pivot/sentinel", b"overwritten", false)]);
    let result = do_unzip(&archive, &destination);
    fixture.assert_outside_unchanged();
    assert!(result.is_err());
}

#[test]
fn rejects_absolute_symlink_target() {
    let fixture = Fixture::new();
    let target = fixture.0.path().join("outside");
    let archive = fixture.archive(&[("pivot", target.to_str().expect("UTF-8 fixture path").as_bytes(), true)]);
    let result = do_unzip(&archive, &fixture.0.path().join("install"));
    fixture.assert_outside_unchanged();
    assert!(result.is_err());
}
