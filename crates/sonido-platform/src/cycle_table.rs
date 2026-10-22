//! Per-effect cost table for export-time CPU/memory budgeting on the pedal.
//!
//! The GUI uses this to warn (or refuse) before flashing a patch the pedal
//! can't run in real time. Keyed by stable effect UID
//! ([`sonido_registry::EFFECT_UIDS`](../sonido_registry/constant.EFFECT_UIDS.html)).
//!
//! # Measured on a Daisy Seed
//!
//! Cycles are the **worst** 32-sample block of each kernel measured on a Daisy
//! Seed (STM32H750 at 480 MHz) by `crates/sonido-daisy/examples/bench_kernels.rs`
//! on 2026-10-22, rounded up to the next 1,000: 8 warm-up blocks, then 128
//! timed blocks of a 220 Hz sine, kernel state in SDRAM as on the pedal.
//! Each entry is the worse of two runs of different builds, because a rebuild
//! alone moves unchanged kernels by up to ±8% (code layout); the distortion's
//! is from the second run only, after its antiderivative changed.
//! SDRAM is each kernel's measured heap at 48 kHz (`docs/EMBEDDED.md`). Both
//! are bare kernel costs; the firmware's audio and control work comes on top,
//! which is what [`WARN_FRACTION`] leaves room for.
//!
//! (These replaced desktop-derived estimates that were up to 2.8× too low —
//! the reverb was put at 64 k cycles, measured 179 k — so validation would
//! have passed patches near the deadline.) Re-run `bench_kernels` after
//! changing a pedal kernel and update [`EFFECT_CYCLES`].

/// Audio-block cycle budget: 480 MHz × (32 samples / 48 kHz) ≈ 320 k cycles.
pub const CYCLE_BUDGET_PER_BLOCK: u32 = 320_000;

/// Total SDRAM available to effect buffers (64 MB).
pub const SDRAM_BUDGET_BYTES: u32 = 64 * 1024 * 1024;

/// Warn once a patch's estimated cost crosses this fraction of the budget.
pub const WARN_FRACTION: f32 = 0.70;

/// `(effect_uid, worst_cycles_per_block)` — measured, see module docs.
///
/// UIDs are the eight curated pedal effects.
pub const EFFECT_CYCLES: &[(u16, u32)] = &[
    (1, 37_000),   // distortion — worst 36,677
    (17, 31_000),  // bitcrusher — worst 30,157
    (18, 9_000),   // ringmod — worst 8,298
    (7, 11_000),   // filter — worst 10,836
    (3, 24_000),   // chorus — worst 23,220
    (5, 23_000),   // phaser — worst 22,631
    (6, 25_000),   // delay — worst 24,445
    (11, 179_000), // reverb — worst 178,701, the heavyweight
];

/// `(effect_uid, sdram_bytes)` — each kernel's measured heap at 48 kHz.
pub const EFFECT_SDRAM: &[(u16, u32)] = &[
    (1, 0),        // distortion
    (17, 0),       // bitcrusher
    (18, 0),       // ringmod
    (7, 0),        // filter
    (3, 23_040),   // chorus — 4 modulated delay lines
    (5, 0),        // phaser — allpass stages inline
    (6, 775_680),  // delay — 2 s stereo lines + diffusion allpasses
    (11, 116_700), // reverb — FDN lines, allpasses, early reflections, predelay
];

/// Worst-case cycles/block for an effect UID, or `None` if unmeasured.
pub fn effect_cycles(uid: u16) -> Option<u32> {
    EFFECT_CYCLES
        .iter()
        .find(|(u, _)| *u == uid)
        .map(|(_, c)| *c)
}

/// SDRAM bytes for an effect UID, or `None` if unknown.
pub fn effect_sdram(uid: u16) -> Option<u32> {
    EFFECT_SDRAM
        .iter()
        .find(|(u, _)| *u == uid)
        .map(|(_, b)| *b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_cover_the_same_uids() {
        for (uid, _) in EFFECT_CYCLES {
            assert!(
                effect_sdram(*uid).is_some(),
                "uid {uid} missing SDRAM entry"
            );
        }
        assert_eq!(EFFECT_CYCLES.len(), EFFECT_SDRAM.len());
    }

    #[test]
    fn a_single_effect_fits_the_budget() {
        for (uid, cycles) in EFFECT_CYCLES {
            assert!(
                *cycles < CYCLE_BUDGET_PER_BLOCK,
                "uid {uid} alone exceeds the block budget"
            );
        }
    }

    #[test]
    fn reverb_is_the_heaviest() {
        let max = EFFECT_CYCLES.iter().map(|(_, c)| *c).max().unwrap();
        assert_eq!(effect_cycles(11), Some(max));
    }
}
