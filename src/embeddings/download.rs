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

/// Default model: `all-MiniLM-L6-v2` FP32 (384-dimensional embeddings, ~86 MiB).
///
/// URLs are pinned to a specific Hugging Face git commit rather than `main`,
/// so downloads are reproducible and immune to upstream file renames. To adopt
/// a new upstream revision, bump the commit hash here and update the checksums.
///
/// FP32 is used because `tract-onnx` (pure-Rust inference) is the supported
/// runtime and loads this export cleanly across every platform — no CPU-specific
/// variants needed.
pub const DEFAULT_MODEL: ModelInfo = ModelInfo {
    name: "all-MiniLM-L6-v2-fp32",
    onnx_url: "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/c9745ed1d9f207416be6d2e6f8de32d1f16199bf/onnx/model.onnx",
    onnx_sha256: "6fd5d72fe4589f189f8ebc006442dbb529bb7ce38f8082112682524616046452",
    onnx_filename: "model.onnx",
    tokenizer_url: "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/c9745ed1d9f207416be6d2e6f8de32d1f16199bf/tokenizer.json",
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
/// When `on_progress` is `Some`, each successfully downloaded file emits a
/// `"{filename}: done ({size})"` message.
///
/// # Errors
///
/// Returns an error if the download fails, the checksum does not match, or
/// the filesystem operations fail.
pub fn download_model(
    dir: &Path,
    info: &ModelInfo,
    on_progress: Option<&dyn Fn(&str)>,
) -> Result<()> {
    fs::create_dir_all(dir)
        .with_context(|| format!("failed to create model directory: {}", dir.display()))?;

    download_verified(
        info.onnx_url,
        info.onnx_sha256,
        &dir.join(info.onnx_filename),
        on_progress,
    )
    .with_context(|| format!("failed to download ONNX model: {}", info.onnx_url))?;

    download_verified(
        info.tokenizer_url,
        info.tokenizer_sha256,
        &dir.join(info.tokenizer_filename),
        on_progress,
    )
    .with_context(|| format!("failed to download tokenizer: {}", info.tokenizer_url))?;

    Ok(())
}

/// Downloads a single file, verifies its SHA-256, and atomically moves it into
/// place. Emits a progress message after success when `on_progress` is `Some`.
fn download_verified(
    url: &str,
    expected_sha256: &str,
    dest: &Path,
    on_progress: Option<&dyn Fn(&str)>,
) -> Result<()> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(CONNECT_TIMEOUT_SECS))
        .timeout(Duration::from_secs(READ_TIMEOUT_SECS))
        .build()
        .context("failed to build HTTP client")?;

    let response = client
        .get(url)
        .send()
        .with_context(|| {
            format!("Download failed: {url}. Check your internet connection and try again.")
        })?
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

    if let Some(cb) = on_progress {
        let filename = dest.file_name().and_then(|n| n.to_str()).unwrap_or("file");
        // `usize -> u64` is widening on every Rust target (usize ≤ u64),
        // so no clippy allow is required here.
        let size_u64 = data.len() as u64;
        cb(&format!(
            "  {filename}: done ({})",
            format_bytes_human(size_u64)
        ));
    }

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

/// Formats a byte count as a human-readable string (B / KiB / MiB / GiB).
#[must_use]
pub(crate) fn format_bytes_human(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = 1024 * 1024 * 1024;

    if bytes >= GIB {
        #[allow(clippy::cast_precision_loss)] // byte counts fit in f64 with negligible loss
        let val = bytes as f64 / GIB as f64;
        format!("{val:.1} GiB")
    } else if bytes >= MIB {
        #[allow(clippy::cast_precision_loss)] // byte counts fit in f64 with negligible loss
        let val = bytes as f64 / MIB as f64;
        format!("{val:.1} MiB")
    } else if bytes >= KIB {
        #[allow(clippy::cast_precision_loss)] // byte counts fit in f64 with negligible loss
        let val = bytes as f64 / KIB as f64;
        format!("{val:.1} KiB")
    } else {
        format!("{bytes} B")
    }
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
    fn format_bytes_helper_renders_common_sizes() {
        assert_eq!(format_bytes_human(0), "0 B");
        assert_eq!(format_bytes_human(512), "512 B");
        assert_eq!(format_bytes_human(1024), "1.0 KiB");
        assert_eq!(format_bytes_human(1536), "1.5 KiB");
        assert_eq!(format_bytes_human(1_048_576), "1.0 MiB");
        assert_eq!(format_bytes_human(22 * 1_048_576 + 420 * 1024), "22.4 MiB");
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
