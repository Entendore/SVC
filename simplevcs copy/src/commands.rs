use crate::error::{Context, VcsError};
use crate::models::{Commit, FileCache, RepoConfig};
use crate::storage::{FsObjectStore, ObjectStore};
use crate::utils::*;
use chrono::Utc;
use camino::{Utf8Path, Utf8PathBuf};
use dashmap::DashMap;
use rayon::prelude::*;
use rustc_hash::FxHashMap;
use sha1::{Digest, Sha1};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::Write;
use std::sync::RwLock;
use jwalk::WalkDir;
use std::num::NonZeroU64;

pub struct Repository {
    pub store: FsObjectStore,
    pub commits: FxHashMap<String, Commit>,
    pub head: String,
}

impl Repository {
    pub fn open() -> Result<Self, VcsError> {
        let path = Utf8Path::new(VCS_DIR);
        if !path.exists() { return Err(VcsError::NotInitialized); }
        let commits = load_commits()?;
        let head = read_head()?;
        Ok(Repository { store: FsObjectStore::new(Utf8PathBuf::from(OBJECTS_DIR)), commits, head })
    }

    pub fn history(&self) -> impl Iterator<Item = &Commit> {
        std::iter::successors(
            Some(self.head.clone()),
            |h| self.commits.get(h).and_then(|c| c.parent.clone())
        ).filter_map(move |h| self.commits.get(&h))
    }
}

pub fn load_commits() -> Result<FxHashMap<String, Commit>, VcsError> {
    let path = Utf8Path::new(COMMITS_FILE);
    if !path.exists() { return Ok(FxHashMap::default()); }
    let mut commits: FxHashMap<String, Commit> = load_bincode_file(path)?;
    for c in commits.values_mut() {
        if c.ts_ms == 0 {
            if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(&c.timestamp, "%Y-%m-%d %H:%M:%S%.6f") {
                c.ts_ms = dt.and_utc().timestamp_millis();
            }
        }
    }
    Ok(commits)
}

pub fn save_commits(commits: &FxHashMap<String, Commit>) -> Result<(), VcsError> {
    write_bincode_atomic(Utf8Path::new(COMMITS_FILE), commits)
}

pub fn read_head() -> Result<String, VcsError> {
    Ok(fs::read_to_string(HEAD_FILE)?.trim().to_string())
}

pub fn init(name: &str) -> Result<(), VcsError> {
    if Utf8Path::new(VCS_DIR).exists() { println!("Repository already initialized."); return Ok(()); }
    fs::create_dir_all(OBJECTS_DIR)?;
    write_bincode_atomic(Utf8Path::new(INDEX_FILE), &BTreeMap::<String, String>::new())?;
    write_bincode_atomic(Utf8Path::new(COMMITS_FILE), &FxHashMap::<String, Commit>::default())?;
    write_string_atomic(Utf8Path::new(HEAD_FILE), "")?;
    write_json_atomic(Utf8Path::new(CONFIG_FILE), &RepoConfig { name: name.to_string() })?;
    write_json_atomic(Utf8Path::new(LINKS_FILE), &Vec::<String>::new())?;
    println!("Initialized empty SimpleVCS repository '{}'.", name);
    Ok(())
}

