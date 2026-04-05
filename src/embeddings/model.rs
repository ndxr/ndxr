//! ONNX model loading and text embedding inference.
//!
//! Wraps a tract ONNX runtime and a `HuggingFace` tokenizer to produce
//! 384-dimensional embedding vectors from symbol text. When model files
//! are not present on disk, [`ModelHandle::load`] returns `Ok(None)` and
//! semantic scoring is silently disabled.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use tract_onnx::prelude::*;

/// Embedding vector dimension (`all-MiniLM-L6-v2` produces 384-d vectors).
pub const EMBEDDING_DIMENSION: usize = 384;

/// Maximum texts per batch call.
pub(crate) const EMBEDDING_BATCH_SIZE: usize = 32;

/// Input text is truncated to this many characters before tokenization.
const MAX_EMBEDDING_INPUT_CHARS: usize = 512;

/// Docstrings longer than this are truncated in embedding text.
const DOCSTRING_TRUNCATION: usize = 200;

/// Fixed token sequence length fed to the model.
const MAX_TOKEN_LENGTH: usize = 128;

/// Handle to a loaded embedding model for text-to-vector inference.
///
/// Wraps a tract ONNX model and `HuggingFace` tokenizer. When model files
/// are not present, [`ModelHandle::load`] returns `Ok(None)` and semantic
/// scoring is disabled.
pub struct ModelHandle {
    /// Optimized tract inference plan.
    model: Arc<TypedRunnableModel<TypedModel>>,
    /// `HuggingFace` tokenizer loaded from `tokenizer.json`.
    tokenizer: Arc<tokenizers::Tokenizer>,
}

// Compile-time assertion that `ModelHandle` is `Send + Sync`.
#[allow(dead_code)] // compile-time check only, never called at runtime
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    const fn check() {
        assert_send_sync::<ModelHandle>();
    }
};

impl ModelHandle {
    /// Loads the embedding model from a directory.
    ///
    /// Expects `model.onnx` and `tokenizer.json` inside `models_dir` — the
    /// filenames declared in [`crate::embeddings::download::DEFAULT_MODEL`].
    /// Returns `Ok(None)` if either file is missing.
    ///
    /// # Errors
    ///
    /// Returns an error if model files exist but cannot be loaded or parsed.
    pub fn load(models_dir: &Path) -> Result<Option<Self>> {
        let info = &crate::embeddings::download::DEFAULT_MODEL;
        let onnx_path = models_dir.join(info.onnx_filename);
        let tokenizer_path = models_dir.join(info.tokenizer_filename);

        if !onnx_path.exists() || !tokenizer_path.exists() {
            return Ok(None);
        }

        #[allow(clippy::cast_possible_wrap)] // MAX_TOKEN_LENGTH=128, well within i64 range
        let seq_len = MAX_TOKEN_LENGTH as i64;

        // BERT-style inputs: input_ids, attention_mask, token_type_ids —
        // all shape [1, MAX_TOKEN_LENGTH], dtype i64.
        let plan = tract_onnx::onnx()
            .model_for_path(&onnx_path)
            .with_context(|| format!("failed to load ONNX model: {}", onnx_path.display()))?
            .with_input_fact(0, i64::datum_type().fact([1, seq_len]).into())
            .context("failed to set input_ids fact")?
            .with_input_fact(1, i64::datum_type().fact([1, seq_len]).into())
            .context("failed to set attention_mask fact")?
            .with_input_fact(2, i64::datum_type().fact([1, seq_len]).into())
            .context("failed to set token_type_ids fact")?
            .into_optimized()
            .context("failed to optimize ONNX model")?
            .into_runnable()
            .context("failed to build runnable model")?;

        let tokenizer = tokenizers::Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| anyhow::anyhow!("failed to load tokenizer: {e}"))?;

