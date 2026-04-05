//! Embedding-based semantic search using a local transformer model.
//!
//! Provides dense vector embeddings for symbols and queries using
//! `all-MiniLM-L6-v2` (384-dimensional, FP32) via `tract-onnx`.
//! Model files are stored in `.ndxr/models/` and downloaded on demand
//! via `ndxr model download`. When model files are absent, all embedding
//! operations return gracefully and semantic scoring is disabled.

pub mod download;
pub mod model;
pub mod similarity;
pub mod storage;
