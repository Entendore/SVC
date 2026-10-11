use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio, ChildStdin};
use std::rc::Rc;
use std::thread;
use std::time::Duration;
use std::fmt::Write as FmtWrite;

use ab_glyph::{Font, FontVec, Scale};
use chrono::Utc;
use clap::{Parser, Subcommand};
use image::Rgb;
use imageproc::drawing::draw_text_mut;
use memmap2::Mmap;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use serde::de::DeserializeOwned;
use sha1::{Digest, Sha1};
use smallvec::SmallVec;
use thiserror::Error;
use walkdir::WalkDir;

const VCS_DIR: &str = ".minivcs";
const OBJECTS_DIR: &str = ".minivcs/objects";
const INDEX_FILE: &str = ".minivcs/index.json";
const COMMITS_FILE: &str = ".minivcs/commits.json";
const HEAD_FILE: &str = ".minivcs/HEAD";
const CONFIG_FILE: &str = ".minivcs/config.json";
const LINKS_FILE: &str = ".minivcs/links.json";

#[derive(Debug, Error)]
enum VcsError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("Image error: {0}")]
    Image(#[from] image::ImageError),
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
    #[error("FFmpeg failed to start or stream.")]
    FfmpegFailed,
    #[error("FFmpeg not found. Please install it to use the render command.")]
    FfmpegNotFound,
    #[error("Linked repo not found at {0}")]
    RepoNotFound(String),
}

#[derive(Parser)]
#[command(name = "minivcs")]
#[command(about = "A high-performance, crash-safe graph VCS", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Init { #[arg(default_value = "default-repo")] name: String },
    Repo { #[command(subcommand)] action: RepoActions },
    Add { path: String },
    Commit {
        message: String,
        #[arg(short, long, default_value = "Unknown")]
        author: String,
    },
    Merge {
        commit_hashes: Vec<String>,
        #[arg(short, long, default_value = "Merge Bot")]
        author: String,
    },
    Delete { commit_hashes: Vec<String> },
    Restore { commit_hash: String },
    Graph,
    /// Show commit history in a linear format
    Log,
    /// Clean up unreferenced files from the objects directory
    Gc,
    Animate,
    Typewriter { filename: String },
    /// Render a file's history to an MP4 video, colored by author
    Render {
        filename: String,
        /// FFmpeg x264 preset (e.g., ultrafast, fast, medium, slow)
        #[arg(long, default_value = "ultrafast")]
        preset: String,
    },
    Diff {
        commit1: Option<String>,
        commit2: Option<String>,
    },
    Status,
}

#[derive(Subcommand)]
enum RepoActions {
    Link { path: String },
    List,
}

#[derive(Serialize, Deserialize, Clone)]
struct Commit {
    hash: String,
    #[serde(default)]
    parents: SmallVec<[String; 4]>,
    #[serde(default)]
    children: SmallVec<[String; 4]>,
    timestamp: String,
    #[serde(default)]
    ts_ms: i64,
    author: String,
    message: String,
    files: BTreeMap<String, String>,
}

#[derive(Serialize, Deserialize)]
struct RepoConfig { name: String }

fn main() {
    if let Err(e) = Cli::parse().command.dispatch() {
        eprintln!("Error: {}", e);
    }
}

trait Dispatch {
    fn dispatch(self) -> Result<(), VcsError>;
}

impl Dispatch for Commands {
    fn dispatch(self) -> Result<(), VcsError> {
        match self {
            Commands::Init { name } => init(&name),
            Commands::Repo { action } => match action {
                RepoActions::Link { path } => repo_link(&path),
                RepoActions::List => repo_list(),
            },
            Commands::Add { path } => add(&path),
            Commands::Commit { message, author } => commit(&message, &author),
            Commands::Merge { commit_hashes, author } => merge(&commit_hashes, &author),
            Commands::Delete { commit_hashes } => delete(&commit_hashes),
            Commands::Restore { commit_hash } => restore(&commit_hash),
            Commands::Graph => graph(),
            Commands::Log => log(),
            Commands::Gc => gc(),
            Commands::Animate => animate(),
            Commands::Typewriter { filename } => typewriter(&filename),
            Commands::Render { filename, preset } => render(&filename, &preset),
            Commands::Diff { commit1, commit2 } => diff(commit1, commit2),
            Commands::Status => status(),
        }
    }
}

// --- Optimized I/O & Atomic Writes ---

// Expert Pattern: Zero-Allocation JSON Serialization (Stream directly to file)
fn write_json_atomic<T: Serialize>(path: &Path, data: &T) -> Result<(), VcsError> {
    let tmp_path = path.with_extension("json.tmp");
    let file = File::create(&tmp_path)?;
    let mut writer = BufWriter::new(file);
    serde_json::to_writer_pretty(&mut writer, data)?;
    writer.flush()?;
    writer.into_inner()?.sync_all()?;
    fs::rename(&tmp_path, path)?;
    Ok(())
}

fn write_string_atomic(path: &Path, data: &str) -> Result<(), VcsError> {
    let tmp_path = path.with_extension("tmp");
    let mut file = File::create(&tmp_path)?;
    file.write_all(data.as_bytes())?;
    file.sync_all()?;
    fs::rename(&tmp_path, path)?;
    Ok(())
}

fn load_json_file<T: DeserializeOwned>(path: &Path) -> Result<T, VcsError> {
    if !path.exists() {
        return Err(VcsError::Io(io::Error::new(io::ErrorKind::NotFound, "File not found")));
    }
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    
    if metadata.len() > 16384 {
        let mmap = unsafe { Mmap::map(&file)? };
        Ok(serde_json::from_slice(&mmap[..])?)
    } else {
        let mut buf = Vec::with_capacity(metadata.len() as usize);
        let mut reader = io::BufReader::new(file);
        reader.read_to_end(&mut buf)?;
        Ok(serde_json::from_slice(&buf)?)
    }
}