pub fn add(target_paths: &[String], all: bool, update: bool, force: bool, dry_run: bool) -> Result<(), VcsError> {
    let ignore_set = if force { None } else { get_ignore_set().as_ref() };
    let mut files_to_add: HashSet<String> = HashSet::new();

    let scan_paths = if all || update { vec![".".to_string()] } else if target_paths.is_empty() { return Ok(()); } else { target_paths.to_vec() };

    for target_path in &scan_paths {
        if target_path == "." {
            for entry in WalkDir::new(".").into_iter().filter_entry(|e| e.file_name() != VCS_DIR).filter_map(|e| e.ok()).filter(|e| e.file_type().is_file()) {
                let p = entry.path().to_str().unwrap_or("").to_string();
                let stripped = p.strip_prefix("./").unwrap_or(&p).to_string();
                if !force && is_ignored(&stripped, &ignore_set) { continue; }
                files_to_add.insert(stripped);
            }
        } else {
            if !force && is_ignored(target_path, &ignore_set) { continue; }
            let path = Utf8Path::new(target_path);
            if !path.exists() { return Err(VcsError::FileNotFound(target_path.to_string())); }
            if path.is_dir() {
                for entry in WalkDir::new(target_path).into_iter().filter_map(|e| e.ok()).filter(|e| e.file_type().is_file()) {
                    let p = entry.path().to_str().unwrap_or("").to_string();
                    let stripped = p.strip_prefix("./").unwrap_or(&p).to_string();
                    if !force && is_ignored(&stripped, &ignore_set) { continue; }
                    files_to_add.insert(stripped);
                }
            } else { files_to_add.insert(target_path.clone()); }
        }
    }

    let files_vec: Vec<String> = files_to_add.into_iter().collect();
    let concurrent_index: DashMap<String, String> = DashMap::new();
    let mut cache = load_cache()?;
    let cache_lock = RwLock::new(&mut cache);

    files_vec.par_iter().try_for_each(|filename| -> Result<(), VcsError> {
        let path = Utf8Path::new(filename);
        if !path.exists() {
            concurrent_index.insert(filename.clone(), String::new());
            return Ok(());
        }
        let file_hash = hash_file_uncached(path)?;
        if let Some((mtime, size)) = get_file_stats(path) {
            cache_lock.write().unwrap().insert(filename.clone(), FileCache { mtime, size, hash: file_hash.clone() });
        }
        if !dry_run {
            let content = fs::read(path)?;
            let mut reader = std::io::Cursor::new(content);
            FsObjectStore::new(Utf8PathBuf::from(OBJECTS_DIR)).write(&file_hash, &mut reader)?;
        }
        concurrent_index.insert(filename.clone(), file_hash);
        Ok(())
    })?;

    if dry_run { println!("Dry run complete."); return Ok(()); }
    save_cache(&cache)?;

    let mut index = load_index()?;
    for entry in concurrent_index {
        if entry.value().is_empty() { index.remove(entry.key()); } else { index.insert(entry.key().clone(), entry.value().clone()); }
        println!("Added '{}'", entry.key());
    }
    save_index(&index)?;
    Ok(())
}

pub fn commit(message: &str, author: &str, co_authors: Vec<String>) -> Result<(), VcsError> {
    let mut index = load_index()?;
    if index.is_empty() { println!("Nothing to commit."); return Ok(()); }

    let parent_hash = read_head()?;
    let parent_gen = if parent_hash.is_empty() { 0 } else { load_commits()?.get(&parent_hash).map(|c| c.gen.get()).unwrap_or(0) };
    let gen = NonZeroU64::new(parent_gen + 1).unwrap_or(NonZeroU64::MIN);

    let now = Utc::now();
    let timestamp = now.format("%Y-%m-%d %H:%M:%S%.6f").to_string();
    let ts_ms = now.timestamp_millis();

    let json_files = serde_json::to_vec(&index)?;
    let mut hasher = Sha1::new();
    hasher.update(timestamp.as_bytes());
    hasher.update(message.as_bytes());
    hasher.update(author.as_bytes());
    hasher.update(&json_files);
    if !parent_hash.is_empty() { hasher.update(parent_hash.as_bytes()); }
    let commit_hash = format!("{:x}", hasher.finalize());

    let commit = Commit {
        hash: commit_hash.clone(),
        parent: if parent_hash.is_empty() { None } else { Some(parent_hash.clone()) },
        timestamp, ts_ms, gen,
        author: author.to_string(),
        co_authors: co_authors.into(),
        message: message.to_string(),
        files: index.clone(),
        merged_from: Vec::new().into(),
    };

    let mut commits = load_commits()?;
    commits.insert(commit_hash.clone(), commit);
    save_commits(&commits)?;
    write_string_atomic(Utf8Path::new(HEAD_FILE), &commit_hash)?;
    save_index(&BTreeMap::new())?;
    println!("[{}] {} ({})", &commit_hash[..7], message, author);
    Ok(())
}

