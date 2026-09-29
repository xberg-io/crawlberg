//! Launching a managed one-shot Chrome process (or connecting to an external
//! CDP endpoint), including `--user-data-dir` / browser-profile resolution.
//!
//! Each ephemeral launch creates a unique user data directory to avoid Chrome's
//! `SingletonLock` conflicts when multiple instances run concurrently or a
//! previous instance crashed without cleanup. When `config.browser_profile` is
//! set, the launch uses that named profile's directory instead (see
//! [`resolve_user_data_dir`]).

use chromiumoxide::Handler;
use chromiumoxide::browser::{Browser, BrowserConfig as ChromeBrowserConfig, BrowserConfigBuilder};

use crate::browser_pool::ScratchProfileDir;
use crate::error::CrawlError;
use crate::types::CrawlConfig;

/// The Chrome `--user-data-dir` a one-shot launch uses.
///
/// ~keep A scratch directory is removed when this value drops, on every
/// ~keep exit path: a failed launch, a cancelled launch or fetch (xberg-io/crawlberg#131), and a
/// ~keep teardown task that the runtime drops before it finishes, which is how every one-shot
/// ~keep fetch at the end of a `#[tokio::test]` leaked its directory (xberg-io/crawlberg#415).
#[derive(Debug)]
pub(super) enum UserDataDir {
    /// A named profile launched with `save_browser_profile: true`, used in place and kept.
    Persistent(std::path::PathBuf),
    /// An ephemeral directory, or a scratch copy of a named profile, removed on drop.
    Scratch(ScratchProfileDir),
}

impl UserDataDir {
    fn path(&self) -> &std::path::Path {
        match self {
            Self::Persistent(path) => path,
            Self::Scratch(dir) => dir.path(),
        }
    }
}

/// Resolve the `--user-data-dir` for a one-shot Chrome launch from `config`.
///
/// - No `browser_profile`: a fresh ephemeral temp directory, deleted on exit.
/// - `browser_profile` set and `save_browser_profile: true`: the named profile's
///   own directory (created if missing), used and written to in place.
/// - `browser_profile` set and `save_browser_profile: false`: the named profile's
///   directory (created if missing) is copied into a scratch temp directory so
///   the session starts from existing profile state but any changes made during
///   the session are discarded rather than written back.
fn resolve_user_data_dir(config: &CrawlConfig) -> Result<UserDataDir, CrawlError> {
    let Some(name) = config.browser_profile.as_deref() else {
        return Ok(UserDataDir::Scratch(ScratchProfileDir::create("crawlberg-browser-")?));
    };

    let profile = crate::browser_profile::BrowserProfile::new(name)?;
    if !profile.exists() {
        profile.create()?;
    }

    if config.save_browser_profile {
        Ok(UserDataDir::Persistent(profile.user_data_dir))
    } else {
        let scratch = ScratchProfileDir::create(&format!("crawlberg-profile-{name}-"))?;
        copy_dir_recursive(&profile.user_data_dir, scratch.path())?;
        Ok(UserDataDir::Scratch(scratch))
    }
}

/// Recursively copy `src` into `dst`, creating `dst` if needed. Symlinks inside
/// `src` are skipped rather than followed or copied as links.
fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> Result<(), CrawlError> {
    std::fs::create_dir_all(dst)
        .map_err(|e| CrawlError::other(format!("failed to create profile scratch directory: {e}")))?;
    let entries =
        std::fs::read_dir(src).map_err(|e| CrawlError::other(format!("failed to read profile directory: {e}")))?;
    for entry in entries {
        let entry = entry.map_err(|e| CrawlError::other(format!("failed to read profile entry: {e}")))?;
        let file_type = entry
            .file_type()
            .map_err(|e| CrawlError::other(format!("failed to stat profile entry: {e}")))?;
        let dest_path = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&entry.path(), &dest_path)?;
        } else if file_type.is_file() {
            std::fs::copy(entry.path(), &dest_path)
                .map_err(|e| CrawlError::other(format!("failed to copy profile file: {e}")))?;
        }
    }
    Ok(())
}

