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
fn returns_error_when_no_git_root() {
    let tmp = TempDir::new().unwrap();
    let result = ndxr::workspace::find_workspace_root(tmp.path());
    assert!(result.is_err());
}

#[test]
fn finds_git_root_from_exact_root() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir(tmp.path().join(".git")).unwrap();

    let root = ndxr::workspace::find_workspace_root(tmp.path()).unwrap();
    assert_eq!(root, tmp.path().canonicalize().unwrap());
}
