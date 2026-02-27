//! Sonido Chain — Multi-effect CLAP plugin.
//!
//! Dynamic effect chain with add/remove/reorder. 16 slots × 32 params = 512
//! pre-allocated CLAP parameters. Chain mutations use `rescan(INFO | VALUES)` —
//! no host restart, no automation loss.
//!
//! Build: `cargo build -p sonido-plugin --example sonido-chain`
//! Output: `target/debug/examples/libsonido_chain.so` (rename to `.clap`)

clack_plugin::clack_export_entry!(
    clack_plugin::prelude::SinglePluginEntry<sonido_plugin::chain::ChainPlugin>
);