fn load_commits_from(path: &Path) -> Result<HashMap<String, Commit>, VcsError> {
    let mut commits: HashMap<String, Commit> = load_json_file(path)?;
    for c in commits.values_mut() {
        if c.ts_ms == 0 {
            if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(&c.timestamp, "%Y-%m-%d %H:%M:%S%.6f") {
                c.ts_ms = dt.and_utc().timestamp_millis();
            }
        }
    }
    Ok(commits)
}

fn load_commits() -> Result<HashMap<String, Commit>, VcsError> {
    load_commits_from(Path::new(COMMITS_FILE))
}

fn save_commits(commits: &HashMap<String, Commit>) -> Result<(), VcsError> {
    write_json_atomic(Path::new(COMMITS_FILE), commits)
}

// --- Core Commands ---

fn init(name: &str) -> Result<(), VcsError> {
    if Path::new(VCS_DIR).exists() {
        println!("Repository already initialized.");
        return Ok(());
    }
    fs::create_dir_all(OBJECTS_DIR)?;
    write_json_atomic(Path::new(INDEX_FILE), &BTreeMap::<String, String>::new())?;
    write_json_atomic(Path::new(COMMITS_FILE), &HashMap::<String, Commit>::new())?;
    write_string_atomic(Path::new(HEAD_FILE), "")?;
    
    let config = RepoConfig { name: name.to_string() };
    write_json_atomic(Path::new(CONFIG_FILE), &config)?;
    write_json_atomic(Path::new(LINKS_FILE), &Vec::<String>::new())?;
    
    println!("Initialized empty MiniVCS repository '{}'.", name);
    Ok(())
}

fn repo_link(target_path: &str) -> Result<(), VcsError> {
    if !Path::new(VCS_DIR).exists() { return Err(VcsError::NotInitialized); }
    let abs_path = fs::canonicalize(target_path)?;
    let linked_vcs = abs_path.join(VCS_DIR);
    if !linked_vcs.exists() {
        return Err(VcsError::RepoNotFound(abs_path.display().to_string()));
    }
    let mut links: Vec<String> = load_json_file(Path::new(LINKS_FILE))?;
    let link_str = linked_vcs.to_str().unwrap().to_string();
    if !links.contains(&link_str) {
        links.push(link_str.clone());
        write_json_atomic(Path::new(LINKS_FILE), &links)?;
        println!("Successfully linked repository.");
    } else {
        println!("Repository already linked.");
    }
    Ok(())
}

fn repo_list() -> Result<(), VcsError> {
    if !Path::new(VCS_DIR).exists() { return Err(VcsError::NotInitialized); }
    let links: Vec<String> = load_json_file(Path::new(LINKS_FILE))?;
    if links.is_empty() {
        println!("No linked repositories.");
    } else {
        println!("Linked repositories:");
        for link in links { println!(" - {}", link); }
    }
    Ok(())
}

fn hash_file(path: &Path) -> Result<String, VcsError> {
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    let mut hasher = Sha1::new();
    
    if metadata.len() == 0 {
        return Ok("da39a3ee5e6b4b0d3255bfef95601890afd80709".to_string());
    }
    
    if metadata.len() > 16384 {
        let mmap = unsafe { Mmap::map(&file)? };
        hasher.update(&mmap[..]);
    } else {
        let mut reader = BufReader::new(file);
        let mut buffer = [0; 4096];
        loop {
            let n = reader.read(&mut buffer)?;
            if n == 0 { break; }
            hasher.update(&buffer[..n]);
        }
    }
    Ok(format!("{:x}", hasher.finalize()))
}

// Expert Pattern: Pruned Directory Walking (Skip .minivcs at OS level)
fn add(target_path: &str) -> Result<(), VcsError> {
    if !Path::new(VCS_DIR).exists() { return Err(VcsError::NotInitialized); }

    let files_to_add: Vec<String> = if target_path == "." {
        WalkDir::new(".")
            .into_iter()
            .filter_entry(|e| e.file_name() != VCS_DIR)
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .filter_map(|e| {
                let path_str = e.path().to_str()?.to_string();
                Some(path_str.strip_prefix("./").unwrap_or(&path_str).to_string())
            }).collect()
    } else {
        if !Path::new(target_path).exists() { return Err(VcsError::FileNotFound(target_path.to_string())); }
        vec![target_path.to_string()]
    };

    let hashed_files: Result<Vec<(String, String)>, VcsError> = files_to_add
        .par_iter()
        .map(|filename| {
            let path = Path::new(filename);
            let file_hash = hash_file(path)?;
            let object_path = Path::new(OBJECTS_DIR).join(&file_hash);
            if !object_path.exists() {
                match fs::hard_link(path, &object_path) {
                    Ok(_) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(e) => return Err(e.into()),
                }
            }
            Ok((filename.clone(), file_hash))
        }).collect();

    let mut index: BTreeMap<String, String> = load_json_file(Path::new(INDEX_FILE))?;
    for (filename, file_hash) in hashed_files? {
        index.insert(filename.clone(), file_hash);
        println!("Added '{}'", filename);
    }
    write_json_atomic(Path::new(INDEX_FILE), &index)?;
    Ok(())
}

