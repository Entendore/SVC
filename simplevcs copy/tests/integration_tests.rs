use simplevcs::*;
use std::fs;
use std::sync::Mutex;
use tempfile::TempDir;

// Because SimpleVCS relies on the Current Working Directory (CWD),
// we must serialize tests to prevent them from interfering with each other.
static TEST_MUTEX: Mutex<()> = Mutex::new(());

fn setup_repo(name: &str) -> TempDir {
    let tmp = tempfile::tempdir().expect("Failed to create temp dir");
    std::env::set_current_dir(tmp.path()).expect("Failed to change CWD");
    init(name).expect("Init failed");
    tmp
}

fn write_commit(filename: &str, content: &str, msg: &str, author: &str) -> String {
    fs::write(filename, content).unwrap();
    add(&[filename.to_string()], false, false, false, false).unwrap();
    commit(msg, author, Vec::new()).unwrap();
    read_head().unwrap()
}

#[test]
fn test_full_lifecycle() {
    let _lock = TEST_MUTEX.lock().unwrap();
    let _tmp = setup_repo("lifecycle-repo");

    // 1. Initial commit
    let h1 = write_commit("file.txt", "line1\nline2\n", "Initial commit", "Alice");
    
    // 2. Modify and commit
    fs::write("file.txt", "line1\nline2 modified\n").unwrap();
    add(&["file.txt".to_string()], false, false, false, false).unwrap();
    commit("Modified line 2", "Bob", vec![]).unwrap();
    let h2 = read_head().unwrap();

    // 3. Check history
    let repo = Repository::open().unwrap();
    let history: Vec<_> = repo.history().collect();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].hash, h2);
    assert_eq!(history[1].hash, h1);

    // 4. Restore to initial state (moves HEAD)
    restore(&h1, &[], false).unwrap();
    let content = fs::read_to_string("file.txt").unwrap();
    assert_eq!(content, "line1\nline2\n");
    
    // HEAD should be back to h1
    assert_eq!(read_head().unwrap(), h1);
}

#[test]
fn test_restore_single_file() {
    let _lock = TEST_MUTEX.lock().unwrap();
    let _tmp = setup_repo("restore-file-repo");

    let h1 = write_commit("file1.txt", "v1", "Commit 1", "Alice");
    write_commit("file2.txt", "v1", "Commit 2", "Bob");
    let h2 = read_head().unwrap();
    
    // Modify both files
    fs::write("file1.txt", "v2").unwrap();
    fs::write("file2.txt", "v2").unwrap();
    
    // Restore only file1.txt from h1 (should NOT move HEAD)
    restore(&h1, &["file1.txt".to_string()], false).unwrap();
    
    assert_eq!(fs::read_to_string("file1.txt").unwrap(), "v1");
    assert_eq!(fs::read_to_string("file2.txt").unwrap(), "v2");
    
    // HEAD should still be h2
    assert_eq!(read_head().unwrap(), h2);
}

#[test]
fn test_ignore_mechanism() {
    let _lock = TEST_MUTEX.lock().unwrap();
    let _tmp = setup_repo("ignore-repo");

    fs::write(".simplevcsignore", "*.log\ntarget/\n").unwrap();
    fs::write("app.log", "log entries").unwrap();
    fs::write("src main.rs", "fn main() {}").unwrap();
    
    // Add all files
    add(&[".".to_string()], true, false, false, false).unwrap();
    
    let index = load_index().unwrap();
    assert!(!index.contains_key("app.log"), "Ignored file was staged");
    assert!(index.contains_key("src main.rs"), "Valid file was not staged");
}

