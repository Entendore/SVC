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
    cmd.current_dir(temp.path()).args(["commit", "-m", "Initial commit", "-a", "Bot"]).assert().success();
    temp
}

#[test]
fn test_ignore_file_respected() {
    let temp = setup_repo();
    let mut cmd = Command::cargo_bin("simplevcs").unwrap();
    cmd.current_dir(temp.path()).arg("status").assert().success().stdout(predicate::str::contains("app.log").not());
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
    cmd.current_dir(temp.path()).args(["commit", "-m", "Should fail", "-a", "Bot"]).assert().failure().stderr(predicate::str::contains("Hook 'pre-commit' failed"));
}

#[test]
fn test_bundle_workflow() {
    let server_dir = setup_repo();
    let client_dir = TempDir::new().unwrap();
    let bundle_path = server_dir.path().join("history.svbnd");
    let mut cmd = Command::cargo_bin("simplevcs").unwrap();
    cmd.current_dir(server_dir.path()).args(["bundle", "create", bundle_path.to_str().unwrap()]).assert().success();
    assert!(bundle_path.exists());
    let mut cmd = Command::cargo_bin("simplevcs").unwrap();
    cmd.current_dir(client_dir.path()).arg("init").assert().success();
    let mut cmd = Command::cargo_bin("simplevcs").unwrap();
    cmd.current_dir(client_dir.path()).args(["bundle", "unbundle", bundle_path.to_str().unwrap()]).assert().success();
    let restored_content = fs::read_to_string(client_dir.path().join("README.md")).unwrap();
    assert_eq!(restored_content, "# Project\n");
}