fn commit(message: &str, author: &str) -> Result<(), VcsError> {
    let index: BTreeMap<String, String> = load_json_file(Path::new(INDEX_FILE))?;
    if index.is_empty() {
        println!("Nothing to commit. Use 'add' first.");
        return Ok(());
    }

    let head_hash = fs::read_to_string(HEAD_FILE)?.trim().to_string();
    let parents: SmallVec<[String; 4]> = if head_hash.is_empty() { SmallVec::new() } else { smallvec::smallvec![head_hash.clone()] };
    
    let now = Utc::now();
    let timestamp = now.format("%Y-%m-%d %H:%M:%S%.6f").to_string();
    let ts_ms = now.timestamp_millis();
    
    let json_index = serde_json::to_vec(&index)?;
    let mut hasher = Sha1::new();
    hasher.update(timestamp.as_bytes());
    hasher.update(message.as_bytes());
    hasher.update(author.as_bytes());
    hasher.update(&json_index);
    for p in &parents { hasher.update(p.as_bytes()); }
    let commit_hash = format!("{:x}", hasher.finalize());

    let commit_data = Commit {
        hash: commit_hash.clone(),
        parents: parents.clone(),
        children: SmallVec::new(),
        timestamp: timestamp.clone(),
        ts_ms,
        author: author.to_string(),
        message: message.to_string(),
        files: index,
    };
    
    let mut commits = load_commits()?;
    if let Some(parent_hash) = parents.first() {
        if let Some(parent_commit) = commits.get_mut(parent_hash) {
            parent_commit.children.push(commit_hash.clone());
        }
    }
    commits.insert(commit_hash.clone(), commit_data);
    save_commits(&commits)?;
    
    write_string_atomic(Path::new(HEAD_FILE), &commit_hash)?;
    write_json_atomic(Path::new(INDEX_FILE), &BTreeMap::<String, String>::new())?;
        
    println!("[{}] {} {} ({})", &commit_hash[..7], timestamp, message, author);
    Ok(())
}

// --- Cross-Repo Linking Helpers ---

fn get_linked_repos() -> Result<Vec<PathBuf>, VcsError> {
    if !Path::new(LINKS_FILE).exists() { return Ok(vec![]); }
    let links: Vec<String> = load_json_file(Path::new(LINKS_FILE))?;
    Ok(links.into_iter().map(PathBuf::from).collect())
}

fn batch_import_linked_commits(targets: &[String], local_commits: &mut HashMap<String, Commit>) -> Result<(), VcsError> {
    let linked_repos = get_linked_repos()?;
    let mut missing: HashSet<String> = targets.iter().filter(|t| !local_commits.contains_key(**t)).cloned().collect();

    if missing.is_empty() { return Ok(()); }

    for repo_path in linked_repos {
        if missing.is_empty() { break; }

        let commits_file = repo_path.join("commits.json");
        if !commits_file.exists() { continue; }

        let src_commits = load_commits_from(&commits_file)?;
        
        let found_in_this_repo: Vec<String> = missing.iter().filter(|t| src_commits.contains_key(**t)).cloned().collect();
        if found_in_this_repo.is_empty() { continue; }

        println!("Found {} commits in linked repo {:?}. Importing commit trees in-memory...", found_in_this_repo.len(), repo_path);
        
        let source_objects = repo_path.join("objects");
        let local_objects = Path::new(OBJECTS_DIR);

        let mut stack: Vec<String> = found_in_this_repo.clone();
        let mut visited = HashSet::new();
        let mut objects_to_copy: HashSet<String> = HashSet::new();

        while let Some(h) = stack.pop() {
            if !visited.insert(h.clone()) { continue; }

            if let Some(commit) = src_commits.get(&h) {
                for file_hash in commit.files.values() {
                    objects_to_copy.insert(file_hash.clone());
                }
                if !local_commits.contains_key(&h) {
                    local_commits.insert(h.clone(), commit.clone());
                }
                for p in &commit.parents {
                    stack.push(p.clone());
                }
            }
        }

        for t in &found_in_this_repo {
            missing.remove(t);
        }

        objects_to_copy.into_par_iter().try_for_each(|file_hash| -> Result<(), VcsError> {
            let src_obj = source_objects.join(&file_hash);
            let dst_obj = local_objects.join(&file_hash);
            if src_obj.exists() && !dst_obj.exists() {
                match fs::hard_link(&src_obj, &dst_obj) {
                    Ok(_) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(_) => {
                        fs::copy(&src_obj, &dst_obj)?;
                    }
                }
            }
            Ok(())
        })?;
    }

    if !missing.is_empty() {
        return Err(VcsError::CommitNotFound(missing.iter().next().unwrap().clone()));
    }

    Ok(())
}

// --- Merge & Delete ---

fn merge(target_hashes: &[String], author: &str) -> Result<(), VcsError> {
    if target_hashes.is_empty() {
        println!("No commits specified to merge.");
        return Ok(());
    }

    let head_hash = fs::read_to_string(HEAD_FILE)?.trim().to_string();
    if head_hash.is_empty() { return Err(VcsError::NoHead); }

    let index: BTreeMap<String, String> = load_json_file(Path::new(INDEX_FILE))?;
    if index.is_empty() {
        println!("Nothing to merge. Stage changes first.");
        return Ok(());
    }

    let mut commits = load_commits()?;
    
    let mut unique_targets: HashSet<&str> = HashSet::new();
    for h in target_hashes {
        if head_hash == *h { return Err(VcsError::SelfMerge); }
        unique_targets.insert(h.as_str());
    }

    let mut parents: SmallVec<[String; 4]> = SmallVec::with_capacity(unique_targets.len() + 1);
    parents.push(head_hash.clone());

    let missing_targets: Vec<String> = unique_targets.iter()
        .filter(|t| !commits.contains_key(**t))
        .map(|t| t.to_string())
        .collect();

    if !missing_targets.is_empty() {
        batch_import_linked_commits(&missing_targets, &mut commits)?;
    }

    for target_hash in &unique_targets {
        parents.push(target_hash.to_string());
    }

    let now = Utc::now();
    let timestamp = now.format("%Y-%m-%d %H:%M:%S%.6f").to_string();
    let ts_ms = now.timestamp_millis();
    
    let json_index = serde_json::to_vec(&index)?;
    let mut hasher = Sha1::new();
    hasher.update(timestamp.as_bytes());
    hasher.update(b"Merge");
    hasher.update(author.as_bytes());
    hasher.update(&json_index);
    for p in &parents { hasher.update(p.as_bytes()); }
    let merge_hash = format!("{:x}", hasher.finalize());

    let short_hashes: Vec<&str> = unique_targets.iter().map(|h| &h[..7]).collect();
    let merge_commit = Commit {
        hash: merge_hash.clone(),
        parents: parents.clone(),
        children: SmallVec::new(),
        timestamp: timestamp.clone(),
        ts_ms,
        author: author.to_string(),
        message: format!("Merge commits {}", short_hashes.join(", ")),
        files: index,
    };

    for p in &parents {
        if let Some(p_commit) = commits.get_mut(p) { p_commit.children.push(merge_hash.clone()); }
    }

    commits.insert(merge_hash.clone(), merge_commit);
    save_commits(&commits)?;
    
    write_string_atomic(Path::new(HEAD_FILE), &merge_hash)?;
    write_json_atomic(Path::new(INDEX_FILE), &BTreeMap::<String, String>::new())?;

    println!("Created merge commit [{}] {} ({})", &merge_hash[..7], timestamp, author);
    Ok(())
}

