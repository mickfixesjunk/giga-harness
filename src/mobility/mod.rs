//! Move agents + the harness through space (teleport), runtimes (takeover), and versions (upgrade). These re-invoke the giga binary itself via foundation::self_invoke.

pub mod takeover;
pub mod teleport;
pub mod upgrade;
