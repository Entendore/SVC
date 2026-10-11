use crate::error::VcsError;
use crate::models::FileCache;
use camino::{Utf8Path, Utf8PathBuf};
use memmap2::Mmap;
use serde::de::DeserializeOwned;
use serde::Serialize;
use sha1::{Digest, Sha1};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::sync::OnceLock;
use std::time::SystemTime;
use globset::{GlobSet, GlobSetBuilder};

pub const VCS_DIR: &str = ".simplevcs";
pub const OBJECTS_DIR: &str = ".simplevcs/objects";
pub const INDEX_FILE: &str = ".simplevcs/index.bin";
pub const COMMITS_FILE: &str = ".simplevcs/commits.bin";
pub const HEAD_FILE: &str = ".simplevcs/HEAD";
pub const CONFIG_FILE: &str = ".simplevcs/config.json";
pub const LINKS_FILE: &str = ".simplevcs/links.json";
pub const MERGE_STATE_FILE: &str = ".simplevcs/MERGE_STATE";
pub const CACHE_FILE: &str = ".simplevcs/cache.bin";
pub const BACKUP_FILE: &str = ".simplevcs/BACKUP";
pub const IGNORE_FILE: &str = ".simplevcsignore";

static IGNORE_SET: OnceLock<Option<GlobSet>> = OnceLock::new();

pub trait PathExt {
    fn ensure_parent_dir(&self) -> Result<(), VcsError>;
}

impl PathExt for Utf8Path {
    fn ensure_parent_dir(&self) -> Result<(), VcsError> {
        if let Some(parent) = self.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(())
    }
}

pub fn get_ignore_set() -> &'static Option<GlobSet> {
    IGNORE_SET.get_or_init(|| load_ignore_set_from_disk().ok())
}

fn load_ignore_set_from_disk() -> Result<GlobSet, VcsError> {
    let path = Utf8Path::new(IGNORE_FILE);
    if !path.exists() { return Ok(GlobSetBuilder::new().build().unwrap()); }
    let content = fs::read_to_string(path)?;
    let mut builder = GlobSetBuilder::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') { continue; }
        if let Ok(glob) = globset::Glob::new(line) { builder.add(glob); }
    }
    Ok(builder.build()?)
}

pub fn is_ignored(path: &str, ignore_set: &Option<GlobSet>) -> bool {
    if let Some(set) = ignore_set { return set.is_match(path); }
    false
}

pub fn write_json_atomic<T: Serialize>(path: &Utf8Path, data: &T) -> Result<(), VcsError> {
    let tmp_path = path.with_extension("json.tmp");
    let file = File::create(&tmp_path)?;
    let mut writer = BufWriter::new(file);
    serde_json::to_writer_pretty(&mut writer, data)?;
    writer.flush()?;
    writer.into_inner()?.sync_all()?;
    fs::rename(&tmp_path, path)?;
    Ok(())
}

pub fn write_string_atomic(path: &Utf8Path, data: &str) -> Result<(), VcsError> {
    let tmp_path = path.with_extension("tmp");
    let mut file = File::create(&tmp_path)?;
    file.write_all(data.as_bytes())?;
    file.sync_all()?;
    fs::rename(&tmp_path, path)?;
    Ok(())
}

pub fn write_bincode_atomic<T: Serialize>(path: &Utf8Path, data: &T) -> Result<(), VcsError> {
    let tmp_path = path.with_extension("bin.tmp");
    let file = File::create(&tmp_path)?;
    let mut writer = BufWriter::new(file);
    let bytes = bincode::serialize(data)?;
    writer.write_all(&bytes)?;
    writer.flush()?;
    writer.into_inner()?.sync_all()?;
    fs::rename(&tmp_path, path)?;
    Ok(())
}