#[test]
fn test_merge_workflow() {
    let _lock = TEST_MUTEX.lock().unwrap();
    let _tmp = setup_repo("merge-repo");

    // Base commit
    write_commit("base.txt", "base", "Base commit", "Alice");
    let base = read_head().unwrap();

    // Diverging commit 1
    write_commit("feature1.txt", "feat1", "Feature 1", "Bob");
    let feat1 = read_head().unwrap();

    // Reset to base for diverging commit 2
    restore(&base, &[], false).unwrap();
    write_commit("feature2.txt", "feat2", "Feature 2", "Charlie");
    let feat2 = read_head().unwrap();

    // Merge feat1 into feat2
    merge(
        vec![feat1.clone()],
        "Merger".to_string(),
        Some("Merging feature 1".to_string()),
        false, false, false, false, false, "manual".to_string(), false, false, None
    ).unwrap();

    let repo = Repository::open().unwrap();
    let head_commit = repo.commits.get(&repo.head).unwrap();
    
    assert!(head_commit.merged_from.contains(&feat1));
    assert!(head_commit.files.contains_key("feature1.txt"));
    assert!(head_commit.files.contains_key("feature2.txt"));
    assert!(head_commit.co_authors.contains(&"Bob".to_string()));
}

#[test]
fn test_delete_and_gc() {
    let _lock = TEST_MUTEX.lock().unwrap();
    let _tmp = setup_repo("gc-repo");

    let h1 = write_commit("file1.txt", "content1", "Commit 1", "Alice");
    let _h2 = write_commit("file2.txt", "content2", "Commit 2", "Bob");
    
    // Delete HEAD (Commit 2)
    delete(&read_head().unwrap(), false, true, true).unwrap();
    
    let commits = load_commits().unwrap();
    assert_eq!(commits.len(), 1, "Should only have 1 commit left");
    assert!(commits.contains_key(&h1));
    
    // GC should remove orphaned objects associated with file2.txt
    gc().unwrap();
}

#[test]
fn test_diff_logic() {
    let _lock = TEST_MUTEX.lock().unwrap();
    let _tmp = setup_repo("diff-repo");

    let h1 = write_commit("doc.txt", "Hello World\nFoo\nBar\n", "Initial", "Alice");
    let h2 = write_commit("doc.txt", "Hello World\nBar\nBaz\n", "Updated", "Alice");
    
    // Just ensuring diff runs without panicking on LCS algorithm
    diff(Some(h1.clone()), Some(h2.clone())).unwrap();
    diff(None, None).unwrap(); // Should diff HEAD against parent
}

#[test]
fn test_status_states() {
    let _lock = TEST_MUTEX.lock().unwrap();
    let _tmp = setup_repo("status-repo");

    write_commit("tracked.txt", "original", "Commit 1", "Alice");
    
    // Untracked file
    fs::write("untracked.txt", "new").unwrap();
    
    // Modified file (unstaged)
    fs::write("tracked.txt", "modified").unwrap();
    
    // Should not panic and correctly identify states
    status().unwrap();
    
    // Stage the modification
    add(&["tracked.txt".to_string()], false, false, false, false).unwrap();
    status().unwrap();
}

#[test]
fn test_blame_attribution() {
    let _lock = TEST_MUTEX.lock().unwrap();
    let _tmp = setup_repo("blame-repo");

    write_commit("code.rs", "fn main() {\n    println!(\"Hi\");\n}\n", "Initial", "Alice");
    
    fs::write("code.rs", "fn main() {\n    println!(\"Hello\");\n}\n").unwrap();
    add(&["code.rs".to_string()], false, false, false, false).unwrap();
    commit("Updated greeting", "Bob", vec![]).unwrap();

    // Just ensure blame executes successfully
    blame("code.rs", None, false).unwrap();
    blame("code.rs", Some("Bob".to_string()), true).unwrap();
}

#[test]
fn test_show_command() {
    let _lock = TEST_MUTEX.lock().unwrap();
    let _tmp = setup_repo("show-repo");

    let h1 = write_commit("data.txt", "some data\nhere\n", "Initial", "Alice");
    
    // Modify the file in working directory
    fs::write("data.txt", "working tree data").unwrap();
    
    // Show should retrieve the committed version, not the working tree
    // We can't easily capture stdout in this simple test, but we ensure it doesn't error
    show(&h1, "data.txt").unwrap();
}
