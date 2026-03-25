//! Integration tests for the `ndxr upgrade` command.

use assert_cmd::Command;
use predicates::str::contains;

#[test]
fn upgrade_help_shows_flags() {
    Command::cargo_bin("ndxr")
        .unwrap()
        .args(["upgrade", "--help"])
        .assert()
        .success()
        .stdout(contains("--check"))
        .stdout(contains("--force"));
}

#[test]
fn quick_start_lists_upgrade_command() {
    Command::cargo_bin("ndxr")
        .unwrap()
        .assert()
        .success()
        .stdout(contains("upgrade"));
}

/// Smoke test: `ndxr upgrade --check` hits the live GitHub API and returns
/// a valid version comparison. Requires network access.
#[test]
#[ignore]
fn upgrade_check_returns_version_info() {
    let assert = Command::cargo_bin("ndxr")
        .unwrap()
        .args(["upgrade", "--check"])
        .assert();

    // Should always print "Current: v" regardless of outcome.
    assert.stdout(contains("Current: v"));
}

/// CLI test: `ndxr upgrade --check` exercises the real CLI binary against
/// the live GitHub API. Verifies output format regardless of whether
/// an update is available or not.
#[test]
#[ignore]
fn upgrade_check_prints_version_comparison() {
    let output = Command::cargo_bin("ndxr")
        .unwrap()
        .args(["upgrade", "--check"])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Both version lines must always be present.
    assert!(
        stdout.contains("Current: v"),
        "Should print current version, got: {stdout}"
    );
    assert!(
        stdout.contains("Latest:  v"),
        "Should print latest version, got: {stdout}"
    );

    // Exactly one of these outcomes must appear.
    let up_to_date = stdout.contains("Already up to date.");
    let update_available = stdout.contains("Update available: v");
    assert!(
        up_to_date || update_available,
        "Should print either 'Already up to date.' or 'Update available', got: {stdout}"
    );

    // Exit code: 0 if up-to-date, 1 if update available.
    if up_to_date {
        assert!(output.status.success(), "Up-to-date should exit 0");
    } else {
        assert!(!output.status.success(), "Update available should exit 1");
    }
}

// ---------------------------------------------------------------------------
// End-to-end upgrade pipeline test
//
// This test exercises the FULL upgrade flow against the real GitHub Releases
// API, exactly as a user would experience it. It downloads a real release
// binary, verifies the SHA256 checksum, extracts it from the archive, and
// then *executes* the downloaded binary to prove it is a valid ndxr binary.
//
// The only step NOT tested is `replace_binary()` — actually overwriting the
// running binary would be destructive and is delegated to the `self_replace`
// crate which has its own test suite.
//
// Requires network access. Run with: cargo test --test test_upgrade -- --ignored
// ---------------------------------------------------------------------------

/// End-to-end test: check → download → verify checksum → extract → execute binary.
///
/// Proves the entire upgrade pipeline works against a real GitHub release.
#[test]
#[ignore]
fn end_to_end_upgrade_pipeline() {
    // Step 1: Check for updates against the real GitHub API.
    let status = ndxr::upgrade::check_for_update().unwrap();

    // The compiled-in version must match CARGO_PKG_VERSION.
    let expected_current = semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap();
    assert_eq!(
        status.current, expected_current,
        "Compiled-in version should match CARGO_PKG_VERSION"
    );

    // Latest version must be a valid semver.
    assert!(
        !status.latest.to_string().is_empty(),
        "Latest version from GitHub should be a valid semver string"
    );

    // Step 2: Verify platform detection found an asset for this machine.
    let suffix = ndxr::upgrade::current_platform_suffix();
    assert!(
        suffix.is_some(),
        "This test requires a supported platform (got OS={}, ARCH={})",
        std::env::consts::OS,
        std::env::consts::ARCH
    );

    let Some(asset) = status.asset else {
        panic!(
            "Platform {} is supported but no matching asset found in release v{}",
            suffix.unwrap(),
            status.latest
        );
    };

    // Asset name should follow the naming convention.
    assert!(
        asset.name.starts_with("ndxr-v"),
        "Asset name should start with 'ndxr-v', got: {}",
        asset.name
    );
    assert!(
        asset.name.contains(suffix.unwrap()),
        "Asset name should contain platform suffix '{}', got: {}",
        suffix.unwrap(),
        asset.name
    );

    // Both URLs must be HTTPS.
    assert!(
        ndxr::upgrade::validate_https_url(&asset.url).is_ok(),
        "Asset download URL must be HTTPS: {}",
        asset.url
    );
    assert!(
        ndxr::upgrade::validate_https_url(&asset.checksums_url).is_ok(),
        "Checksums URL must be HTTPS: {}",
        asset.checksums_url
    );

    // Step 3: Download the real archive and verify checksum.
    let result = ndxr::upgrade::download_and_verify(&asset).unwrap();

    // GitHub always provides Content-Length for release assets.
    assert!(
        result.download_size.is_some(),
        "Download size should be reported by GitHub"
    );
    let size = result.download_size.unwrap();
    // A real ndxr binary archive should be at least 1 MiB.
    assert!(
        size > 1_000_000,
        "Downloaded archive seems too small ({size} bytes) — expected >1 MiB for a real release"
    );

    // The extracted binary must exist on disk.
    assert!(
        result.binary_path.exists(),
        "Extracted binary should exist at: {}",
        result.binary_path.display()
    );

    // The binary file should be non-trivially sized (real ndxr is >10 MiB).
    let binary_size = std::fs::metadata(&result.binary_path).unwrap().len();
    assert!(
        binary_size > 1_000_000,
        "Extracted binary seems too small ({binary_size} bytes) — expected >1 MiB"
    );

    // Step 4: Execute the downloaded binary to prove it's a valid ndxr executable.
    // This is the ultimate proof — if we can run it, the upgrade would work.
    let output = std::process::Command::new(&result.binary_path)
        .arg("--version")
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "Failed to execute downloaded binary at {}: {e}",
                result.binary_path.display()
            )
        });

    assert!(
        output.status.success(),
        "Downloaded binary should exit successfully with --version (exit code: {:?})",
        output.status.code()
    );

    let version_output = String::from_utf8_lossy(&output.stdout);
    assert!(
        version_output.contains("ndxr"),
        "Version output should contain 'ndxr', got: {version_output}"
    );

    // The binary should also respond to --help (proves CLI parsing works).
    let help_output = std::process::Command::new(&result.binary_path)
        .arg("--help")
        .output()
        .expect("--help should succeed on the downloaded binary");

    assert!(help_output.status.success());
    let help_text = String::from_utf8_lossy(&help_output.stdout);
    // Check for "ndxr" in help — the binary name will always be there.
    // Avoid asserting on tagline text that could change between releases.
    assert!(
        help_text.contains("ndxr"),
        "Help output should contain 'ndxr', got: {help_text}"
    );

    // Step 5: Clean up.
    let _ = std::fs::remove_file(&result.binary_path);
    if let Some(parent) = result.binary_path.parent() {
        let _ = std::fs::remove_dir(parent);
    }

    eprintln!("=== End-to-end upgrade pipeline test passed ===");
    eprintln!("  Current version: v{}", status.current);
    eprintln!("  Latest version:  v{}", status.latest);
    eprintln!("  Downloaded:      {} ({} bytes)", asset.name, size);
    eprintln!("  Binary size:     {binary_size} bytes");
    eprintln!("  Binary executed: --version and --help both succeeded");
}
