pub mod error;
pub mod odb;
pub mod repo;
pub mod network;
pub mod render;
pub mod tui;
pub mod config;

pub use error::{VcsError, VcsResult};
pub use repo::{Repo, RepoLock};
pub use odb::{Commit, Tree, TreeEntry, StatusInfo, FileDiff};
pub use network::{run_server, NetworkObject, UploadRequest};

pub const VCS_DIR_NAME: &str = ".simplevcs";