        Ok(Some(Self {
            model: Arc::new(plan),
            tokenizer: Arc::new(tokenizer),
        }))
    }

    /// Embeds a single text string into a 384-dimensional vector.
    ///
    /// The text is truncated, tokenized, padded to a fixed length, run
    /// through the ONNX model, mean-pooled, and L2-normalized.
    ///
    /// # Errors
    ///
    /// Returns an error if tokenization or inference fails.
    pub fn embed_text(&self, text: &str) -> Result<Vec<f32>> {
        let truncated: String = text.chars().take(MAX_EMBEDDING_INPUT_CHARS).collect();

        let encoding = self
            .tokenizer
            .encode(truncated.as_str(), true)
            .map_err(|e| anyhow::anyhow!("tokenization failed: {e}"))?;

        let raw_ids = encoding.get_ids();

        // Pad or truncate to MAX_TOKEN_LENGTH.
        let mut input_ids = vec![0i64; MAX_TOKEN_LENGTH];
        let mut attention_mask = vec![0i64; MAX_TOKEN_LENGTH];
        let token_count = raw_ids.len().min(MAX_TOKEN_LENGTH);

        for i in 0..token_count {
            #[allow(clippy::cast_lossless)] // u32 -> i64 is always lossless
            {
                input_ids[i] = raw_ids[i] as i64;
            }
            attention_mask[i] = 1;
        }

        let ids_tensor =
            tract_ndarray::Array2::<i64>::from_shape_vec((1, MAX_TOKEN_LENGTH), input_ids)
                .context("failed to create input_ids tensor")?
                .into_arc_tensor();

        let mask_tensor = tract_ndarray::Array2::<i64>::from_shape_vec(
            (1, MAX_TOKEN_LENGTH),
            attention_mask.clone(),
        )
        .context("failed to create attention_mask tensor")?
        .into_arc_tensor();

        // token_type_ids: all zeros for single-sentence inputs (segment 0).
        let token_type_tensor = tract_ndarray::Array2::<i64>::from_shape_vec(
            (1, MAX_TOKEN_LENGTH),
            vec![0i64; MAX_TOKEN_LENGTH],
        )
        .context("failed to create token_type_ids tensor")?
        .into_arc_tensor();

        let outputs = self
            .model
            .run(tvec![
                TValue::from_const(ids_tensor),
                TValue::from_const(mask_tensor),
                TValue::from_const(token_type_tensor),
            ])
            .context("ONNX inference failed")?;

        // Output shape: (1, seq_len, EMBEDDING_DIMENSION)
        let output_view = outputs[0]
            .to_array_view::<f32>()
            .context("failed to read model output")?;

        // Mean-pool: sum embeddings weighted by attention_mask, divide by count.
        let mut pooled = vec![0f32; EMBEDDING_DIMENSION];

        #[allow(clippy::cast_precision_loss)] // mask count fits in f32
        let mask_sum: f32 = attention_mask.iter().sum::<i64>() as f32;

        if mask_sum > 0.0 {
            for (i, &mask_val) in attention_mask.iter().enumerate().take(MAX_TOKEN_LENGTH) {
                if mask_val == 1 {
                    for (j, p) in pooled.iter_mut().enumerate() {
                        *p += output_view[[0, i, j]];
                    }
                }
            }
            for p in &mut pooled {
                *p /= mask_sum;
            }
        }

        // L2-normalize.
        l2_normalize(&mut pooled);

        Ok(pooled)
    }

    /// Embeds a batch of texts into 384-dimensional vectors.
    ///
    /// Processes all texts sequentially in chunks of `EMBEDDING_BATCH_SIZE`
    /// (tract does not easily batch variable-length inputs).
    ///
    /// # Errors
    ///
    /// Returns an error if any individual embedding fails.
    pub fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let mut results = Vec::with_capacity(texts.len());
        for chunk in texts.chunks(EMBEDDING_BATCH_SIZE) {
            for text in chunk {
                results.push(self.embed_text(text)?);
            }
        }
        Ok(results)
    }
}

/// L2-normalizes a vector in place.
fn l2_normalize(vec: &mut [f32]) {
    let norm: f32 = vec.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for v in vec.iter_mut() {
            *v /= norm;
        }
    }
}

/// Builds the text string used to compute a symbol's embedding.
///
/// Concatenates the symbol name, optional signature, and an optionally
/// truncated docstring into a single space-separated string suitable for
/// embedding inference.
#[must_use]
pub fn symbol_to_embedding_text(
    name: &str,
    signature: Option<&str>,
    docstring: Option<&str>,
) -> String {
    let mut parts = Vec::with_capacity(3);
    parts.push(name.to_owned());
    if let Some(sig) = signature {
        parts.push(sig.to_owned());
    }
    if let Some(doc) = docstring {
        let truncated: String = doc.chars().take(DOCSTRING_TRUNCATION).collect();
        parts.push(truncated);
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_to_embedding_text_full() {
        let text = symbol_to_embedding_text(
            "validateToken",
            Some("fn validate_token(token: &str) -> bool"),
            Some("Validates an authentication token."),
        );
        assert!(text.contains("validateToken"));
        assert!(text.contains("fn validate_token"));
        assert!(text.contains("Validates an authentication"));
    }

    #[test]
    fn symbol_to_embedding_text_minimal() {
        let text = symbol_to_embedding_text("main", None, None);
        assert_eq!(text, "main");
    }

    #[test]
    fn symbol_to_embedding_text_truncates_long_docstring() {
        let long_doc = "a".repeat(500);
        let text = symbol_to_embedding_text("sym", None, Some(&long_doc));
        // "sym" (3) + " " (1) + 200 'a's = 204 chars max
        assert!(
            text.len() <= 204,
            "text should be truncated, got len {}",
            text.len()
        );
    }

    #[test]
    fn load_returns_none_when_missing() {
        let tmp = tempfile::TempDir::new().expect("tempdir creation failed in test");
        let result = ModelHandle::load(tmp.path()).expect("load should not error");
        assert!(result.is_none());
    }
}