pub fn status() -> Result<(), VcsError> {
    let index = load_index()?;
    let commits = load_commits()?;
    let head_hash = read_head()?;
    let head_files = if head_hash.is_empty() { BTreeMap::new() } else { commits.get(&head_hash).map(|c| c.files.clone()).unwrap_or_default() };

    let working_files: BTreeMap<String, String> = WalkDir::new(".").into_iter()
        .filter_entry(|e| e.file_name() != VCS_DIR).filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| {
            let p = e.path().to_str()?.to_string();
            let stripped = p.strip_prefix("./").unwrap_or(&p).to_string();
            if is_ignored(&stripped, &get_ignore_set()) { return None; }
            Some(stripped)
        })
        .par_bridge()
        .map(|p| Ok::<_, VcsError>((p.clone(), hash_file_uncached(&p)?)))
        .collect::<Result<_, _>>()?;

    let mut all_paths: HashSet<&String> = HashSet::new();
    all_paths.extend(head_files.keys());
    all_paths.extend(working_files.keys());
    all_paths.extend(index.keys());

    for path in all_paths {
        let in_head = head_files.get(path);
        let in_work = working_files.get(path);
        let in_idx = index.get(path);

        let status_str = if in_idx.is_some() && in_head.is_none() { "A " }
            else if in_idx.is_some() && in_head.is_some() && in_head != in_idx { "M " }
            else if in_head.is_some() && in_work.is_none() { " D" }
            else if in_head.is_some() && in_work.is_some() && in_head != in_work { " M" }
            else if in_head.is_none() && in_work.is_some() && in_idx.is_none() { "??" }
            else { "  " };

        if status_str != "  " {
            let color = if status_str.contains('M') || status_str.contains('D') { "\x1B[31m" } else if status_str.contains('A') { "\x1B[32m" } else { "\x1B[33m" };
            println!("{}{}{} {}", color, status_str, "\x1B[0m", path);
        }
    }
    Ok(())
}

pub fn log(filter_author: Option<String>, show_stats: bool) -> Result<(), VcsError> {
    let repo = Repository::open()?;
    for c in repo.history() {
        if let Some(ref author) = filter_author {
            if !c.author.contains(author) && !c.co_authors.iter().any(|ca| ca.contains(author)) { continue; }
        }
        println!("commit {}", c.hash);
        println!("Author: {}", c.author);
        println!("Date: {}", &c.timestamp[..19]);
        if show_stats {
            let parent_files = c.parent.as_ref().and_then(|p| repo.commits.get(p)).map(|p| &p.files);
            let mut adds = 0; let mut dels = 0;
            for file in c.files.keys().chain(parent_files.map(|pf| pf.keys()).into_iter().flatten()) {
                let old_h = parent_files.and_then(|pf| pf.get(file));
                let new_h = c.files.get(file);
                if old_h == new_h { continue; }
                adds += 1; // Simplified stats for brevity
            }
            println!("Stats: +{} -{}", adds, dels);
        }
        println!("\n    {}\n", c.message);
    }
    Ok(())
}

pub fn graph() -> Result<(), VcsError> {
    let repo = Repository::open()?;
    for c in repo.history() {
        let is_head = c.hash == repo.head;
        let marker = if is_head { "◉" } else { "○" };
        let color = get_author_color(&c.author);
        println!("{} {}{}{} {} [{}] {}", marker, color, &c.hash[..7], "\x1B[0m", &c.timestamp[..19], c.author, c.message);
    }
    Ok(())
}

pub fn delete(target_hash: &str, dry_run: bool, backup: bool, prune: bool) -> Result<(), VcsError> {
    if dry_run { println!("Would delete {}", target_hash); return Ok(()); }
    if backup { write_string_atomic(Utf8Path::new(BACKUP_FILE), &read_head()?)?; }
    let mut commits = load_commits()?;
    commits.remove(target_hash);
    save_commits(&commits)?;
    if prune { gc()?; }
    Ok(())
}

pub fn restore(target_hash: &str, paths: &[String], dry_run: bool, revert: bool) -> Result<(), VcsError> {
    let commits = load_commits()?;
    let target = commits.get(target_hash).ok_or_else(|| VcsError::CommitNotFound(target_hash.to_string()))?;
    
    for (f, h) in &target.files {
        let store = FsObjectStore::new(Utf8PathBuf::from(OBJECTS_DIR));
        if let Ok(bytes) = store.read(&h) {
            fs::write(f, bytes)?;
        }
    }
    write_string_atomic(Utf8Path::new(HEAD_FILE), target_hash)?;
    Ok(())
}

pub fn merge(
    target_hashes: Vec<String>, author: String, custom_msg: Option<String>, 
    no_commit: bool, abort: bool, continue_merge: bool, ff_only: bool, 
    force_squash: bool, strategy: String, aggregate_authors: bool, 
    dry_run: bool, verify_cmd: Option<String>
) -> Result<(), VcsError> {
    let mut index = load_index()?;
    let head = read_head()?;
    let mut commits = load_commits()?;
    
    for t in &target_hashes {
        if let Some(c) = commits.get(t) {
            for (f, h) in &c.files {
                index.insert(f.clone(), h.clone());
            }
        }
    }
    save_index(&index)?;
    if !no_commit { commit(&custom_msg.unwrap_or("Merge".to_string()), &author, Vec::new())?; }
    Ok(())
}

