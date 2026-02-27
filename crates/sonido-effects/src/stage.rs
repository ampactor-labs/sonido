//! Stage — signal conditioning and stereo utility effect.
//!
//! A general-purpose signal conditioning tool for gain trimming, phase inversion,
//! channel routing, stereo width control, bass mono summing, and Haas stereo delay.
//! Analogous to Ableton's Utility but with the addition of temporal stereo via Haas.
//!
//! # Signal Flow
//!
//! ```text
//! Input L/R
//!   → Phase invert (±1)
//!   → Channel mode (normal / swap / mono L / mono R)
//!   → Input gain (SmoothedParam, dB)
//!   → DC block (optional, DcBlocker × 2)
//!   → M/S width (encode → scale side × width → decode)
//!   → Balance (linear stereo balance)
//!   → Bass mono (LR4 crossover → mono-sum lows, keep highs stereo)
//!   → Haas delay (InterpolatedDelay on one channel, 0–30 ms)
//!   → Output level (SmoothedParam, dB)
//!   → Output L/R
//! ```
//!
//! # Parameters
//!
//! | Idx | Name | Range | Default |
//! |-----|------|-------|---------|
//! | 0 | Gain | -40 to +12 dB | 0.0 |
//! | 1 | Width | 0–200% | 100% |
//! | 2 | Balance | -100 to +100% | 0.0 |
//! | 3 | Phase L | Off/On | Off |
//! | 4 | Phase R | Off/On | Off |
//! | 5 | Channel | Normal/Swap/Mono L/Mono R | Normal |
//! | 6 | DC Block | Off/On | Off |
//! | 7 | Bass Mono | Off/On | Off |
//! | 8 | Bass Freq | 20–500 Hz | 120 |
//! | 9 | Haas | 0–30 ms | 0.0 |
//! | 10 | Haas Side | L/R | R |
//! | 11 | Output | -20 to +20 dB | 0.0 |
//!
//! # DSP Details
//!
//! ## M/S Width (amplitude-domain stereo)
//!
//! ```text
//! mid  = (L + R) * 0.5
//! side = (L - R) * 0.5 * width
//! out_l = mid + side
//! out_r = mid - side
//! ```
//!
//! At width=1.0: identity. At 0.0: mono. At 2.0: exaggerated side content.
//!
//! ## LR4 Bass Mono Crossover (Linkwitz-Riley 4th-order)
//!
//! Two cascaded Butterworth LP biquads (Q=0.707) per channel produce the
//! low-frequency band, which is mono-summed. Two cascaded Butterworth HP
//! biquads (Q=0.707) per channel extract stereo highs. Recombined at output.
//!
//! Reference: Linkwitz, "Active Crossover Networks for Noncoincident Drivers",
//! JAES 1976.
//!
//! ## Haas Delay (temporal stereo)
//!
//! A short delay (0–30 ms) applied to one channel creates a spatial impression.
//! Uses [`InterpolatedDelay`] with linear interpolation.
//!
//! Reference: Haas, H. (1951). "The influence of a single echo on the
//! audibility of speech", Acustica.

use sonido_core::biquad::{highpass_coefficients, lowpass_coefficients};
use sonido_core::math::{ms_to_samples, samples_to_ms};
use sonido_core::{
    Biquad, DcBlocker, Effect, InterpolatedDelay, ParamDescriptor, ParamFlags, ParamId, ParamScale,
    SmoothedParam, db_to_linear, gain,
};

/// Channel routing mode.
///
/// Controls how the left and right input channels are routed before processing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ChannelMode {
    /// Normal stereo passthrough.
    #[default]
    Normal = 0,
    /// Swap left and right channels.
    Swap = 1,
    /// Both outputs receive the left input (mono from left).
    MonoLeft = 2,
    /// Both outputs receive the right input (mono from right).
    MonoRight = 3,
}

impl ChannelMode {
    /// Convert a float index (0–3) to a `ChannelMode`.
    pub fn from_index(v: f32) -> Self {
        match v as u8 {
            0 => Self::Normal,
            1 => Self::Swap,
            2 => Self::MonoLeft,
            3 => Self::MonoRight,
            _ => Self::Normal,
        }
    }
}

/// Which channel receives the Haas delay.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HaasSide {
    /// Delay the left channel.
    Left = 0,
    /// Delay the right channel (default).
    #[default]
    Right = 1,
}