fn delete(target_hashes: &[String]) -> Result<(), VcsError> {
    if target_hashes.is_empty() { return Ok(()); }
    
    let mut local_commits = load_commits()?;
    let mut head_hash = fs::read_to_string(HEAD_FILE)?.trim().to_string();
    let mut needs_head_update = false;

    let unique_targets: HashSet<String> = target_hashes.iter().cloned().collect();
    let mut deleted_count = 0;

    for target_hash in &unique_targets {
        if local_commits.contains_key(target_hash) {
            if let Some(d) = splice_commit(&mut local_commits, target_hash) {
                deleted_count += 1;
                if head_hash == *target_hash {
                    head_hash = d.parents.first().cloned().unwrap_or_default();
                    needs_head_update = true;
                }
            }
        } else {
            let mut found = false;
            for repo_path in get_linked_repos()? {
                let linked_commits_file = repo_path.join("commits.json");
                if linked_commits_file.exists() {
                    let mut linked_commits = load_commits_from(&linked_commits_file)?;
                    if linked_commits.contains_key(target_hash) {
                        if let Some(d) = splice_commit(&mut linked_commits, target_hash) {
                            deleted_count += 1;
                            write_json_atomic(&linked_commits_file, &linked_commits)?;
                            
                            let linked_head_file = repo_path.join("HEAD");
                            let linked_head_hash = fs::read_to_string(&linked_head_file)?.trim().to_string();
                            if linked_head_hash == *target_hash {
                                let new_head = d.parents.first().cloned().unwrap_or_default();
                                write_string_atomic(&linked_head_file, &new_head)?;
                            }
                        }
                        found = true;
                        break;
                    }
                }
            }
            if !found {
                return Err(VcsError::CommitNotFound(target_hash.to_string()));
            }
        }
    }

    save_commits(&local_commits)?;
    if needs_head_update {
        write_string_atomic(Path::new(HEAD_FILE), &head_hash)?;
    }

    println!("Deleted {} commits and spliced graph links.", deleted_count);
    Ok(())
}

fn splice_commit(commits: &mut HashMap<String, Commit>, target_hash: &str) -> Option<Commit> {
    let target_commit = commits.get(target_hash).cloned()?;

    for p in &target_commit.parents {
        if let Some(p_commit) = commits.get_mut(p) {
            p_commit.children.retain(|c| c != target_hash);
            for c in &target_commit.children {
                if !p_commit.children.contains(c) { p_commit.children.push(c.clone()); }
            }
        }
    }

    for c in &target_commit.children {
        if let Some(c_commit) = commits.get_mut(c) {
            c_commit.parents.retain(|p| p != target_hash);
            for p in &target_commit.parents {
                if !c_commit.parents.contains(p) { c_commit.parents.push(p.clone()); }
            }
        }
    }

    commits.remove(target_hash);
    Some(target_commit)
}

fn restore(target_hash: &str) -> Result<(), VcsError> {
    let commits = load_commits()?;
    let target_commit = commits.get(target_hash).ok_or_else(|| VcsError::CommitNotFound(target_hash.to_string()))?;

    let head_hash = fs::read_to_string(HEAD_FILE)?.trim().to_string();
    if !head_hash.is_empty() {
        if let Some(head_commit) = commits.get(&head_hash) {
            head_commit.files.par_iter().try_for_each(|(filename, _)| -> Result<(), VcsError> {
                if !target_commit.files.contains_key(filename) && Path::new(filename).exists() {
                    fs::remove_file(filename)?;
                    println!("  Removed {}", filename);
                }
                Ok(())
            })?;
        }
    }
    
    target_commit.files.par_iter().try_for_each(|(filename, file_hash)| -> Result<(), VcsError> {
        let object_path = Path::new(OBJECTS_DIR).join(file_hash);
        if object_path.exists() {
            if let Some(parent) = Path::new(filename).parent() { fs::create_dir_all(parent)?; }
            fs::copy(&object_path, filename)?;
            println!("  Restored {}", filename);
        }
        Ok(())
    })?;

    write_string_atomic(Path::new(HEAD_FILE), target_hash)?;
    write_json_atomic(Path::new(INDEX_FILE), &BTreeMap::<String, String>::new())?;
    println!("HEAD is now at {}", &target_hash[..7]);
    Ok(())
}

// --- Graph, Log & Animate ---

