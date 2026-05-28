//! Scan orchestration and the scan cache.
//!
//! Detects available providers, runs them concurrently, assembles a
//! `ScanResult`, and persists it to the cache. Never analyzes data.
//!
//! Built in v0.0.2 (orchestration) and v0.0.3 (cache).
