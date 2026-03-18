//! Integration tests for the ndxr CLI.

use std::fs;

use assert_cmd::Command;
use predicates::str::contains;
use tempfile::TempDir;

#[test]
fn ndxr_version() {
    Command::cargo_bin("ndxr")
        .unwrap()
        .arg("--version")
        .assert()
        .success()
        .stdout(contains("ndxr"));
}

#[test]
fn ndxr_help_shows_commands() {
    // No args shows the quick start guide.
    Command::cargo_bin("ndxr")
        .unwrap()
        .assert()
        .success()
        .stdout(contains("COMMANDS:"));
}

#[test]
fn ndxr_index_then_status() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir(tmp.path().join(".git")).unwrap();
    fs::write(tmp.path().join("main.ts"), "export function main() {}").unwrap();

    Command::cargo_bin("ndxr")
        .unwrap()
        .current_dir(tmp.path())
        .arg("index")
        .assert()
        .success();

    Command::cargo_bin("ndxr")
        .unwrap()
        .current_dir(tmp.path())
        .arg("status")
        .assert()
        .success()
        .stdout(contains("Files:"));
}

#[test]
fn ndxr_search_returns_results() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir(tmp.path().join(".git")).unwrap();
    fs::write(
        tmp.path().join("auth.ts"),
        "export function validateToken(token: string) { return true; }",
    )
    .unwrap();

    Command::cargo_bin("ndxr")
        .unwrap()
        .current_dir(tmp.path())
        .arg("index")
        .assert()
        .success();

    Command::cargo_bin("ndxr")
        .unwrap()
        .current_dir(tmp.path())
        .args(["search", "validateToken"])
        .assert()
        .success()
        .stdout(contains("validateToken"));
}

#[test]
fn ndxr_status_json() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir(tmp.path().join(".git")).unwrap();
    fs::write(tmp.path().join("main.ts"), "export function main() {}").unwrap();

    Command::cargo_bin("ndxr")
        .unwrap()
        .current_dir(tmp.path())
        .arg("index")
        .assert()
        .success();

    Command::cargo_bin("ndxr")
        .unwrap()
        .current_dir(tmp.path())
        .args(["status", "--json"])
        .assert()
        .success()
        .stdout(contains("\"files\""));
}

#[test]
fn ndxr_setup_creates_mcp_json() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir(tmp.path().join(".git")).unwrap();

    Command::cargo_bin("ndxr")
        .unwrap()
        .current_dir(tmp.path())
        .args(["setup", "--scope", "project"])
        .assert()
        .success();

    let mcp_json = fs::read_to_string(tmp.path().join(".mcp.json")).unwrap();
    assert!(mcp_json.contains("ndxr"));
    assert!(mcp_json.contains("mcp"));

    // CLAUDE.md should also be created.
    let claude_md = fs::read_to_string(tmp.path().join("CLAUDE.md")).unwrap();
    assert!(claude_md.contains("ndxr context engine"));
}

#[test]
fn ndxr_setup_merges_existing_mcp_json() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir(tmp.path().join(".git")).unwrap();

    // Write existing .mcp.json with another server.
    fs::write(
        tmp.path().join(".mcp.json"),
        r#"{
  "mcpServers": {
    "other-server": {
      "command": "other",
      "args": []
    }
  }
}"#,
    )
    .unwrap();

    Command::cargo_bin("ndxr")
        .unwrap()
        .current_dir(tmp.path())
        .args(["setup", "--scope", "project"])
        .assert()
        .success();

    let mcp_json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(tmp.path().join(".mcp.json")).unwrap()).unwrap();

    // Both servers should be present.
    assert!(mcp_json["mcpServers"]["ndxr"].is_object());
    assert!(mcp_json["mcpServers"]["other-server"].is_object());
}

#[test]
fn ndxr_skeleton_shows_signatures() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir(tmp.path().join(".git")).unwrap();
    fs::write(
        tmp.path().join("auth.ts"),
        r"
export class AuthService {
    validate(token: string): boolean { return true; }
    refresh(token: string): string { return token; }
}
",
    )
    .unwrap();

    Command::cargo_bin("ndxr")
        .unwrap()
        .current_dir(tmp.path())
        .arg("index")
        .assert()
        .success();

    Command::cargo_bin("ndxr")
        .unwrap()
        .current_dir(tmp.path())
        .args(["skeleton", "auth.ts"])
        .assert()
        .success();
}

#[test]
fn ndxr_setup_merges_existing_claude_md() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir(tmp.path().join(".git")).unwrap();

    // Write existing CLAUDE.md with other content.
    fs::write(
        tmp.path().join("CLAUDE.md"),
        "# Project Rules\n\nThis project uses TypeScript.\n",
    )
    .unwrap();

    Command::cargo_bin("ndxr")
        .unwrap()
        .current_dir(tmp.path())
        .args(["setup", "--scope", "project"])
        .assert()
        .success();

    let claude_md = fs::read_to_string(tmp.path().join("CLAUDE.md")).unwrap();
    // Original content should be preserved.
    assert!(claude_md.contains("# Project Rules"));
    assert!(claude_md.contains("This project uses TypeScript."));
    // ndxr section should be appended.
    assert!(claude_md.contains("## ndxr context engine"));
}

#[test]
fn ndxr_reindex_works() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir(tmp.path().join(".git")).unwrap();
    fs::write(tmp.path().join("main.ts"), "export function main() {}").unwrap();

    // Index first.
    Command::cargo_bin("ndxr")
        .unwrap()
        .current_dir(tmp.path())
        .arg("index")
        .assert()
        .success();

    // Re-index.
    Command::cargo_bin("ndxr")
        .unwrap()
        .current_dir(tmp.path())
        .arg("reindex")
        .assert()
        .success()
        .stdout(contains("Re-indexed"));
}

#[test]
fn ndxr_search_with_explain() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir(tmp.path().join(".git")).unwrap();
    fs::write(
        tmp.path().join("auth.ts"),
        "export function validateToken(token: string) { return true; }",
    )
    .unwrap();

    Command::cargo_bin("ndxr")
        .unwrap()
        .current_dir(tmp.path())
        .arg("index")
        .assert()
        .success();

    Command::cargo_bin("ndxr")
        .unwrap()
        .current_dir(tmp.path())
        .args(["search", "validateToken", "--explain"])
        .assert()
        .success()
        .stdout(contains("bm25="));
}

#[test]
fn ndxr_search_with_intent_override() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir(tmp.path().join(".git")).unwrap();
    fs::write(
        tmp.path().join("auth.ts"),
        "export function validateToken(token: string) { return true; }",
    )
    .unwrap();

    Command::cargo_bin("ndxr")
        .unwrap()
        .current_dir(tmp.path())
        .arg("index")
        .assert()
        .success();

    Command::cargo_bin("ndxr")
        .unwrap()
        .current_dir(tmp.path())
        .args(["search", "validateToken", "--intent", "debug", "--explain"])
        .assert()
        .success()
        .stdout(contains("intent=debug"));
}