/// Launch a new managed browser or connect to an external CDP endpoint.
pub(super) async fn launch_or_connect(
    config: &CrawlConfig,
) -> Result<(Browser, Handler, Option<UserDataDir>), CrawlError> {
    if let Some(ref endpoint) = config.browser.endpoint {
        if config.browser_profile.is_some() {
            tracing::warn!(
                profile = config.browser_profile.as_deref().unwrap_or_default(),
                "browser_profile is ignored when connecting to an external browser.endpoint; \
                 the remote Chrome process's profile is managed externally"
            );
        }
        let (browser, handler) = crate::browser_pool::connect_endpoint(endpoint).await?;
        Ok((browser, handler, None))
    } else {
        let user_data = resolve_user_data_dir(config)?;

        let builder = build_one_shot_launch_builder(user_data.path());
        let browser_config = builder
            .build()
            .map_err(|e| CrawlError::browser_error(format!("invalid browser config: {e}")))?;

        // ~keep A failed or cancelled launch drops `user_data`, which removes a scratch directory.
        let launched = match user_data {
            UserDataDir::Scratch(dir) => dir
                .launch(browser_config)
                .await
                .map(|(browser, handler, dir)| (browser, handler, UserDataDir::Scratch(dir))),
            UserDataDir::Persistent(path) => Browser::launch(browser_config)
                .await
                .map(|(browser, handler)| (browser, handler, UserDataDir::Persistent(path))),
        };
        let (browser, handler, user_data) =
            launched.map_err(|e| CrawlError::browser_error(format!("failed to launch browser: {e}")))?;
        Ok((browser, handler, Some(user_data)))
    }
}

/// Build the [`ChromeBrowserConfig`] builder for a fresh one-shot launch (not the
/// `browser.endpoint` connect branch).
///
/// ~keep Split out from `launch_or_connect` so a test can assert on the flags this
/// ~keep path actually passes without spawning a real Chrome process.
fn build_one_shot_launch_builder(user_data_dir: &std::path::Path) -> BrowserConfigBuilder {
    let mut builder = ChromeBrowserConfig::builder()
        .no_sandbox()
        .new_headless_mode()
        .user_data_dir(user_data_dir)
        .disable_default_args();
    // ~keep Mirror browser_pool's fork-safety env vars so one-shot and pooled Chrome launch paths match.
    builder = builder
        .env("OBJC_DISABLE_INITIALIZE_FORK_SAFETY", "YES")
        .env("OS_ACTIVITY_MODE", "disable");
    crate::browser_pool::apply_default_args(builder)
}

/// Returns a modern Chrome user-agent string suitable for the runtime environment.
/// Used as the default UA when stealth mode is enabled.
pub(super) fn resolve_default_user_agent() -> &'static str {
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36"
}

#[cfg(test)]
mod user_data_dir_tests {
    //! Hermetic, Chrome-free unit tests for the `browser_profile` /
    //! `save_browser_profile` wiring in [`resolve_user_data_dir`] and
    //! [`copy_dir_recursive`]. End-to-end proof that Chrome actually launches
    //! against these resolved directories lives in
    //! `crates/crawlberg/tests/test_browser_profile.rs`.
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::browser_profile::BrowserProfile;
    use crate::types::CrawlConfig;

    static NAME_COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A collision-free profile name so parallel test runs never race on the
    /// same on-disk profile directory.
    fn unique_profile_name(tag: &str) -> String {
        format!(
            "crawlberg-unit-test-{tag}-{}-{}",
            std::process::id(),
            NAME_COUNTER.fetch_add(1, Ordering::Relaxed)
        )
    }

    /// Deletes the backing profile directory on drop, regardless of outcome.
    struct ProfileGuard(BrowserProfile);
    impl Drop for ProfileGuard {
        fn drop(&mut self) {
            let _ = self.0.delete();
        }
    }

    #[test]
    fn no_profile_resolves_to_ephemeral_temp_dir_marked_for_cleanup() {
        let config = CrawlConfig::default();
        let resolved = resolve_user_data_dir(&config).expect("resolve must succeed without a profile configured");
        assert!(
            matches!(resolved, UserDataDir::Scratch(_)),
            "ephemeral (no browser_profile) launches must be cleaned up after the session"
        );
    }

