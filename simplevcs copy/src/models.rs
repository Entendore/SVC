use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::num::NonZeroU64;
use derive_more::{Deref, Display, From, Into};
use smallvec::SmallVec;
use typed_builder::TypedBuilder;
use std::marker::PhantomData;

#[derive(
    Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq, Hash, 
    PartialOrd, Ord, Deref, Display, From, Into
)]
pub struct Id<T> {
    pub hash: [u8; 20],
    _marker: PhantomData<T>,
}

pub struct CommitMarker;
pub struct FileMarker;

pub type CommitId = Id<CommitMarker>;
pub type FileId = Id<FileMarker>;

impl<T> Id<T> {
    pub fn to_hex(&self) -> String { self.0.iter().map(|b| format!("{:02x}", b)).collect() }
    pub fn short(&self) -> String { self.to_hex()[..7].to_string() }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, TypedBuilder)]
pub struct Commit {
    #[builder(default)]
    pub hash: String, // Kept as String for simplified map keys in this example
    #[builder(default)]
    pub parent: Option<String>,
    #[builder(default)]
    pub timestamp: String,
    #[builder(default)]
    pub ts_ms: i64,
    #[builder(default)]
    pub gen: NonZeroU64,
    pub author: String,
    #[builder(default)]
    pub co_authors: SmallVec<[String; 4]>,
    pub message: String,
    #[builder(default)]
    pub files: BTreeMap<String, String>,
    #[builder(default)]
    pub merged_from: SmallVec<[String; 4]>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct RepoConfig { pub name: String }

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct MergeState {
    pub parent: String,
    pub target_hashes: Vec<String>,
    pub author: String,
    pub message: Option<String>,
    pub no_commit: bool,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct FileCache {
    pub mtime: u64,
    pub size: u64,
    pub hash: String,
}
