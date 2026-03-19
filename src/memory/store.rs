//! Session and observation CRUD operations.
//!
//! Provides functions to create sessions, save observations with linked FQNs,
//! query observations by session, and retrieve recent sessions.

use anyhow::{Context, Result};
use rusqlite::{Connection, params};

use crate::util::unix_now;

/// Data for creating a new observation.
pub struct NewObservation {
    /// Session this observation belongs to.
    pub session_id: String,
    /// Observation kind: `auto`, `insight`, `decision`, `error`, or `manual`.
    pub kind: String,
    /// Full observation content.
    pub content: String,
    /// Optional compact headline (~20 tokens).
    pub headline: Option<String>,
    /// Detail level: 1 (L1, ~20 tokens), 2 (L2, ~50 tokens), or 3 (L3, ~100 tokens).
    pub detail_level: i32,
    /// Fully-qualified symbol names linked to this observation.
    pub linked_fqns: Vec<String>,
}

/// A stored observation.
#[derive(Debug, Clone)]
pub struct Observation {
    /// Database row ID.
    pub id: i64,
    /// Session this observation belongs to.
    pub session_id: String,
    /// Observation kind: `auto`, `insight`, `decision`, `error`, or `manual`.
    pub kind: String,
    /// Full observation content.
    pub content: String,
    /// Optional compact headline.
    pub headline: Option<String>,
    /// Detail level (1, 2, or 3).
    pub detail_level: i32,
    /// Whether this observation has been marked stale.
    pub is_stale: bool,
    /// Unix timestamp when the observation was created.
    pub created_at: i64,
    /// Optional relevance score.
    pub score: Option<f64>,
}

/// A stored session.
#[derive(Debug, Clone)]
pub struct Session {
    /// Unique session identifier (UUID v4).
    pub id: String,
    /// Unix timestamp when the session was started.
    pub started_at: i64,
    /// Unix timestamp of the most recent activity.
    pub last_active: i64,
    /// Whether this session has been compressed.
    pub is_compressed: bool,
    /// Summary text generated during compression.
    pub summary: Option<String>,
    /// Comma-separated key terms extracted during compression.
    pub key_terms: Option<String>,
    /// Comma-separated key file paths extracted during compression.
    pub key_files: Option<String>,
}

/// Creates a new session and returns its UUID.
///
/// # Errors
///
/// Returns an error if the database insert fails.
pub fn create_session(conn: &Connection) -> Result<String> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = unix_now();
    conn.execute(
        "INSERT INTO sessions (id, started_at, last_active) VALUES (?1, ?2, ?3)",
        params![id, now, now],
    )
    .context("insert new session")?;
    Ok(id)
}

/// Updates the `last_active` timestamp for a session.
///
/// # Errors
///
/// Returns an error if the database update fails.
pub fn update_session_active(conn: &Connection, session_id: &str) -> Result<()> {
    conn.execute(
        "UPDATE sessions SET last_active = ?1 WHERE id = ?2",
        params![unix_now(), session_id],
    )
    .context("update session last_active")?;
    Ok(())
}

/// Saves a new observation and its symbol links.
///
/// Inserts the observation row and then creates `observation_links` entries
/// for each fully-qualified name in `obs.linked_fqns`.
///
/// # Errors
///
/// Returns an error if any database insert fails.
pub fn save_observation(conn: &Connection, obs: &NewObservation) -> Result<i64> {
    let now = unix_now();
    conn.execute(
        "INSERT INTO observations (session_id, kind, content, headline, detail_level, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            obs.session_id,
            obs.kind,
            obs.content,
            obs.headline,
            obs.detail_level,
            now
        ],
    )
    .context("insert observation")?;
    let obs_id = conn.last_insert_rowid();

    for fqn in &obs.linked_fqns {
        conn.execute(
            "INSERT OR IGNORE INTO observation_links (observation_id, symbol_fqn) \
             VALUES (?1, ?2)",
            params![obs_id, fqn],
        )
        .context("insert observation link")?;
    }

    Ok(obs_id)
}

/// Retrieves all observations for a session, ordered by creation time (newest first).
///
/// # Errors
///
/// Returns an error if the database query fails.
pub fn get_session_observations(conn: &Connection, session_id: &str) -> Result<Vec<Observation>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, session_id, kind, content, headline, detail_level, is_stale, \
             created_at, score \
             FROM observations WHERE session_id = ?1 ORDER BY created_at DESC",
        )
        .context("prepare get_session_observations")?;

    let rows = stmt
        .query_map(params![session_id], |row| {
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
        .context("query session observations")?;

    let mut observations = Vec::new();
    for row in rows {
        observations.push(row.context("read observation row")?);
    }
    Ok(observations)
}

/// Retrieves recent sessions, ordered by `last_active` (newest first).
///
/// When `include_compressed` is `false`, only uncompressed sessions are returned.
///
/// # Errors
///
/// Returns an error if the database query fails.
#[allow(clippy::cast_possible_wrap)] // small usize fits in i64
pub fn get_recent_sessions(
    conn: &Connection,
    count: usize,
    include_compressed: bool,
) -> Result<Vec<Session>> {
    let sql = if include_compressed {
        "SELECT id, started_at, last_active, is_compressed, summary, key_terms, key_files \
         FROM sessions ORDER BY last_active DESC LIMIT ?1"
    } else {
        "SELECT id, started_at, last_active, is_compressed, summary, key_terms, key_files \
         FROM sessions WHERE is_compressed = 0 ORDER BY last_active DESC LIMIT ?1"
    };

    let mut stmt = conn.prepare(sql).context("prepare get_recent_sessions")?;
    let rows = stmt
        .query_map(params![count as i64], |row| {
            Ok(Session {
                id: row.get(0)?,
                started_at: row.get(1)?,
                last_active: row.get(2)?,
                is_compressed: row.get(3)?,
                summary: row.get(4)?,
                key_terms: row.get(5)?,
                key_files: row.get(6)?,
            })
        })
        .context("query recent sessions")?;

    let mut sessions = Vec::new();
    for row in rows {
        sessions.push(row.context("read session row")?);
    }
    Ok(sessions)
}

/// Gets linked FQNs for an observation.
///
/// # Errors
///
/// Returns an error if the database query fails.
pub fn get_observation_links(conn: &Connection, observation_id: i64) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare("SELECT symbol_fqn FROM observation_links WHERE observation_id = ?1")
        .context("prepare get_observation_links")?;

    let rows = stmt
        .query_map(params![observation_id], |row| row.get(0))
        .context("query observation links")?;

    let mut fqns = Vec::new();
    for row in rows {
        fqns.push(row.context("read observation link row")?);
    }
    Ok(fqns)
}
