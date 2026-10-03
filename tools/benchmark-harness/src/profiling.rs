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

#[cfg(feature = "profiling")]
fn push_folded_label(line: &mut String, label: &str) {
    for character in label.chars() {
        match character {
            ';' => line.push(':'),
            '\n' | '\r' => line.push(' '),
            _ => line.push(character),
        }
    }
}

#[cfg(feature = "profiling")]
fn folded_lines(data: &std::collections::HashMap<pprof::Frames, isize>) -> Vec<String> {
    let mut lines = Vec::with_capacity(data.len());
    for (frames, count) in data {
        let mut line = String::new();
        push_folded_label(&mut line, &frames.thread_name_or_id());

        for frame in frames.frames.iter().rev() {
            for symbol in frame.iter().rev() {
                line.push(';');
                push_folded_label(&mut line, &symbol.name());
            }
        }

        line.push(' ');
        line.push_str(&count.to_string());
        lines.push(line);
    }
    lines.sort_unstable();
    lines
}

#[cfg(feature = "profiling")]
fn write_flamegraph<W>(report: &pprof::Report, writer: W) -> std::io::Result<()>
where
    W: std::io::Write,
{
    let lines = folded_lines(&report.data);
    if lines.is_empty() {
        return Ok(());
    }

    inferno::flamegraph::from_lines(
        &mut inferno::flamegraph::Options::default(),
        lines.iter().map(String::as_str),
        writer,
    )
}

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

        write_flamegraph(&report, file).map_err(|e| Error::Profiling(format!("failed to write flamegraph: {e}")))?;

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
                if let Err(e) = write_flamegraph(&report, file) {
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

#[cfg(all(test, feature = "profiling"))]
mod tests {
    use std::collections::HashMap;
    use std::time::SystemTime;

    use pprof::{Frames, Symbol};

    use super::folded_lines;

    fn symbol(name: &str) -> Symbol {
        Symbol {
            name: Some(name.as_bytes().to_vec()),
            addr: None,
            lineno: None,
            filename: None,
        }
    }

    fn frames(thread_name: &str, thread_id: u64, symbols: &[&[&str]]) -> Frames {
        Frames {
            frames: symbols
                .iter()
                .map(|frame| frame.iter().map(|name| symbol(name)).collect())
                .collect(),
            thread_name: thread_name.to_owned(),
            thread_id,
            sample_timestamp: SystemTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn folded_lines_should_reverse_frames_and_sort_output() {
        let mut data = HashMap::new();
        data.insert(frames("worker", 1, &[&["leaf", "inlined"], &["root"]]), 3);
        data.insert(frames("alpha", 2, &[&["child"], &["parent"]]), 5);

        assert_eq!(
            folded_lines(&data),
            vec!["alpha;parent;child 5", "worker;root;inlined;leaf 3"]
        );
    }

    #[test]
    fn folded_lines_should_escape_format_delimiters() {
        let mut data = HashMap::new();
        data.insert(frames("worker;one\n", 1, &[&["leaf\rname"], &["[u8; 8]"]]), 2);

        assert_eq!(folded_lines(&data), vec!["worker:one ;[u8: 8];leaf name 2"]);
    }

    #[test]
    fn folded_lines_should_return_empty_output_for_empty_data() {
        assert_eq!(folded_lines(&HashMap::new()), Vec::<String>::new());
    }

    #[test]
    fn folded_lines_should_use_thread_id_when_name_is_empty() {
        let mut data = HashMap::new();
        data.insert(frames("", 42, &[]), 1);

        assert_eq!(folded_lines(&data), vec!["42 1"]);
    }
}