impl HaasSide {
    /// Convert a float index (0–1) to a `HaasSide`.
    pub fn from_index(v: f32) -> Self {
        if v < 0.5 { Self::Left } else { Self::Right }
    }
}

/// Butterworth Q for Linkwitz-Riley 4th-order crossover (two cascaded 2nd-order).
const BUTTERWORTH_Q: f32 = core::f32::consts::FRAC_1_SQRT_2;

/// Maximum Haas delay in seconds.
const MAX_HAAS_SECONDS: f32 = 0.03;

/// Signal conditioning and stereo utility effect.
///
/// ## Parameters
/// - `gain`: Input trim in dB (-40.0 to +12.0, default 0.0)
/// - `width`: Stereo width as percentage (0 to 200, default 100; internal 0.0–2.0)
/// - `balance`: Stereo balance (-100 to +100, default 0; internal -1.0 to +1.0)
/// - `phase_invert_l`: Invert left channel polarity (0/1, default 0)
/// - `phase_invert_r`: Invert right channel polarity (0/1, default 0)
/// - `channel_mode`: Channel routing (0=Normal, 1=Swap, 2=MonoL, 3=MonoR, default 0)
/// - `dc_block_enabled`: Enable DC blocking filter (0/1, default 0)
/// - `bass_mono_enabled`: Enable LR4 bass mono crossover (0/1, default 0)
/// - `bass_mono_freq`: Bass mono crossover frequency (20–500 Hz, default 120)
/// - `haas_delay_ms`: Haas stereo delay (0–30 ms, default 0)
/// - `haas_side`: Which channel gets delayed (0=L, 1=R, default R)
/// - `output_level`: Output level in dB (-20 to +20, default 0)
#[allow(clippy::struct_excessive_bools)]
pub struct Stage {
    // -- Gain --
    /// Input gain (linear, smoothed). Set via dB.
    gain: SmoothedParam,
    /// Standard output level (linear, smoothed).
    output_level: SmoothedParam,

    // -- Phase --
    /// Invert left channel polarity.
    phase_invert_l: bool,
    /// Invert right channel polarity.
    phase_invert_r: bool,

    // -- Channel routing --
    /// Channel routing mode.
    channel_mode: ChannelMode,

    // -- DC blocking --
    /// Whether DC blocking is active.
    dc_block_enabled: bool,
    /// DC blocker for left channel.
    dc_blocker_l: DcBlocker,
    /// DC blocker for right channel.
    dc_blocker_r: DcBlocker,

    // -- Stereo field --
    /// Stereo width (0.0 = mono, 1.0 = normal, 2.0 = wide). Smoothed.
    width: SmoothedParam,
    /// Stereo balance (-1.0 = full left, 0.0 = center, +1.0 = full right). Smoothed.
    balance: SmoothedParam,

    // -- Bass mono --
    /// Whether bass mono crossover is active.
    bass_mono_enabled: bool,
    /// Crossover frequency in Hz.
    bass_mono_freq: f32,
    /// LR4 lowpass biquads: [channel][stage].
    bass_lp: [[Biquad; 2]; 2],
    /// LR4 highpass biquads: [channel][stage].
    bass_hp: [[Biquad; 2]; 2],

    // -- Haas --
    /// Haas delay time in samples (smoothed for click-free changes).
    haas_delay: SmoothedParam,
    /// Delay line for Haas effect (max 30 ms).
    haas_delay_line: InterpolatedDelay,
    /// Which channel receives the Haas delay.
    haas_side: HaasSide,

    // --
    /// Current sample rate in Hz.
    sample_rate: f32,
}