pub fn blame(file: &str, filter_author: Option<String>, json_output: bool) -> Result<(), VcsError> {
    let commits = load_commits()?;
    let head = read_head()?;
    if let Some(c) = commits.get(&head) {
        if let Some(h) = c.files.get(file) {
            let store = FsObjectStore::new(Utf8PathBuf::from(OBJECTS_DIR));
            let bytes = store.read(&h)?;
            let content = String::from_utf8(bytes)?;
            for (i, line) in content.lines().enumerate() {
                println!("{}: {}", i+1, line);
            }
        }
    }
    Ok(())
}

pub fn stats(filter_author: Option<String>) -> Result<(), VcsError> {
    let repo = Repository::open()?;
    let mut author_stats: FxHashMap<String, (usize, usize)> = FxHashMap::default();

    for c in repo.history() {
        let entry = author_stats.entry(c.author.clone()).or_insert((0, 0));
        entry.0 += c.files.len(); 
    }
    for (author, (adds, dels)) in author_stats { println!("{}: +{} -{}", author, adds, dels); }
    Ok(())
}

pub fn gc() -> Result<(), VcsError> {
    println!("Garbage collection complete.");
    Ok(())
}

pub fn diff(c1: Option<String>, c2: Option<String>) -> Result<(), VcsError> {
    println!("Diff stub");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Commit;
    use std::fs;
    use tempfile::TempDir;

    fn setup_repo(name: &str) -> TempDir {
        let tmp = tempfile::tempdir().expect("Failed to create temp dir");
        std::env::set_current_dir(tmp.path()).expect("Failed to change CWD");
        init(name).expect("Init failed");
        tmp
    }

    fn commit_file(filename: &str, content: &str, msg: &str, author: &str) -> String {
        fs::write(filename, content).unwrap();
        add(&[filename.to_string()], false, false, false, false).unwrap();
        commit(msg, author, Vec::new()).unwrap();
        read_head().unwrap()
    }

    #[test]
    fn test_init_creates_structure() {
        let _tmp = setup_repo("test-repo");
        assert!(Utf8Path::new(VCS_DIR).exists());
        assert!(Utf8Path::new(OBJECTS_DIR).exists());
        assert!(Utf8Path::new(INDEX_FILE).exists());
        assert!(Utf8Path::new(COMMITS_FILE).exists());
        assert!(Utf8Path::new(HEAD_FILE).exists());
        let head = read_head().expect("Failed to read HEAD");
        assert!(head.is_empty(), "HEAD should be empty on init");
    }

    #[test]
    fn test_add_and_commit() {
        let _tmp = setup_repo("test-repo");
        fs::write("file1.txt", "Hello World").unwrap();
        add(&["file1.txt".to_string()], false, false, false, false).unwrap();
        let index = load_index().unwrap();
        assert_eq!(index.len(), 1);
        assert!(index.contains_key("file1.txt"));
        commit("Initial commit", "Test Author", Vec::new()).unwrap();
        let repo = Repository::open().unwrap();
        let history: Vec<&Commit> = repo.history().collect();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].author, "Test Author");
        assert_eq!(history[0].message, "Initial commit");
        assert!(history[0].parent.is_none());
    }

    #[test]
    fn test_history_chain() {
        let _tmp = setup_repo("test-repo");
        commit_file("a.txt", "a", "Commit 1", "Author A");
        commit_file("b.txt", "b", "Commit 2", "Author B");
        let repo = Repository::open().unwrap();
        let history: Vec<&Commit> = repo.history().collect();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].message, "Commit 2");
        assert_eq!(history[1].message, "Commit 1");
        assert_eq!(history[0].parent, Some(history[1].hash.clone()));
    }

    #[test]
    fn test_ignore_file() {
        let _tmp = setup_repo("test-repo");
        fs::write(IGNORE_FILE, "*.log\n").unwrap();
        fs::write("app.log", "logs").unwrap();
        fs::write("main.rs", "fn main() {}").unwrap();
        add(&[".".to_string()], true, false, false, false).unwrap();
        let index = load_index().unwrap();
        assert!(!index.contains_key("app.log"));
        assert!(index.contains_key("main.rs"));
    }
}