// Expert Pattern: In-place String formatting (Avoid format! allocation)
fn generate_graph_lines(commits: &HashMap<String, Commit>, head_hash: &str) -> Vec<String> {
    let mut lines = Vec::new();
    if commits.is_empty() { lines.push("No commits yet.".to_string()); return lines; }

    let leaves: Vec<String> = commits.iter().filter(|(_, c)| c.children.is_empty()).map(|(h, _)| h.clone()).collect();
    let mut lanes: Vec<Option<String>> = if leaves.is_empty() {
        commits.keys().map(|k| Some(k.clone())).collect()
    } else {
        leaves.iter().map(|h| Some(h.clone())).collect()
    };

    while lanes.iter().any(|l| l.is_some()) {
        let mut lane_map: HashMap<&String, usize> = HashMap::new();
        let mut to_merge = None;
        
        for (i, lane) in lanes.iter().enumerate() {
            if let Some(h) = lane {
                if let Some(&j) = lane_map.get(h) {
                    to_merge = Some((i, j));
                    break;
                }
                lane_map.insert(h, i);
            }
        }

        if let Some((i, j)) = to_merge {
            let mut merge_row = String::with_capacity(lanes.len() * 2);
            for k in 0..lanes.len() {
                if k == i { merge_row.push_str("| "); }
                else if k == j { merge_row.push_str("/ "); }
                else if lanes[k].is_some() { merge_row.push_str("| "); }
                else { merge_row.push_str("  "); }
            }
            lines.push(merge_row);
            lanes.remove(j);
            continue;
        }

        let mut newest_ts = 0;
        let mut chosen_idx = 0;
        let mut found = false;
        for (i, lane) in lanes.iter().enumerate() {
            if let Some(hash) = lane {
                if let Some(commit) = commits.get(hash) {
                    if !found || commit.ts_ms > newest_ts {
                        newest_ts = commit.ts_ms;
                        chosen_idx = i;
                        found = true;
                    }
                }
            }
        }

        if !found { break; }
        let chosen_hash = lanes[chosen_idx].clone().unwrap();
        let commit = commits.get(&chosen_hash).unwrap();

        let mut row = String::with_capacity(lanes.len() * 2 + 50);
        for (i, lane) in lanes.iter().enumerate() {
            if i == chosen_idx { row.push_str("* "); }
            else if lane.is_some() { row.push_str("| "); }
            else { row.push_str("  "); }
        }

        let display_time = &commit.timestamp[..19];
        // Use write! macro to format directly into the pre-allocated String
        write!(row, "{} {} [{}] {}", &chosen_hash[..7], display_time, commit.author, commit.message).unwrap();
        if head_hash == chosen_hash { row.push_str(" (HEAD)"); }
        lines.push(row);

        if commit.parents.is_empty() {
            lanes[chosen_idx] = None;
        } else {
            lanes[chosen_idx] = Some(commit.parents[0].clone());
            if commit.parents.len() > 1 {
                let mut split_row = String::with_capacity(lanes.len() * 2 + commit.parents.len() * 2);
                for k in 0..lanes.len() {
                    if k == chosen_idx { split_row.push_str("| "); }
                    else if lanes[k].is_some() { split_row.push_str("| "); }
                    else { split_row.push_str("  "); }
                }
                for p in &commit.parents[1..] {
                    lanes.push(Some(p.clone()));
                    split_row.push_str("\\ ");
                }
                lines.push(split_row);
            }
        }
    }
    lines
}

fn graph() -> Result<(), VcsError> {
    let commits = load_commits()?;
    let head_hash = fs::read_to_string(HEAD_FILE)?.trim().to_string();
    for line in generate_graph_lines(&commits, &head_hash) { println!("{}", line); }
    Ok(())
}

fn log() -> Result<(), VcsError> {
    let commits = load_commits()?;
    let head_hash = fs::read_to_string(HEAD_FILE)?.trim().to_string();
    if head_hash.is_empty() { println!("No commits yet."); return Ok(()); }

    let mut current_hash = Some(head_hash);
    while let Some(hash) = current_hash {
        if let Some(c) = commits.get(&hash) {
            println!("commit {}", c.hash);
            println!("Author: {}", c.author);
            println!("Date: {}", &c.timestamp[..19]);
            println!("\n    {}\n", c.message);
            current_hash = c.parents.first().cloned();
        } else { break; }
    }
    Ok(())
}

fn animate() -> Result<(), VcsError> {
    let commits = load_commits()?;
    let head_hash = fs::read_to_string(HEAD_FILE)?.trim().to_string();
    let lines = generate_graph_lines(&commits, &head_hash);
    if lines.len() == 1 && lines[0] == "No commits yet." { println!("{}", lines[0]); return Ok(()); }

    let total_lines = lines.len();
    let stdout = io::stdout();
    for i in 1..=total_lines {
        let mut handle = stdout.lock();
        write!(handle, "\x1B[2J\x1B[1;1H")?;
        writeln!(handle, "--- Animating History ({}/{}) ---", i, total_lines)?;
        for line in lines.iter().take(i) { writeln!(handle, "{}", line)?; }
        handle.flush()?;
        thread::sleep(Duration::from_millis(400));
    }
    println!("\nAnimation complete!");
    Ok(())
}

// --- Garbage Collection ---

fn gc() -> Result<(), VcsError> {
    let commits = load_commits()?;
    let used_hashes: HashSet<String> = commits.values().flat_map(|c| c.files.values().cloned()).collect();
    
    let mut deleted_count = 0;
    if Path::new(OBJECTS_DIR).exists() {
        let orphaned_paths: Vec<PathBuf> = fs::read_dir(OBJECTS_DIR)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_file())
            .filter(|e| {
                e.file_name().to_str().map_or(false, |name| !used_hashes.contains(name))
            })
            .map(|e| e.path())
            .collect();

        deleted_count = orphaned_paths.len();

        orphaned_paths.par_iter().try_for_each(|p| -> Result<(), VcsError> {
            fs::remove_file(p)?;
            Ok(())
        })?;
    }
    println!("Garbage collection complete. Removed {} unreferenced objects.", deleted_count);
    Ok(())
}

