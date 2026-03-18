//! Automatic observation capture from MCP tool calls.
//!
//! Transforms tool call records into observations, filtering out tools that
//! should not generate observations (to avoid infinite loops and noise).

use anyhow::Result;
use rusqlite::Connection;

use super::store::{self, NewObservation};

/// Record of an MCP tool call for auto-capture.
pub struct ToolCallRecord {
    /// Name of the MCP tool that was invoked.
    pub tool_name: String,
    /// Optional intent classification (e.g., "debug", "explore", "refactor").
    pub intent: Option<String>,
    /// Optional query string used in the tool call.
    pub query: Option<String>,
    /// Fully-qualified symbol names used as pivots in the tool call.
    pub pivot_fqns: Vec<String>,
    /// Short summary of the tool call result.
    pub result_summary: String,
}

impl ToolCallRecord {
    /// Returns whether this tool call should generate an auto-capture observation.
    ///
    /// Excluded tools: `search_memory`, `save_observation`, `index_status`
    /// (to avoid infinite loops and noise).
    #[must_use]
    pub fn should_capture(&self) -> bool {
        !matches!(
            self.tool_name.as_str(),
            "search_memory" | "save_observation" | "index_status"
        )
    }

    /// Generates a compact headline (~20 tokens) for the L1 detail level.
    #[must_use]
    pub fn to_headline(&self) -> String {
        match self.tool_name.as_str() {
            "run_pipeline" => format!(
                "Pipeline: {} -> {}",
                self.intent.as_deref().unwrap_or("explore"),
                self.result_summary
            ),
            "get_context_capsule" => format!(
                "Capsule: '{}' -> {}",
                truncate(self.query.as_deref().unwrap_or(""), 30),
                self.result_summary
            ),
            "get_skeleton" => format!("Skeleton: {}", self.result_summary),
            "get_impact_graph" => format!("Impact: {}", self.result_summary),
            "get_session_context" => format!("Session context: {}", self.result_summary),
            _ => format!("{}: {}", self.tool_name, self.result_summary),
        }
    }

    /// Generates standard content (~50 tokens) for the L2 detail level.
    #[must_use]
    pub fn to_content(&self) -> String {
        let mut parts = Vec::new();
        parts.push(format!("Tool: {}", self.tool_name));
        if let Some(intent) = &self.intent {
            parts.push(format!("Intent: {intent}"));
        }
        if let Some(query) = &self.query {
            parts.push(format!("Query: {}", truncate(query, 60)));
        }
        parts.push(format!("Result: {}", self.result_summary));
        if !self.pivot_fqns.is_empty() {
            let fqns: Vec<&str> = self
                .pivot_fqns
                .iter()
                .take(5)
                .map(|f| truncate(f, 40))
                .collect();
            parts.push(format!("Pivots: {}", fqns.join(", ")));
        }
        parts.join(". ")
    }
}

/// Saves an auto-capture observation from a tool call.
///
/// Skips capture for excluded tools (see [`ToolCallRecord::should_capture`]).
///
/// # Errors
///
/// Returns an error if the database insert fails.
pub fn auto_capture(conn: &Connection, session_id: &str, record: &ToolCallRecord) -> Result<()> {
    if !record.should_capture() {
        return Ok(());
    }

    let obs = NewObservation {
        session_id: session_id.to_owned(),
        kind: "auto".to_owned(),
        content: record.to_content(),
        headline: Some(record.to_headline()),
        detail_level: 2,
        linked_fqns: record.pivot_fqns.clone(),
    };

    store::save_observation(conn, &obs)?;
    Ok(())
}

/// Truncates a string to `max_len` bytes at a valid UTF-8 char boundary.
///
/// Returns the original string unchanged if it is already within the limit.
fn truncate(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        return s;
    }
    // Find the largest char boundary <= max_len.
    let mut end = max_len;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_ascii() {
        assert_eq!(truncate("hello world", 5), "hello");
        assert_eq!(truncate("hi", 10), "hi");
    }

    #[test]
    fn truncate_multibyte() {
        // Each char is 2 bytes.
        let s = "\u{00e9}\u{00e9}\u{00e9}"; // 6 bytes
        let t = truncate(s, 3);
        assert!(t.len() <= 3);
        assert!(t.is_char_boundary(t.len()));
    }

    #[test]
    fn should_capture_excludes_memory_tools() {
        let record = ToolCallRecord {
            tool_name: "search_memory".to_owned(),
            intent: None,
            query: None,
            pivot_fqns: vec![],
            result_summary: String::new(),
        };
        assert!(!record.should_capture());

        let record = ToolCallRecord {
            tool_name: "run_pipeline".to_owned(),
            intent: None,
            query: None,
            pivot_fqns: vec![],
            result_summary: String::new(),
        };
        assert!(record.should_capture());
    }
}