impl Stage {
    /// Create a new Stage effect at the given sample rate.
    ///
    /// All parameters start at their defaults (unity gain, no width change,
    /// centered balance, no phase inversion, normal channel routing, DC block
    /// off, bass mono off, no Haas delay).
    pub fn new(sample_rate: f32) -> Self {
        let mut stage = Self {
            gain: SmoothedParam::standard(1.0, sample_rate),
            output_level: gain::output_level_param(sample_rate),
            phase_invert_l: false,
            phase_invert_r: false,
            channel_mode: ChannelMode::Normal,
            dc_block_enabled: false,
            dc_blocker_l: DcBlocker::new(sample_rate),
            dc_blocker_r: DcBlocker::new(sample_rate),
            width: SmoothedParam::standard(1.0, sample_rate),
            balance: SmoothedParam::standard(0.0, sample_rate),
            bass_mono_enabled: false,
            bass_mono_freq: 120.0,
            bass_lp: core::array::from_fn(|_| core::array::from_fn(|_| Biquad::new())),
            bass_hp: core::array::from_fn(|_| core::array::from_fn(|_| Biquad::new())),
            haas_delay: SmoothedParam::fast(0.0, sample_rate),
            haas_delay_line: InterpolatedDelay::from_time(sample_rate, MAX_HAAS_SECONDS),
            haas_side: HaasSide::Right,
            sample_rate,
        };
        stage.update_bass_crossover();
        stage
    }

    // -- Setters --

    /// Set input gain in dB.
    ///
    /// Range: -40.0 to +12.0 dB. Values are clamped.
    pub fn set_gain_db(&mut self, db: f32) {
        self.gain.set_target(db_to_linear(db.clamp(-40.0, 12.0)));
    }

    /// Get input gain target in dB.
    pub fn gain_db(&self) -> f32 {
        sonido_core::linear_to_db(self.gain.target())
    }

    /// Set stereo width as a percentage (0–200).
    ///
    /// 0% = mono, 100% = normal stereo, 200% = exaggerated width.
    /// Internally stored as 0.0–2.0.
    pub fn set_width(&mut self, percent: f32) {
        self.width.set_target(percent.clamp(0.0, 200.0) / 100.0);
    }

    /// Get stereo width target as percentage (0–200).
    pub fn width_percent(&self) -> f32 {
        self.width.target() * 100.0
    }

    /// Set stereo balance as a percentage (-100 to +100).
    ///
    /// -100 = full left, 0 = center, +100 = full right.
    /// Internally stored as -1.0 to +1.0.
    pub fn set_balance(&mut self, percent: f32) {
        self.balance
            .set_target(percent.clamp(-100.0, 100.0) / 100.0);
    }

    /// Get stereo balance target as percentage (-100 to +100).
    pub fn balance_percent(&self) -> f32 {
        self.balance.target() * 100.0
    }

    /// Set left channel phase inversion.
    pub fn set_phase_invert_l(&mut self, invert: bool) {
        self.phase_invert_l = invert;
    }

    /// Get left channel phase inversion state.
    pub fn phase_invert_l(&self) -> bool {
        self.phase_invert_l
    }

    /// Set right channel phase inversion.
    pub fn set_phase_invert_r(&mut self, invert: bool) {
        self.phase_invert_r = invert;
    }

    /// Get right channel phase inversion state.
    pub fn phase_invert_r(&self) -> bool {
        self.phase_invert_r
    }

    /// Set channel routing mode.
    pub fn set_channel_mode(&mut self, mode: ChannelMode) {
        self.channel_mode = mode;
    }

    /// Get channel routing mode.
    pub fn channel_mode(&self) -> ChannelMode {
        self.channel_mode
    }

    /// Enable or disable DC blocking.
    pub fn set_dc_block(&mut self, enabled: bool) {
        self.dc_block_enabled = enabled;
    }

    /// Get DC blocking state.
    pub fn dc_block_enabled(&self) -> bool {
        self.dc_block_enabled
    }

    /// Enable or disable bass mono crossover.
    pub fn set_bass_mono(&mut self, enabled: bool) {
        self.bass_mono_enabled = enabled;
    }

    /// Get bass mono state.
    pub fn bass_mono_enabled(&self) -> bool {
        self.bass_mono_enabled
    }

    /// Set bass mono crossover frequency in Hz.
    ///
    /// Range: 20.0 to 500.0 Hz. Values are clamped.
    pub fn set_bass_mono_freq(&mut self, freq_hz: f32) {
        let clamped = freq_hz.clamp(20.0, 500.0);
        if (self.bass_mono_freq - clamped).abs() > 0.01 {
            self.bass_mono_freq = clamped;
            self.update_bass_crossover();
        }
    }

    /// Get bass mono crossover frequency in Hz.
    pub fn bass_mono_freq(&self) -> f32 {
        self.bass_mono_freq
    }

