//! Cosine similarity computation for embedding vectors.

/// Computes cosine similarity between two vectors.
///
/// Returns 0.0 if either vector has zero norm (avoids NaN).
/// Result range: \[-1.0, 1.0\] for normalized vectors.
#[must_use]
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len(), "vectors must have equal length");
    let mut dot = 0.0_f32;
    let mut norm_a = 0.0_f32;
    let mut norm_b = 0.0_f32;
    for (ai, bi) in a.iter().zip(b.iter()) {
        dot += ai * bi;
        norm_a += ai * ai;
        norm_b += bi * bi;
    }
    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom < f32::EPSILON {
        return 0.0;
    }
    dot / denom
}

/// Computes cosine similarity between a query vector and each candidate vector.
///
/// Returns a vector of similarities in the same order as `candidates`.
/// `None` entries produce 0.0.
#[must_use]
pub fn batch_cosine_similarity(query: &[f32], candidates: &[Option<&[f32]>]) -> Vec<f32> {
    candidates
        .iter()
        .map(|c| c.as_ref().map_or(0.0, |emb| cosine_similarity(query, emb)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_similarity_identical_vectors() {
        let v = vec![1.0, 2.0, 3.0];
        let sim = cosine_similarity(&v, &v);
        assert!(
            (sim - 1.0).abs() < 1e-6,
            "identical vectors should be 1.0, got {sim}"
        );
    }

    #[test]
    fn cosine_similarity_orthogonal_vectors() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!(
            sim.abs() < 1e-6,
            "orthogonal vectors should be 0.0, got {sim}"
        );
    }

    #[test]
    fn cosine_similarity_opposite_vectors() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![-1.0, 0.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!(
            (sim - (-1.0)).abs() < 1e-6,
            "opposite vectors should be -1.0, got {sim}"
        );
    }

    #[test]
    fn cosine_similarity_zero_vector() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![0.0, 0.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!(sim.abs() < 1e-6, "zero vector should give 0.0, got {sim}");
    }

    #[test]
    fn batch_cosine_similarity_matches_individual() {
        let query = vec![1.0, 0.0, 0.0];
        let c1 = vec![1.0, 0.0, 0.0];
        let c2 = vec![0.0, 1.0, 0.0];
        let candidates: Vec<Option<&[f32]>> = vec![Some(&c1), None, Some(&c2)];
        let results = batch_cosine_similarity(&query, &candidates);
        assert_eq!(results.len(), 3);
        assert!((results[0] - 1.0).abs() < 1e-6);
        assert!(results[1].abs() < 1e-6);
        assert!(results[2].abs() < 1e-6);
    }
}
