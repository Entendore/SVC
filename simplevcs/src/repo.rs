use crate::error::{VcsError, VcsResult};
use crate::odb::*;
use ed25519_dalek::{SigningKey, VerifyingKey, Signer, Verifier, Signature};
use fs2::FileExt;
use rand::rngs::OsRng;
use sha2::{Sha256, Digest};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use tracing::{info, warn};

pub type Index = BTreeMap<String, TreeEntry>;

#[derive(Clone)]
pub struct Repo {
    pub root: PathBuf,
    pub vcs_root: PathBuf,
    pub db: sled::Db,
}

pub struct RepoLock {
    _file: std::fs::File,
}

impl RepoLock {
    pub fn acquire(repo: &Repo) -> VcsResult<Self> {
        let file = std::fs::OpenOptions::new()
            .create(true).read(true).write(true)
            .open(repo.lock_file())?;
        file.lock_exclusive()?;
        Ok(RepoLock { _file: file })
    }
}

impl Drop for RepoLock {
    fn drop(&mut self) { let _ = self._file.unlock(); }
}

impl Repo {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let vcs_root = root.join(crate::VCS_DIR_NAME);
        let db = sled::open(vcs_root.join("db")).expect("Failed to open Sled DB");
        Repo { root, vcs_root, db }
    }

    pub fn discover() -> VcsResult<Self> {
        let cwd = std::env::current_dir()?;
        let mut dir = cwd.as_path();
        loop {
            if dir.join(crate::VCS_DIR_NAME).exists() {
                let vcs_root = dir.join(crate::VCS_DIR_NAME);
                let db = sled::open(vcs_root.join("db"))?;
                let repo = Repo { root: dir.to_path_buf(), vcs_root, db };
                repo.recover_crash()?;
                return Ok(repo);
            }
            if dir.join(".simplevcslink").exists() {
                let link_content = std::fs::read_to_string(dir.join(".simplevcslink"))?;
                let vcs_root = PathBuf::from(link_content.trim());
                let db = sled::open(vcs_root.join("db"))?;
                let repo = Repo { root: dir.to_path_buf(), vcs_root, db };
                repo.recover_crash()?;
                return Ok(repo);
            }
            dir = match dir.parent() { Some(p) => p, None => return Err(VcsError::NotInitialized) };
        }
    }

    fn recover_crash(&self) -> VcsResult<()> {
        if let Some(next_head) = self.db.get("NEXT_HEAD")? {
            let hash = String::from_utf8_lossy(&next_head).to_string();
            self.write_head(&hash)?;
            self.save_index_atomic(&BTreeMap::new())?;
            self.db.remove("NEXT_HEAD")?;
        }
        Ok(())
    }

    pub fn vcs_dir(&self) -> &Path { &self.vcs_root }
    pub fn objects_dir(&self) -> PathBuf { self.vcs_root.join("objects") }
    pub fn lfs_dir(&self) -> PathBuf { self.vcs_root.join("lfs").join("objects") }
    pub fn lock_file(&self) -> PathBuf { self.vcs_root.join("index.lock") }

    pub fn write_object<T: serde::Serialize>(&self, obj: &T) -> VcsResult<String> {
        let data = bincode::serialize(obj)?;
        let hash = hex::encode(Sha256::digest(&data));
        let path = self.objects_dir().join(&hash);
        if !path.exists() {
            let tmp_path = path.with_extension("tmp");
            let compressed = zstd::encode_all(data.as_slice(), 3)?;
            let mut file = std::fs::File::create(&tmp_path)?;
            file.write_all(&compressed)?;
            file.sync_all()?;
            std::fs::rename(&tmp_path, &path)?;
        }
        Ok(hash)
    }

    pub fn read_object<T: for<'de> serde::Deserialize<'de>>(&self, hash: &str) -> VcsResult<T> {
        let path = self.objects_dir().join(hash);
        if !path.exists() { return Err(VcsError::CommitNotFound(hash.to_string())); }
        let data = zstd::decode_all(std::fs::read(path)?.as_slice())?;
        Ok(bincode::deserialize(&data)?)
    }

    pub fn read_head(&self) -> VcsResult<String> {
        let is_worktree = self.root.join(crate::VCS_DIR_NAME) != self.vcs_root;
        let head_path = if is_worktree { self.root.join(".HEAD") } else { self.vcs_root.join("HEAD") };
        if !head_path.exists() { return Ok(String::new()); }
        Ok(std::fs::read_to_string(head_path)?.trim().to_string())
    }

    pub fn write_head(&self, hash: &str) -> VcsResult<()> {
        let old_head = self.read_head()?;
        let ts = chrono::Utc::now().timestamp_millis();
        let entry = format!("{} {} {}", ts, old_head, hash);
        self.db.insert(format!("reflog:{}", ts), entry.as_bytes())?;
        
        let is_worktree = self.root.join(crate::VCS_DIR_NAME) != self.vcs_root;
        let head_path = if is_worktree { self.root.join(".HEAD") } else { self.vcs_root.join("HEAD") };
        write_string_atomic(&head_path, hash)
    }

    pub fn load_index(&self) -> VcsResult<Index> {
        match self.db.get("index")? {
            Some(bytes) => Ok(bincode::deserialize(&bytes)?),
            None => Ok(Index::new()),
        }
    }

    pub fn save_index_atomic(&self, new_files: &Index) -> VcsResult<()> {
        let bytes = bincode::serialize(new_files)?;
        self.db.insert("index", bytes)?;
        Ok(())
    }

    pub fn resolve_hash(&self, input: &str) -> VcsResult<String> {
        if input.len() == 64 { return Ok(input.to_string()); }
        if let Ok(hash) = self.tag_get(input) { return Ok(hash); }
        if input.len() >= 7 {
            for entry in std::fs::read_dir(self.objects_dir())? {
                let e = entry?;
                let name = e.file_name().to_string_lossy().to_string();
                if name.starts_with(input) { return Ok(name); }
            }
        }
        Err(VcsError::CommitNotFound(input.to_string()))
    }

    pub fn tag_add(&self, name: &str, hash: &str) -> VcsResult<()> {
        let full = self.resolve_hash(hash)?;
        self.db.insert(format!("tag:{}", name), full.as_bytes())?;
        Ok(())
    }

    pub fn tag_get(&self, name: &str) -> VcsResult<String> {
        self.db.get(format!("tag:{}", name))?
            .map(|v| String::from_utf8_lossy(&v).to_string())
            .ok_or_else(|| VcsError::CommitNotFound(name.to_string()))
    }

    pub fn tag_list(&self) -> VcsResult<Vec<(String, String)>> {
        let mut tags = vec![];
        for item in self.db.scan_prefix(b"tag:") {
            let (k, v) = item?;
            let name = String::from_utf8_lossy(&k).strip_prefix("tag:").unwrap().to_string();
            let hash = String::from_utf8_lossy(&v).to_string();
            tags.push((name, hash));
        }
        tags.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(tags)
    }

    pub fn tag_remove(&self, name: &str) -> VcsResult<()> {
        self.db.remove(format!("tag:{}", name))?;
        Ok(())
    }

    pub fn run_hook(&self, name: &str) -> VcsResult<()> {
        let hook_path = self.vcs_dir().join("hooks").join(name);
        if hook_path.exists() {
            let output = Command::new(&hook_path).current_dir(&self.root).output()?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let stdout = String::from_utf8_lossy(&output.stdout);
                return Err(VcsError::HookFailedWithOutput(name.to_string(), format!("STDOUT:\n{}\nSTDERR:\n{}", stdout, stderr)));
            }
        }
        Ok(())
    }

    pub fn commit(&self, message: &str, author: &str, sign: bool) -> VcsResult<String> {
        self.run_hook("pre-commit")?;
        let _lock = RepoLock::acquire(self)?;
        let index = self.load_index()?;
        if index.is_empty() { return Err(VcsError::Io(std::io::Error::new(std::io::ErrorKind::Other, "Nothing to commit"))); }
        
        let tree_hash = self.write_tree(&index)?;
        let head_hash = self.read_head()?;
        let parents: smallvec::SmallVec<[String; 4]> = if head_hash.is_empty() { smallvec::SmallVec::new() } else { smallvec::smallvec![head_hash.clone()] };

        let now = chrono::Utc::now();
        let timestamp = now.format("%Y-%m-%d %H:%M:%S%.6f").to_string();
        let ts_ms = now.timestamp_millis();

        let mut hasher = Sha256::new();
        hasher.update(tree_hash.as_bytes());
        hasher.update(timestamp.as_bytes());
        hasher.update(message.as_bytes());
        hasher.update(author.as_bytes());
        for p in &parents { hasher.update(p.as_bytes()); }
        let commit_hash = hex::encode(hasher.finalize());

        let mut commit_data = Commit {
            hash: commit_hash.clone(), tree: tree_hash, parents,
            timestamp: timestamp.clone(), ts_ms, 
            author: author.to_string(), message: message.to_string(),
            signature: None,
        };

        if sign {
            let sk_bytes = self.db.get("secret_key")?.ok_or_else(|| VcsError::Crypto("No secret key".into()))?;
            let sk_array: [u8; 32] = sk_bytes.as_ref().try_into().map_err(|_| VcsError::Crypto("Invalid sk".into()))?;
            let signing_key = SigningKey::from_bytes(&sk_array);
            let signature = signing_key.sign(commit_hash.as_bytes());
            commit_data.signature = Some(base64::engine::general_purpose::STANDARD.encode(signature.to_bytes()));
        }

        self.write_object(&commit_data)?;
        self.db.insert("NEXT_HEAD", commit_hash.as_bytes())?;
        self.write_head(&commit_hash)?;
        self.save_index_atomic(&BTreeMap::new())?;
        self.db.remove("NEXT_HEAD")?;
        self.db.flush()?;
        self.run_hook("post-commit")?;
        Ok(commit_hash)
    }

    pub fn verify_commit(&self, hash: &str) -> VcsResult<bool> {
        let commit = self.read_commit(hash)?;
        if let Some(sig_b64) = &commit.signature {
            let sig_bytes = base64::engine::general_purpose::STANDARD.decode(sig_b64).map_err(|e| VcsError::Crypto(e.to_string()))?;
            let sig_array: [u8; 64] = sig_bytes.as_slice().try_into().map_err(|_| VcsError::Crypto("Invalid sig".into()))?;
            let signature = Signature::from_bytes(&sig_array);
            let pk_bytes = self.db.get("public_key")?.ok_or_else(|| VcsError::Crypto("No pk".into()))?;
            let pk_array: [u8; 32] = pk_bytes.as_ref().try_into().map_err(|_| VcsError::Crypto("Invalid pk".into()))?;
            let public_key = VerifyingKey::from_bytes(&pk_array)?;
            public_key.verify(commit.hash.as_bytes(), &signature).map_err(|e| VcsError::Crypto(e.to_string()))?;
            Ok(true)
        } else {
            Err(VcsError::InvalidSignature)
        }
    }

    pub fn fsck(&self) -> VcsResult<usize> {
        let _lock = RepoLock::acquire(self)?;
        let mut corrupted = 0;
        for entry in std::fs::read_dir(self.objects_dir())? {
            let e = entry?;
            if !e.file_type()?.is_file() { continue; }
            let name = e.file_name().to_string_lossy().to_string();
            if name.len() != 64 { continue; }
            let path = e.path();
            let data = std::fs::read(&path)?;
            match zstd::decode_all(data.as_slice()) {
                Ok(decompressed) => {
                    let computed_hash = hex::encode(Sha256::digest(&decompressed));
                    if computed_hash != name {
                        warn!(hash = %name, "Object corrupted (hash mismatch)");
                        corrupted += 1;
                    }
                },
                Err(_) => { corrupted += 1; }
            }
        }
        if corrupted > 0 { return Err(VcsError::FsckFailed(corrupted)); }
        info!("Filesystem check passed. All objects verified.");
        Ok(0)
    }

    pub fn gc(&self) -> VcsResult<(u64, u64)> {
        let _lock = RepoLock::acquire(self)?;
        let mut reachable: HashSet<String> = HashSet::new();
        let head = self.read_head()?;
        if !head.is_empty() {
            let mut stack = vec![head];
            let mut visited = HashSet::new();
            while let Some(hash) = stack.pop() {
                if !visited.insert(hash.clone()) { continue; }
                reachable.insert(hash.clone());
                if let Ok(commit) = self.read_commit(&hash) {
                    reachable.insert(commit.tree.clone());
                    stack.push(commit.tree.clone());
                    if let Ok(tree) = self.read_tree(&commit.tree) {
                        for entry in tree.files.values() { reachable.insert(entry.hash.clone()); }
                    }
                    for p in &commit.parents { stack.push(p.clone()); }
                }
            }
        }
        let mut deleted = 0u64;
        let mut kept = 0u64;
        for entry in std::fs::read_dir(self.objects_dir())? {
            let e = entry?;
            let name = e.file_name().to_string_lossy().to_string();
            if name.len() == 64 && !reachable.contains(&name) {
                std::fs::remove_file(e.path())?;
                deleted += 1;
            } else if name.len() == 64 { kept += 1; }
        }
        self.db.compact()?;
        info!(deleted = deleted, kept = kept, "Garbage collection complete");
        Ok((deleted, kept))
    }

    pub fn write_tree(&self, files: &Index) -> VcsResult<String> { self.write_object(&Tree { files: files.clone() }) }
    pub fn read_tree(&self, hash: &str) -> VcsResult<Tree> { self.read_object::<Tree>(hash) }
    pub fn read_commit(&self, hash: &str) -> VcsResult<Commit> { self.read_object::<Commit>(hash) }
    pub fn read_blob(&self, hash: &str) -> VcsResult<Vec<u8>> { self.read_object::<Vec<u8>>(hash) }

    pub fn add(&self, target_path: &str) -> VcsResult<Vec<String>> {
        let _lock = RepoLock::acquire(self)?;
        let mut builder = ignore::WalkBuilder::new(&self.root);
        builder.add_custom_ignore_filename(".simplevcsignore");
        builder.hidden(false);
        builder.filter_entry(|e| e.file_name() != crate::VCS_DIR_NAME && e.file_name() != ".simplevcslink" && e.file_name() != ".HEAD");
        
        let files_to_add: Vec<String> = builder.build()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map_or(false, |t| t.is_file() || t.is_symlink()))
            .filter_map(|e| Some(e.path().strip_prefix(&self.root).unwrap_or(e.path()).to_str()?.to_string()))
            .collect();

        let mut index = self.load_index()?;
        for filename in &files_to_add {
            let (hash, mode) = self.write_blob(&self.root.join(filename))?;
            index.insert(filename.clone(), TreeEntry { mode, hash });
        }
        self.save_index_atomic(&index)?;
        Ok(files_to_add)
    }

    pub fn write_blob(&self, file_path: &Path) -> VcsResult<(String, u32)> {
        let metadata = std::fs::symlink_metadata(file_path)?;
        if metadata.file_type().is_symlink() {
            let target = std::fs::read_link(file_path)?;
            let content = format!("SYMLINK:{}", target.to_str().unwrap_or(""));
            return self.write_object(&content.into_bytes()).map(|h| (h, 0o120000)); 
        }
        let mode = 0o100644;
        let bytes = std::fs::read(file_path)?;
        self.write_object(&bytes).map(|h| (h, mode))
    }

    pub fn restore(&self, target_hash: &str, force: bool) -> VcsResult<()> {
        let _lock = RepoLock::acquire(self)?;
        let full_hash = self.resolve_hash(target_hash)?;
        if !force {
            let status = self.status()?;
            if !status.modified.is_empty() || !status.untracked.is_empty() || !status.deleted.is_empty() {
                return Err(VcsError::UncommittedChangesRestore);
            }
        }
        let target_commit = self.read_commit(&full_hash)?;
        let target_tree = self.read_tree(&target_commit.tree)?;
        
        for (filename, entry) in &target_tree.files {
            if !is_path_safe(&self.root, filename) { return Err(VcsError::PathTraversal(filename.clone())); }
            let dest = self.root.join(filename);
            let bytes = self.read_blob(&entry.hash)?;
            if let Some(parent) = dest.parent() { std::fs::create_dir_all(parent)?; }
            std::fs::write(&dest, bytes)?;
        }
        self.write_head(&full_hash)?;
        self.save_index_atomic(&BTreeMap::new())?;
        Ok(())
    }

    pub fn status(&self) -> VcsResult<StatusInfo> {
        let index = self.load_index()?;
        let head_hash = self.read_head()?;
        let head_files: Index = if head_hash.is_empty() { Index::new() } else {
            self.read_tree(&self.read_commit(&head_hash)?.tree)?.files
        };

        let mut builder = ignore::WalkBuilder::new(&self.root);
        builder.add_custom_ignore_filename(".simplevcsignore");
        builder.hidden(false);
        builder.filter_entry(|e| e.file_name() != crate::VCS_DIR_NAME && e.file_name() != ".simplevcslink" && e.file_name() != ".HEAD");

        let working_files_vec: Vec<String> = builder.build()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map_or(false, |t| t.is_file() || t.is_symlink()))
            .filter_map(|e| Some(e.path().strip_prefix(&self.root).unwrap_or(e.path()).to_str()?.to_string()))
            .collect();

        let staged: Vec<String> = index.keys()
            .filter(|file| head_files.get(*file).map_or(true, |he| he.hash != index.get(*file).unwrap().hash))
            .cloned().collect();

        let untracked: Vec<String> = working_files_vec.iter()
            .filter(|file| !head_files.contains_key(*file) && !index.contains_key(*file))
            .cloned().collect();

        Ok(StatusInfo { head: head_hash, staged, modified: vec![], untracked, deleted: vec![] })
    }

    pub fn diff(&self) -> VcsResult<Vec<FileDiff>> {
        let index = self.load_index()?;
        let head_hash = self.read_head()?;
        let head_files = if head_hash.is_empty() { Index::new() } else {
            self.read_tree(&self.read_commit(&head_hash)?.tree)?.files
        };

        let mut diffs = Vec::new();
        for (file, head_entry) in &head_files {
            if let Some(index_entry) = index.get(file) {
                if head_entry.hash != index_entry.hash {
                    let old_bytes = self.read_blob(&head_entry.hash)?;
                    let new_bytes = self.read_blob(&index_entry.hash)?;
                    let binary = is_binary(&old_bytes) || is_binary(&new_bytes);
                    let content = if binary { None } else {
                        Some(format!("--- {}\n+++ {}\n{}", file, file, unified_diff(&String::from_utf8_lossy(&old_bytes), &String::from_utf8_lossy(&new_bytes))))
                    };
                    diffs.push(FileDiff { file: file.clone(), binary, content });
                }
            }
        }
        Ok(diffs)
    }

    pub fn history(&self) -> VcsResult<Vec<Commit>> {
        let head_hash = self.read_head()?;
        if head_hash.is_empty() { return Ok(vec![]); }
        let mut history = Vec::new();
        let mut visited = HashSet::new();
        let mut stack = vec![head_hash];
        while let Some(hash) = stack.pop() {
            if !visited.insert(hash.clone()) { continue; }
            if let Ok(commit) = self.read_commit(&hash) {
                history.push(commit.clone());
                for p in &commit.parents { stack.push(p.clone()); }
            }
        }
        history.sort_by(|a, b| b.ts_ms.cmp(&a.ts_ms));
        Ok(history)
    }

    pub fn generate_keys(&self) -> VcsResult<(String, String)> {
        let mut csprng = OsRng;
        let signing_key = SigningKey::generate(&mut csprng);
        let verifying_key = signing_key.verifying_key();
        let sk_bytes = signing_key.to_bytes();
        let pk_bytes = verifying_key.to_bytes();
        self.db.insert("secret_key", &sk_bytes)?;
        self.db.insert("public_key", &pk_bytes)?;
        self.db.flush()?;
        Ok((base64::engine::general_purpose::STANDARD.encode(sk_bytes), base64::engine::general_purpose::STANDARD.encode(pk_bytes)))
    }

    pub fn create_bundle(&self, out_path: &str) -> VcsResult<()> {
        let head = self.read_head()?;
        if head.is_empty() { return Err(VcsError::Bundle("Repository is empty".into())); }
        let objects = self.gather_objects(&head, None)?;
        let packfile = self.pack_objects(&head)?;
        let mut file = std::fs::File::create(out_path)?;
        file.write_all(b"SVBND1\n")?;
        file.write_all(format!("{}\n", head).as_bytes())?;
        file.write_all(&(packfile.len() as u64).to_le_bytes())?;
        file.write_all(&packfile)?;
        info!(path = out_path, objects = objects.len(), "Bundle created");
        Ok(())
    }

    pub fn unbundle(&self, bundle_path: &str) -> VcsResult<String> {
        let _lock = RepoLock::acquire(self)?;
        let mut file = std::fs::File::open(bundle_path)?;
        let mut magic = [0u8; 7];
        std::io::Read::read_exact(&mut file, &mut magic)?;
        if &magic != b"SVBND1\n" { return Err(VcsError::Bundle("Invalid bundle format".into())); }
        let mut head_buf = String::new();
        std::io::BufRead::read_line(&mut std::io::BufReader::new(&file), &mut head_buf)?;
        let new_head = head_buf.trim().to_string();
        let mut len_buf = [0u8; 8];
        std::io::Read::read_exact(&mut file, &mut len_buf)?;
        let pack_len = u64::from_le_bytes(len_buf) as usize;
        let mut packfile = vec![0u8; pack_len];
        std::io::Read::read_exact(&mut file, &mut packfile)?;
        self.unpack_and_write_objects(&packfile)?;
        self.write_head(&new_head)?;
        self.restore(&new_head, true)?;
        info!(head = %new_head, "Bundle applied");
        Ok(new_head)
    }

    pub fn pack_objects(&self, start_hash: &str) -> VcsResult<Vec<u8>> {
        let objects = self.gather_objects(start_hash, None)?;
        let bincode_data = bincode::serialize(&objects)?;
        let hash = Sha256::digest(&bincode_data);
        let mut stream = hash.to_vec();
        stream.extend_from_slice(&bincode_data);
        Ok(zstd::encode_all(stream.as_slice(), 3)?)
    }

    pub fn unpack_and_write_objects(&self, stream: &[u8]) -> VcsResult<usize> {
        let decompressed = zstd::decode_all(stream)?;
        if decompressed.len() < 32 { return Err(VcsError::PackfileIntegrityFailed); }
        let (hash_bytes, payload) = decompressed.split_at(32);
        let computed_hash = Sha256::digest(payload);
        if computed_hash.as_slice() != hash_bytes { return Err(VcsError::PackfileIntegrityFailed); }
        let objects: Vec<NetworkObject> = bincode::deserialize(payload)?;
        let count = objects.len();
        for obj in &objects {
            if !verify_integrity(&obj.hash, &obj.data) { return Err(VcsError::IntegrityError(obj.hash.clone())); }
            let path = self.objects_dir().join(&obj.hash);
            if !path.exists() {
                let compressed = zstd::encode_all(&obj.data[..], 3)?;
                std::fs::write(&path, &compressed)?;
            }
        }
        Ok(count)
    }

    pub fn gather_objects(&self, start_hash: &str, depth: Option<usize>) -> VcsResult<Vec<NetworkObject>> {
        let mut objects = Vec::new();
        let mut visited = HashSet::new();
        let mut stack = vec![(start_hash.to_string(), 0usize)];
        while let Some((h, curr_depth)) = stack.pop() {
            if !visited.insert(h.clone()) { continue; }
            let path = self.objects_dir().join(&h);
            if !path.exists() { continue; }
            let data = zstd::decode_all(std::fs::read(path)?.as_slice())?;
            objects.push(NetworkObject { hash: h.clone(), data: data.clone() });
            if let Ok(commit) = bincode::deserialize::<Commit>(&data) {
                stack.push((commit.tree.clone(), curr_depth));
                if let Ok(tree) = self.read_tree(&commit.tree) { 
                    for blob in tree.files.values() { stack.push((blob.hash.clone(), curr_depth)); } 
                }
                if depth.map_or(true, |d| curr_depth < d) {
                    for p in &commit.parents { stack.push((p.clone(), curr_depth + 1)); }
                }
            }
        }
        Ok(objects)
    }

    pub fn apply_network_objects(&self, objects: Vec<NetworkObject>) -> VcsResult<usize> {
        let _lock = RepoLock::acquire(self)?;
        let mut new_head = String::new();
        let mut count = 0;
        self.db.transaction(|db| {
            for obj in &objects {
                if !verify_integrity(&obj.hash, &obj.data) {
                    return Err(sled::ConflictableTransactionError::Abort(VcsError::IntegrityError(obj.hash.clone())));
                }
                let path = self.objects_dir().join(&obj.hash);
                if !path.exists() {
                    let compressed = zstd::encode_all(&obj.data[..], 3).map_err(|e| sled::ConflictableTransactionError::Abort(VcsError::Io(e)))?;
                    std::fs::write(&path, &compressed).map_err(|e| sled::ConflictableTransactionError::Abort(VcsError::Io(e)))?;
                }
                count += 1;
                if let Ok(commit) = bincode::deserialize::<Commit>(&obj.data) { 
                    if new_head.is_empty() || commit.ts_ms > 0 { new_head = commit.hash.clone(); } 
                }
            }
            Ok(())
        })?;
        if !new_head.is_empty() {
            self.db.insert("NEXT_HEAD", new_head.as_bytes())?;
            self.write_head(&new_head)?;
            self.db.remove("NEXT_HEAD")?;
            self.db.flush()?;
        }
        Ok(count)
    }
}
