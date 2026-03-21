//! Hybrid search over observations: BM25 + TF-IDF + recency + proximity.
//!
//! Combines full-text search (FTS5 BM25) with TF-IDF cosine similarity,
//! exponential recency decay, and symbol proximity scoring to surface the
//! most relevant observations for a given query context.

use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};
use rusqlite::{Connection, params};

use crate::indexer::tokenizer::{self, build_fts_query};
use crate::storage::db::{BATCH_PARAM_LIMIT, build_batch_placeholders};
use crate::util::unix_now;

use super::store::Observation;

/// A memory search result with composite scoring.
#[derive(Debug, Clone)]
pub struct MemoryResult {
    /// The matched observation.
    pub observation: Observation,
    /// Composite relevance score (higher is better).
    pub memory_score: f64,
    /// Fully-qualified symbol names linked to this observation.
    pub linked_fqns: Vec<String>,
}

/// Weight for the BM25 (full-text search) score component.
const W_BM25: f64 = 0.35;
/// Weight for the TF-IDF cosine similarity component.
const W_TFIDF: f64 = 0.25;
/// Weight for the recency decay component.
const W_RECENCY: f64 = 0.20;
/// Weight for the symbol proximity component.
const W_PROXIMITY: f64 = 0.15;
/// Penalty applied to stale observations.
const STALENESS_PENALTY: f64 = 0.30;
/// Maximum number of FTS5 candidates to retrieve before scoring.
const FTS_CANDIDATE_LIMIT: usize = 50;

/// Searches observations using hybrid scoring.
///
/// The composite score is computed as:
///
/// ```text
/// score = 0.35 * bm25_norm + 0.25 * tfidf_cosine + 0.20 * recency + 0.15 * proximity - staleness_penalty
/// ```
///
/// - `bm25_norm`: FTS5 BM25 score on `observations_fts`, min-max normalised.
/// - `tfidf_cosine`: Cosine similarity between query and observation content TF vectors.
/// - `recency`: Exponential decay `0.5^(age_days / half_life_days)`.
/// - `proximity`: Fraction of linked FQNs shared with `pivot_fqns`.
/// - `staleness_penalty`: 0.30 if `is_stale`, else 0.0.
///
/// # Errors
///
/// Returns an error if any database query fails.
#[allow(clippy::cast_precision_loss)] // usize->f64 loss negligible for counts
pub fn search_memories(
    conn: &Connection,
    query: &str,
    pivot_fqns: &[String],
    limit: usize,
    include_stale: bool,
    recency_half_life_days: f64,
    kind: Option<&str>,
) -> Result<Vec<MemoryResult>> {
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }

    let recency_half_life_days = recency_half_life_days.max(f64::EPSILON);

    // 1. Collect FTS5 candidates with BM25 scores.
    let candidates = fts_candidates(conn, query)?;
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    // 2. Batch-load all observations and their links in two queries instead of N+1.
    let candidate_ids: Vec<i64> = candidates.iter().map(|(id, _)| *id).collect();
    let observations = batch_load_observations(conn, &candidate_ids)?;
    let all_links = batch_load_observation_links(conn, &candidate_ids)?;

    // 3. Tokenize query for TF-IDF cosine similarity.
    let query_tokens = tokenizer::tokenize_text(query);
    let query_tf = tokenizer::compute_tf(&query_tokens);

    // 4. Compute min/max BM25 for normalisation.
    let (bm25_min, bm25_max) = bm25_range(&candidates);

    let now_secs = unix_now();
    let pivot_set: HashSet<&str> = pivot_fqns.iter().map(String::as_str).collect();

    // 5. Score each candidate.
    let mut results: Vec<MemoryResult> = Vec::with_capacity(candidates.len());

    for (obs_id, raw_bm25) in &candidates {
        let Some(obs) = observations.get(obs_id) else {
            continue;
        };

        // Filter stale if requested.
        if !include_stale && obs.is_stale {
            continue;
        }

        let linked_fqns = all_links.get(obs_id).cloned().unwrap_or_default();

        // a) BM25 normalised.
        let bm25_norm = if (bm25_max - bm25_min).abs() < f64::EPSILON {
            0.0
        } else {
            (raw_bm25 - bm25_min) / (bm25_max - bm25_min)
        };

        // b) TF-IDF cosine similarity.
        let obs_tokens = tokenizer::tokenize_text(&obs.content);
        let obs_tf = tokenizer::compute_tf(&obs_tokens);
        let tfidf_cosine = cosine_similarity(&query_tf, &obs_tf);

        // c) Recency decay.
        let age_days = (now_secs - obs.created_at).max(0) as f64 / 86400.0;
        let recency = 0.5_f64.powf(age_days / recency_half_life_days);

        // d) Proximity: fraction of observation's linked FQNs that appear in pivot_fqns.
        let proximity = if linked_fqns.is_empty() {
            0.0
        } else {
            let shared = linked_fqns
                .iter()
                .filter(|f| pivot_set.contains(f.as_str()))
                .count();
            shared as f64 / linked_fqns.len() as f64
        };

        // e) Staleness penalty.
        let stale_penalty = if obs.is_stale { STALENESS_PENALTY } else { 0.0 };

        // Combine using fused multiply-add for accuracy.
        let weighted = W_PROXIMITY.mul_add(
            proximity,
            W_RECENCY.mul_add(recency, W_BM25.mul_add(bm25_norm, W_TFIDF * tfidf_cosine)),
        );
        let score = (weighted - stale_penalty).max(0.0);

        results.push(MemoryResult {
            observation: obs.clone(),
            memory_score: score,
            linked_fqns,
        });
    }

    // 6. Sort by score descending, filter by kind, and truncate.
    results.sort_by(|a, b| b.memory_score.total_cmp(&a.memory_score));
    if let Some(kind_filter) = kind {
        results.retain(|r| r.observation.kind == kind_filter);
    }
    results.truncate(limit);

    // 7. Persist scores to the database for later retrieval without recomputation.
    persist_scores(conn, &results);

    Ok(results)
}