// --- Shared Helpers ---

fn get_file_content_bytes(hash: &str) -> Vec<u8> {
    let path = Path::new(OBJECTS_DIR).join(hash);
    fs::read(&path).unwrap_or_default()
}

fn is_binary(bytes: &[u8]) -> bool {
    if bytes.is_empty() { return false; }
    let check_len = std::cmp::min(bytes.len(), 8192);
    if bytes[..check_len].contains(&0u8) { return true; }
    false
}

fn get_author_color(author: &str) -> &'static str {
    let colors = [
        "\x1B[31m", "\x1B[32m", "\x1B[33m", "\x1B[34m", 
        "\x1B[35m", "\x1B[36m", "\x1B[91m", "\x1B[92m", 
        "\x1B[93m", "\x1B[94m", "\x1B[95m", "\x1B[96m",
    ];
    let mut hash = 0u32;
    for c in author.chars() {
        hash = hash.wrapping_add(c as u32);
        hash = hash.wrapping_mul(31);
    }
    colors[(hash as usize) % colors.len()]
}

fn diff_lcs(old: &[char], new: &[char]) -> Vec<(char, Option<usize>, Option<usize>)> {
    let m = old.len();
    let n = new.len();
    let mut dp = vec![0u32; (m + 1) * (n + 1)];

    for i in 1..=m {
        for j in 1..=n {
            if old[i - 1] == new[j - 1] {
                dp[i * (n + 1) + j] = dp[(i - 1) * (n + 1) + (j - 1)] + 1;
            } else {
                dp[i * (n + 1) + j] = dp[(i - 1) * (n + 1) + j].max(dp[i * (n + 1) + (j - 1)]);
            }
        }
    }

    let mut i = m;
    let mut j = n;
    let mut result = Vec::with_capacity(m.max(n));

    while i > 0 || j > 0 {
        if i > 0 && j > 0 && old[i - 1] == new[j - 1] {
            result.push((old[i - 1], Some(i - 1), Some(j - 1)));
            i -= 1;
            j -= 1;
        } else if j > 0 && (i == 0 || dp[i * (n + 1) + (j - 1)] >= dp[(i - 1) * (n + 1) + j]) {
            result.push((new[j - 1], None, Some(j - 1)));
            j -= 1;
        } else {
            result.push((old[i - 1], Some(i - 1), None));
            i -= 1;
        }
    }
    result.reverse();
    result
}

// --- Typewriter Logic (Terminal) ---

// Expert Pattern: Zero-Copy UTF-8 Conversion (Avoid from_utf8_lossy allocation)
fn typewriter(filename: &str) -> Result<(), VcsError> {
    let commits = load_commits()?;
    let head_hash = fs::read_to_string(HEAD_FILE)?.trim().to_string();
    if head_hash.is_empty() { println!("No commits to animate."); return Ok(()); }

    let mut ancestors = HashSet::new();
    let mut stack = vec![head_hash.clone()];
    while let Some(h) = stack.pop() {
        if ancestors.insert(h.clone()) {
            if let Some(c) = commits.get(&h) {
                for p in &c.parents { stack.push(p.clone()); }
            }
        }
    }

    let mut sorted_commits: Vec<Commit> = ancestors.iter().filter_map(|h| commits.get(h)).cloned().collect();
    sorted_commits.sort_by(|a, b| a.ts_ms.cmp(&b.ts_ms));

    let mut current_state: Vec<(char, String)> = Vec::new();
    let stdout = io::stdout();

    for commit in &sorted_commits {
        let new_bytes = commit.files.get(filename).map(get_file_content_bytes).unwrap_or_default();
        if is_binary(&new_bytes) {
            println!("Cannot animate binary file: {}", filename);
            return Ok(());
        }

        // Take ownership of bytes without copying/scanning
        let new_content = String::from_utf8(new_bytes).unwrap_or_default();
        let new_chars: Vec<char> = new_content.chars().collect();
        let old_chars: Vec<char> = current_state.iter().map(|(c, _)| *c).collect();
        let diff_ops = diff_lcs(&old_chars, &new_chars);

        let mut new_state = Vec::with_capacity(new_chars.len());
        let mut handle = stdout.lock();
        
        write!(handle, "\x1B[2J\x1B[H")?;
        writeln!(handle, "Commit: {} | Author: {} | File: {}", &commit.hash[..7], commit.author, filename)?;
        writeln!(handle, "{}", "-".repeat(50))?;
        
        for (c, old_idx, _) in diff_ops {
            let author = if let Some(idx) = old_idx { current_state[idx].1.clone() } else { commit.author.clone() };
            let is_new = old_idx.is_none();
            new_state.push((c, author.clone()));
            
            let color = get_author_color(&author);
            write!(handle, "{}{}\x1B[0m", color, c)?;
            
            if is_new && c != '\n' {
                handle.flush()?;
                thread::sleep(Duration::from_millis(25));
            }
        }
        current_state = new_state;
        handle.flush()?;
        thread::sleep(Duration::from_millis(800));
    }
    println!("\n\nTerminal animation complete!");
    Ok(())
}

// --- Render Logic (Optimized Raw RGB Streaming Pipeline) ---

fn check_ffmpeg() -> Result<(), VcsError> {
    let status = Command::new("ffmpeg").arg("-version").stdout(Stdio::null()).stderr(Stdio::null()).status();
    match status {
        Ok(s) if s.success() => Ok(()),
        _ => Err(VcsError::FfmpegNotFound),
    }
}

fn load_font() -> FontVec {
    let paths = [
        "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
        "/System/Library/Fonts/Menlo.ttc",
        "/System/Library/Fonts/Courier.ttc",
        "C:\\Windows\\Fonts\\consola.ttf",
        "C:\\Windows\\Fonts\\cour.ttf",
        "font.ttf",
    ];
    for p in paths {
        if let Ok(data) = fs::read(p) {
            if let Ok(font) = FontVec::try_from_vec(data) { return font; }
        }
    }
    panic!("No suitable monospace font found. Please place a 'font.ttf' in the directory.");
}

