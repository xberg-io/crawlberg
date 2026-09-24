//! Dedicated native browser worker pool used by the adapter's free functions.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::page::PageError;

use super::{
    NativeBrowserConfig, NativeInteractionResult, NativePageAction, RenderedPage, interact_url_local, render_url_local,
};

const DEFAULT_NATIVE_WORKER_LIMIT: usize = 8;
const DEFAULT_QUEUE_CAPACITY_PER_WORKER: usize = 64;

/// Configuration for [`NativeBrowserExecutor`].
///
/// Rust-only: excluded from alef-generated polyglot bindings. Intended for
/// long-lived Rust processes that own an executor at the application layer
/// and reuse it across crawls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(alef, alef(skip))]
pub struct NativeBrowserExecutorConfig {
    /// Number of dedicated browser worker threads.
    pub workers: usize,
    /// Bounded job queue capacity for each worker.
    pub queue_capacity_per_worker: usize,
}

impl NativeBrowserExecutorConfig {
    /// Create a config with an explicit worker count and default queue capacity.
    pub fn with_workers(workers: usize) -> Self {
        Self {
            workers,
            queue_capacity_per_worker: DEFAULT_QUEUE_CAPACITY_PER_WORKER,
        }
    }
}

impl Default for NativeBrowserExecutorConfig {
    fn default() -> Self {
        let workers = std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1)
            .clamp(1, DEFAULT_NATIVE_WORKER_LIMIT);
        Self {
            workers,
            queue_capacity_per_worker: DEFAULT_QUEUE_CAPACITY_PER_WORKER,
        }
    }
}

/// Dedicated native browser worker pool.
///
/// Each worker owns one OS thread and one current-thread Tokio runtime. Native
/// page state and V8 `JsRuntime`s are created and used only inside a worker, so
/// public executor futures stay `Send` without sharing V8 isolates across
/// threads.
///
/// Rust-only: excluded from alef-generated polyglot bindings. Bindings drive
/// the engine API directly and don't manipulate the executor.
#[derive(Clone)]
#[cfg_attr(alef, alef(skip))]
pub struct NativeBrowserExecutor {
    inner: Arc<NativeBrowserExecutorInner>,
}

struct NativeBrowserExecutorInner {
    workers: Mutex<Vec<tokio::sync::mpsc::Sender<NativeBrowserJob>>>,
    handles: Mutex<Vec<JoinHandle<()>>>,
    next_worker: AtomicUsize,
}

enum NativeBrowserJob {
    Render {
        url: String,
        config: NativeBrowserConfig,
        reply: tokio::sync::oneshot::Sender<Result<RenderedPage, PageError>>,
    },
    Interact {
        url: String,
        config: NativeBrowserConfig,
        actions: Vec<NativePageAction>,
        post_navigation_wait: Option<Duration>,
        reply: tokio::sync::oneshot::Sender<Result<NativeInteractionResult, PageError>>,
    },
}

impl NativeBrowserExecutor {
    /// Start a native browser worker pool.
    pub fn new(config: NativeBrowserExecutorConfig) -> Result<Self, PageError> {
        if config.workers == 0 {
            return Err(PageError::ParseError(
                "native browser executor requires at least one worker".to_owned(),
            ));
        }
        if config.queue_capacity_per_worker == 0 {
            return Err(PageError::ParseError(
                "native browser executor requires queue_capacity_per_worker > 0".to_owned(),
            ));
        }

        let mut workers = Vec::with_capacity(config.workers);
        let mut handles = Vec::with_capacity(config.workers);
        for index in 0..config.workers {
            let (sender, receiver) = tokio::sync::mpsc::channel(config.queue_capacity_per_worker);
            let (startup_sender, startup_receiver) = std::sync::mpsc::channel();
            let handle = std::thread::Builder::new()
                .name(format!("crawlberg-native-browser-{index}"))
                .spawn(move || run_native_worker(receiver, startup_sender))
                .map_err(|e| PageError::NetworkError(format!("failed to start native browser worker: {e}")))?;

            match startup_receiver.recv() {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    let _ = handle.join();
                    return Err(PageError::NetworkError(format!(
                        "failed to start native browser worker runtime: {error}"
                    )));
                }
                Err(error) => {
                    let _ = handle.join();
                    return Err(PageError::NetworkError(format!(
                        "native browser worker stopped during startup: {error}"
                    )));
                }
            }

            workers.push(sender);
            handles.push(handle);
        }