/// Retrieves FTS5 candidates from `observations_fts` with BM25 scores.
///
/// Returns `(observation_id, abs(bm25_score))` pairs. BM25 column weights:
/// content = 5.0, headline = 1.0.
fn fts_candidates(conn: &Connection, query: &str) -> Result<Vec<(i64, f64)>> {
    // Escape FTS5 special characters by quoting each token.
    let fts_query = build_fts_query(query);
    if fts_query.is_empty() {
        return Ok(Vec::new());
    }

    let mut stmt = conn
        .prepare(
            "SELECT rowid, bm25(observations_fts, 5.0, 1.0) AS score \
             FROM observations_fts \
             WHERE observations_fts MATCH ?1 \
             ORDER BY score \
             LIMIT ?2",
        )
        .context("prepare FTS5 candidate query")?;

    let rows = stmt
        .query_map(
            params![
                fts_query,
                i64::try_from(FTS_CANDIDATE_LIMIT).expect("candidate limit exceeds i64")
            ],
            |row| {
                let rowid: i64 = row.get(0)?;
                let score: f64 = row.get(1)?;
                Ok((rowid, score.abs()))
            },
        )
        .context("query observations_fts")?;

    let mut candidates = Vec::new();
    for row in rows {
        candidates.push(row.context("read FTS5 candidate row")?);
    }
    Ok(candidates)
}

/// Computes the min and max BM25 scores from candidates.
fn bm25_range(candidates: &[(i64, f64)]) -> (f64, f64) {
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    for &(_, score) in candidates {
        if score < min {
            min = score;
        }
        if score > max {
            max = score;
        }
    }
    (min, max)
}