pub fn load_json_file<T: DeserializeOwned>(path: &Utf8Path) -> Result<T, VcsError> {
    if !path.exists() { return Err(VcsError::Io(io::Error::new(io::ErrorKind::NotFound, "File not found"))); }
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    if metadata.len() > 16384 {
        let mmap = unsafe { Mmap::map(&file)? };
        Ok(serde_json::from_slice(&mmap[..])?)
    } else {
        let mut buf = Vec::with_capacity(metadata.len() as usize);
        io::BufReader::new(file).read_to_end(&mut buf)?;
        Ok(serde_json::from_slice(&buf)?)
    }
}

pub fn load_bincode_file<T: DeserializeOwned>(path: &Utf8Path) -> Result<T, VcsError> {
    if !path.exists() { return Err(VcsError::Io(io::Error::new(io::ErrorKind::NotFound, "File not found"))); }
    let file = File::open(path)?;
    let mmap = unsafe { Mmap::map(&file)? };
    Ok(bincode::deserialize(&mmap[..])?)
}

pub fn get_object_path(hash_hex: &str) -> Utf8PathBuf {
    Utf8PathBuf::from(OBJECTS_DIR).join(&hash_hex[..2]).join(&hash_hex[2..])
}

pub fn load_index() -> Result<BTreeMap<String, String>, VcsError> {
    let path = Utf8Path::new(INDEX_FILE);
    if !path.exists() { return Ok(BTreeMap::new()); }
    load_bincode_file(path)
}

pub fn save_index(index: &BTreeMap<String, String>) -> Result<(), VcsError> {
    write_bincode_atomic(Utf8Path::new(INDEX_FILE), index)
}

pub fn load_cache() -> Result<HashMap<String, FileCache>, VcsError> {
    let path = Utf8Path::new(CACHE_FILE);
    if !path.exists() { return Ok(HashMap::new()); }
    load_bincode_file(path)
}

pub fn save_cache(cache: &HashMap<String, FileCache>) -> Result<(), VcsError> {
    write_bincode_atomic(Utf8Path::new(CACHE_FILE), cache)
}

pub fn get_file_stats(path: &Utf8Path) -> Option<(u64, u64)> {
    let metadata = fs::metadata(path).ok()?;
    let mtime = metadata.modified().ok()?.duration_since(SystemTime::UNIX_EPOCH).ok()?.as_secs();
    let size = metadata.len();
    Some((mtime, size))
}

pub fn hash_file_uncached(path: &Utf8Path) -> Result<String, VcsError> {
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    if metadata.len() == 0 { return Ok("da39a3ee5e6b4b0d3255bfef95601890afd80709".to_string()); }
    let mmap = unsafe { Mmap::map(&file)? };
    let mut hasher = Sha1::new();
    hasher.update(&mmap[..]);
    Ok(format!("{:x}", hasher.finalize()))
}

pub fn hash_bytes(content: &[u8]) -> String {
    if content.is_empty() { return "da39a3ee5e6b4b0d3255bfef95601890afd80709".to_string(); }
    let mut hasher = Sha1::new();
    hasher.update(content);
    format!("{:x}", hasher.finalize())
}

pub fn is_binary(bytes: &[u8]) -> bool {
    if bytes.is_empty() { return false; }
    let check_len = std::cmp::min(bytes.len(), 8192);
    bytes[..check_len].contains(&0u8)
}

pub fn get_author_color(author: &str) -> &'static str {
    let colors = ["\x1B[31m", "\x1B[32m", "\x1B[33m", "\x1B[34m", "\x1B[35m", "\x1B[36m"];
    let mut hash = 0u32;
    for c in author.chars() {
        hash = hash.wrapping_add(c as u32);
        hash = hash.wrapping_mul(31);
    }
    colors[(hash as usize) % colors.len()]
}

// Utf8Path extension for with_extension (since camino doesn't have it natively in older versions)
pub trait Utf8PathExt {
    fn with_extension(&self, ext: &str) -> Utf8PathBuf;
}

impl Utf8PathExt for Utf8Path {
    fn with_extension(&self, ext: &str) -> Utf8PathBuf {
        let mut buf = self.to_path_buf();
        buf.set_extension(ext);
        buf
    }
}
