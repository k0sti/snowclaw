//! Context-VM bridge: runs Nomen's Context-VM server within Snowclaw's daemon.
//!
//! Initialises a Nomen instance with relay connectivity and delegates to
//! `nomen::contextvm::ContextVmServer` for the kind 21900/21901 protocol.
//!
//! # Status: Deferred
//!
//! Integration is architecturally straightforward but blocked on two issues:
//!
//! 1. **nostr crate version mismatch** — Snowclaw uses nostr-sdk 0.44 (nostr 0.44)
//!    while Nomen uses nostr-sdk 0.39 (nostr 0.39). `Keys`, `PublicKey`, etc. are
//!    incompatible across versions. Fix: add `parse_keys(nsec: &str) -> Keys` to
//!    `nomen::signer` so Snowclaw can construct keys without importing nostr 0.39.
//!
//! 2. **Non-Clone types** — `RelayManager` and `GroupStore` don't implement `Clone`.
//!    `ContextVmServer::new()` takes them by value, but `Nomen` only exposes them by
//!    reference. Fix: either add a constructor that takes `&Nomen` directly, or
//!    implement `Clone` for these types.
//!
//! # What needs to happen (in Nomen):
//!
//! ```ignore
//! // In nomen::signer:
//! pub fn parse_keys(nsec: &str) -> anyhow::Result<Keys> {
//!     Keys::parse(nsec).map_err(|e| anyhow::anyhow!("{e}"))
//! }
//!
//! // In nomen::contextvm:
//! impl ContextVmServer {
//!     pub fn from_nomen(nomen: &Nomen, allowed_npubs: Vec<String>, channel: String) -> Result<Self>;
//! }
//! ```
//!
//! # Daemon wiring (ready in src/daemon/mod.rs)
//!
//! The daemon supervisor spawn block is already wired. Once the Nomen-side
//! changes land, uncomment the body of `run()` below.

use anyhow::{bail, Result};
use tracing::info;

use crate::config::Config;

/// Run the Context-VM server as a long-lived daemon component.
///
/// Currently a stub — see module docs for blockers and next steps.
pub async fn run(config: &Config) -> Result<()> {
    let cvm_config = config.contextvm.as_ref();

    let enabled = cvm_config.is_some_and(|c| c.enabled);

    if !enabled {
        info!("Context-VM disabled in config");
        return Ok(());
    }

    bail!(
        "Context-VM is not yet available: blocked on nostr crate version alignment \
         between Snowclaw (0.44) and Nomen (0.39). See src/memory/contextvm_bridge.rs \
         module docs for the fix path."
    );
}