/// Batch-loads observations by their row IDs.
///
/// Chunks IDs into groups of `BATCH_PARAM_LIMIT` to stay within the `SQLite`
/// parameter limit, querying `WHERE id IN (...)` per chunk.
fn batch_load_observations(conn: &Connection, ids: &[i64]) -> Result<HashMap<i64, Observation>> {
    let mut result = HashMap::with_capacity(ids.len());
    for chunk in ids.chunks(BATCH_PARAM_LIMIT) {
        let placeholders = build_batch_placeholders(chunk.len());
        let sql = format!(
            "SELECT id, session_id, kind, content, headline, detail_level, is_stale, \
             created_at, score \
             FROM observations WHERE id IN ({placeholders})"
        );
        let mut stmt = conn
            .prepare(&sql)
            .context("prepare batch_load_observations")?;
        let params: Vec<&dyn rusqlite::types::ToSql> = chunk
            .iter()
            .map(|id| id as &dyn rusqlite::types::ToSql)
            .collect();
        let rows = stmt
            .query_map(params.as_slice(), |row| {
                Ok(Observation {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    kind: row.get(2)?,
                    content: row.get(3)?,
                    headline: row.get(4)?,
                    detail_level: row.get(5)?,
                    is_stale: row.get(6)?,
                    created_at: row.get(7)?,
                    score: row.get(8)?,
                })
            })
            .context("query batch observations")?;
        for row in rows {
            let obs = row.context("read observation row")?;
            result.insert(obs.id, obs);
        }
    }
    Ok(result)
}

/// Batch-loads linked FQNs for a set of observation IDs.
///
/// Returns a map from observation ID to its list of linked symbol FQNs.
fn batch_load_observation_links(
    conn: &Connection,
    ids: &[i64],
) -> Result<HashMap<i64, Vec<String>>> {
    let mut result: HashMap<i64, Vec<String>> = HashMap::with_capacity(ids.len());
    for chunk in ids.chunks(BATCH_PARAM_LIMIT) {
        let placeholders = build_batch_placeholders(chunk.len());
        let sql = format!(
            "SELECT observation_id, symbol_fqn FROM observation_links \
             WHERE observation_id IN ({placeholders})"
        );
        let mut stmt = conn
            .prepare(&sql)
            .context("prepare batch_load_observation_links")?;
        let params: Vec<&dyn rusqlite::types::ToSql> = chunk
            .iter()
            .map(|id| id as &dyn rusqlite::types::ToSql)
            .collect();
        let rows = stmt
            .query_map(params.as_slice(), |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })
            .context("query batch observation links")?;
        for row in rows {
            let (obs_id, fqn) = row.context("read observation link row")?;
            result.entry(obs_id).or_default().push(fqn);
        }
    }
    Ok(result)
}

/// Persists relevance scores back to the database (best-effort).
///
/// Errors are intentionally silenced because score persistence is
/// an optimization, not a correctness requirement. A failed update
/// only means the next search won't benefit from cached scores.
///
/// Wraps all updates in a single transaction for efficiency.
fn persist_scores(conn: &Connection, results: &[MemoryResult]) {
    if let Ok(tx) = conn.unchecked_transaction() {
        for result in results {
            let _ = tx.execute(
                "UPDATE observations SET score = ?1 WHERE id = ?2",
                params![result.memory_score, result.observation.id],
            );
        }
        let _ = tx.commit();
    }
}

/// Computes cosine similarity between two TF vectors.
///
/// Returns 0.0 if either vector is empty or both have zero magnitude.
fn cosine_similarity(a: &HashMap<String, f64>, b: &HashMap<String, f64>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }

    let mut dot = 0.0;
    for (term, a_val) in a {
        if let Some(b_val) = b.get(term) {
            dot += a_val * b_val;
        }
    }

    let mag_a: f64 = a.values().map(|v| v * v).sum::<f64>().sqrt();
    let mag_b: f64 = b.values().map(|v| v * v).sum::<f64>().sqrt();

    let denom = mag_a * mag_b;
    if denom < f64::EPSILON {
        0.0
    } else {
        dot / denom
    }
}
