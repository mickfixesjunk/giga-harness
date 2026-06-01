//! Concrete transport plugs. The trait + factory live in `crate::transport`.

pub mod git;
pub mod local;
pub mod rsync_tailscale;
