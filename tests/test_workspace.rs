use std::fs;
use tempfile::TempDir;

#[test]
fn finds_git_root_from_cwd() {
    let tmp = TempDir::new().unwrap();
    let git_dir = tmp.path().join(".git");
    fs::create_dir(&git_dir).unwrap();
    let sub = tmp.path().join("src").join("deep");
    fs::create_dir_all(&sub).unwrap();

    let root = ndxr::workspace::find_workspace_root(&sub).unwrap();
    assert_eq!(root, tmp.path().canonicalize().unwrap());
}

#[test]
fn finds_git_root_from_exact_root() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir(tmp.path().join(".git")).unwrap();

    let root = ndxr::workspace::find_workspace_root(tmp.path()).unwrap();
    assert_eq!(root, tmp.path().canonicalize().unwrap());
}

#[test]
fn finds_ndxr_root_when_no_git() {
    let tmp = TempDir::new().unwrap();
    let ndxr_dir = tmp.path().join(".ndxr");
    fs::create_dir(&ndxr_dir).unwrap();
    let sub = tmp.path().join("src").join("deep");
    fs::create_dir_all(&sub).unwrap();

    let root = ndxr::workspace::find_workspace_root(&sub).unwrap();
    assert_eq!(root, tmp.path().canonicalize().unwrap());
}

#[test]
fn git_root_takes_priority_over_ndxr() {
    let tmp = TempDir::new().unwrap();
    // .git/ at root, .ndxr/ in a subdirectory
    fs::create_dir(tmp.path().join(".git")).unwrap();
    let sub = tmp.path().join("subproject");
    fs::create_dir_all(&sub).unwrap();
    fs::create_dir(sub.join(".ndxr")).unwrap();

    let root = ndxr::workspace::find_workspace_root(&sub).unwrap();
    // Should find .git/ at the parent, not .ndxr/ in the child
    assert_eq!(root, tmp.path().canonicalize().unwrap());
}

#[test]
fn error_message_when_no_git_or_ndxr() {
    let tmp = TempDir::new().unwrap();
    let result = ndxr::workspace::find_workspace_root(tmp.path());
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("not an ndxr workspace"),
        "error message should be actionable, got: {msg}"
    );
    assert!(
        msg.contains("git init"),
        "error should suggest git init, got: {msg}"
    );
}
