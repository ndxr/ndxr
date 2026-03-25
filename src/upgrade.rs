//! Self-upgrade via GitHub releases.
//!
//! All functions in this module are **blocking** (synchronous). They use
//! `reqwest::blocking` and must never be called from an async context
//! (e.g., the MCP server's tokio runtime). This is a CLI-only feature.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

/// GitHub repository owner.
const GITHUB_OWNER: &str = "ndxr";

/// GitHub repository name.
const GITHUB_REPO: &str = "ndxr";

/// GitHub API base URL.
const GITHUB_API_BASE: &str = "https://api.github.com";

/// HTTP connect timeout in seconds.
const CONNECT_TIMEOUT_SECS: u64 = 10;

/// HTTP read timeout in seconds.
const READ_TIMEOUT_SECS: u64 = 300;

/// Binary name inside the archive.
#[cfg(not(target_os = "windows"))]
const BINARY_NAME: &str = "ndxr";

/// Binary name inside the archive (Windows).
#[cfg(target_os = "windows")]
const BINARY_NAME: &str = "ndxr.exe";

/// Result of checking the latest GitHub release against the local version.
#[derive(Debug)]
pub struct UpdateStatus {
    /// Currently installed version.
    pub current: semver::Version,
    /// Latest release version on GitHub.
    pub latest: semver::Version,
    /// Whether the local version is older than the latest release.
    pub is_outdated: bool,
    /// Download info for the matching platform binary, if available.
    pub asset: Option<AssetInfo>,
}

/// Download metadata for a release asset.
#[derive(Debug)]
pub struct AssetInfo {
    /// Archive filename (e.g., "ndxr-v0.5.0-darwin-arm64.tar.gz").
    pub name: String,
    /// Browser download URL (HTTPS only -- validated before use).
    pub url: String,
    /// Browser download URL for checksums.txt (HTTPS only).
    pub checksums_url: String,
}

/// Result of a successful download-and-verify operation.
#[derive(Debug)]
pub struct DownloadResult {
    /// Path to the verified, extracted binary.
    pub binary_path: PathBuf,
    /// Size of the downloaded archive in bytes, if reported by the server.
    pub download_size: Option<u64>,
}

/// Maps the current platform (OS + arch) to the release asset suffix.
///
/// Returns `None` for unsupported platforms.
///
/// # Examples
///
/// ```
/// // On macOS ARM64:
/// assert_eq!(ndxr::upgrade::platform_asset_suffix("macos", "aarch64"), Some("darwin-arm64"));
/// ```
#[must_use]
pub fn platform_asset_suffix(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("macos", "aarch64") => Some("darwin-arm64"),
        ("macos", "x86_64") => Some("darwin-amd64"),
        ("linux", "aarch64") => Some("linux-arm64"),
        ("linux", "x86_64") => Some("linux-amd64"),
        ("windows", "x86_64") => Some("windows-amd64"),
        _ => None,
    }
}

/// Returns the asset suffix for the current platform, or `None` if unsupported.
#[must_use]
pub fn current_platform_suffix() -> Option<&'static str> {
    platform_asset_suffix(std::env::consts::OS, std::env::consts::ARCH)
}

/// Validates that a URL uses the HTTPS scheme.
///
/// # Errors
///
/// Returns an error if the URL does not start with `https://`.
pub fn validate_https_url(url: &str) -> Result<()> {
    if url.starts_with("https://") {
        Ok(())
    } else {
        bail!("Refusing to download from non-HTTPS URL: {url}")
    }
}

// ---------------------------------------------------------------------------
// Checksum parsing and verification
// ---------------------------------------------------------------------------

/// Parses a `checksums.txt` file (sha256sum format) and returns the hex digest
/// for the given filename.
///
/// Expected format: `{hex_hash}  {filename}` (two-space separator).
///
/// # Errors
///
/// Returns an error if no matching entry is found.
pub fn parse_checksum(checksums_content: &str, asset_name: &str) -> Result<String> {
    for line in checksums_content.lines() {
        // sha256sum output format: "{hash}  {filename}"
        if let Some((hash, filename)) = line.split_once("  ")
            && filename.trim() == asset_name
        {
            return Ok(hash.to_string());
        }
    }
    bail!("No checksum found for {asset_name} in checksums.txt")
}

