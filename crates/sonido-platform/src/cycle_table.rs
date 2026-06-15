//! Per-effect cost table for export-time CPU/memory budgeting on the pedal.
//!
//! The GUI uses this to warn (or refuse) before flashing a patch the pedal
//! can't run in real time. Keyed by stable effect UID
//! ([`sonido_registry::EFFECT_UIDS`](../sonido_registry/constant.EFFECT_UIDS.html)).
//!
//! # ⚠ These are ESTIMATES, not measurements
//!
//! Real per-effect cycle counts must come from the device — desktop benchmark
//! µs/sample do **not** translate to Cortex-M7 cycles (different ISA, caches,
//! FPU). The values below are conservative (biased high, relative complexity
//! from `docs/BENCHMARKS.md`) so validation *warns* rather than under-reports.
//!
//! Regenerate on hardware once available: run a `bench_kernels`-style DWT cycle
//! probe per pedal effect and replace [`EFFECT_CYCLES`]. Until then, effects
//! missing from the table degrade gracefully — the validator flags the estimate
//! as a lower bound.

/// Audio-block cycle budget: 480 MHz × (32 samples / 48 kHz) ≈ 320 k cycles.
pub const CYCLE_BUDGET_PER_BLOCK: u32 = 320_000;

/// Total SDRAM available to effect buffers (64 MB).
pub const SDRAM_BUDGET_BYTES: u32 = 64 * 1024 * 1024;

/// Warn once a patch's estimated cost crosses this fraction of the budget.
pub const WARN_FRACTION: f32 = 0.70;

/// `(effect_uid, estimated_cycles_per_block)` — ESTIMATES, see module docs.
///
/// UIDs are the eight curated pedal effects. Ordered by rough cost.
pub const EFFECT_CYCLES: &[(u16, u32)] = &[
    (1, 16_000),  // distortion — ADAA waveshaping
    (17, 12_000), // bitcrusher — cheap, sample/bit reduction
    (18, 14_000), // ringmod — carrier osc + multiply
    (7, 18_000),  // filter — SVF
    (3, 26_000),  // chorus — dual modulated delay
    (5, 30_000),  // phaser — 4-stage allpass + LFO
    (6, 40_000),  // delay — interpolated delay + filtered feedback
    (11, 64_000), // reverb — comb/allpass bank, the heavyweight
];

/// `(effect_uid, estimated_sdram_bytes)` — ESTIMATES, see module docs.
pub const EFFECT_SDRAM: &[(u16, u32)] = &[
    (1, 0),           // distortion — no large buffer
    (17, 0),          // bitcrusher
    (18, 0),          // ringmod
    (7, 0),           // filter
    (3, 64 * 1024),   // chorus — modulation delay lines
    (5, 8 * 1024),    // phaser — short allpass delays
    (6, 768 * 1024),  // delay — long stereo delay line
    (11, 512 * 1024), // reverb — comb/allpass buffers
];

/// Estimated cycles/block for an effect UID, or `None` if unmeasured.
pub fn effect_cycles(uid: u16) -> Option<u32> {
    EFFECT_CYCLES
        .iter()
        .find(|(u, _)| *u == uid)
        .map(|(_, c)| *c)
}

/// Estimated SDRAM bytes for an effect UID, or `None` if unknown.
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
