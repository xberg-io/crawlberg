//! CPU and memory profiling support.
//!
//! When compiled with `--features profiling`, [`ProfileGuard`] captures a
//! `pprof` CPU profile for its lifetime and writes a flamegraph SVG on drop.
//! When compiled without the feature the guard is a zero-cost no-op, so call
//! sites need no `#[cfg]` annotations.

use std::path::Path;

#[cfg(feature = "profiling")]
use crate::error::Error;
use crate::error::Result;

/// A scoped CPU-profiling guard.
///
/// Create one at the start of a profiling session and let it drop at the end.
/// With `--features profiling` the guard drives `pprof`; without it every
/// method is a no-op and the type has zero size.
#[cfg(feature = "profiling")]
pub struct ProfileGuard {
    guard: pprof::ProfilerGuard<'static>,
    output_path: std::path::PathBuf,
}

#[cfg(not(feature = "profiling"))]
pub struct ProfileGuard;

impl ProfileGuard {
    /// Start a profiling session that samples at `frequency` Hz and writes the
    /// resulting flamegraph to `output_path` when dropped.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Profiling`] if `pprof` fails to start (feature-gated
    /// build only; always returns `Ok` in the no-op build).
    #[cfg(feature = "profiling")]
    pub fn start(frequency: i32, output_path: impl AsRef<Path>) -> Result<Self> {
        let guard = pprof::ProfilerGuardBuilder::default()
            .frequency(frequency)
            .blocklist(&["libc", "libgcc", "pthread", "vdso"])
            .build()
            .map_err(|e| Error::Profiling(format!("failed to start profiler: {e}")))?;
        Ok(Self {
            guard,
            output_path: output_path.as_ref().to_owned(),
        })
    }

    #[cfg(not(feature = "profiling"))]
    pub fn start(_frequency: i32, _output_path: impl AsRef<Path>) -> Result<Self> {
        Ok(Self)
    }

    /// Finalise the profile and write a flamegraph SVG to the configured path.
    ///
    /// This is called automatically on drop, but calling it explicitly lets you
    /// surface any write errors.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Profiling`] if the flamegraph cannot be rendered or the
    /// output file cannot be written (feature-gated build only).
    #[cfg(feature = "profiling")]
    pub fn finish(self) -> Result<()> {
        use std::fs::File;

        let report = self
            .guard
            .report()
            .build()
            .map_err(|e| Error::Profiling(format!("failed to build profile report: {e}")))?;

        if let Some(parent) = self.output_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::Profiling(format!("failed to create output dir: {e}")))?;
        }

        let file = File::create(&self.output_path)
            .map_err(|e| Error::Profiling(format!("failed to create flamegraph file: {e}")))?;

        report
            .flamegraph(file)
            .map_err(|e| Error::Profiling(format!("failed to write flamegraph: {e}")))?;

        Ok(())
    }

    #[cfg(not(feature = "profiling"))]
    pub fn finish(self) -> Result<()> {
        Ok(())
    }
}

#[cfg(feature = "profiling")]
impl Drop for ProfileGuard {
    fn drop(&mut self) {
        use std::fs::File;

        let report = match self.guard.report().build() {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "failed to build profile report on drop");
                return;
            }
        };

        if let Some(parent) = self.output_path.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            tracing::warn!(error = %e, "failed to create profile output dir on drop");
            return;
        }

        match File::create(&self.output_path) {
            Ok(file) => {
                if let Err(e) = report.flamegraph(file) {
                    tracing::warn!(error = %e, "failed to write flamegraph on drop");
                }
            }
            Err(e) => {
                tracing::warn!(
                    path = %self.output_path.display(),
                    error = %e,
                    "failed to create flamegraph file on drop",
                );
            }
        }
    }
}
