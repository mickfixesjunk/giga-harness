//! `foundation` — the dependency-free leaf layer.
//!
//! Everything here is a primitive the rest of the crate builds on:
//! path/string normalization, the timestamp format, crash-safe file
//! writes, the subprocess/ssh substrate, slice-file naming, and the
//! tailnet roster parser. In a later phase this also hosts the
//! `===`-frame grammar, the byte-cursor tailer, and the locked append.
//!
//! ## The leaf-layer contract
//!
//! `foundation` knows nothing domain-specific. It must NOT depend on any
//! other in-crate module (`config`, `coordination`, `transport`, …) —
//! only on `std` and external crates. The dependency arrow always points
//! *into* `foundation`, never out of it. This keeps the bottom of the
//! crate's dependency DAG free of cycles: if you find yourself wanting to
//! `use crate::config` from here, the helper belongs one layer up, not in
//! `foundation`.
//!
//! Modules are one-concept-per-file and individually unit-tested so the
//! primitives can be verified in isolation.

// TEMP (removed in Phase 13): a few primitives (the Tailer, config_dir /
// walk_up, capture) are intentionally consumed by LATER structural phases
// (coordination in 8; config/registry in 5/12), so they read as dead code
// until then. Suppressing here keeps intervening builds clean enough to
// spot genuine regressions; Phase 13's `-D warnings` clippy gate re-checks
// for anything actually unused once every caller is wired.
#![allow(dead_code)]

pub mod append;
pub mod atomic_io;
pub mod dirs;
pub mod frame;
pub mod paths;
pub mod proc;
pub mod self_invoke;
pub mod slices;
pub mod ssh;
pub mod tail;
pub mod tailscale;
pub mod timefmt;
