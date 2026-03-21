//! Session memory: persistent observations across coding sessions.
//!
//! Provides CRUD operations for sessions and observations, hybrid search over
//! observation history, automatic observation capture from tool calls, staleness
//! detection when linked symbols change, and session compression for inactive
//! sessions.

pub mod antipatterns;
pub mod capture;
pub mod changes;
pub mod compression;
pub mod search;
pub mod staleness;
pub mod store;
