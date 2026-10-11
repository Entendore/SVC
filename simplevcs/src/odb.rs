use crate::error::VcsResult;
use serde::{Deserialize, Serialize};
use sha2::{Sha256, Digest};
use similar::{ChangeTag, TextDiff};
use std::fs::{self, File};
use std::io::{BufReader, Read, Write};
use std::path::Path;

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Commit {
    pub hash: String,
    pub tree: String,
    #[serde(default)]
    pub parents: smallvec::SmallVec<[String; 4]>,
    pub timestamp: String,
    #[serde(default)]
    pub ts_ms: i64,
    pub author: String,
    pub message: String,
    #[serde(default)]
    pub signature: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TreeEntry { pub mode: u32, pub hash: String }

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Tree { pub files: std::collections::BTreeMap<String, TreeEntry> }

#[derive(Serialize, Deserialize, Debug, Default)]
pub struct StatusInfo {
    pub head: String,
    pub staged: Vec<String>,
    pub modified: Vec<String>,
    pub untracked: Vec<String>,
    pub deleted: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct FileDiff { pub file: String, pub binary: bool, pub content: Option<String> }

pub fn write_string_atomic(path: &Path, data: &str) -> VcsResult<()> {
    let tmp_path = path.with_extension("tmp");
    let mut file = File::create(&tmp_path)?;
    file.write_all(data.as_bytes())?;
    file.sync_all()?;
    fs::rename(&tmp_path, path)?;
    Ok(())
}

pub fn hash_file(path: &Path) -> VcsResult<String> {
    let file = File::open(path)?;
    let mut hasher = Sha256::new();
    let metadata = file.metadata()?;
    if metadata.len() > 16384 {
        let mmap = unsafe { memmap2::Mmap::map(&file)? };
        hasher.update(&mmap[..]);
    } else {
        let mut reader = BufReader::new(file);
        let mut buffer = [0; 4096];
        loop { let n = reader.read(&mut buffer)?; if n == 0 { break; } hasher.update(&buffer[..n]); }
    }
    Ok(hex::encode(hasher.finalize()))
}

pub fn unified_diff(old: &str, new: &str) -> String {
    let diff = TextDiff::from_lines(old, new);
    let mut out = String::new();
    for change in diff.iter_all_changes() {
        let sign = match change.tag() {
            ChangeTag::Delete => "-",
            ChangeTag::Insert => "+",
            ChangeTag::Equal => " ",
        };
        out.push_str(&format!("{}{}\n", sign, change.value()));
    }
    out
}

pub fn diff_lcs(old: &[char], new: &[char]) -> Vec<(char, Option<usize>, Option<usize>)> {
    let m = old.len();
    let n = new.len();
    let mut dp = vec![0u32; (m + 1) * (n + 1)];
    for i in 1..=m {
        for j in 1..=n {
            if old[i - 1] == new[j - 1] { dp[i * (n + 1) + j] = dp[(i - 1) * (n + 1) + (j - 1)] + 1; }
            else { dp[i * (n + 1) + j] = dp[(i - 1) * (n + 1) + j].max(dp[i * (n + 1) + (j - 1)]); }
        }
    }
    let mut i = m; let mut j = n;
    let mut result = Vec::with_capacity(m.max(n));
    while i > 0 || j > 0 {
        if i > 0 && j > 0 && old[i - 1] == new[j - 1] {
            result.push((old[i - 1], Some(i - 1), Some(j - 1))); i -= 1; j -= 1;
        } else if j > 0 && (i == 0 || dp[i * (n + 1) + (j - 1)] >= dp[(i - 1) * (n + 1) + j]) {
            result.push((new[j - 1], None, Some(j - 1))); j -= 1;
        } else {
            result.push((old[i - 1], Some(i - 1), None)); i -= 1;
        }
    }
    result.reverse();
    result
}

pub fn is_binary(bytes: &[u8]) -> bool {
    if bytes.is_empty() { return false; }
    bytes.iter().take(8192).any(|&b| b == 0)
}

pub fn verify_integrity(hash: &str, data: &[u8]) -> bool {
    let computed = hex::encode(Sha256::digest(data));
    computed == hash
}

pub fn is_path_safe(root: &Path, file_str: &str) -> bool {
    let target_path = root.join(file_str);
    if !target_path.exists() {
        if let Some(parent) = target_path.parent() {
            if let Ok(canonical_parent) = fs::canonicalize(parent) {
                if let Ok(canonical_root) = fs::canonicalize(root) {
                    return canonical_parent.starts_with(&canonical_root);
                }
            }
        }
        return true;
    }
    if let Ok(canonical_target) = fs::canonicalize(&target_path) {
        if let Ok(canonical_root) = fs::canonicalize(root) {
            return canonical_target.starts_with(&canonical_root);
        }
    }
    false
}