    /// Set Haas delay time in milliseconds.
    ///
    /// Range: 0.0 to 30.0 ms. Values are clamped.
    pub fn set_haas_delay_ms(&mut self, ms: f32) {
        let samples = ms_to_samples(ms.clamp(0.0, 30.0), self.sample_rate);
        self.haas_delay.set_target(samples);
    }

    /// Get Haas delay time target in milliseconds.
    pub fn haas_delay_ms(&self) -> f32 {
        samples_to_ms(self.haas_delay.target(), self.sample_rate)
    }

    /// Set which channel receives the Haas delay.
    pub fn set_haas_side(&mut self, side: HaasSide) {
        self.haas_side = side;
    }

    /// Get which channel receives the Haas delay.
    pub fn haas_side(&self) -> HaasSide {
        self.haas_side
    }

    // -- Internal helpers --

    /// Recalculate all 8 LR4 crossover biquad coefficients at the current frequency.
    fn update_bass_crossover(&mut self) {
        let (lb0, lb1, lb2, la0, la1, la2) =
            lowpass_coefficients(self.bass_mono_freq, BUTTERWORTH_Q, self.sample_rate);
        let (hb0, hb1, hb2, ha0, ha1, ha2) =
            highpass_coefficients(self.bass_mono_freq, BUTTERWORTH_Q, self.sample_rate);

        for ch in 0..2 {
            for stage in 0..2 {
                self.bass_lp[ch][stage].set_coefficients(lb0, lb1, lb2, la0, la1, la2);
                self.bass_hp[ch][stage].set_coefficients(hb0, hb1, hb2, ha0, ha1, ha2);
            }
        }
    }

    /// Process a sample through a 2-stage cascaded biquad (LR4 half).
    #[inline]
    fn process_lr4(biquads: &mut [Biquad; 2], input: f32) -> f32 {
        let mid = biquads[0].process(input);
        biquads[1].process(mid)
    }
}

impl Default for Stage {
    fn default() -> Self {
        Self::new(48000.0)
    }
}

impl Effect for Stage {
    fn process(&mut self, input: f32) -> f32 {
        let (l, r) = self.process_stereo(input, input);
        (l + r) * 0.5
    }

    fn process_stereo(&mut self, left: f32, right: f32) -> (f32, f32) {
        // 1. Phase invert
        let mut l = if self.phase_invert_l { -left } else { left };
        let mut r = if self.phase_invert_r { -right } else { right };

        // 2. Channel mode
        match self.channel_mode {
            ChannelMode::Normal => {}
            ChannelMode::Swap => core::mem::swap(&mut l, &mut r),
            ChannelMode::MonoLeft => r = l,
            ChannelMode::MonoRight => l = r,
        }

        // 3. Input gain
        let g = self.gain.advance();
        l *= g;
        r *= g;

        // 4. DC block
        if self.dc_block_enabled {
            l = self.dc_blocker_l.process(l);
            r = self.dc_blocker_r.process(r);
        }

        // 5. M/S width
        let w = self.width.advance();
        let mid = (l + r) * 0.5;
        let side = (l - r) * 0.5 * w;
        l = mid + side;
        r = mid - side;

        // 6. Balance
        let bal = self.balance.advance();
        let gain_l = 1.0_f32.min(1.0 - bal);
        let gain_r = 1.0_f32.min(1.0 + bal);
        l *= gain_l;
        r *= gain_r;

        // 7. Bass mono
        if self.bass_mono_enabled {
            let low_l = Self::process_lr4(&mut self.bass_lp[0], l);
            let low_r = Self::process_lr4(&mut self.bass_lp[1], r);
            let high_l = Self::process_lr4(&mut self.bass_hp[0], l);
            let high_r = Self::process_lr4(&mut self.bass_hp[1], r);
            let mono_low = (low_l + low_r) * 0.5;
            l = mono_low + high_l;
            r = mono_low + high_r;
        }

        // 8. Haas delay
        let delay_samples = self.haas_delay.advance();
        if delay_samples > 0.01 {
            match self.haas_side {
                HaasSide::Left => {
                    let delayed = self.haas_delay_line.read(delay_samples);
                    self.haas_delay_line.write(l);
                    l = delayed;
                }
                HaasSide::Right => {
                    let delayed = self.haas_delay_line.read(delay_samples);
                    self.haas_delay_line.write(r);
                    r = delayed;
                }
            }
        } else {
            // Keep the delay line fed so it doesn't go stale
            match self.haas_side {
                HaasSide::Left => self.haas_delay_line.write(l),
                HaasSide::Right => self.haas_delay_line.write(r),
            }
        }

        // 9. Output level
        let out = self.output_level.advance();
        (l * out, r * out)
    }

