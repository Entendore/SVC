use thiserror::Error;

#[derive(Debug, Error)]
pub enum VcsError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("Bincode error: {0}")]
    Bincode(#[from] bincode::Error),
    #[error("Sled DB error: {0}")]
    Sled(#[from] sled::Error),
    #[error("Network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("Not a repository. Run 'init' first.")]
    NotInitialized,
    #[error("Commit or tag '{0}' not found")]
    CommitNotFound(String),
    #[error("File {0} not found")]
    FileNotFound(String),
    #[error("No HEAD to merge into. Commit something first.")]
    NoHead,
    #[error("Path traversal detected for file: {0}")]
    PathTraversal(String),
    #[error("Merge conflict detected. Run 'simplevcs mergetool'.")]
    MergeConflict,
    #[error("Local changes detected. Commit or restore before pulling.")]
    UncommittedChanges,
    #[error("TUI error: {0}")]
    Tui(String),
    #[error("Git import failed: {0}")]
    GitImportFailed(String),
    #[error("Remote '{0}' not found")]
    RemoteNotFound(String),
    #[error("Auth failed: {0}")]
    AuthFailed(String),
    #[error("FFmpeg failed to start or stream.")]
    FfmpegFailed,
    #[error("FFmpeg not found. Please install it to use the render command.")]
    FfmpegNotFound,
    #[error("Data integrity check failed for object {0}. Object may be corrupted or tampered with.")]
    IntegrityError(String),
    #[error("Payload too large. Maximum size is 2GB.")]
    PayloadTooLarge,
    #[error("Packfile integrity check failed. The stream may be corrupted or tampered with.")]
    PackfileIntegrityFailed,
    #[error("Local changes detected. Commit them or use --force to overwrite.")]
    UncommittedChangesRestore,
    #[error("Hook '{0}' failed. Aborting operation.")]
    HookFailed(String),
    #[error("Bisect error: {0}")]
    Bisect(String),
    #[error("No stashed changes found.")]
    NoStash,
    #[error("Cryptographic error: {0}")]
    Crypto(String),
    #[error("Commit signature is missing or invalid.")]
    InvalidSignature,
    #[error("Audit log integrity check failed. The database may have been tampered with.")]
    AuditTampered,
    #[error("Bundle error: {0}")]
    Bundle(String),
    #[error("TLS configuration error: {0}")]
    TlsConfig(String),
    #[error("Server-side hook '{0}' failed. Push rejected.")]
    ServerHookFailed(String),
    #[error("Filesystem check failed. {0} objects are corrupted.")]
    FsckFailed(usize),
    #[error("Hook '{0}' failed with output:\n{1}")]
    HookFailedWithOutput(String, String),
    #[error("Unauthorized. Token invalid or not recognized.")]
    Unauthorized,
    #[error("Symlink target escapes repository root. Potential security risk.")]
    SymlinkEscape,
}

pub type VcsResult<T> = Result<T, VcsError>;
