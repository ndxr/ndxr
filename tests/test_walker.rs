//! Integration tests for the filesystem walker.

use std::fs;

use tempfile::TempDir;

fn setup_workspace(tmp: &TempDir) {
    fs::create_dir(tmp.path().join(".git")).unwrap();
    fs::create_dir_all(tmp.path().join("src")).unwrap();
    fs::write(tmp.path().join("src/main.ts"), "export function main() {}").unwrap();
    fs::write(tmp.path().join("src/util.js"), "function util() {}").unwrap();
    fs::write(tmp.path().join("src/helper.py"), "def helper(): pass").unwrap();

    // node_modules should be gitignored.
    fs::write(tmp.path().join(".gitignore"), "node_modules/\n").unwrap();
    fs::create_dir_all(tmp.path().join("node_modules")).unwrap();
    fs::write(tmp.path().join("node_modules/dep.js"), "ignored").unwrap();

    // Unsupported extension.
    fs::write(tmp.path().join("src/data.json"), "{}").unwrap();

    // .ndxr directory should be skipped.
    fs::create_dir_all(tmp.path().join(".ndxr")).unwrap();
    fs::write(tmp.path().join(".ndxr/index.db"), "ignored").unwrap();
}

#[test]
fn walks_supported_files_only() {
    let tmp = TempDir::new().unwrap();
    setup_workspace(&tmp);
    let files = ndxr::indexer::walker::walk_workspace(tmp.path()).unwrap();
    // Should find main.ts, util.js, helper.py (3 supported files).
    assert_eq!(files.len(), 3);
}

#[test]
fn respects_ndxrignore() {
    let tmp = TempDir::new().unwrap();
    setup_workspace(&tmp);
    fs::write(tmp.path().join(".ndxrignore"), "src/helper.py\n").unwrap();
    let files = ndxr::indexer::walker::walk_workspace(tmp.path()).unwrap();
    assert!(
        !files
            .iter()
            .any(|p| p.display().to_string().contains("helper.py"))
    );
}

#[test]
fn skips_ndxr_directory() {
    let tmp = TempDir::new().unwrap();
    setup_workspace(&tmp);
    let files = ndxr::indexer::walker::walk_workspace(tmp.path()).unwrap();
    assert!(
        !files
            .iter()
            .any(|p| p.display().to_string().contains(".ndxr"))
    );
}

#[test]
fn skips_files_over_size_limit() {
    let tmp = TempDir::new().unwrap();
    setup_workspace(&tmp);
    let large_content = "x".repeat(2_000_000);
    fs::write(tmp.path().join("src/large.ts"), large_content).unwrap();
    let files = ndxr::indexer::walker::walk_workspace(tmp.path()).unwrap();
    assert!(
        !files
            .iter()
            .any(|p| p.display().to_string().contains("large.ts"))
    );
}

#[test]
fn deterministic_ordering() {
    let tmp = TempDir::new().unwrap();
    setup_workspace(&tmp);
    let files1 = ndxr::indexer::walker::walk_workspace(tmp.path()).unwrap();
    let files2 = ndxr::indexer::walker::walk_workspace(tmp.path()).unwrap();
    assert_eq!(files1, files2);
}

#[test]
fn respects_custom_max_size() {
    let tmp = TempDir::new().unwrap();
    setup_workspace(&tmp);
    // With a very small limit, most files should be excluded.
    let files = ndxr::indexer::walker::walk_workspace_with_max_size(tmp.path(), 5).unwrap();
    // "def helper(): pass" is 19 bytes, all files exceed 5 bytes.
    assert!(files.is_empty());
}
