//! Build script for sonido-daisy.
//!
//! The Electrosmith bootloader copies user firmware from QSPI flash to AXI SRAM
//! (BOOT_SRAM mode). Both embassy-stm32 and daisy-embassy generate a `memory.x`
//! with FLASH at 0x08000000, which conflicts with our SRAM layout.
//!
//! Since Cargo features are additive and we can't disable their `memory-x`
//! generation, we overwrite all generated `memory.x` files with our BOOT_SRAM layout.
//!
//! It also encodes the shared factory rigs (`sonido_rigs::factory_rigs`) into
//! `$OUT_DIR/factory_rigs.rs`, which `src/rigs.rs` includes: the pedal ships
//! the same rigs as the plugin and the editor, as bytes, with none of the code
//! that builds them.

fn main() {
    write_factory_rigs();

    let target_dir = std::env::var("OUT_DIR")
        .map(std::path::PathBuf::from)
        .unwrap();
    // OUT_DIR is <target>/build/sonido-daisy-<hash>/out
    // Navigate up to <target>/build/
    let build_dir = target_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("could not find build directory");

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let our_memory_x = std::path::Path::new(&manifest_dir).join("memory.x");

    // Overwrite all generated memory.x files from dependencies
    if let Ok(entries) = std::fs::read_dir(build_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with("embassy-stm32-") || name_str.starts_with("daisy-embassy-") {
                let their_memory_x = entry.path().join("out").join("memory.x");
                if their_memory_x.exists() {
                    std::fs::copy(&our_memory_x, &their_memory_x)
                        .expect("failed to overwrite generated memory.x");
                }
            }
        }
    }

    println!("cargo:rerun-if-changed=memory.x");
}

/// Encode every factory rig to its binary patch form, as Rust byte arrays.
fn write_factory_rigs() {
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("factory_rigs.rs");
    let mut code = String::from(
        "/// The factory rigs, encoded by `build.rs` from `sonido_rigs::factory_rigs`.\n\
         pub const FACTORY_RIGS: &[&[u8]] = &[\n",
    );
    for rig in sonido_rigs::factory_rigs() {
        let mut buf = vec![0u8; sonido_patch::SECTOR_SIZE];
        let n = sonido_patch::encode(&rig, &mut buf)
            .unwrap_or_else(|e| panic!("factory rig {} does not encode: {e}", rig.name));
        code.push_str(&format!("    // {}\n    &{:?},\n", rig.name, &buf[..n]));
    }
    code.push_str("];\n");
    std::fs::write(out, code).expect("write factory_rigs.rs");
    println!("cargo:rerun-if-changed=../sonido-rigs/src");
}
