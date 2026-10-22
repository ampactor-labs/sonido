//! The Sonido plugin bundle: all 36 effects plus the graph player in one
//! `.clap` file.
//!
//! Build: `cargo build --release -p sonido-plugin --example sonido`
//! Output: `target/release/examples/libsonido.so` (Linux; install as `sonido.clap`).
//! `scripts/bundle-clap.sh` packages it per platform.

clack_plugin::clack_export_entry!(sonido_plugin::bundle::SonidoEntry);