    #[test]
    fn missing_named_profile_is_created_and_used_directly_when_saved() {
        let name = unique_profile_name("create-save");
        let profile = BrowserProfile::new(&name).expect("profile name must be valid");
        assert!(!profile.exists(), "precondition: profile must not exist yet");
        let _guard = ProfileGuard(profile.clone());

        let config = CrawlConfig {
            browser_profile: Some(name.clone()),
            save_browser_profile: true,
            ..CrawlConfig::default()
        };
        let resolved = resolve_user_data_dir(&config).expect("resolve must succeed");

        assert!(
            profile.exists(),
            "resolve_user_data_dir must create the named profile directory when missing"
        );
        assert_eq!(
            resolved.path(),
            profile.user_data_dir,
            "save_browser_profile: true must launch directly against the profile's own directory"
        );
        assert!(
            matches!(resolved, UserDataDir::Persistent(_)),
            "save_browser_profile: true must not mark the profile directory for cleanup"
        );
    }

    #[test]
    fn unsaved_profile_launches_from_a_scratch_copy_that_preserves_the_original() {
        let name = unique_profile_name("no-save");
        let profile = BrowserProfile::new(&name).expect("profile name must be valid");
        profile.create().expect("profile directory must be creatable");
        let _guard = ProfileGuard(profile.clone());
        std::fs::write(profile.user_data_dir.join("marker.txt"), b"original").expect("marker file must be writable");

        let config = CrawlConfig {
            browser_profile: Some(name.clone()),
            save_browser_profile: false,
            ..CrawlConfig::default()
        };
        let resolved = resolve_user_data_dir(&config).expect("resolve must succeed");

        assert_ne!(
            resolved.path(),
            profile.user_data_dir,
            "save_browser_profile: false must launch from a scratch copy, never the profile dir itself"
        );
        assert!(
            matches!(resolved, UserDataDir::Scratch(_)),
            "the scratch copy must be marked for cleanup after the session"
        );
        assert_eq!(
            std::fs::read(resolved.path().join("marker.txt")).expect("scratch copy must contain the marker file"),
            b"original",
            "the scratch copy must start from the existing profile state"
        );

        std::fs::write(resolved.path().join("marker.txt"), b"mutated-in-session")
            .expect("writing into the scratch copy must succeed");
        assert_eq!(
            std::fs::read(profile.user_data_dir.join("marker.txt")).expect("original marker file must still exist"),
            b"original",
            "writes into the scratch copy must never be reflected back into the saved profile"
        );

        let scratch = resolved.path().to_path_buf();
        drop(resolved);
        assert!(
            crate::browser_pool::tests::wait_for_removal(&scratch),
            "the scratch copy must be removed when it is dropped"
        );
    }

    #[test]
    fn copy_dir_recursive_copies_nested_files_and_skips_symlinks() {
        let root = std::env::temp_dir().join(unique_profile_name("copy"));
        let src = root.join("src");
        let dst = root.join("dst");
        std::fs::create_dir_all(src.join("nested")).expect("nested src dir must be creatable");
        std::fs::write(src.join("top.txt"), b"top").expect("top-level file must be writable");
        std::fs::write(src.join("nested").join("deep.txt"), b"deep").expect("nested file must be writable");

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let _ = symlink(src.join("top.txt"), src.join("link.txt"));
        }

        copy_dir_recursive(&src, &dst).expect("recursive copy must succeed");

