use thiserror::Error;

pub type Result<T, E = VersionError> = std::result::Result<T, E>;

#[derive(Debug, Error)]
pub enum VersionError {
    #[error("Invalid channel")]
    InvalidChannel(String),

    #[error("Invalid build")]
    InvalidBuild(String),

    #[error("Invalid milestone")]
    InvalidMilestone(String),

    #[error("Invalid revision")]
    InvalidRevision(String),

    #[error("Invalid version")]
    InvalidVersion(String),

    #[error("Failed to resolve version")]
    ResolveFailed(#[source] anyhow::Error),
}
