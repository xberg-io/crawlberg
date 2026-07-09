//! File-system watcher for atomic hot-reload of WAF fingerprint rules.
//!
//! [`WatchHandle`] is returned by [`super::TomlClassifier::watch`] and stops
//! the watcher when dropped.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};

use super::TomlClassifier;
use crate::waf::rules::load_from_path;

/// Drop-on-shutdown handle returned by [`TomlClassifier::watch`].
///
/// Holds the watcher and the debounce task; dropping stops both.
#[derive(Debug)]
pub struct WatchHandle {
    pub(super) _watcher: RecommendedWatcher,
    pub(super) shutdown: Option<oneshot::Sender<()>>,
    pub(super) task: tokio::task::JoinHandle<()>,
}

impl Drop for WatchHandle {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        self.task.abort();
    }
}

/// Error returned when setting up a [`WatchHandle`].
#[derive(Debug, Error)]
pub enum WatchError {
    /// The underlying `notify` watcher could not be created or configured.
    #[error("watch setup: {0}")]
    Setup(#[from] notify::Error),
    /// The supplied path has no parent directory to watch.
    #[error("path has no parent: {0}")]
    NoParent(PathBuf),
}

/// Spawns the debounce task and configures a `RecommendedWatcher` on the
/// parent directory of `watch_path`.
///
/// The caller receives a [`WatchHandle`]; dropping it stops the watcher and
/// the debounce task.
pub(super) fn start_watch(classifier: Arc<TomlClassifier>, watch_path: &Path) -> Result<WatchHandle, WatchError> {
    let watch_path = watch_path.to_owned();
    let parent = watch_path
        .parent()
        .ok_or_else(|| WatchError::NoParent(watch_path.clone()))?
        .to_owned();
    let parent = parent.canonicalize().unwrap_or(parent);
    let file_name = watch_path.file_name().map(|n| n.to_owned());

    let (event_tx, event_rx) = mpsc::channel::<()>(16);
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    let task = spawn_debounce_task(classifier, watch_path.clone(), event_rx, shutdown_rx);

    let path_for_closure = watch_path.clone();
    let canonical_target = path_for_closure.canonicalize().ok();
    let mut watcher = notify::recommended_watcher(move |result: notify::Result<Event>| {
        let event = match result {
            Ok(e) => e,
            Err(err) => {
                tracing::warn!(
                    target = "crawlberg::waf::watch",
                    error = %err,
                    "notify error; skipping event"
                );
                return;
            }
        };

        let is_relevant = matches!(
            event.kind,
            EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
        ) && event.paths.iter().any(|p| {
            if p == &path_for_closure {
                return true;
            }
            if let (Ok(p_canon), Some(target)) = (p.canonicalize(), canonical_target.as_ref())
                && &p_canon == target
            {
                return true;
            }
            file_name.as_deref().is_some_and(|name| p.file_name() == Some(name))
        });

        if is_relevant {
            let _ = event_tx.try_send(());
        }
    })?;

    watcher.watch(&parent, RecursiveMode::NonRecursive)?;

    Ok(WatchHandle {
        _watcher: watcher,
        shutdown: Some(shutdown_tx),
        task,
    })
}

/// Runs the debounce loop: waits for the first tick, sleeps 500 ms, drains
/// any additional ticks, then reloads the rules file and swaps atomically.
fn spawn_debounce_task(
    classifier: Arc<TomlClassifier>,
    path: PathBuf,
    mut event_rx: mpsc::Receiver<()>,
    mut shutdown_rx: oneshot::Receiver<()>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                _ = &mut shutdown_rx => {
                    break;
                }
                tick = event_rx.recv() => {
                    if tick.is_none() {
                        break;
                    }
                }
            }

            tokio::time::sleep(Duration::from_millis(500)).await;

            while event_rx.try_recv().is_ok() {}

            match load_from_path(&path) {
                Ok(new_rules) => {
                    classifier.swap(new_rules);
                    tracing::info!(
                        target = "crawlberg::waf::watch",
                        path = %path.display(),
                        "waf rules reloaded"
                    );
                }
                Err(err) => {
                    tracing::warn!(
                        target = "crawlberg::waf::watch",
                        error = %err,
                        "reload failed; keeping previous rules"
                    );
                }
            }
        }
    })
}
