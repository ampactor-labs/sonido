//! Per-effect cost table for CPU and memory budgeting on the pedal.
//!
//! One rule decides whether a rig runs on the pedal, used by the editor's
//! export check (through `sonido_patch::validate_for_pedal`) and by the pedal
//! itself, which refuses rigs sent over USB and skips jam effects that would
//! break it: the measured worst blocks of a rig's effects, plus
//! [`NODE_OVERHEAD_CYCLES`] per node, must stay within [`CHAIN_FRACTION`] of
//! the block budget ([`chain_cycles`], [`chain_fits`]). Keyed by stable effect
//! UID ([`sonido_registry::EFFECT_UIDS`](../sonido_registry/constant.EFFECT_UIDS.html)).
//!
//! # Measured on a Daisy Seed
//!
//! Cycles are the **worst** 32-sample block of each effect measured on a Daisy
//! Seed (STM32H750 at 480 MHz) by `crates/sonido-daisy/examples/bench_kernels.rs`
//! on 2026-10-22, rounded up to the next 1,000: 8 warm-up blocks, then 128
//! timed blocks of a 220 Hz sine, state in SDRAM as on the pedal. They are the
//! benchmark's *rig path*: the effect as the registry builds it (smoothed
//! adapter), one `process_block_stereo` per block, every parameter re-set each
//! 15th block like the pedal's control tick. That is what a rig runs; it costs
//! 2–13 k cycles more per effect than the bare kernel. Built with the
//! firmware's shipping profile (fat LTO, size-optimized HAL). A rebuild alone
//! moves an unchanged effect by up to ±8% (code layout), which the 80% rule's
//! margin absorbs. SDRAM is each effect's heap at 48 kHz as the
//! registry builds it (kernel plus adapter), measured on the host.
//!
//! # Why 80%
//!
//! Kernel costs are bare; the whole pedal adds the codec DMA, the graph and
//! the controls. Measured in a Hothouse, distortion → reverb → bitcrusher
//! sums to 75.6% of the budget in worst blocks and ran at 78% mean with its
//! slowest callbacks at 89%, about 13 points above the sum. A rig at the
//! 80% limit should therefore peak near 93%.
//!
//! Re-run `bench_kernels` after changing a kernel and update [`EFFECT_CYCLES`].

/// Audio-block cycle budget: 480 MHz × (32 samples / 48 kHz) ≈ 320 k cycles.
pub const CYCLE_BUDGET_PER_BLOCK: u32 = 320_000;

/// Total SDRAM available to effect buffers (64 MB).
pub const SDRAM_BUDGET_BYTES: u32 = 64 * 1024 * 1024;

/// Warn once a rig's estimated cost crosses this fraction of the budget.
pub const WARN_FRACTION: f32 = 0.70;

/// Refuse a rig whose estimated cost crosses this fraction of the budget.
pub const CHAIN_FRACTION: f32 = 0.80;

/// Graph scheduling and buffer cost per node, added to the kernels' cost.
pub const NODE_OVERHEAD_CYCLES: u32 = 400;

