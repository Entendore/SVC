#!/bin/bash

echo "🧪 Setting up comprehensive test suite in tests/ directory..."

# 1. Update src/odb.rs to make pure functions public for unit testing
echo "📝 Exposing pure functions in src/odb.rs for testing..."
sed -i.bak 's/^fn unified_diff/pub fn unified_diff/g; s/^pub fn unified_diff/pub fn unified_diff/g; s/^fn diff_lcs/pub fn diff_lcs/g; s/^fn is_binary/pub fn is_binary/g; s/^pub fn verify_integrity/pub fn verify_integrity/g; s/^fn is_path_safe/pub fn is_path_safe/g; s/^pub fn hash_file/pub fn hash_file/g; s/^fn write_string_atomic/pub fn write_string_atomic/g' src/odb.rs
rm -f src/odb.rs.bak

# Remove any existing inline tests from odb.rs to avoid duplication
echo "📝 Removing inline tests from src/odb.rs..."
python - << 'EOF'
import re
with open('src/odb.rs', 'r') as f:
    content = f.read()
content = re.sub(r'#\[cfg\(test\)\]\s*mod tests \{.*?\n\}\n', '', content, flags=re.DOTALL)
with open('src/odb.rs', 'w') as f:
    f.write(content)
EOF

# 2. Create the core unit tests file for pure algorithms
echo "📝 Writing tests/core_tests.rs..."
cat << 'EOF' > tests/core_tests.rs
use simplevcs::odb::*;
use sha2::{Sha256, Digest};
use tempfile::tempdir;

#[test]
fn test_unified_diff() {
    let old = "hello\nworld\n";
    let new = "hello\nrust\n";
    let diff = unified_diff(old, new);
    assert!(diff.contains(" hello"));
    assert!(diff.contains("-world"));
    assert!(diff.contains("+rust"));
}

#[test]
fn test_diff_lcs_char_tracking() {
    let old: Vec<char> = "abc".chars().collect();
    let new: Vec<char> = "axc".chars().collect();
    let ops = diff_lcs(&old, &new);
    
    // Should be: a (match), x (add), b (del), c (match)
    assert_eq!(ops.len(), 4);
    
    // Verify 'x' is marked as new (old_idx is None)
    let x_op = ops.iter().find(|(c, _, _)| *c == 'x').unwrap();
    assert!(x_op.1.is_none()); 
}

#[test]
fn test_verify_integrity() {
    let data = b"my cool commit data";
    let hash = hex::encode(Sha256::digest(data));
    
    assert!(verify_integrity(&hash, data), "Valid data should pass");
    
    let tampered_data = b"my evil commit data";
    assert!(!verify_integrity(&hash, tampered_data), "Tampered data should fail");
}

#[test]
fn test_path_safety() {
    let temp = tempdir().unwrap();
    let root = temp.path();
    
    assert!(is_path_safe(root, "safe_file.txt"));
    assert!(is_path_safe(root, "folder/safe_file.txt"));
    
    // Path traversal attempts
    assert!(!is_path_safe(root, "../escape.txt"));
    assert!(!is_path_safe(root, "folder/../../escape.txt"));
}

#[test]
fn test_is_binary() {
    assert!(!is_binary(b"hello world"));
    assert!(is_binary(&[0x00, 0x01, 0x02, 0x03]));
}

#[test]
fn test_hash_file() {
    let temp = tempdir().unwrap();
    let file_path = temp.path().join("test.txt");
    std::fs::write(&file_path, "test content").unwrap();
    
    let hash = hash_file(&file_path).unwrap();
    assert_eq!(hash.len(), 64); // SHA-256 hex length
}
EOF

# 3. Create the comprehensive E2E CLI tests file
echo "📝 Writing tests/cli_tests.rs..."
cat << 'EOF' > tests/cli_tests.rs
use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;

fn setup_repo() -> TempDir {
    let temp = TempDir::new().unwrap();
    
    let mut cmd = Command::cargo_bin("simplevcs").unwrap();
    cmd.current_dir(temp.path()).arg("init").assert().success();

    fs::write(temp.path().join(".simplevcsignore"), "*.log\n").unwrap();
    fs::write(temp.path().join("README.md"), "# Project\n").unwrap();
    fs::write(temp.path().join("app.log"), "logs\n").unwrap();

    let mut cmd = Command::cargo_bin("simplevcs").unwrap();
    cmd.current_dir(temp.path()).args(["add", "."]).assert().success();

    let mut cmd = Command::cargo_bin("simplevcs").unwrap();
    cmd.current_dir(temp.path())
        .args(["commit", "-m", "Initial commit", "-a", "Test Bot"])
        .assert()
        .success();
    
    temp
}

#[test]
fn test_init_creates_dirs() {
    let temp = TempDir::new().unwrap();
    let mut cmd = Command::cargo_bin("simplevcs").unwrap();
    cmd.current_dir(temp.path()).args(["init", "my-repo"]).assert().success();
    
    assert!(temp.path().join(".simplevcs").exists());
    assert!(temp.path().join(".simplevcs/objects").exists());
    assert!(temp.path().join(".simplevcs/db").exists());
}