fn get_author_color_rgb(author_id: usize) -> Rgb<u8> {
    let colors = [
        Rgb([235, 99, 99]), Rgb([99, 235, 99]), Rgb([99, 132, 235]),
        Rgb([235, 219, 99]), Rgb([204, 99, 235]), Rgb([99, 235, 224]),
        Rgb([235, 159, 99]),
    ];
    colors[author_id % colors.len()]
}

fn draw_and_send(
    state: &[(char, usize)],
    canvas: &mut image::RgbImage,
    font: &FontVec,
    stdin: &mut ChildStdin,
) -> Result<(), VcsError> {
    canvas.as_raw_mut().fill(30);
    
    let scale = Scale::uniform(32.0);
    let line_height = 40.0;
    let max_x = canvas.width() as f32 - 50.0;
    let mut x = 50.0;
    let mut y = 50.0;
    let mut buf = [0u8; 4];

    for (ch, author_id) in state {
        if *ch == '\n' {
            y += line_height;
            x = 50.0;
        } else {
            let color = get_author_color_rgb(*author_id);
            let s = ch.encode_utf8(&mut buf);
            draw_text_mut(canvas, color, x as i32, y as i32, scale, font, s);
            
            let advance = if let Some(glyph_id) = font.glyph_id(*ch) {
                glyph_id.with_scale(scale).h_advance()
            } else { 20.0 };
            
            if x + advance > max_x {
                y += line_height;
                x = 50.0;
            } else { x += advance; }
        }
    }
    stdin.write_all(canvas.as_raw())?;
    Ok(())
}

// Expert Pattern: Zero-Copy UTF-8 Conversion
fn render(filename: &str, preset: &str) -> Result<(), VcsError> {
    check_ffmpeg()?;

    let commits = load_commits()?;
    let head_hash = fs::read_to_string(HEAD_FILE)?.trim().to_string();
    if head_hash.is_empty() { println!("No commits to render."); return Ok(()); }

    let mut ancestors = HashSet::new();
    let mut stack = vec![head_hash.clone()];
    while let Some(h) = stack.pop() {
        if ancestors.insert(h.clone()) {
            if let Some(c) = commits.get(&h) {
                for p in &c.parents { stack.push(p.clone()); }
            }
        }
    }

    let mut sorted_commits: Vec<Commit> = ancestors.iter().filter_map(|h| commits.get(h)).cloned().collect();
    sorted_commits.sort_by(|a, b| a.ts_ms.cmp(&b.ts_ms));

    let font = load_font();
    let mut authors_map: HashMap<Rc<str>, usize> = HashMap::new();
    
    let mut current_text: Vec<(char, usize)> = Vec::new();
    let mut canvas = image::RgbImage::from_pixel(1280, 720, Rgb([30, 30, 30]));
    
    let mp4_name = format!("{}.mp4", filename);
    println!("Rendering video directly to FFmpeg (preset: {})...", preset);

    let mut child = Command::new("ffmpeg")
        .args([
            "-y", "-f", "rawvideo", "-pix_fmt", "rgb24", "-s", "1280x720",
            "-framerate", "30", "-i", "pipe:0",
            "-c:v", "libx264", "-preset", preset, "-tune", "zerolatency",
            "-crf", "18", "-pix_fmt", "yuv420p", "-threads", "0", &mp4_name
        ])
        .stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::inherit()).spawn()?;

    let mut stdin = child.stdin.take().ok_or(VcsError::FfmpegFailed)?;

    for commit in &sorted_commits {
        let author_rc: Rc<str> = Rc::from(commit.author.as_str());
        let author_id = *authors_map.entry(author_rc).or_insert_with(|| authors_map.len());

        let new_bytes = commit.files.get(filename).map(get_file_content_bytes).unwrap_or_default();
        if is_binary(&new_bytes) {
            println!("Cannot render binary file: {}", filename);
            return Ok(());
        }

        // Take ownership of bytes without copying/scanning
        let new_content = String::from_utf8(new_bytes).unwrap_or_default();
        let new_chars: Vec<char> = new_content.chars().collect();
        let old_chars: Vec<char> = current_text.iter().map(|(c, _)| *c).collect();

        let diff_ops = diff_lcs(&old_chars, &new_chars);

        draw_and_send(&current_text, &mut canvas, &font, &mut stdin)?;

        let mut render_state = Vec::with_capacity(new_chars.len());

        for (c, old_idx, _) in diff_ops {
            if let Some(idx) = old_idx {
                let aid = current_text[idx].1;
                render_state.push((c, aid));
            } else {
                render_state.push((c, author_id));
                if c != '\n' {
                    draw_and_send(&render_state, &mut canvas, &font, &mut stdin)?;
                }
            }
        }
        
        current_text = render_state;

        for _ in 0..30 {
            draw_and_send(&current_text, &mut canvas, &font, &mut stdin)?;
        }
    }

    drop(stdin);
    let status = child.wait()?;

    if status.success() {
        println!("Successfully created {}", mp4_name);
    } else {
        return Err(VcsError::FfmpegFailed);
    }

    Ok(())
}

// --- Diff Logic ---

