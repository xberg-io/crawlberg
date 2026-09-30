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

use crate::error::CrawlError;
use crate::types::CrawlConfig;

/// The Chrome `--user-data-dir` to launch with, and what to do with it once the
/// browser session ends.
struct UserDataDir {
    path: std::path::PathBuf,
    /// When `true`, the directory is deleted after the session (ephemeral launch,
    /// or a scratch copy of a named profile whose changes should not be saved).
    /// When `false`, the directory is left in place so its contents persist
    /// (a named profile launched with `save_browser_profile: true`).
    cleanup_on_exit: bool,
    /// Set by [`UserDataDir::hand_over`] once the launched session owns the directory, so this
    /// value's `Drop` leaves it alone.
    handed_over: bool,
}

impl UserDataDir {
    /// Pass ownership of the directory to a launched session, yielding the path that session
    /// must delete when it ends, or `None` when the directory is meant to persist.
    fn hand_over(mut self) -> Option<std::path::PathBuf> {
        self.handed_over = true;
        self.cleanup_on_exit.then(|| self.path.clone())
    }
}

impl Drop for UserDataDir {
    /// ~keep Covers every way the launch can fail to hand the directory on, including the one
    /// ~keep straight-line cleanup cannot reach: the launch future being dropped because the
    /// ~keep caller cancelled the fetch. The directory is created before Chrome starts, so it
    /// ~keep exists for the whole of `Browser::launch` with nothing else owning it, and a
    /// ~keep cancellation there used to leave tens of megabytes in the temp directory for good
    /// ~keep (xberg-io/crawlberg#131). `std::fs`, not `tokio::fs`: `Drop` cannot await.
    fn drop(&mut self) {
        if self.handed_over || !self.cleanup_on_exit {
            return;
        }
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Unique-per-launch temp directory name, avoiding Chrome `SingletonLock` collisions
/// when multiple browsers launch concurrently or a previous instance crashed uncleanly.
fn unique_temp_dir(prefix: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
    static LAUNCH_COUNTER: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "{prefix}-{}-{}",
        std::process::id(),
        LAUNCH_COUNTER.fetch_add(1, AtomicOrdering::Relaxed),
    ))
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
        return Ok(UserDataDir {
            path: unique_temp_dir("crawlberg-browser"),
            cleanup_on_exit: true,
            handed_over: false,
        });
    };

    let profile = crate::browser_profile::BrowserProfile::new(name)?;
    if !profile.exists() {
        profile.create()?;
    }

    if config.save_browser_profile {
        Ok(UserDataDir {
            path: profile.user_data_dir,
            cleanup_on_exit: false,
            handed_over: false,
        })
    } else {
        let scratch = unique_temp_dir(&format!("crawlberg-profile-{name}"));
        copy_dir_recursive(&profile.user_data_dir, &scratch)?;
        Ok(UserDataDir {
            path: scratch,
            cleanup_on_exit: true,
            handed_over: false,
        })
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
            std::fs::copy(entry.path(), &dest_path).map_err(|e| {
                CrawlError::other(format!("failed to copy profile file {}: {e}", entry.path().display()))
            })?;
        }
    }
    Ok(())
}

/// Launch a new managed browser or connect to an external CDP endpoint.
pub(super) async fn launch_or_connect(
    config: &CrawlConfig,
) -> Result<(Browser, Handler, Option<std::path::PathBuf>), CrawlError> {
    if let Some(ref endpoint) = config.browser.endpoint {
        crate::types::warn_ignored_launch_options(
            &config.browser,
            "connecting to an external browser.endpoint, whose Chrome process is launched externally",
        );
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
        let proxy = crate::proxy::chrome_proxy_for(config)?;
        let user_data = resolve_user_data_dir(config)?;

        let browser_config = build_one_shot_launch_builder(&user_data.path, &config.browser, proxy.as_ref())?
            .build()
            .map_err(|e| CrawlError::browser_error(format!("invalid browser config: {e}")))?;

        match Browser::launch(browser_config).await {
            Ok((browser, handler)) => Ok((browser, handler, user_data.hand_over())),
            // ~keep `user_data`'s `Drop` removes the directory on this path and on cancellation.
            Err(e) => Err(CrawlError::browser_error(format!("failed to launch browser: {e}"))),
        }
    }
}