#[test]
fn test_ignore_file_respected() {
    let temp = setup_repo();
    
    let mut cmd = Command::cargo_bin("simplevcs").unwrap();
    cmd.current_dir(temp.path())
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("app.log").not());
}

#[test]
fn test_status_shows_modified() {
    let temp = setup_repo();
    fs::write(temp.path().join("README.md"), "V2\n").unwrap();
    
    let mut cmd = Command::cargo_bin("simplevcs").unwrap();
    cmd.current_dir(temp.path())
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("README.md"));
}

#[test]
fn test_hooks_execute_and_block() {
    let temp = setup_repo();
    let hooks_dir = temp.path().join(".simplevcs/hooks");
    fs::create_dir_all(&hooks_dir).unwrap();
    
    #[cfg(unix)]
    {
        let hook_path = hooks_dir.join("pre-commit");
        fs::write(&hook_path, "#!/bin/sh\necho 'No commits allowed!'\nexit 1\n").unwrap();
        std::fs::set_permissions(&hook_path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    }

    fs::write(temp.path().join("README.md"), "V2\n").unwrap();
    let mut cmd = Command::cargo_bin("simplevcs").unwrap();
    cmd.current_dir(temp.path()).args(["add", "README.md"]).assert().success();

    let mut cmd = Command::cargo_bin("simplevcs").unwrap();
    cmd.current_dir(temp.path())
        .args(["commit", "-m", "Should fail", "-a", "Test Bot"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Hook 'pre-commit' failed"));
}

#[test]
fn test_bundle_workflow() {
    let server_dir = setup_repo();
    let client_dir = TempDir::new().unwrap();

    let bundle_path = server_dir.path().join("history.svbnd");
    let mut cmd = Command::cargo_bin("simplevcs").unwrap();
    cmd.current_dir(server_dir.path())
        .args(["bundle", "create", bundle_path.to_str().unwrap()])
        .assert()
        .success();
    
    assert!(bundle_path.exists());

    let mut cmd = Command::cargo_bin("simplevcs").unwrap();
    cmd.current_dir(client_dir.path()).arg("init").assert().success();

    let mut cmd = Command::cargo_bin("simplevcs").unwrap();
    cmd.current_dir(client_dir.path())
        .args(["bundle", "unbundle", bundle_path.to_str().unwrap()])
        .assert()
        .success();

    let restored_content = fs::read_to_string(client_dir.path().join("README.md")).unwrap();
    assert_eq!(restored_content, "# Project\n");
}

#[test]
fn test_tags_and_history() {
    let temp = setup_repo();
    
    let mut cmd = Command::cargo_bin("simplevcs").unwrap();
    let output = cmd.current_dir(temp.path())
        .args(["--json", "log"])
        .output()
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let hash = json["data"]["head"].as_str().unwrap();

    let mut cmd = Command::cargo_bin("simplevcs").unwrap();
    cmd.current_dir(temp.path())
        .args(["tag", "add", "v1.0", hash])
        .assert()
        .success();

    let mut cmd = Command::cargo_bin("simplevcs").unwrap();
    cmd.current_dir(temp.path())
        .args["tag", "list"]
        .assert()
        .success()
        .stdout(predicate::str::contains("v1.0"));

    let mut cmd = Command::cargo_bin("simplevcs").unwrap();
    cmd.current_dir(temp.path())
        .args["history"]
        .assert()
        .success()
        .stdout(predicate::str::contains("Initial commit"));
}

#[test]
fn test_fsck_passes() {
    let temp = setup_repo();
    let mut cmd = Command::cargo_bin("simplevcs").unwrap();
    cmd.current_dir(temp.path())
        .arg("fsck")
        .assert()
        .success()
        .stdout(predicate::str::contains("Filesystem check passed"));
}

#[test]
fn test_gc_runs() {
    let temp = setup_repo();
    let mut cmd = Command::cargo_bin("simplevcs").unwrap();
    cmd.current_dir(temp.path())
        .arg("gc")
        .assert()
        .success()
        .stdout(predicate::str::contains("Deleted").or(predicate::str::contains("kept")));
}

#[test]
fn test_render_graceful_failure_without_ffmpeg() {
    let temp = setup_repo();
    
    let mut cmd = Command::cargo_bin("simplevcs").unwrap();
    let assert = cmd.current_dir(temp.path())
        .args(["render", "README.md"])
        .assert();
        
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr);
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    let output_str = stderr + &stdout;
    
    assert!(
        output_str.contains("FFmpeg not found") || output_str.contains("Rendered"),
        "Should either render or fail gracefully with ffmpeg message. Got: {}", output_str
    );
}
EOF

echo "✅ Test suite generated in tests/ directory!"
echo "👉 Run tests with: cargo test --verbose"