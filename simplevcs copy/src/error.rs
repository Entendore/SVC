use thiserror::Error;
use std::fmt::Display;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum VcsError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("Bincode error: {0}")]
    Bincode(#[from] bincode::Error),
    #[error("Zstd compression error: {0}")]
    Zstd(#[from] zstd::Error),
    #[error("Glob pattern error: {0}")]
    Glob(#[from] globset::Error),
    #[error("{context}: {source}")]
    Contextual {
        context: String,
        #[source]
        source: Box<VcsError>,
    },
    #[error("Not a repository. Run 'init' first.")]
    NotInitialized,
    #[error("Commit {0} not found")]
    CommitNotFound(String),
    #[error("File {0} not found")]
    FileNotFound(String),
    #[error("No HEAD to merge into. Commit something first.")]
    NoHead,
    #[error("Cannot merge a commit with itself.")]
    SelfMerge,
    #[error("Linked repo not found at {0}")]
    RepoNotFound(String),
    #[error("Cannot merge with uncommitted changes. Commit or reset first.")]
    MergeDirty,
    #[error("Merge in progress. Fix conflicts and run 'commit', or run 'merge --abort'.")]
    MergeInProgress,
}

pub trait Context<T> {
    fn context(self, msg: impl Display) -> Result<T, VcsError>;
}

impl<T, E: Into<VcsError>> Context<T> for Result<T, E> {
    fn context(self, msg: impl Display) -> Result<T, VcsError> {
        self.map_err(|e| VcsError::Contextual {
            context: msg.to_string(),
            source: Box::new(e.into()),
        })
    }
}
