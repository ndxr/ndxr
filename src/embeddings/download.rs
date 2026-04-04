//! Model download and verification.
//!
//! Downloads and verifies embedding model files (ONNX model + tokenizer) from
//! `HuggingFace`. Uses `reqwest::blocking` for HTTP, `sha2` for integrity
//! verification, and atomic temp-file-then-rename for crash safety.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

/// HTTP connect timeout in seconds.
const CONNECT_TIMEOUT_SECS: u64 = 10;

/// HTTP read timeout in seconds.
const READ_TIMEOUT_SECS: u64 = 600;

/// Metadata for a downloadable embedding model.
#[derive(Debug, Clone, Copy)]
pub struct ModelInfo {
    /// Human-readable model name.
    pub name: &'static str,
    /// Download URL for the ONNX model file.
    pub onnx_url: &'static str,
    /// Expected SHA-256 hex digest of the ONNX model file.
    pub onnx_sha256: &'static str,
    /// Filename to store the ONNX model as.
    pub onnx_filename: &'static str,
    /// Download URL for the tokenizer JSON file.
    pub tokenizer_url: &'static str,
    /// Expected SHA-256 hex digest of the tokenizer JSON file.
    pub tokenizer_sha256: &'static str,
    /// Filename to store the tokenizer as.
    pub tokenizer_filename: &'static str,
}

/// Default model: `all-MiniLM-L6-v2` INT8 quantized (384 dimensions).
pub const DEFAULT_MODEL: ModelInfo = ModelInfo {
    name: "all-MiniLM-L6-v2-int8",
    onnx_url: "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/onnx/model_quantized.onnx",
    onnx_sha256: "f36668ddf22403a332f978057d527cf285b01468bc3431b04094a7bafa6aba59",
    onnx_filename: "model_quantized.onnx",
    tokenizer_url: "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/tokenizer.json",
    tokenizer_sha256: "be50c3628f2bf5bb5e3a7f17b1f74611b2561a3a27eeab05e5aa30f411572037",
    tokenizer_filename: "tokenizer.json",
};

/// Checks whether the model files exist on disk and match expected checksums.
///
/// Returns `Ok(true)` if both files exist and their SHA-256 digests match.
/// Returns `Ok(false)` if either file is missing or has a mismatched checksum.
///
/// # Errors
///
/// Returns an error if a file exists but cannot be read.
pub fn verify_model(dir: &Path, info: &ModelInfo) -> Result<bool> {
    let onnx_path = dir.join(info.onnx_filename);
    let tokenizer_path = dir.join(info.tokenizer_filename);

    if !onnx_path.exists() || !tokenizer_path.exists() {
        return Ok(false);
    }

    let onnx_hex = sha256_file(&onnx_path)
        .with_context(|| format!("failed to hash {}", onnx_path.display()))?;
    if onnx_hex != info.onnx_sha256 {
        return Ok(false);
    }

    let tokenizer_hex = sha256_file(&tokenizer_path)
        .with_context(|| format!("failed to hash {}", tokenizer_path.display()))?;
    if tokenizer_hex != info.tokenizer_sha256 {
        return Ok(false);
    }

    Ok(true)
}

/// Downloads both model files into `dir`, verifying each checksum.
///
/// Creates `dir` if it does not exist. Each file is downloaded to a temporary
/// path and renamed into place only after SHA-256 verification succeeds.
///
/// # Errors
///
/// Returns an error if the download fails, the checksum does not match, or
/// the filesystem operations fail.
pub fn download_model(dir: &Path, info: &ModelInfo) -> Result<()> {
    fs::create_dir_all(dir)
        .with_context(|| format!("failed to create model directory: {}", dir.display()))?;

    download_verified(
        info.onnx_url,
        info.onnx_sha256,
        &dir.join(info.onnx_filename),
    )
    .with_context(|| format!("failed to download ONNX model: {}", info.onnx_url))?;

    download_verified(
        info.tokenizer_url,
        info.tokenizer_sha256,
        &dir.join(info.tokenizer_filename),
    )
    .with_context(|| format!("failed to download tokenizer: {}", info.tokenizer_url))?;

    Ok(())
}

/// Downloads a single file, verifies its SHA-256, and atomically moves it into
/// place.
fn download_verified(url: &str, expected_sha256: &str, dest: &Path) -> Result<()> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(CONNECT_TIMEOUT_SECS))
        .timeout(Duration::from_secs(READ_TIMEOUT_SECS))
        .build()
        .context("failed to build HTTP client")?;

    let response = client
        .get(url)
        .send()
        .with_context(|| format!("HTTP request failed: {url}"))?
        .error_for_status()
        .with_context(|| format!("HTTP error for {url}"))?;

    let data = response
        .bytes()
        .with_context(|| format!("failed to read response body from {url}"))?;

    let computed = hex_sha256(&data);
    if computed != expected_sha256 {
        bail!("SHA-256 mismatch for {url}\n  expected: {expected_sha256}\n  got:      {computed}");
    }

    // Write to a temp file in the same directory, then rename for atomicity.
    let parent = dest
        .parent()
        .context("destination path has no parent directory")?;
    let tmp_path = parent.join(format!(
        ".{}.tmp",
        dest.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("download")
    ));

    let mut file = fs::File::create(&tmp_path)
        .with_context(|| format!("failed to create temp file: {}", tmp_path.display()))?;
    file.write_all(&data)
        .with_context(|| format!("failed to write temp file: {}", tmp_path.display()))?;
    file.flush()
        .with_context(|| format!("failed to flush temp file: {}", tmp_path.display()))?;
    drop(file);

    fs::rename(&tmp_path, dest).with_context(|| {
        format!(
            "failed to rename {} -> {}",
            tmp_path.display(),
            dest.display()
        )
    })?;

    Ok(())
}

/// Computes the SHA-256 hex digest of a file on disk.
fn sha256_file(path: &Path) -> Result<String> {
    let data =
        fs::read(path).with_context(|| format!("failed to read file: {}", path.display()))?;
    Ok(hex_sha256(&data))
}

/// Computes the SHA-256 hex digest of a byte slice.
fn hex_sha256(data: &[u8]) -> String {
    format!("{:x}", Sha256::digest(data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_model_returns_false_when_missing() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let result =
            verify_model(dir.path(), &DEFAULT_MODEL).expect("verify_model should not error");
        assert!(
            !result,
            "verify_model should return false when files are missing"
        );
    }

    #[test]
    fn verify_model_returns_false_on_bad_checksum() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");

        // Write files with wrong content (checksums won't match).
        fs::write(
            dir.path().join(DEFAULT_MODEL.onnx_filename),
            b"bad onnx data",
        )
        .expect("failed to write onnx file");
        fs::write(
            dir.path().join(DEFAULT_MODEL.tokenizer_filename),
            b"bad tokenizer data",
        )
        .expect("failed to write tokenizer file");

        let result =
            verify_model(dir.path(), &DEFAULT_MODEL).expect("verify_model should not error");
        assert!(
            !result,
            "verify_model should return false when checksums do not match"
        );
    }
}