        Ok(Self {
            inner: Arc::new(NativeBrowserExecutorInner {
                workers: Mutex::new(workers),
                handles: Mutex::new(handles),
                next_worker: AtomicUsize::new(0),
            }),
        })
    }

    /// Navigate to a URL and return the rendered page.
    pub async fn render_url(&self, url: &str, config: &NativeBrowserConfig) -> Result<RenderedPage, PageError> {
        let (reply, result) = tokio::sync::oneshot::channel();
        let job = NativeBrowserJob::Render {
            url: url.to_owned(),
            config: config.clone(),
            reply,
        };
        self.send_job(job).await?;
        result.await.map_err(|_| {
            PageError::NetworkError("native browser worker stopped before returning render result".to_owned())
        })?
    }

    /// Navigate to a URL and execute page actions.
    pub async fn interact_url(
        &self,
        url: &str,
        config: &NativeBrowserConfig,
        actions: &[NativePageAction],
        post_navigation_wait: Option<Duration>,
    ) -> Result<NativeInteractionResult, PageError> {
        let (reply, result) = tokio::sync::oneshot::channel();
        let job = NativeBrowserJob::Interact {
            url: url.to_owned(),
            config: config.clone(),
            actions: actions.to_vec(),
            post_navigation_wait,
            reply,
        };
        self.send_job(job).await?;
        result.await.map_err(|_| {
            PageError::NetworkError("native browser worker stopped before returning interact result".to_owned())
        })?
    }

    async fn send_job(&self, mut job: NativeBrowserJob) -> Result<(), PageError> {
        let workers = self.worker_senders()?;
        let start = self.inner.next_worker.fetch_add(1, Ordering::Relaxed);

        for offset in 0..workers.len() {
            let index = (start + offset) % workers.len();
            match workers[index].send(job).await {
                Ok(()) => return Ok(()),
                Err(error) => {
                    job = error.0;
                }
            }
        }

        Err(PageError::NetworkError(
            "native browser worker pool is shut down".to_owned(),
        ))
    }

    fn worker_senders(&self) -> Result<Vec<tokio::sync::mpsc::Sender<NativeBrowserJob>>, PageError> {
        let workers = self
            .inner
            .workers
            .lock()
            .map_err(|_| PageError::NetworkError("native browser worker pool lock is poisoned".to_owned()))?;
        if workers.is_empty() {
            return Err(PageError::NetworkError(
                "native browser worker pool is shut down".to_owned(),
            ));
        }
        Ok(workers.clone())
    }
}

impl Drop for NativeBrowserExecutorInner {
    fn drop(&mut self) {
        if let Ok(mut workers) = self.workers.lock() {
            workers.clear();
        }
        if let Ok(mut handles) = self.handles.lock() {
            for handle in handles.drain(..) {
                let _ = handle.join();
            }
        }
    }
}

fn run_native_worker(
    mut receiver: tokio::sync::mpsc::Receiver<NativeBrowserJob>,
    startup_sender: std::sync::mpsc::Sender<Result<(), String>>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => {
            let _ = startup_sender.send(Ok(()));
            runtime
        }
        Err(error) => {
            let _ = startup_sender.send(Err(error.to_string()));
            return;
        }
    };

    runtime.block_on(async move {
        while let Some(job) = receiver.recv().await {
            match job {
                NativeBrowserJob::Render { url, config, reply } => {
                    let _ = reply.send(render_url_local(&url, &config).await);
                }
                NativeBrowserJob::Interact {
                    url,
                    config,
                    actions,
                    post_navigation_wait,
                    reply,
                } => {
                    let _ = reply.send(interact_url_local(&url, &config, &actions, post_navigation_wait).await);
                }
            }
        }
    });
}