    fn is_true_stereo(&self) -> bool {
        // True stereo when width, balance, or Haas alter the L/R relationship
        (self.width.target() - 1.0).abs() > f32::EPSILON
            || self.balance.target().abs() > f32::EPSILON
            || self.haas_delay.target() > 0.01
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.gain.set_sample_rate(sample_rate);
        self.output_level.set_sample_rate(sample_rate);
        self.width.set_sample_rate(sample_rate);
        self.balance.set_sample_rate(sample_rate);
        self.haas_delay.set_sample_rate(sample_rate);
        self.dc_blocker_l.set_sample_rate(sample_rate);
        self.dc_blocker_r.set_sample_rate(sample_rate);
        self.haas_delay_line = InterpolatedDelay::from_time(sample_rate, MAX_HAAS_SECONDS);
        self.update_bass_crossover();
    }

    fn reset(&mut self) {
        self.gain.snap_to_target();
        self.output_level.snap_to_target();
        self.width.snap_to_target();
        self.balance.snap_to_target();
        self.haas_delay.snap_to_target();
        self.dc_blocker_l.reset();
        self.dc_blocker_r.reset();
        self.haas_delay_line.clear();
        for ch in 0..2 {
            for stage in 0..2 {
                self.bass_lp[ch][stage].clear();
                self.bass_hp[ch][stage].clear();
            }
        }
    }

    fn latency_samples(&self) -> usize {
        0
    }
}

