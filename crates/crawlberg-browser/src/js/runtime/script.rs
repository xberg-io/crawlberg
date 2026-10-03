//! Statement-level script execution: the wall-clock watchdog that reclaims a wedged
//! isolate, and the event-loop drivers used to settle pending promises/timers.

use std::task::Poll;

use deno_core::error::{CoreError, CoreErrorKind};

use super::BrowserJsRuntime;

/// How long an awaited evaluation may wait for its own promise to settle.
const AWAIT_BUDGET: tokio::time::Duration = tokio::time::Duration::from_secs(5);

/// Wait until the flag in `pair` is set or `timeout` passes. `true` means the flag was set in time.
pub(super) fn wait_for_cancel(
    pair: &(std::sync::Mutex<bool>, std::sync::Condvar),
    timeout: std::time::Duration,
) -> bool {
    let (lock, cvar) = pair;
    // ~keep Read the flag before waiting: a script can finish before this thread starts, and its notify is lost.
    let cancelled = lock.lock().unwrap();
    let (_cancelled, wait) = cvar
        .wait_timeout_while(cancelled, timeout, |cancelled| !*cancelled)
        .unwrap();
    !wait.timed_out()
}

impl BrowserJsRuntime {
    pub fn execute_script(&mut self, _name: &str, source: &str) -> Result<(), String> {
        self.runtime
            .execute_script("<script>", source.to_string())
            .map_err(|e| format!("JS error: {}", e))?;
        Ok(())
    }

    pub fn execute_script_guarded(&mut self, _name: &str, source: &str) -> Result<(), String> {
        // ~keep Source length is not a runtime bound; even tiny infinite loops must run under the watchdog.
        self.execute_script_with_timeout(source, std::time::Duration::from_secs(5))
    }

    pub fn execute_script_with_timeout(&mut self, source: &str, timeout: std::time::Duration) -> Result<(), String> {
        if timeout.is_zero() {
            self.runtime
                .execute_script("<script>", source.to_string())
                .map_err(|e| format!("JS error: {}", e))?;
            return Ok(());
        }

        let isolate_handle = self.runtime.v8_isolate().thread_safe_handle();

        let pair = std::sync::Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let pair_clone = pair.clone();

        let watchdog = std::thread::spawn(move || {
            if !wait_for_cancel(&pair_clone, timeout) {
                isolate_handle.terminate_execution();
            }
        });

        let result = self.runtime.execute_script("<script>", source.to_string());

        {
            let (lock, cvar) = &*pair;
            let mut cancelled = lock.lock().unwrap();
            *cancelled = true;
            cvar.notify_one();
        }
        let _ = watchdog.join();

        self.runtime.v8_isolate().cancel_terminate_execution();

        match result {
            Ok(_) => Ok(()),
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("Uncaught Error: execution terminated") {
                    tracing::warn!("Script killed after {}s timeout", timeout.as_secs());
                    Ok(())
                } else {
                    Err(format!("JS error: {}", msg))
                }
            }
        }
    }

    pub async fn run_event_loop(&mut self) -> Result<(), String> {
        self.runtime
            .run_event_loop(deno_core::PollEventLoopOptions::default())
            .await
            .map_err(|e| format!("Event loop error: {}", e))
    }

    /// Drive the event loop until an awaited evaluation's `promise` settles, for at most [`AWAIT_BUDGET`].
    pub async fn resolve_promises(
        &mut self,
        promise: deno_core::v8::Global<deno_core::v8::Value>,
    ) -> Result<(), String> {
        let settled = self.runtime.resolve(promise);
        let own = async { settled.await.map(|_| ()).map_err(CoreError::from) };
        match self.run_until_settled(own, AWAIT_BUDGET, "an awaited evaluation").await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(format!("Awaited evaluation error: {}", e)),
            Err(_) => Err(format!("Awaited evaluation did not settle within {:?}", AWAIT_BUDGET)),
        }
    }

    /// Drive the event loop until `own` settles, for at most `budget`. `Err` means the budget ran out.
    pub(super) async fn run_until_settled<T>(
        &mut self,
        own: impl Future<Output = Result<T, CoreError>>,
        budget: tokio::time::Duration,
        name: &str,
    ) -> Result<Result<T, CoreError>, tokio::time::error::Elapsed> {
        // ~keep Wait for this evaluation, not for the whole event loop to go idle: an earlier script's
        // ~keep stalled await or fetch leaves an op that never settles, and every later wait would sit on it.
        let mut own = std::pin::pin!(own);
        let options = deno_core::PollEventLoopOptions::default();
        let runtime = &mut self.runtime;
        tokio::time::timeout(
            budget,
            std::future::poll_fn(|cx| {
                if let Poll::Ready(settled) = own.as_mut().poll(cx) {
                    return Poll::Ready(settled);
                }
                let error = match runtime.poll_event_loop(cx, options) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(Ok(())) => {
                        return Poll::Ready(match own.as_mut().poll(cx) {
                            Poll::Ready(settled) => settled,
                            Poll::Pending => Err(CoreError(Box::new(CoreErrorKind::PendingPromiseResolution))),
                        });
                    }
                    Poll::Ready(Err(error)) => error,
                };
                // ~keep An error the event loop reports while this evaluation is still running belongs to other
                // ~keep work on the page, such as a fetch an earlier script did not await, and must not cut this
                // ~keep evaluation short. The wait goes on only while the loop has other work: with none left, the
                // ~keep error is deno_core reporting this evaluation's await as stalled, and it repeats on every poll.
                // ~keep The same tick can also settle this evaluation, so it is checked again before the error wins.
                match runtime.poll_event_loop(cx, options) {
                    Poll::Pending => {
                        tracing::warn!("Script error while {} ran: {}", name, error);
                        Poll::Pending
                    }
                    Poll::Ready(_) => Poll::Ready(match own.as_mut().poll(cx) {
                        Poll::Ready(settled) => {
                            tracing::warn!("Script error while {} ran: {}", name, error);
                            settled
                        }
                        Poll::Pending => Err(error),
                    }),
                }
            }),
        )
        .await
    }
}