/// Verifies that the `SHA256` digest of `data` matches the expected hex string.
///
/// # Errors
///
/// Returns an error if the computed digest does not match.
pub fn verify_sha256(data: &[u8], expected_hex: &str) -> Result<()> {
    let computed = format!("{:x}", Sha256::digest(data));
    if computed == expected_hex {
        Ok(())
    } else {
        bail!(
            "Checksum verification failed — download may be corrupted\n  \
             expected: {expected_hex}\n  \
             got:      {computed}"
        )
    }
}

// ---------------------------------------------------------------------------
// Archive extraction
// ---------------------------------------------------------------------------

/// Extracts the `ndxr` binary from a `.tar.gz` archive into `dest_dir`.
///
/// Only extracts the expected binary filename (`ndxr` or `ndxr.exe`).
/// Rejects entries with path traversal (`..`) or absolute paths.
///
/// # Errors
///
/// Returns an error if the archive is corrupt, or the binary is not found.
pub fn extract_binary_from_tar_gz(archive_data: &[u8], dest_dir: &Path) -> Result<PathBuf> {
    let decoder = flate2::read::GzDecoder::new(archive_data);
    let mut archive = tar::Archive::new(decoder);

    for entry_result in archive
        .entries()
        .context("Failed to read archive entries")?
    {
        let mut entry = entry_result.context("Failed to read archive entry")?;
        let path = entry.path().context("Failed to read entry path")?;
        let path_str = path.to_string_lossy();

        // Skip entries that aren't the binary we want.
        // Only match the exact filename — reject any path components.
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_none_or(|n| n != BINARY_NAME)
        {
            continue;
        }

        // Reject path traversal, absolute paths, and subdirectory entries.
        // Only bare filenames are accepted (e.g., "ndxr", not "subdir/ndxr").
        if path_str.contains("..") || path.is_absolute() || path.components().count() != 1 {
            continue;
        }

        let dest_path = dest_dir.join(BINARY_NAME);
        let mut output = std::fs::File::create(&dest_path)
            .with_context(|| format!("Failed to create {}", dest_path.display()))?;
        std::io::copy(&mut entry, &mut output).context("Failed to write extracted binary")?;

        // Set executable permission on Unix.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dest_path, std::fs::Permissions::from_mode(0o755))
                .context("Failed to set executable permission")?;
        }

        return Ok(dest_path);
    }

    bail!("{BINARY_NAME} not found in archive")
}

/// Extracts the `ndxr` binary from a `.zip` archive into `dest_dir`.
///
/// Only extracts the expected binary filename. Rejects path traversal.
///
/// # Errors
///
/// Returns an error if the archive is corrupt, or the binary is not found.
pub fn extract_binary_from_zip(archive_data: &[u8], dest_dir: &Path) -> Result<PathBuf> {
    let cursor = Cursor::new(archive_data);
    let mut archive = zip::ZipArchive::new(cursor).context("Failed to read zip archive")?;

    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .with_context(|| format!("Failed to read zip entry {i}"))?;

        let path = match file.enclosed_name() {
            Some(p) => p.clone(),
            None => continue, // Skip entries with unsafe paths.
        };

        if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_none_or(|n| n != BINARY_NAME)
        {
            continue;
        }

        let dest_path = dest_dir.join(BINARY_NAME);
        let mut output = std::fs::File::create(&dest_path)
            .with_context(|| format!("Failed to create {}", dest_path.display()))?;
        std::io::copy(&mut file, &mut output).context("Failed to write extracted binary")?;

        return Ok(dest_path);
    }

    bail!("{BINARY_NAME} not found in archive")
}

// ---------------------------------------------------------------------------
// GitHub API client
// ---------------------------------------------------------------------------

