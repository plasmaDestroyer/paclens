//! Analysis layer: dep graph, overlap detection, orphan detection.
//!
//! Pure: given the same `ScanResult`, always produces the same output. Never
//! calls providers or subprocesses, never writes to disk.
//!
//! Built in v0.0.7 (dep graph + why) and v0.0.8 (overlap detection).