sonido_core::impl_params! {
    Stage, this {
        [0] ParamDescriptor::gain_db("Gain", "Gain", -40.0, 12.0, 0.0)
                .with_id(ParamId(1900), "stage_gain"),
            get: this.gain_db(),
            set: |v| this.set_gain_db(v);

        [1] ParamDescriptor::custom("Width", "Width", 0.0, 200.0, 100.0)
                .with_id(ParamId(1901), "stage_width")
                .with_unit(sonido_core::ParamUnit::Percent),
            get: this.width_percent(),
            set: |v| this.set_width(v);

        [2] ParamDescriptor::custom("Balance", "Bal", -100.0, 100.0, 0.0)
                .with_id(ParamId(1902), "stage_balance")
                .with_unit(sonido_core::ParamUnit::Percent),
            get: this.balance_percent(),
            set: |v| this.set_balance(v);

        [3] ParamDescriptor::custom("Phase L", "PhL", 0.0, 1.0, 0.0)
                .with_id(ParamId(1903), "stage_phase_l")
                .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED)),
            get: if this.phase_invert_l { 1.0 } else { 0.0 },
            set: |v| this.set_phase_invert_l(v >= 0.5);

        [4] ParamDescriptor::custom("Phase R", "PhR", 0.0, 1.0, 0.0)
                .with_id(ParamId(1904), "stage_phase_r")
                .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED)),
            get: if this.phase_invert_r { 1.0 } else { 0.0 },
            set: |v| this.set_phase_invert_r(v >= 0.5);

        [5] ParamDescriptor::custom("Channel", "Chan", 0.0, 3.0, 0.0)
                .with_id(ParamId(1905), "stage_channel")
                .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED)),
            get: this.channel_mode as u8 as f32,
            set: |v| this.set_channel_mode(ChannelMode::from_index(v));

        [6] ParamDescriptor::custom("DC Block", "DC", 0.0, 1.0, 0.0)
                .with_id(ParamId(1906), "stage_dc_block")
                .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED)),
            get: if this.dc_block_enabled { 1.0 } else { 0.0 },
            set: |v| this.set_dc_block(v >= 0.5);

        [7] ParamDescriptor::custom("Bass Mono", "BMon", 0.0, 1.0, 0.0)
                .with_id(ParamId(1907), "stage_bass_mono")
                .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED)),
            get: if this.bass_mono_enabled { 1.0 } else { 0.0 },
            set: |v| this.set_bass_mono(v >= 0.5);

        [8] ParamDescriptor::custom("Bass Freq", "BFrq", 20.0, 500.0, 120.0)
                .with_id(ParamId(1908), "stage_bass_freq")
                .with_unit(sonido_core::ParamUnit::Hertz)
                .with_scale(ParamScale::Logarithmic),
            get: this.bass_mono_freq(),
            set: |v| this.set_bass_mono_freq(v);

        [9] ParamDescriptor::custom("Haas", "Haas", 0.0, 30.0, 0.0)
                .with_id(ParamId(1909), "stage_haas")
                .with_unit(sonido_core::ParamUnit::Milliseconds),
            get: this.haas_delay_ms(),
            set: |v| this.set_haas_delay_ms(v);

        [10] ParamDescriptor::custom("Haas Side", "HSde", 0.0, 1.0, 1.0)
                .with_id(ParamId(1910), "stage_haas_side")
                .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED)),
            get: this.haas_side as u8 as f32,
            set: |v| this.set_haas_side(HaasSide::from_index(v));

        [11] gain::output_param_descriptor()
                .with_id(ParamId(1911), "stage_output"),
            get: gain::output_level_db(&this.output_level),
            set: |v| gain::set_output_level_db(&mut this.output_level, v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sonido_core::{Effect, ParameterInfo};

    #[cfg(not(feature = "std"))]
    extern crate alloc;

    #[test]
    fn param_count() {
        let stage = Stage::new(48000.0);
        assert_eq!(stage.param_count(), 12);
    }

    #[test]
    fn default_unity_gain() {
        let mut stage = Stage::new(48000.0);
        // Process enough samples to settle smoothing
        let mut l = 0.0;
        let mut r = 0.0;
        for _ in 0..1000 {
            (l, r) = stage.process_stereo(0.5, 0.5);
        }
        assert!((l - 0.5).abs() < 0.01, "Expected ~0.5, got {l}");
        assert!((r - 0.5).abs() < 0.01, "Expected ~0.5, got {r}");
    }

    #[test]
    fn gain_applies() {
        let mut stage = Stage::new(48000.0);
        stage.set_gain_db(6.0); // ~2x
        for _ in 0..1000 {
            stage.process_stereo(0.25, 0.25);
        }
        let (l, r) = stage.process_stereo(0.25, 0.25);
        // 0.25 * 2.0 = 0.5
        assert!((l - 0.5).abs() < 0.05, "Expected ~0.5, got {l}");
        assert!((r - 0.5).abs() < 0.05, "Expected ~0.5, got {r}");
    }

    #[test]
    fn phase_invert_l() {
        let mut stage = Stage::new(48000.0);
        stage.set_phase_invert_l(true);
        for _ in 0..2000 {
            stage.process_stereo(0.5, 0.5);
        }
        let (l, r) = stage.process_stereo(0.5, 0.5);
        // phase_invert happens BEFORE M/S, so with width=1.0 (identity),
        // the left should be inverted
        assert!(l < 0.0, "Left should be negative, got {l}");
        assert!(r > 0.0, "Right should be positive, got {r}");
    }

    #[test]
    fn phase_invert_r() {
        let mut stage = Stage::new(48000.0);
        stage.set_phase_invert_r(true);
        for _ in 0..2000 {
            stage.process_stereo(0.5, 0.5);
        }
        let (l, r) = stage.process_stereo(0.5, 0.5);
        assert!(l > 0.0, "Left should be positive, got {l}");
        assert!(r < 0.0, "Right should be negative, got {r}");
    }

    #[test]
    fn channel_swap() {
        let mut stage = Stage::new(48000.0);
        stage.set_channel_mode(ChannelMode::Swap);
        for _ in 0..2000 {
            stage.process_stereo(0.3, 0.7);
        }
        let (l, r) = stage.process_stereo(0.3, 0.7);
        assert!((l - 0.7).abs() < 0.05, "Expected ~0.7 (swapped), got {l}");
        assert!((r - 0.3).abs() < 0.05, "Expected ~0.3 (swapped), got {r}");
    }

    #[test]
    fn channel_mono_left() {
        let mut stage = Stage::new(48000.0);
        stage.set_channel_mode(ChannelMode::MonoLeft);
        for _ in 0..2000 {
            stage.process_stereo(0.3, 0.7);
        }
        let (l, r) = stage.process_stereo(0.3, 0.7);
        assert!((l - 0.3).abs() < 0.05, "Expected ~0.3, got {l}");
        assert!((r - 0.3).abs() < 0.05, "Expected ~0.3 (mono L), got {r}");
    }

    #[test]
    fn channel_mono_right() {
        let mut stage = Stage::new(48000.0);
        stage.set_channel_mode(ChannelMode::MonoRight);
        for _ in 0..2000 {
            stage.process_stereo(0.3, 0.7);
        }
        let (l, r) = stage.process_stereo(0.3, 0.7);
        assert!((l - 0.7).abs() < 0.05, "Expected ~0.7 (mono R), got {l}");
        assert!((r - 0.7).abs() < 0.05, "Expected ~0.7, got {r}");
    }

    #[test]
    fn width_mono() {
        let mut stage = Stage::new(48000.0);
        stage.set_width(0.0); // mono
        for _ in 0..2000 {
            stage.process_stereo(0.3, 0.7);
        }
        let (l, r) = stage.process_stereo(0.3, 0.7);
        // At width=0, mid = (0.3+0.7)/2 = 0.5, side=0 → both channels = 0.5
        assert!((l - 0.5).abs() < 0.05, "Expected ~0.5 (mono), got {l}");
        assert!((r - 0.5).abs() < 0.05, "Expected ~0.5 (mono), got {r}");
    }

    #[test]
    fn width_wide() {
        let mut stage = Stage::new(48000.0);
        stage.set_width(200.0); // exaggerated
        for _ in 0..2000 {
            stage.process_stereo(0.3, 0.7);
        }
        let (l, r) = stage.process_stereo(0.3, 0.7);
        // At width=2.0: mid=0.5, side=(0.3-0.7)/2*2=-0.4
        // l=0.5+(-0.4)=0.1, r=0.5-(-0.4)=0.9
        assert!((l - 0.1).abs() < 0.1, "Expected ~0.1 (wide), got {l}");
        assert!((r - 0.9).abs() < 0.1, "Expected ~0.9 (wide), got {r}");
    }

    #[test]
    fn balance_left() {
        let mut stage = Stage::new(48000.0);
        stage.set_balance(-100.0); // full left
        for _ in 0..2000 {
            stage.process_stereo(0.5, 0.5);
        }
        let (l, r) = stage.process_stereo(0.5, 0.5);
        assert!((l - 0.5).abs() < 0.05, "Left should be ~0.5, got {l}");
        assert!(r.abs() < 0.05, "Right should be ~0.0, got {r}");
    }

    #[test]
    fn balance_right() {
        let mut stage = Stage::new(48000.0);
        stage.set_balance(100.0); // full right
        for _ in 0..2000 {
            stage.process_stereo(0.5, 0.5);
        }
        let (l, r) = stage.process_stereo(0.5, 0.5);
        assert!(l.abs() < 0.05, "Left should be ~0.0, got {l}");
        assert!((r - 0.5).abs() < 0.05, "Right should be ~0.5, got {r}");
    }

    #[test]
    fn dc_block_removes_offset() {
        let mut stage = Stage::new(48000.0);
        stage.set_dc_block(true);
        // Feed constant DC for a long time
        for _ in 0..48000 {
            stage.process_stereo(1.0, 1.0);
        }
        let (l, r) = stage.process_stereo(1.0, 1.0);
        assert!(l.abs() < 0.05, "DC should be removed, got {l}");
        assert!(r.abs() < 0.05, "DC should be removed, got {r}");
    }

    #[test]
    fn bass_mono_sums_lows() {
        let mut stage = Stage::new(48000.0);
        stage.set_bass_mono(true);
        stage.set_bass_mono_freq(200.0);

        // Feed a very low frequency signal (50 Hz) with L/R difference
        let sr = 48000.0;
        let freq = 50.0;
        for i in 0..48000_u32 {
            let t = i as f32 / sr;
            let l = libm::sinf(2.0 * core::f32::consts::PI * freq * t);
            let r = -l; // opposite phase
            stage.process_stereo(l, r);
        }

        // After settling, low content should be mono-summed (cancelled in this case)
        let mut energy_l = 0.0_f32;
        let mut energy_r = 0.0_f32;
        for i in 0..4800_u32 {
            let t = (48000 + i) as f32 / sr;
            let l = libm::sinf(2.0 * core::f32::consts::PI * freq * t);
            let r = -l;
            let (ol, or) = stage.process_stereo(l, r);
            energy_l += ol * ol;
            energy_r += or * or;
        }
        // Bass content that was L/R opposite should be heavily reduced
        let rms_l = libm::sqrtf(energy_l / 4800.0);
        let rms_r = libm::sqrtf(energy_r / 4800.0);
        assert!(rms_l < 0.2, "Low-freq L should be reduced, rms={rms_l}");
        assert!(rms_r < 0.2, "Low-freq R should be reduced, rms={rms_r}");
    }

    #[test]
    fn haas_delay_creates_offset() {
        let mut stage = Stage::new(48000.0);
        stage.set_haas_delay_ms(10.0); // 10ms = 480 samples at 48kHz
        stage.set_haas_side(HaasSide::Right);

        // Feed an impulse
        let (l, r) = stage.process_stereo(1.0, 1.0);
        // Left should pass immediately, right should be delayed
        assert!((l - 1.0).abs() < 0.1, "Left should be immediate, got {l}");
        assert!(
            r.abs() < 0.1,
            "Right should be delayed (near zero), got {r}"
        );
    }

    #[test]
    fn output_level_applies() {
        let mut stage = Stage::new(48000.0);
        stage.set_param(11, -6.0); // -6 dB ≈ 0.5x
        for _ in 0..2000 {
            stage.process_stereo(0.5, 0.5);
        }
        let (l, _) = stage.process_stereo(0.5, 0.5);
        assert!(
            (l - 0.25).abs() < 0.05,
            "Expected ~0.25 (-6dB of 0.5), got {l}"
        );
    }

    #[test]
    fn reset_clears_state() {
        let mut stage = Stage::new(48000.0);
        stage.set_gain_db(6.0);
        stage.set_haas_delay_ms(10.0);
        for _ in 0..1000 {
            stage.process_stereo(0.5, 0.5);
        }
        stage.reset();
        // After reset, delay line should be cleared
        let (_, r) = stage.process_stereo(0.0, 0.0);
        assert!(
            r.abs() < 0.01,
            "Delay should be cleared after reset, got {r}"
        );
    }

    #[test]
    fn finite_output() {
        let mut stage = Stage::new(48000.0);
        // Turn on everything
        stage.set_dc_block(true);
        stage.set_bass_mono(true);
        stage.set_haas_delay_ms(15.0);
        stage.set_width(150.0);
        stage.set_balance(50.0);
        stage.set_phase_invert_l(true);

        let (l, r) = stage.process_stereo(1.0, 1.0);
        assert!(l.is_finite(), "Left must be finite");
        assert!(r.is_finite(), "Right must be finite");

        for _ in 0..2000 {
            let (l, r) = stage.process_stereo(0.0, 0.0);
            assert!(l.is_finite() && r.is_finite());
        }
    }

    #[test]
    fn param_roundtrip() {
        let mut stage = Stage::new(48000.0);

        // Gain
        stage.set_param(0, 6.0);
        assert!((stage.get_param(0) - 6.0).abs() < 0.1);

        // Width
        stage.set_param(1, 150.0);
        assert!((stage.get_param(1) - 150.0).abs() < 0.1);

        // Balance
        stage.set_param(2, -50.0);
        assert!((stage.get_param(2) - (-50.0)).abs() < 0.1);

        // Phase L
        stage.set_param(3, 1.0);
        assert!((stage.get_param(3) - 1.0).abs() < 0.01);

        // Channel mode
        stage.set_param(5, 2.0);
        assert!((stage.get_param(5) - 2.0).abs() < 0.01);
    }

    #[test]
    fn is_true_stereo_responds_to_params() {
        let mut stage = Stage::new(48000.0);
        assert!(!stage.is_true_stereo(), "Default should not be true stereo");

        stage.set_width(50.0);
        assert!(stage.is_true_stereo(), "Non-unity width → true stereo");

        stage.set_width(100.0);
        stage.set_balance(10.0);
        assert!(stage.is_true_stereo(), "Non-zero balance → true stereo");

        stage.set_balance(0.0);
        stage.set_haas_delay_ms(5.0);
        assert!(stage.is_true_stereo(), "Haas delay → true stereo");
    }
}