fn compute_lcs_diff(old: &[&str], new: &[&str]) -> String {
    let m = old.len();
    let n = new.len();
    let mut dp = vec![0u32; (m + 1) * (n + 1)];

    for i in 1..=m {
        for j in 1..=n {
            if old[i - 1] == new[j - 1] {
                dp[i * (n + 1) + j] = dp[(i - 1) * (n + 1) + (j - 1)] + 1;
            } else {
                dp[i * (n + 1) + j] = dp[(i - 1) * (n + 1) + j].max(dp[i * (n + 1) + (j - 1)]);
            }
        }
    }

    let mut i = m;
    let mut j = n;
    let mut lines = Vec::with_capacity(m + n);

    while i > 0 || j > 0 {
        if i > 0 && j > 0 && old[i - 1] == new[j - 1] {
            lines.push(format!(" {}", old[i - 1]));
            i -= 1;
            j -= 1;
        } else if j > 0 && (i == 0 || dp[i * (n + 1) + (j - 1)] >= dp[(i - 1) * (n + 1) + j]) {
            lines.push(format!("+{}", new[j - 1]));
            j -= 1;
        } else {
            lines.push(format!("-{}", old[i - 1]));
            i -= 1;
        }
    }
    lines.reverse();
    lines.join("\n") + "\n"
}

fn diff(commit1: Option<String>, commit2: Option<String>) -> Result<(), VcsError> {
    let commits = load_commits()?;
    let head_hash = fs::read_to_string(HEAD_FILE)?.trim().to_string();

    let c1 = commit1.map(|c| if c == "HEAD" { head_hash.clone() } else { c });
    let c2 = commit2.map(|c| if c == "HEAD" { head_hash.clone() } else { c });

    let (target1, target2) = match (c1, c2) {
        (Some(t1), Some(t2)) => (Some(t1), Some(t2)),
        (Some(t1), None) => (None, Some(t1)),
        (None, None) => (None, Some(head_hash)),
        (None, Some(t2)) => (None, Some(t2)), 
    };

    let files1 = get_files_for_target(&target1, &commits)?;
    let files2 = get_files_for_target(&target2, &commits)?;

    let mut all_files: Vec<String> = files1.keys().chain(files2.keys()).cloned().collect();
    all_files.sort();
    all_files.dedup();

    for file in all_files {
        let h1 = files1.get(&file);
        let h2 = files2.get(&file);

        if h1 == h2 { continue; }

        println!("diff -- a/{} b/{}", file, file);

        let bytes1 = h1.map(get_file_content_bytes).unwrap_or_default();
        let bytes2 = h2.map(get_file_content_bytes).unwrap_or_default();

        if is_binary(&bytes1) || is_binary(&bytes2) {
            println!("Binary files differ");
            println!();
            continue;
        }

        // Use zero-copy conversion
        let content1 = String::from_utf8(bytes1).unwrap_or_default();
        let content2 = String::from_utf8(bytes2).unwrap_or_default();

        let lines1: Vec<&str> = content1.lines().collect();
        let lines2: Vec<&str> = content2.lines().collect();

        let diff_str = compute_lcs_diff(&lines1, &lines2);
        print!("{}", diff_str);
        println!();
    }
    Ok(())
}

// Expert Pattern: Pruned Directory Walking
fn get_files_for_target(target: &Option<String>, commits: &HashMap<String, Commit>) -> Result<BTreeMap<String, String>, VcsError> {
    if let Some(hash) = target {
        commits.get(hash).map(|c| c.files.clone()).ok_or_else(|| VcsError::CommitNotFound(hash.clone()))
    } else {
        let working_files: Vec<String> = WalkDir::new(".")
            .into_iter()
            .filter_entry(|e| e.file_name() != VCS_DIR)
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .filter_map(|e| {
                let path_str = e.path().to_str()?.to_string();
                Some(path_str.strip_prefix("./").unwrap_or(&path_str).to_string())
            }).collect();

        let hashed: Vec<(String, String)> = working_files.par_iter()
            .map(|f| Ok((f.clone(), hash_file(Path::new(f))?)))
            .collect::<Result<_, VcsError>>()?;

        Ok(hashed.into_iter().collect())
    }
}

// --- Status Logic ---

// Expert Pattern: Pruned Directory Walking
fn status() -> Result<(), VcsError> {
    let index: BTreeMap<String, String> = load_json_file(Path::new(INDEX_FILE))?;
    let commits = load_commits()?;
    let head_hash = fs::read_to_string(HEAD_FILE)?.trim().to_string();

    let head_files: BTreeMap<String, String> = if head_hash.is_empty() {
        BTreeMap::new()
    } else {
        commits.get(&head_hash).map(|c| c.files.clone()).unwrap_or_default()
    };

    let working_files_vec: Vec<String> = WalkDir::new(".")
        .into_iter()
        .filter_entry(|e| e.file_name() != VCS_DIR)
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| {
            let path_str = e.path().to_str()?.to_string();
            Some(path_str.strip_prefix("./").unwrap_or(&path_str).to_string())
        }).collect();

    let working_files: BTreeMap<String, String> = working_files_vec.par_iter()
        .map(|f| Ok((f.clone(), hash_file(Path::new(f))?)))
        .collect::<Result<_, VcsError>>()?
        .into_iter()
        .collect();

    println!("On HEAD: {}", if head_hash.is_empty() { "(empty)".to_string() } else { head_hash.chars().take(7).collect() });
    
    println!("\nChanges to be committed:");
    if index.is_empty() { println!("  (none)"); }
    for (file, _) in &index { println!("  staged: {}", file); }

    println!("\nChanges not staged for commit:");
    let mut modified_exists = false;
    for (file, hash) in &working_files {
        if head_files.contains_key(file) && !index.contains_key(file) {
            if hash != &head_files[file] {
                println!("  modified: {}", file);
                modified_exists = true;
            }
        }
    }
    if !modified_exists { println!("  (none)"); }

    println!("\nUntracked files:");
    let mut untracked_exists = false;
    for file in working_files.keys() {
        if !head_files.contains_key(file) && !index.contains_key(file) {
            println!("  {}", file);
            untracked_exists = true;
        }
    }
    if !untracked_exists { println!("  (none)"); }

    Ok(())
}