/// `(effect_uid, worst_cycles_per_block)` for all 36 effects, measured (see module docs).
pub const EFFECT_CYCLES: &[(u16, u32)] = &[
    (1, 41_000),   // distortion — worst 40,023
    (2, 46_000),   // compressor — worst 45,099
    (3, 31_000),   // chorus — worst 30,392
    (4, 28_000),   // flanger — worst 27,940
    (5, 28_000),   // phaser — worst 27,301
    (6, 34_000),   // delay — worst 33,003
    (7, 15_000),   // filter — worst 14,082
    (8, 52_000),   // vibrato — worst 51,646
    (9, 97_000),   // tape — worst 96,905
    (10, 26_000),  // preamp — worst 25,092
    (11, 198_000), // reverb — worst 197,300
    (12, 17_000),  // tremolo — worst 16,883
    (13, 25_000),  // gate — worst 24,054
    (14, 24_000),  // wah — worst 23,152
    (15, 17_000),  // eq — worst 16,590
    (16, 25_000),  // limiter — worst 24,074
    (17, 14_000),  // bitcrusher — worst 13,151
    (18, 14_000),  // ringmod — worst 13,642
    (19, 15_000),  // stage — worst 14,926
    (20, 8_000),   // looper — worst 7,754
    (21, 132_000), // amp — worst 131,223
    (22, 87_000),  // cabinet — worst 86,600
    (23, 15_000),  // deesser — worst 14,245
    (24, 68_000),  // drone — worst 67,628
    (25, 14_000),  // glitch — worst 13,971
    (26, 59_000),  // multiband_comp — worst 58,476
    (27, 34_000),  // pitch_shift — worst 33,272
    (28, 73_000),  // plate_reverb — worst 72,552
    (29, 12_000),  // shelving_eq — worst 11,172
    (30, 30_000),  // spring_reverb — worst 29,582
    (31, 10_000),  // stereo_widener — worst 9,151
    (32, 20_000),  // texture — worst 19,148
    (33, 46_000),  // time_stretch — worst 45,723
    (34, 16_000),  // transient_shaper — worst 15,608
    (35, 204_000), // tuner — worst 203,837
    (36, 114_000), // harmonic_habitat — worst 113,621
];

/// `(effect_uid, sdram_bytes)` for all 36 effects: heap at 48 kHz as the registry builds them.
pub const EFFECT_SDRAM: &[(u16, u32)] = &[
    (1, 576),         // distortion
    (2, 560),         // compressor
    (3, 23_704),      // chorus
    (4, 6_352),       // flanger
    (5, 752),         // phaser
    (6, 776_432),     // delay
    (7, 304),         // filter
    (8, 13_104),      // vibrato
    (9, 4_848),       // tape
    (10, 272),        // preamp
    (11, 118_276),    // reverb
    (12, 368),        // tremolo
    (13, 720),        // gate
    (14, 296),        // wah
    (15, 728),        // eq
    (16, 19_560),     // limiter
    (17, 216),        // bitcrusher
    (18, 200),        // ringmod
    (19, 6_548),      // stage
    (20, 23_040_312), // looper
    (21, 1_328),      // amp
    (22, 3_320),      // cabinet
    (23, 328),        // deesser
    (24, 272),        // drone
    (25, 524_608),    // glitch
    (26, 1_120),      // multiband_comp
    (27, 65_872),     // pitch_shift
    (28, 106_188),    // plate_reverb
    (29, 456),        // shelving_eq
    (30, 114_780),    // spring_reverb
    (31, 6_300),      // stereo_widener
    (32, 524_912),    // texture
    (33, 1_049_032),  // time_stretch
    (34, 248),        // transient_shaper
    (35, 33_112),     // tuner
    (36, 100_208),    // harmonic_habitat
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

/// Estimated cycles per block for a rig made of these effects: each one's
/// measured worst block plus [`NODE_OVERHEAD_CYCLES`]. An effect with no
/// measurement counts as `u32::MAX`, so it never fits.
pub fn chain_cycles(uids: impl IntoIterator<Item = u16>) -> u32 {
    uids.into_iter().fold(0u32, |sum, uid| {
        sum.saturating_add(effect_cycles(uid).unwrap_or(u32::MAX))
            .saturating_add(NODE_OVERHEAD_CYCLES)
    })
}

/// Whether a rig made of these effects runs within [`CHAIN_FRACTION`] of the
/// block budget.
pub fn chain_fits(uids: impl IntoIterator<Item = u16>) -> bool {
    chain_cycles(uids) as f32 <= CYCLE_BUDGET_PER_BLOCK as f32 * CHAIN_FRACTION
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
    fn covers_all_36_effects() {
        assert_eq!(EFFECT_CYCLES.len(), 36);
        for uid in 1..=36 {
            assert!(effect_cycles(uid).is_some(), "uid {uid} unmeasured");
        }
    }

    #[test]
    fn heaviest_jam_fits_and_two_reverbs_do_not() {
        // distortion → reverb → bitcrusher: measured on the pedal at 89% peak.
        assert!(chain_fits([1, 11, 17]));
        assert!(!chain_fits([11, 11]));
        assert!(!chain_fits([99]));
    }
}
