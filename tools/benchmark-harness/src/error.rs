//! Error types for the benchmark harness.

use thiserror::Error;

/// All errors that can arise during benchmark execution.
#[derive(Debug, Error)]
pub enum Error {
    /// An I/O operation failed.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON serialization or deserialization failed.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// An HTTP request failed.
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    /// The benchmark configuration is invalid.
    #[error("invalid config: {0}")]
    Config(String),

    /// A fixture could not be loaded or parsed.
    #[error("fixture error: {0}")]
    Fixture(String),

    /// A scrape adapter reported an error.
    #[error("adapter error: {0}")]
    Adapter(String),

    /// A profiling operation failed.
    #[error("profiling error: {0}")]
    Profiling(String),

    /// Process inspection could not be initialized for benchmark measurements.
    #[error("failed to initialize benchmark process inspection: {0}")]
    Monitoring(#[from] sysinfo::Error),

    /// A dataset download or access operation failed.
    #[error("dataset error: {0}")]
    Dataset(String),
}

/// Convenience `Result` alias using the harness [`Error`] type.
pub type Result<T> = std::result::Result<T, Error>;
