//! Sonido Graph Player — a CLAP plugin that runs an exported patch (a whole rig)
//! with six macro params, A/B morph, and global gains.
//!
//! Build: `cargo build -p sonido-plugin --example sonido-graph-player`
//! Output: `target/debug/examples/libsonido_graph_player.so` (rename to `.clap`)
//!
//! State is the patch JSON the Studio GUI exports, so "Export as CLAP" loads
//! directly here.

use sonido_plugin::graph_player::GraphPlugin;

clack_plugin::clack_export_entry!(clack_plugin::prelude::SinglePluginEntry<GraphPlugin>);
