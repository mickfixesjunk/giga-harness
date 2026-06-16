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