        assert_eq!(std::fs::read(dst.join("top.txt")).unwrap(), b"top");
        assert_eq!(std::fs::read(dst.join("nested").join("deep.txt")).unwrap(), b"deep");
        assert!(
            !dst.join("link.txt").exists(),
            "symlinks in the source directory must not be copied"
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_one_shot_launch_builder_carries_no_double_dashed_flag_and_the_macos_keychain_flag() {
        // ~keep Behavioral, not textual: this calls the exact function `launch_or_connect`
        // ~keep uses to build its `BrowserConfig`, so a path that stops calling
        // ~keep `apply_default_args` (even behind a comment claiming it still does) fails
        // ~keep here because the returned flags actually change.
        let builder = build_one_shot_launch_builder(std::path::Path::new("/tmp/browser-rs-test-profile"));
        crate::browser_pool::assert_launch_flags_are_normalized(&builder);
    }

    /// A scratch profile directory that never reaches a launched Chrome is removed when it drops.
    ///
    /// ~keep This is the failed-launch and cancelled-launch case stated as a unit: `launch_or_connect`
    /// ~keep drops `user_data` without returning it, exactly as here. Proven at this level rather than
    /// ~keep through a real Chrome because an integration test cannot reliably choose which window a
    /// ~keep cancellation lands in, or make a present Chrome fail to start -- see xberg-io/crawlberg#198.
    #[test]
    #[serial_test::serial(process_table)]
    fn an_unclaimed_scratch_profile_directory_is_removed_when_it_drops() {
        let resolved = resolve_user_data_dir(&CrawlConfig::default()).expect("resolve must succeed");
        let path = resolved.path().to_path_buf();
        assert!(path.is_dir(), "the scratch directory must exist before it drops");

        drop(resolved);

        assert!(
            crate::browser_pool::tests::wait_for_removal(&path),
            "an unclaimed scratch profile directory must be removed"
        );
    }

    /// A one-shot launch records the Chrome it starts, so dropping its profile directory stops
    /// that Chrome and removes the directory.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial(process_table)]
    #[allow(clippy::print_stderr, reason = "test-only skip announcement")]
    async fn dropping_a_one_shot_launchs_profile_directory_stops_its_chrome() {
        let (browser, handler, user_data) = match launch_or_connect(&CrawlConfig::default()).await {
            Ok(launched) => launched,
            Err(error) => {
                eprintln!("skipping: no usable Chrome: {error}");
                return;
            }
        };
        let user_data = user_data.expect("a launched Chrome must have a profile directory");
        let path = user_data.path().to_path_buf();

        crate::browser_pool::tests::assert_dropping_the_profile_stops_its_chrome(browser, handler, user_data, path)
            .await;
    }

    /// A one-shot launch cut off by the overall deadline hands its profile teardown off the
    /// executor thread.
    ///
    /// ~keep No Chrome is needed: without one the launch fails before the deadline, and the
    /// ~keep profile directory drops on the same path.
    #[tokio::test]
    #[serial_test::serial(process_table)]
    async fn a_cancelled_one_shot_launch_tears_its_profile_down_off_the_executor_thread() {
        let mut config = CrawlConfig::default();
        config.browser.overall_timeout = std::time::Duration::from_millis(1);
        let before = crate::browser_pool::tests::profile_drops_here();

        let fetched = super::super::one_shot_fetch("about:blank", &config, None, false).await;

        assert!(fetched.is_err(), "a Chrome launch cannot finish within a millisecond");
        crate::browser_pool::tests::assert_profile_teardown_left_this_thread(before);
    }

    /// `launch_or_connect`'s connect-error message must never carry a `browser.endpoint`
    /// password or path token, though the failing origin must still be readable for debugging.
    ///
    /// ~keep A closed local port refuses the connection immediately, so this needs no real
    /// ~keep Chrome and stays fast. `ws://` skips chromiumoxide's `json/version` HTTP probe
    /// ~keep and goes straight to the WebSocket handshake. The endpoint-listener test just
    /// ~keep below reaches the same error path with a local socket that answers HTTP 418, so
    /// ~keep a closed port is not the only way here; it stays because it needs no listener at all.
    #[tokio::test]
    async fn connect_error_prints_only_the_endpoint_origin() {
        let config = CrawlConfig {
            browser: crate::types::BrowserConfig {
                endpoint: Some("ws://user:hunter2@127.0.0.1:1/devtools/browser/b1946ac9-guid".into()),
                ..Default::default()
            },
            ..Default::default()
        };

        let err = launch_or_connect(&config)
            .await
            .expect_err("a refused local port must fail the connect");
        let msg = err.to_string();
        assert!(
            !msg.contains("hunter2"),
            "password must not survive into the error, got: {msg}"
        );
        assert!(
            !msg.contains("b1946ac9-guid"),
            "the CDP path token must not survive into the error, got: {msg}"
        );
        assert!(
            msg.contains("127.0.0.1"),
            "host must still appear in the error, got: {msg}"
        );
    }

    /// Every spelling of `browser.endpoint` that the config check accepts must reach the browser.
    #[tokio::test]
    async fn connects_every_endpoint_spelling_the_checks_accept() {
        crate::browser_pool::tests::assert_every_accepted_endpoint_reaches_the_browser(|endpoint| async move {
            let config = CrawlConfig {
                browser: crate::types::BrowserConfig {
                    endpoint: Some(endpoint),
                    ..Default::default()
                },
                ..Default::default()
            };
            launch_or_connect(&config).await
        })
        .await;
    }

    /// A saved named profile is never removed when its value drops.
    #[test]
    fn a_persistent_profile_directory_is_never_removed() {
        let dir = tempfile::tempdir().expect("the directory must be creatable");

        drop(UserDataDir::Persistent(dir.path().to_path_buf()));

        assert!(
            dir.path().is_dir(),
            "a persistent profile directory must survive its value"
        );
    }

    /// Dropping a saved named profile neither removes it nor stops a Chrome still using it.
    #[cfg(unix)]
    #[test]
    #[serial_test::serial(process_table)]
    fn a_persistent_profile_in_use_is_neither_removed_nor_its_user_killed() {
        let dir = tempfile::tempdir().expect("the directory must be creatable");
        let flag = crate::browser_pool::user_data_dir_flag(dir.path());
        let mut user = crate::browser_pool::tests::spawn_bystander(&flag);

        drop(UserDataDir::Persistent(dir.path().to_path_buf()));

        let running = user.try_wait().expect("the status must be readable").is_none();
        let _ = user.kill();
        let _ = user.wait();
        assert!(
            running,
            "a Chrome using a saved profile must not be killed when its value drops"
        );
        assert!(dir.path().is_dir(), "a saved profile directory must survive its value");
    }

    /// A one-shot session whose teardown task runs to the end removes its profile directory.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial(process_table)]
    #[allow(clippy::print_stderr, reason = "test-only skip announcement")]
    async fn a_one_shot_session_torn_down_by_its_task_leaves_no_profile_directory() {
        use tokio_stream::StreamExt;

        let (browser, mut handler, data_dir) = match launch_or_connect(&CrawlConfig::default()).await {
            Ok(launched) => launched,
            Err(error) => {
                eprintln!("skipping: no usable Chrome: {error}");
                return;
            }
        };
        let path = data_dir
            .as_ref()
            .map(|dir| dir.path().to_path_buf())
            .expect("a launched Chrome must have a profile directory");
        let handler_handle = tokio::spawn(async move { while handler.next().await.is_some() {} });
        drop(super::super::OneShotSession {
            browser: Some(browser),
            open_tab: None,
            handler_handle: Some(handler_handle),
            data_dir,
            shutdown_timeout: std::time::Duration::from_secs(5),
        });

        tokio::task::spawn_blocking(move || {
            crate::browser_pool::tests::assert_profile_directory_is_gone_for_good(&path)
        })
        .await
        .expect("the teardown task must stop Chrome and remove the profile directory");
    }

    /// A one-shot session dropped just before its runtime stops still removes its profile directory.
    ///
    /// ~keep The session's `Drop` spawns its teardown, and a runtime that stops right after, as every
    /// ~keep `#[tokio::test]` ending on a one-shot fetch does, drops that task unfinished. That left one
    /// ~keep `crawlberg-browser-*` directory per fetch in the temp directory (xberg-io/crawlberg#415).
    #[test]
    #[serial_test::serial(process_table)]
    #[allow(clippy::print_stderr, reason = "test-only skip announcement")]
    fn a_one_shot_session_dropped_as_its_runtime_stops_leaves_no_profile_directory() {
        use tokio_stream::StreamExt;

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("a runtime must build");
        let path = runtime.block_on(async {
            let (browser, mut handler, data_dir) = match launch_or_connect(&CrawlConfig::default()).await {
                Ok(launched) => launched,
                Err(error) => {
                    eprintln!("skipping: no usable Chrome: {error}");
                    return None;
                }
            };
            let path = data_dir
                .as_ref()
                .map(|dir| dir.path().to_path_buf())
                .expect("a launched Chrome must have a profile directory");
            let handler_handle = tokio::spawn(async move { while handler.next().await.is_some() {} });
            drop(super::super::OneShotSession {
                browser: Some(browser),
                open_tab: None,
                handler_handle: Some(handler_handle),
                data_dir,
                shutdown_timeout: std::time::Duration::from_secs(5),
            });
            Some(path)
        });
        drop(runtime);
        let Some(path) = path else {
            return;
        };
        crate::browser_pool::tests::assert_profile_directory_is_gone_for_good(&path);
    }
}