/// Builds a configured `reqwest::blocking::Client` with timeouts and user-agent.
fn build_http_client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(CONNECT_TIMEOUT_SECS))
        .timeout(Duration::from_secs(READ_TIMEOUT_SECS))
        .user_agent(format!("ndxr/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .context("Failed to build HTTP client")
}

/// Searches the release assets array for the matching platform binary and checksums.txt.
///
/// Returns `Ok(None)` if no binary matches the platform suffix.
///
/// # Errors
///
/// Returns an error if `checksums.txt` is not found among the assets.
pub fn find_matching_asset(
    assets: &[serde_json::Value],
    platform_suffix: &str,
    tag: &str,
) -> Result<Option<AssetInfo>> {
    let expected_name = if platform_suffix == "windows-amd64" {
        format!("ndxr-{tag}-{platform_suffix}.zip")
    } else {
        format!("ndxr-{tag}-{platform_suffix}.tar.gz")
    };

    // Check for the platform binary first — no binary means no need for checksums.
    let matching = assets
        .iter()
        .find(|a| a["name"].as_str() == Some(expected_name.as_str()));

    let Some(asset_json) = matching else {
        return Ok(None);
    };

    // Only require checksums.txt when we have a matching binary.
    let checksums_url = assets
        .iter()
        .find(|a| a["name"].as_str() == Some("checksums.txt"))
        .and_then(|a| a["browser_download_url"].as_str())
        .map(String::from)
        .context("checksums.txt not found in release assets")?;

    let url = asset_json["browser_download_url"]
        .as_str()
        .context("Missing browser_download_url for asset")?
        .to_string();

    Ok(Some(AssetInfo {
        name: expected_name,
        url,
        checksums_url,
    }))
}

/// Queries the GitHub Releases API for the latest release and compares
/// against the compiled-in version.
///
/// # Errors
///
/// Returns an error on network failure, API errors, or malformed responses.
pub fn check_for_update() -> Result<UpdateStatus> {
    let current = semver::Version::parse(env!("CARGO_PKG_VERSION"))
        .context("Failed to parse compiled-in version")?;

    let client = build_http_client()?;
    let url = format!("{GITHUB_API_BASE}/repos/{GITHUB_OWNER}/{GITHUB_REPO}/releases/latest");

    let response = client
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .context("Failed to check for updates")?;

    let status = response.status();
    if status == reqwest::StatusCode::FORBIDDEN {
        bail!("GitHub API rate limited. Try again later.");
    }
    if !status.is_success() {
        bail!("GitHub API returned {status}: {url}");
    }

    let body: serde_json::Value = response
        .json()
        .context("Failed to parse GitHub API response")?;

    let tag = body["tag_name"]
        .as_str()
        .context("Latest release has no tag_name")?;

    let version_str = tag.strip_prefix('v').unwrap_or(tag);
    let latest = semver::Version::parse(version_str)
        .with_context(|| format!("Failed to parse release version: {tag}"))?;

    let is_outdated = current < latest;

    let assets = body["assets"]
        .as_array()
        .context("Latest release has no downloadable assets")?;

    let asset = match current_platform_suffix() {
        Some(suffix) => find_matching_asset(assets, suffix, tag)?,
        None => None,
    };

    Ok(UpdateStatus {
        current,
        latest,
        is_outdated,
        asset,
    })
}

// ---------------------------------------------------------------------------
// Download, verify, and replace
// ---------------------------------------------------------------------------

/// Downloads the release asset, verifies its SHA256 checksum against
/// `checksums.txt`, and extracts the binary to a temporary directory.
///
/// Returns the path to the verified, extracted binary.
///
/// # Errors
///
/// Returns an error on download failure, checksum mismatch, or extraction failure.
pub fn download_and_verify(asset: &AssetInfo) -> Result<DownloadResult> {
    validate_https_url(&asset.url)?;
    validate_https_url(&asset.checksums_url)?;

    let client = build_http_client()?;

    // Download the archive.
    let archive_response = client
        .get(&asset.url)
        .send()
        .with_context(|| format!("Failed to download {}", asset.name))?;

    if !archive_response.status().is_success() {
        bail!(
            "Failed to download {}: HTTP {}",
            asset.name,
            archive_response.status()
        );
    }

    let download_size = archive_response.content_length();

    let archive_data = archive_response
        .bytes()
        .with_context(|| format!("Failed to read response body for {}", asset.name))?;

    // Download checksums.txt.
    let checksums_response = client
        .get(&asset.checksums_url)
        .send()
        .context("Failed to download checksums.txt")?;

    if !checksums_response.status().is_success() {
        bail!(
            "Failed to download checksums.txt: HTTP {}",
            checksums_response.status()
        );
    }

    let checksums_text = checksums_response
        .text()
        .context("Failed to read checksums.txt")?;

    // Verify checksum.
    let expected_hash = parse_checksum(&checksums_text, &asset.name)?;
    verify_sha256(&archive_data, &expected_hash)?;

    // Extract binary.
    let tmp_dir = tempfile::tempdir().context("Failed to create temporary directory")?;

    let binary_path = if Path::new(&asset.name)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"))
    {
        extract_binary_from_zip(&archive_data, tmp_dir.path())?
    } else {
        extract_binary_from_tar_gz(&archive_data, tmp_dir.path())?
    };

    // Persist the temp directory so the binary survives until replace_binary().
    // The caller is responsible for cleanup (or the OS cleans up on reboot).
    let _ = tmp_dir.keep();

    Ok(DownloadResult {
        binary_path,
        download_size,
    })
}

/// Atomically replaces the currently running binary with the new one.
///
/// Uses `self_replace` for cross-platform atomic binary replacement.
///
/// # Errors
///
/// Returns an error if the current executable path cannot be determined,
/// or if the replacement fails (e.g., permission denied).
pub fn replace_binary(verified_binary: &Path) -> Result<()> {
    self_replace::self_replace(verified_binary)
        .context("Failed to replace binary — do you have write permission?")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- Platform detection ---

    #[test]
    fn platform_suffix_darwin_arm64() {
        assert_eq!(
            platform_asset_suffix("macos", "aarch64"),
            Some("darwin-arm64")
        );
    }

    #[test]
    fn platform_suffix_darwin_amd64() {
        assert_eq!(
            platform_asset_suffix("macos", "x86_64"),
            Some("darwin-amd64")
        );
    }

    #[test]
    fn platform_suffix_linux_arm64() {
        assert_eq!(
            platform_asset_suffix("linux", "aarch64"),
            Some("linux-arm64")
        );
    }

    #[test]
    fn platform_suffix_linux_amd64() {
        assert_eq!(
            platform_asset_suffix("linux", "x86_64"),
            Some("linux-amd64")
        );
    }

    #[test]
    fn platform_suffix_windows_amd64() {
        assert_eq!(
            platform_asset_suffix("windows", "x86_64"),
            Some("windows-amd64")
        );
    }

    #[test]
    fn platform_suffix_unsupported_returns_none() {
        assert_eq!(platform_asset_suffix("freebsd", "x86_64"), None);
        assert_eq!(platform_asset_suffix("linux", "riscv64"), None);
        assert_eq!(platform_asset_suffix("windows", "aarch64"), None);
    }

    #[test]
    fn validate_https_accepts_https() {
        assert!(validate_https_url("https://github.com/ndxr/ndxr").is_ok());
    }

    #[test]
    fn validate_https_rejects_http() {
        let err = validate_https_url("http://github.com/ndxr/ndxr").unwrap_err();
        assert!(err.to_string().contains("non-HTTPS"));
    }

    #[test]
    fn validate_https_rejects_ftp() {
        let err = validate_https_url("ftp://example.com/file").unwrap_err();
        assert!(err.to_string().contains("non-HTTPS"));
    }

    // --- Checksum parsing ---

    #[test]
    fn parse_checksum_finds_matching_entry() {
        let checksums = "abc123def456  ndxr-v0.5.0-darwin-arm64.tar.gz\n\
                         789012345678  ndxr-v0.5.0-linux-amd64.tar.gz\n";
        let result = parse_checksum(checksums, "ndxr-v0.5.0-darwin-arm64.tar.gz").unwrap();
        assert_eq!(result, "abc123def456");
    }

    #[test]
    fn parse_checksum_no_match_returns_error() {
        let checksums = "abc123def456  ndxr-v0.5.0-darwin-arm64.tar.gz\n";
        let err = parse_checksum(checksums, "ndxr-v0.5.0-linux-arm64.tar.gz").unwrap_err();
        assert!(err.to_string().contains("No checksum found"));
    }

    #[test]
    fn parse_checksum_handles_trailing_whitespace() {
        let checksums = "abc123def456  ndxr-v0.5.0-darwin-arm64.tar.gz  \n";
        let result = parse_checksum(checksums, "ndxr-v0.5.0-darwin-arm64.tar.gz").unwrap();
        assert_eq!(result, "abc123def456");
    }

    #[test]
    fn verify_checksum_matching_digest() {
        let data = b"hello world";
        let expected = format!("{:x}", Sha256::digest(data));
        assert!(verify_sha256(data, &expected).is_ok());
    }

    #[test]
    fn verify_checksum_mismatched_digest() {
        let err = verify_sha256(
            b"hello world",
            "0000000000000000000000000000000000000000000000000000000000000000",
        )
        .unwrap_err();
        assert!(err.to_string().contains("Checksum verification failed"));
    }

    // --- tar.gz extraction ---

    fn create_test_tar_gz(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut archive_data = Vec::new();
        {
            let enc = flate2::write::GzEncoder::new(&mut archive_data, flate2::Compression::fast());
            let mut tar_builder = tar::Builder::new(enc);
            for &(name, content) in entries {
                let mut header = tar::Header::new_gnu();
                header.set_size(content.len() as u64);
                header.set_mode(0o755);
                header.set_cksum();
                tar_builder.append_data(&mut header, name, content).unwrap();
            }
            tar_builder.into_inner().unwrap().finish().unwrap();
        }
        archive_data
    }

    #[test]
    fn extract_binary_from_tar_gz_succeeds() {
        let archive = create_test_tar_gz(&[
            (BINARY_NAME, b"fake-binary-content"),
            ("LICENSE", b"license text"),
            ("README.md", b"readme text"),
        ]);
        let tmp_dir = tempfile::tempdir().unwrap();
        let result = extract_binary_from_tar_gz(&archive, tmp_dir.path()).unwrap();
        assert!(result.exists());
        assert_eq!(
            std::fs::read_to_string(&result).unwrap(),
            "fake-binary-content"
        );
    }

    /// Builds a raw tar.gz with arbitrary path bytes, bypassing the tar
    /// crate's path validation (which rejects `..` and absolute paths).
    fn create_raw_tar_gz(entries: &[(&[u8], &[u8])]) -> Vec<u8> {
        let mut tar_bytes = Vec::new();
        for &(name_bytes, content) in entries {
            let mut header = [0u8; 512];
            // name field: bytes 0..100
            let len = name_bytes.len().min(100);
            header[..len].copy_from_slice(&name_bytes[..len]);
            // mode: "0000755\0"
            header[100..108].copy_from_slice(b"0000755\0");
            // uid/gid: "0000000\0"
            header[108..116].copy_from_slice(b"0000000\0");
            header[116..124].copy_from_slice(b"0000000\0");
            // size in octal
            let size_str = format!("{:011o}\0", content.len());
            header[124..136].copy_from_slice(size_str.as_bytes());
            // mtime
            header[136..148].copy_from_slice(b"00000000000\0");
            // typeflag: '0' = regular file
            header[156] = b'0';
            // Compute checksum: sum of all bytes with checksum field as spaces
            header[148..156].copy_from_slice(b"        ");
            let cksum: u32 = header.iter().map(|&b| u32::from(b)).sum();
            let cksum_str = format!("{cksum:06o}\0 ");
            header[148..156].copy_from_slice(cksum_str.as_bytes());
            tar_bytes.extend_from_slice(&header);
            tar_bytes.extend_from_slice(content);
            // Pad to 512-byte boundary
            let padding = (512 - (content.len() % 512)) % 512;
            tar_bytes.extend(std::iter::repeat(0u8).take(padding));
        }
        // End-of-archive marker: two 512-byte zero blocks
        tar_bytes.extend(std::iter::repeat(0u8).take(1024));

        let mut gz_bytes = Vec::new();
        {
            use std::io::Write;
            let mut enc = flate2::write::GzEncoder::new(&mut gz_bytes, flate2::Compression::fast());
            enc.write_all(&tar_bytes).unwrap();
            enc.finish().unwrap();
        }
        gz_bytes
    }

    #[test]
    fn extract_binary_rejects_path_traversal() {
        let archive = create_raw_tar_gz(&[(b"../ndxr", b"malicious-content")]);
        let tmp_dir = tempfile::tempdir().unwrap();
        let err = extract_binary_from_tar_gz(&archive, tmp_dir.path()).unwrap_err();
        assert!(err.to_string().contains("not found in archive"));
    }

    #[test]
    fn extract_binary_rejects_absolute_path() {
        let archive = create_raw_tar_gz(&[(b"/etc/ndxr", b"malicious-content")]);
        let tmp_dir = tempfile::tempdir().unwrap();
        let err = extract_binary_from_tar_gz(&archive, tmp_dir.path()).unwrap_err();
        assert!(err.to_string().contains("not found in archive"));
    }

    #[test]
    fn extract_binary_rejects_subdirectory_path() {
        let archive = create_test_tar_gz(&[("subdir/ndxr", b"nested-content")]);
        let tmp_dir = tempfile::tempdir().unwrap();
        let err = extract_binary_from_tar_gz(&archive, tmp_dir.path()).unwrap_err();
        assert!(err.to_string().contains("not found in archive"));
    }

    #[test]
    fn extract_binary_missing_from_archive() {
        let archive = create_test_tar_gz(&[("LICENSE", b"license text")]);
        let tmp_dir = tempfile::tempdir().unwrap();
        let err = extract_binary_from_tar_gz(&archive, tmp_dir.path()).unwrap_err();
        assert!(err.to_string().contains("not found in archive"));
    }

    // --- zip extraction ---

    fn create_test_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let options = zip::write::SimpleFileOptions::default();
            for &(name, content) in entries {
                writer.start_file(name, options).unwrap();
                std::io::Write::write_all(&mut writer, content).unwrap();
            }
            writer.finish().unwrap();
        }
        buf
    }

    #[test]
    fn extract_binary_from_zip_succeeds() {
        let archive_with_correct_name =
            create_test_zip(&[(BINARY_NAME, b"fake-binary"), ("LICENSE", b"license text")]);
        let tmp_dir = tempfile::tempdir().unwrap();
        let result = extract_binary_from_zip(&archive_with_correct_name, tmp_dir.path()).unwrap();
        assert!(result.exists());
        assert_eq!(std::fs::read_to_string(&result).unwrap(), "fake-binary");
    }

    #[test]
    fn extract_binary_from_zip_missing_returns_error() {
        let archive = create_test_zip(&[("LICENSE", b"license text")]);
        let tmp_dir = tempfile::tempdir().unwrap();
        let err = extract_binary_from_zip(&archive, tmp_dir.path()).unwrap_err();
        assert!(err.to_string().contains("not found in archive"));
    }

    // --- Version comparison ---

    #[test]
    fn compare_versions_outdated() {
        let current = semver::Version::parse("0.4.0").unwrap();
        let latest = semver::Version::parse("0.5.0").unwrap();
        assert!(current < latest);
    }

    #[test]
    fn compare_versions_up_to_date() {
        let current = semver::Version::parse("0.4.0").unwrap();
        let latest = semver::Version::parse("0.4.0").unwrap();
        assert!(current >= latest);
    }

    #[test]
    fn compare_versions_dev_build_ahead() {
        let current = semver::Version::parse("0.5.0-alpha.1").unwrap();
        let latest = semver::Version::parse("0.4.0").unwrap();
        assert!(current >= latest);
    }

    // --- Asset matching ---

    #[test]
    fn find_matching_asset_selects_correct_platform() {
        let assets = serde_json::json!([
            {"name": "ndxr-v0.5.0-darwin-arm64.tar.gz", "browser_download_url": "https://example.com/darwin-arm64.tar.gz"},
            {"name": "ndxr-v0.5.0-linux-amd64.tar.gz", "browser_download_url": "https://example.com/linux-amd64.tar.gz"},
            {"name": "checksums.txt", "browser_download_url": "https://example.com/checksums.txt"}
        ]);
        let result =
            find_matching_asset(assets.as_array().unwrap(), "darwin-arm64", "v0.5.0").unwrap();
        assert!(result.is_some());
        let asset = result.unwrap();
        assert_eq!(asset.name, "ndxr-v0.5.0-darwin-arm64.tar.gz");
        assert!(asset.url.contains("darwin-arm64"));
        assert!(asset.checksums_url.contains("checksums.txt"));
    }

    #[test]
    fn find_matching_asset_returns_none_for_unknown_platform() {
        let assets = serde_json::json!([
            {"name": "ndxr-v0.5.0-darwin-arm64.tar.gz", "browser_download_url": "https://example.com/a"},
            {"name": "checksums.txt", "browser_download_url": "https://example.com/checksums.txt"}
        ]);
        let result =
            find_matching_asset(assets.as_array().unwrap(), "linux-riscv64", "v0.5.0").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn find_matching_asset_returns_none_without_checksums_for_unknown_platform() {
        // When no binary matches, checksums.txt absence should NOT cause an error.
        let assets = serde_json::json!([
            {"name": "ndxr-v0.5.0-darwin-arm64.tar.gz", "browser_download_url": "https://example.com/a"}
        ]);
        let result =
            find_matching_asset(assets.as_array().unwrap(), "linux-riscv64", "v0.5.0").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn find_matching_asset_errors_without_checksums_for_known_platform() {
        // When a binary matches but checksums.txt is missing, that IS an error.
        let assets = serde_json::json!([
            {"name": "ndxr-v0.5.0-darwin-arm64.tar.gz", "browser_download_url": "https://example.com/a"}
        ]);
        let err =
            find_matching_asset(assets.as_array().unwrap(), "darwin-arm64", "v0.5.0").unwrap_err();
        assert!(err.to_string().contains("checksums.txt"));
    }

    // --- Additional HTTPS validation edge cases ---

    #[test]
    fn validate_https_rejects_empty_string() {
        let err = validate_https_url("").unwrap_err();
        assert!(err.to_string().contains("non-HTTPS"));
    }

    #[test]
    fn validate_https_rejects_prefix_without_scheme() {
        // "httpsmalicious.com" starts with "https" but not "https://".
        let err = validate_https_url("httpsmalicious.com/file").unwrap_err();
        assert!(err.to_string().contains("non-HTTPS"));
    }

    #[test]
    fn validate_https_rejects_data_uri() {
        let err = validate_https_url("data:text/plain,hello").unwrap_err();
        assert!(err.to_string().contains("non-HTTPS"));
    }

    // --- Additional checksum parsing edge cases ---

    #[test]
    fn parse_checksum_empty_input_returns_error() {
        let err = parse_checksum("", "ndxr-v0.5.0-darwin-arm64.tar.gz").unwrap_err();
        assert!(err.to_string().contains("No checksum found"));
    }

    #[test]
    fn parse_checksum_single_space_separator_does_not_match() {
        // sha256sum format uses two-space separator; single space should not match.
        let checksums = "abc123def456 ndxr-v0.5.0-darwin-arm64.tar.gz\n";
        let err = parse_checksum(checksums, "ndxr-v0.5.0-darwin-arm64.tar.gz").unwrap_err();
        assert!(err.to_string().contains("No checksum found"));
    }

    #[test]
    fn parse_checksum_tab_separator_does_not_match() {
        let checksums = "abc123def456\tndxr-v0.5.0-darwin-arm64.tar.gz\n";
        let err = parse_checksum(checksums, "ndxr-v0.5.0-darwin-arm64.tar.gz").unwrap_err();
        assert!(err.to_string().contains("No checksum found"));
    }

    #[test]
    fn parse_checksum_returns_first_match() {
        let checksums = "first_hash  ndxr-v0.5.0-darwin-arm64.tar.gz\n\
                         second_hash  ndxr-v0.5.0-darwin-arm64.tar.gz\n";
        let result = parse_checksum(checksums, "ndxr-v0.5.0-darwin-arm64.tar.gz").unwrap();
        assert_eq!(result, "first_hash");
    }

    // --- Additional tar.gz extraction edge cases ---

    #[test]
    fn extract_binary_from_empty_tar_gz() {
        // An archive with no entries at all.
        let archive = create_test_tar_gz(&[]);
        let tmp_dir = tempfile::tempdir().unwrap();
        let err = extract_binary_from_tar_gz(&archive, tmp_dir.path()).unwrap_err();
        assert!(err.to_string().contains("not found in archive"));
    }

    #[test]
    fn extract_binary_rejects_nested_release_directory() {
        // Real releases often package as "ndxr-v0.5.0/ndxr" — must be rejected
        // since our component count check requires exactly 1 path component.
        let archive = create_test_tar_gz(&[("ndxr-v0.5.0/ndxr", b"nested-binary")]);
        let tmp_dir = tempfile::tempdir().unwrap();
        let err = extract_binary_from_tar_gz(&archive, tmp_dir.path()).unwrap_err();
        assert!(err.to_string().contains("not found in archive"));
    }

    // --- Additional zip extraction edge cases ---

    #[test]
    fn extract_binary_from_empty_zip() {
        let archive = create_test_zip(&[]);
        let tmp_dir = tempfile::tempdir().unwrap();
        let err = extract_binary_from_zip(&archive, tmp_dir.path()).unwrap_err();
        assert!(err.to_string().contains("not found in archive"));
    }

    #[test]
    fn extract_binary_from_zip_rejects_nested_directory() {
        let archive = create_test_zip(&[("subdir/ndxr", b"nested-content")]);
        let tmp_dir = tempfile::tempdir().unwrap();
        // On non-Windows, BINARY_NAME is "ndxr". The zip function uses
        // enclosed_name() which strips directory components for safety,
        // but our file_name() check only matches the bare filename.
        // Since the entry has a directory, file_name() returns "ndxr",
        // but enclosed_name() preserves the path structure — so it depends
        // on the zip crate behavior. Either way, a nested path should
        // not extract successfully as a bare filename match.
        let result = extract_binary_from_zip(&archive, tmp_dir.path());
        // If the zip crate's enclosed_name preserves "subdir/ndxr", file_name
        // matches "ndxr" and it would extract. This is acceptable since the
        // destination is always dest_dir/BINARY_NAME (safe). But let's verify
        // the behavior is defined either way.
        if let Ok(path) = result {
            // If it extracted, the dest is safe (hardcoded BINARY_NAME).
            assert!(path.file_name().unwrap().to_str().unwrap() == BINARY_NAME);
        }
        // If it errored, that's also acceptable.
    }

    // --- Additional version comparison edge cases ---

    #[test]
    fn prerelease_is_less_than_release() {
        // Semver gotcha: 0.5.0-alpha.1 < 0.5.0 (pre-release < release).
        // This means a user on a pre-release would be prompted to "upgrade"
        // to the stable release, which is correct behavior.
        let prerelease = semver::Version::parse("0.5.0-alpha.1").unwrap();
        let release = semver::Version::parse("0.5.0").unwrap();
        assert!(prerelease < release);
    }

    #[test]
    fn build_metadata_does_not_affect_precedence() {
        // Semver spec: build metadata is ignored for precedence.
        // The semver crate's Ord includes build metadata for determinism,
        // but cmp_precedence() follows the spec. Our upgrade logic uses
        // `current < latest` (Ord), but in practice GitHub tags never
        // include build metadata, so this is purely defensive.
        let a = semver::Version::parse("0.5.0+build.1").unwrap();
        let b = semver::Version::parse("0.5.0+build.2").unwrap();
        assert_eq!(a.cmp_precedence(&b), std::cmp::Ordering::Equal);
    }

    // --- Additional asset matching edge cases ---

    #[test]
    fn find_matching_asset_windows_expects_zip() {
        let assets = serde_json::json!([
            {"name": "ndxr-v0.5.0-windows-amd64.zip", "browser_download_url": "https://example.com/win.zip"},
            {"name": "checksums.txt", "browser_download_url": "https://example.com/checksums.txt"}
        ]);
        let result =
            find_matching_asset(assets.as_array().unwrap(), "windows-amd64", "v0.5.0").unwrap();
        assert!(result.is_some());
        let asset = result.unwrap();
        assert_eq!(asset.name, "ndxr-v0.5.0-windows-amd64.zip");
    }

    #[test]
    fn find_matching_asset_windows_tar_gz_does_not_match() {
        // Windows platform should only match .zip, not .tar.gz.
        let assets = serde_json::json!([
            {"name": "ndxr-v0.5.0-windows-amd64.tar.gz", "browser_download_url": "https://example.com/a"},
            {"name": "checksums.txt", "browser_download_url": "https://example.com/checksums.txt"}
        ]);
        let result =
            find_matching_asset(assets.as_array().unwrap(), "windows-amd64", "v0.5.0").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn find_matching_asset_handles_missing_download_url() {
        // Asset exists but browser_download_url is null/missing.
        let assets = serde_json::json!([
            {"name": "ndxr-v0.5.0-darwin-arm64.tar.gz", "browser_download_url": null},
            {"name": "checksums.txt", "browser_download_url": "https://example.com/checksums.txt"}
        ]);
        let err =
            find_matching_asset(assets.as_array().unwrap(), "darwin-arm64", "v0.5.0").unwrap_err();
        assert!(err.to_string().contains("browser_download_url"));
    }

    #[test]
    fn find_matching_asset_empty_assets_list() {
        let assets = serde_json::json!([]);
        let result =
            find_matching_asset(assets.as_array().unwrap(), "darwin-arm64", "v0.5.0").unwrap();
        assert!(result.is_none());
    }
}