/// Build the [`ChromeBrowserConfig`] builder for a fresh one-shot launch (not the
/// `browser.endpoint` connect branch).
///
/// ~keep Split out from `launch_or_connect` so a test can assert on the flags this
/// ~keep path actually passes without spawning a real Chrome process.
fn build_one_shot_launch_builder(
    user_data_dir: &std::path::Path,
    browser: &crate::types::BrowserConfig,
    proxy: Option<&crate::proxy::ChromeProxy>,
) -> Result<BrowserConfigBuilder, CrawlError> {
    let mut builder = ChromeBrowserConfig::builder()
        .no_sandbox()
        .new_headless_mode()
        .user_data_dir(user_data_dir)
        .disable_default_args();
    // ~keep Mirror browser_pool's fork-safety env vars so one-shot and pooled Chrome launch paths match.
    builder = builder
        .env("OBJC_DISABLE_INITIALIZE_FORK_SAFETY", "YES")
        .env("OS_ACTIVITY_MODE", "disable");
    builder = crate::browser_pool::apply_default_args(builder, &browser.chrome_args);
    crate::browser_pool::apply_launch_overrides(
        builder,
        "browser",
        browser.chrome_path.as_deref(),
        &browser.chrome_args,
        proxy,
    )
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
            resolved.cleanup_on_exit,
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
            resolved.path, profile.user_data_dir,
            "save_browser_profile: true must launch directly against the profile's own directory"
        );
        assert!(
            !resolved.cleanup_on_exit,
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
            resolved.path, profile.user_data_dir,
            "save_browser_profile: false must launch from a scratch copy, never the profile dir itself"
        );
        assert!(
            resolved.cleanup_on_exit,
            "the scratch copy must be marked for cleanup after the session"
        );
        assert_eq!(
            std::fs::read(resolved.path.join("marker.txt")).expect("scratch copy must contain the marker file"),
            b"original",
            "the scratch copy must start from the existing profile state"
        );

        std::fs::write(resolved.path.join("marker.txt"), b"mutated-in-session")
            .expect("writing into the scratch copy must succeed");
        assert_eq!(
            std::fs::read(profile.user_data_dir.join("marker.txt")).expect("original marker file must still exist"),
            b"original",
            "writes into the scratch copy must never be reflected back into the saved profile"
        );

        let _ = std::fs::remove_dir_all(&resolved.path);
    }

    /// The error of a profile copy that fails names the file it could not copy.
    #[cfg(unix)]
    #[test]
    fn a_failed_profile_copy_names_the_file() {
        use std::os::unix::fs::PermissionsExt;

        let name = unique_profile_name("copy-error");
        let profile = BrowserProfile::new(&name).expect("profile name must be valid");
        profile.create().expect("profile directory must be creatable");
        let _guard = ProfileGuard(profile.clone());
        let unreadable = profile.user_data_dir.join("unreadable-marker");
        std::fs::write(&unreadable, b"x").expect("marker file must be writable");
        std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000))
            .expect("permissions must be settable");
        if std::fs::read(&unreadable).is_ok() {
            // ~keep Root reads a mode 000 file, so the copy cannot fail here.
            return;
        }

        let config = CrawlConfig {
            browser_profile: Some(name),
            save_browser_profile: false,
            ..CrawlConfig::default()
        };
        let error = resolve_user_data_dir(&config)
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();
        let _ = std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o600));

        assert!(
            error.contains("unreadable-marker") && error.contains("failed to copy profile file"),
            "the copy error must name the file: {error:?}"
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
        let builder = build_one_shot_launch_builder(
            std::path::Path::new("/tmp/browser-rs-test-profile"),
            &crate::types::BrowserConfig::default(),
            None,
        )
        .expect("the default browser config names no binary to check");
        crate::browser_pool::assert_launch_flags_are_normalized(&builder);
    }

    #[test]
    fn the_one_shot_launch_routes_chrome_through_the_configured_proxy() {
        let config = CrawlConfig {
            browser: crate::types::BrowserConfig {
                proxy: Some(crate::types::ProxyConfig {
                    url: "127.0.0.1:3128".into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        let proxy = crate::proxy::chrome_proxy_for(&config).expect("a usable proxy");
        let debug = format!(
            "{:?}",
            build_one_shot_launch_builder(
                std::path::Path::new("/tmp/browser-rs-test-profile"),
                &config.browser,
                proxy.as_ref()
            )
            .expect("the config names no binary to check")
        );
        assert!(
            debug.contains("key: \"proxy-server=http://127.0.0.1:3128\""),
            "the render's Chrome must be launched through the proxy: {debug}"
        );
        assert!(
            debug.contains("proxy-bypass-list=<-loopback>"),
            "loopback requests must not bypass the proxy: {debug}"
        );
    }

    #[test]
    fn the_configured_proxy_replaces_a_caller_proxy_flag() {
        let proxy = crate::proxy::chrome_proxy(&crate::types::ProxyConfig {
            url: "http://127.0.0.1:9".into(),
            ..Default::default()
        })
        .expect("an http proxy is a Chrome proxy");
        for (caller_flag, caller_value) in [
            ("--proxy-server=http://127.0.0.1:7", "127.0.0.1:7"),
            ("--proxy-bypass-list=*.internal", "*.internal"),
        ] {
            let browser = crate::types::BrowserConfig {
                chrome_args: vec![caller_flag.to_owned()],
                ..Default::default()
            };
            let (built, fields) = crate::tracing_capture::capture_events(|| {
                build_one_shot_launch_builder(
                    std::path::Path::new("/tmp/browser-rs-test-profile"),
                    &browser,
                    Some(&proxy),
                )
            });
            let debug = format!("{:?}", built.expect("the config names no binary to check"));
            for configured in ["proxy-server=http://127.0.0.1:9", "proxy-bypass-list=<-loopback>"] {
                assert!(
                    debug.contains(&format!("key: \"{configured}\"")),
                    "{caller_flag}: the configured proxy's {configured} is missing: {debug}"
                );
            }
            assert!(
                !debug.contains(caller_value),
                "{caller_flag}: the caller's flag must be dropped: {debug}"
            );
            let switch = caller_flag.split('=').next().expect("a switch name");
            crate::tracing_capture::assert_logged_without_secret(&fields, caller_value, switch);
        }
    }

    #[test]
    fn a_caller_proxy_flag_reaches_chrome_when_no_proxy_is_configured() {
        let browser = crate::types::BrowserConfig {
            chrome_args: vec![
                "--proxy-server=http://127.0.0.1:7".to_owned(),
                "--proxy-bypass-list=*.internal".to_owned(),
            ],
            ..Default::default()
        };
        let debug = format!(
            "{:?}",
            build_one_shot_launch_builder(std::path::Path::new("/tmp/browser-rs-test-profile"), &browser, None)
                .expect("the config names no binary to check")
        );
        for flag in ["proxy-server=http://127.0.0.1:7", "proxy-bypass-list=*.internal"] {
            assert!(
                debug.contains(&format!("key: \"{flag}\"")),
                "without a configured proxy the caller's {flag} must reach Chrome: {debug}"
            );
        }
    }

    #[test]
    fn the_one_shot_launch_builder_uses_the_configured_chrome_path_and_args() {
        crate::browser_pool::assert_launch_overrides_reach_the_builder(|chrome_path, chrome_args| {
            build_one_shot_launch_builder(
                std::path::Path::new("/tmp/browser-rs-test-profile"),
                &crate::types::BrowserConfig {
                    chrome_path,
                    chrome_args,
                    ..Default::default()
                },
                None,
            )
        });
    }

    /// A profile directory nobody took ownership of is removed when its guard drops.
    ///
    /// ~keep This is the cancellation case stated as a unit: the launch future being dropped
    /// ~keep drops `user_data` without `hand_over` ever running, which is indistinguishable here
    /// ~keep from `Browser::launch` returning an error. Proven at this level rather than through
    /// ~keep a real Chrome because an integration test cannot reliably choose which window a
    /// ~keep cancellation lands in -- see xberg-io/crawlberg#198.
    #[test]
    fn an_unclaimed_ephemeral_profile_directory_is_removed_when_its_guard_drops() {
        let path = unique_temp_dir("crawlberg-launch-guard-test");
        std::fs::create_dir_all(&path).expect("the directory must be creatable");
        assert!(path.exists(), "the directory must exist before the guard drops");

        drop(UserDataDir {
            path: path.clone(),
            cleanup_on_exit: true,
            handed_over: false,
        });

        assert!(
            !path.exists(),
            "an unclaimed ephemeral profile directory must be removed"
        );
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

    /// `hand_over` passes the path on and stops the guard from removing it.
    #[test]
    fn handing_an_ephemeral_profile_directory_over_leaves_it_for_the_session_to_remove() {
        let path = unique_temp_dir("crawlberg-launch-guard-test");
        std::fs::create_dir_all(&path).expect("the directory must be creatable");

        let handed = UserDataDir {
            path: path.clone(),
            cleanup_on_exit: true,
            handed_over: false,
        }
        .hand_over();

        assert_eq!(
            handed.as_deref(),
            Some(path.as_path()),
            "the session must be given the path"
        );
        assert!(path.exists(), "the guard must not remove a directory it handed over");
        std::fs::remove_dir_all(&path).expect("test cleanup");
    }

    /// A saved named profile is never removed, handed over or not.
    #[test]
    fn a_persistent_profile_directory_is_never_removed() {
        let path = unique_temp_dir("crawlberg-launch-guard-test");
        std::fs::create_dir_all(&path).expect("the directory must be creatable");

        let handed = UserDataDir {
            path: path.clone(),
            cleanup_on_exit: false,
            handed_over: false,
        }
        .hand_over();

        assert!(
            handed.is_none(),
            "a persistent profile must not be handed over for removal"
        );
        assert!(path.exists(), "a persistent profile directory must survive its guard");
        std::fs::remove_dir_all(&path).expect("test cleanup");
    }
}
