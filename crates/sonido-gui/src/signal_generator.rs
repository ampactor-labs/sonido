//! Built-in signal generator for the Sonido GUI.
//!
//! Provides a variety of test and demonstration signals (sine, sweep, white
//! noise, pink noise, impulse train, and a naive-saw chord) that can replace
//! live microphone input as the default audio source. All generators produce
//! stereo output: most signals are identical on L/R, while white noise is
//! decorrelated across channels.
//!
//! The generator runs at the device sample rate and is driven by the audio
//! callback via [`SignalGenerator::generate`], which fills caller-provided
//! left/right buffers.

use std::f64::consts::TAU;

// ---------------------------------------------------------------------------
// SignalType
// ---------------------------------------------------------------------------

/// Available signal types for the built-in generator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalType {
    /// Pure sine wave at the configured frequency.
    Sine,
    /// Logarithmic frequency sweep from start to end Hz.
    Sweep,
    /// Decorrelated white noise (uniform spectral density).
    WhiteNoise,
    /// Pink noise via the Voss-McCartney algorithm (approximately -3 dB/octave).
    PinkNoise,
    /// Impulse train at a configurable rate.
    Impulse,
    /// Naive-sawtooth chord: root + major 3rd (5:4) + perfect 5th (3:2).
    SawChord,
}

impl SignalType {
    /// All variants in display order.
    pub const ALL: [SignalType; 6] = [
        SignalType::Sine,
        SignalType::Sweep,
        SignalType::WhiteNoise,
        SignalType::PinkNoise,
        SignalType::Impulse,
        SignalType::SawChord,
    ];

    /// Human-readable label for UI display.
    pub fn label(self) -> &'static str {
        match self {
            SignalType::Sine => "Sine",
            SignalType::Sweep => "Sweep",
            SignalType::WhiteNoise => "White Noise",
            SignalType::PinkNoise => "Pink Noise",
            SignalType::Impulse => "Impulse",
            SignalType::SawChord => "Saw Chord",
        }
    }
}

// ---------------------------------------------------------------------------
// SourceMode
// ---------------------------------------------------------------------------

/// Selects between the built-in generator and file playback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceMode {
    /// Use the built-in signal generator.
    Generator,
    /// Play back an audio file.
    File,
}

// ---------------------------------------------------------------------------
// SignalGenerator
// ---------------------------------------------------------------------------

/// Stereo signal generator with multiple waveform types.
///
/// ## Parameters
/// - `signal_type`: active waveform (default [`SignalType::Sine`])
/// - `frequency`: primary frequency in Hz (20.0 to 20000.0, default 440.0)
/// - `amplitude`: output level 0.0 to 1.0 (default 0.5, i.e. -6 dB)
/// - `playing`: whether the generator produces output (default `false`)
///
/// Sweep, impulse, pink-noise, and saw-chord parameters are configured
/// through dedicated setters.
pub struct SignalGenerator {
    /// Current signal type.
    signal_type: SignalType,
    /// Continuous phase accumulator in the range 0.0 to 1.0.
    phase: f64,
    /// Primary frequency in Hz.
    frequency: f32,
    /// Output amplitude in 0.0 to 1.0.
    amplitude: f32,
    /// Device sample rate in Hz.
    sample_rate: f32,
    /// Whether the generator is actively producing output.
    playing: bool,

    // -- Sweep state --
    /// Sweep start frequency in Hz.
    sweep_start_hz: f32,
    /// Sweep end frequency in Hz.
    sweep_end_hz: f32,
    /// Sweep duration in seconds.
    sweep_duration_secs: f32,
    /// Current sweep position (0.0 to 1.0).
    sweep_position: f64,
    /// Whether the sweep loops when it reaches the end.
    sweep_loop: bool,

    // -- Impulse state --
    /// Impulse repetition rate in Hz.
    impulse_rate_hz: f32,
    /// Sample counter within the current impulse period.
    impulse_counter: usize,
    /// Number of samples between impulses.
    impulse_interval: usize,

    // -- Pink noise (Voss-McCartney) --
    /// 16 independent random rows for the Voss-McCartney algorithm.
    pink_rows: [f32; 16],
    /// Running sum of all pink rows.
    pink_running_sum: f32,
    /// Row update index (bits select which row to refresh).
    pink_index: u32,

    // -- Noise PRNG --
    /// Xorshift64 state (must never be zero).
    noise_state: u64,

    // -- Saw chord --
    /// Phase accumulators for root, major 3rd, and perfect 5th.
    saw_phases: [f64; 3],
}

impl SignalGenerator {
    /// Creates a new signal generator configured for the given sample rate.
    ///
    /// Defaults: sine at 440 Hz, amplitude 0.5 (-6 dB), paused.
    pub fn new(sample_rate: f32) -> Self {
        let impulse_rate_hz: f32 = 2.0;
        let impulse_interval = (sample_rate / impulse_rate_hz) as usize;

        Self {
            signal_type: SignalType::Sine,
            phase: 0.0,
            frequency: 440.0,
            amplitude: 0.5,
            sample_rate,
            playing: false,

            sweep_start_hz: 20.0,
            sweep_end_hz: 20_000.0,
            sweep_duration_secs: 2.0,
            sweep_position: 0.0,
            sweep_loop: true,

            impulse_rate_hz,
            impulse_counter: 0,
            impulse_interval,

            pink_rows: [0.0; 16],
            pink_running_sum: 0.0,
            pink_index: 0,

            noise_state: 0xDEAD_BEEF_CAFE_BABE,

            saw_phases: [0.0; 3],
        }
    }

    // -- Accessors --------------------------------------------------------

    /// Returns `true` if the generator is actively producing output.
    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// Starts or stops the generator.
    pub fn set_playing(&mut self, playing: bool) {
        self.playing = playing;
    }

    /// Stops the generator and resets all phase accumulators to their initial
    /// state.
    pub fn stop(&mut self) {
        self.playing = false;
        self.phase = 0.0;
        self.sweep_position = 0.0;
        self.impulse_counter = 0;
        self.pink_rows = [0.0; 16];
        self.pink_running_sum = 0.0;
        self.pink_index = 0;
        self.saw_phases = [0.0; 3];
    }

    /// Returns the active signal type.
    pub fn signal_type(&self) -> SignalType {
        self.signal_type
    }

    /// Sets the signal type and resets phase state so the new waveform starts
    /// cleanly.
    pub fn set_signal_type(&mut self, signal_type: SignalType) {
        self.signal_type = signal_type;
        // Reset all phase / position state for a clean start.
        self.phase = 0.0;
        self.sweep_position = 0.0;
        self.impulse_counter = 0;
        self.pink_rows = [0.0; 16];
        self.pink_running_sum = 0.0;
        self.pink_index = 0;
        self.saw_phases = [0.0; 3];
    }

    /// Returns the primary frequency in Hz.
    pub fn frequency(&self) -> f32 {
        self.frequency
    }

    /// Sets the primary frequency.
    ///
    /// Range: 20.0 to 20000.0 Hz. Values are clamped to this range.
    pub fn set_frequency(&mut self, hz: f32) {
        self.frequency = hz.clamp(20.0, 20_000.0);
    }

    /// Returns the output amplitude (0.0 to 1.0).
    pub fn amplitude(&self) -> f32 {
        self.amplitude
    }

    /// Sets the output amplitude.
    ///
    /// Range: 0.0 to 1.0. Values are clamped.
    pub fn set_amplitude(&mut self, amp: f32) {
        self.amplitude = amp.clamp(0.0, 1.0);
    }

    /// Configures the frequency sweep parameters.
    ///
    /// - `start_hz`: sweep start frequency in Hz
    /// - `end_hz`: sweep end frequency in Hz
    /// - `duration_secs`: sweep duration in seconds
    /// - `looping`: whether the sweep loops when complete
    pub fn set_sweep_params(
        &mut self,
        start_hz: f32,
        end_hz: f32,
        duration_secs: f32,
        looping: bool,
    ) {
        self.sweep_start_hz = start_hz.max(1.0);
        self.sweep_end_hz = end_hz.max(1.0);
        self.sweep_duration_secs = duration_secs.max(0.01);
        self.sweep_loop = looping;
    }

    /// Sets the impulse train repetition rate.
    ///
    /// Range: 0.5 to 20.0 Hz. Values are clamped. The interval in samples
    /// is recalculated from the current sample rate.
    pub fn set_impulse_rate(&mut self, hz: f32) {
        self.impulse_rate_hz = hz.clamp(0.5, 20.0);
        self.impulse_interval = (self.sample_rate / self.impulse_rate_hz) as usize;
    }

    /// Updates the sample rate and recalculates rate-dependent state
    /// (impulse interval).
    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.impulse_interval = (self.sample_rate / self.impulse_rate_hz) as usize;
    }

    // -- Main entry point -------------------------------------------------

    /// Fills the provided stereo buffers with generated audio.
    ///
    /// When the generator is paused (`playing == false`) the buffers are
    /// filled with silence. The two slices must have the same length.
    pub fn generate(&mut self, left: &mut [f32], right: &mut [f32]) {
        debug_assert_eq!(left.len(), right.len());

        if !self.playing {
            left.fill(0.0);
            right.fill(0.0);
            return;
        }

        match self.signal_type {
            SignalType::Sine => self.gen_sine(left, right),
            SignalType::Sweep => self.gen_sweep(left, right),
            SignalType::WhiteNoise => self.gen_white_noise(left, right),
            SignalType::PinkNoise => self.gen_pink_noise(left, right),
            SignalType::Impulse => self.gen_impulse(left, right),
            SignalType::SawChord => self.gen_saw_chord(left, right),
        }
    }

    // -- Private generators -----------------------------------------------

    /// Pure sine wave at `self.frequency`.
    fn gen_sine(&mut self, left: &mut [f32], right: &mut [f32]) {
        let phase_inc = self.frequency as f64 / self.sample_rate as f64;
        let amp = self.amplitude;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let sample = (self.phase * TAU).sin() as f32 * amp;
            *l = sample;
            *r = sample;
            self.phase += phase_inc;
            if self.phase >= 1.0 {
                self.phase -= 1.0;
            }
        }
    }

    /// Logarithmic frequency sweep from `sweep_start_hz` to `sweep_end_hz`.
    fn gen_sweep(&mut self, left: &mut [f32], right: &mut [f32]) {
        let dur_samples = (self.sweep_duration_secs as f64) * (self.sample_rate as f64);
        let pos_inc = 1.0 / dur_samples;
        let log_start = (self.sweep_start_hz as f64).ln();
        let log_end = (self.sweep_end_hz as f64).ln();
        let amp = self.amplitude;

        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            // Current instantaneous frequency via log interpolation.
            let freq = (log_start + (log_end - log_start) * self.sweep_position).exp();
            let phase_inc = freq / self.sample_rate as f64;

            let sample = (self.phase * TAU).sin() as f32 * amp;
            *l = sample;
            *r = sample;

            self.phase += phase_inc;
            if self.phase >= 1.0 {
                self.phase -= 1.0;
            }

            self.sweep_position += pos_inc;
            if self.sweep_position >= 1.0 {
                if self.sweep_loop {
                    self.sweep_position -= 1.0;
                } else {
                    self.sweep_position = 1.0;
                }
            }
        }
    }

    /// Decorrelated white noise (independent L/R via xorshift64).
    fn gen_white_noise(&mut self, left: &mut [f32], right: &mut [f32]) {
        let amp = self.amplitude;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            *l = self.next_noise() as f32 * amp;
            *r = self.next_noise() as f32 * amp;
        }
    }

    /// Pink noise via the Voss-McCartney 16-row algorithm.
    ///
    /// The Voss-McCartney method maintains 16 independent white-noise rows
    /// that are refreshed at geometrically spaced intervals (controlled by
    /// the trailing-zero count of an incrementing counter). The running sum
    /// of all rows produces an approximate -3 dB/octave spectrum.
    ///
    /// Output is mono (L = R).
    fn gen_pink_noise(&mut self, left: &mut [f32], right: &mut [f32]) {
        let amp = self.amplitude;
        // Scale factor: 16 rows each in -1..1 could sum to +-16.
        // Normalise so that the typical RMS is close to unity before amp
        // scaling.  Empirically ~1/6 keeps peaks under control.
        let scale = amp / 6.0;

        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            // Determine which row to update based on trailing zeros of
            // the index counter.
            let tz = self.pink_index.trailing_zeros().min(15) as usize;
            let old = self.pink_rows[tz];
            let new_val = self.next_noise() as f32;
            self.pink_rows[tz] = new_val;
            self.pink_running_sum += new_val - old;
            self.pink_index = self.pink_index.wrapping_add(1);

            let sample = self.pink_running_sum * scale;
            *l = sample;
            *r = sample;
        }
    }

    /// Impulse train: outputs 1.0 at the impulse interval and 0.0 elsewhere.
    fn gen_impulse(&mut self, left: &mut [f32], right: &mut [f32]) {
        let amp = self.amplitude;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let sample = if self.impulse_counter == 0 {
                amp
            } else {
                0.0
            };
            *l = sample;
            *r = sample;
            self.impulse_counter += 1;
            if self.impulse_counter >= self.impulse_interval {
                self.impulse_counter = 0;
            }
        }
    }

    /// Naive sawtooth chord: root + major 3rd (5:4 ratio) + perfect 5th
    /// (3:2 ratio), mixed at equal levels.
    ///
    /// Each voice is a simple `2 * phase - 1` sawtooth (no anti-aliasing).
    fn gen_saw_chord(&mut self, left: &mut [f32], right: &mut [f32]) {
        let root_freq = self.frequency as f64;
        let freqs = [root_freq, root_freq * 5.0 / 4.0, root_freq * 3.0 / 2.0];
        let inv_sr = 1.0 / self.sample_rate as f64;
        let amp = self.amplitude / 3.0; // equal voice mix
        let phase_incs: [f64; 3] = [
            freqs[0] * inv_sr,
            freqs[1] * inv_sr,
            freqs[2] * inv_sr,
        ];

        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let mut sum: f64 = 0.0;
            for i in 0..3 {
                sum += 2.0 * self.saw_phases[i] - 1.0;
                self.saw_phases[i] += phase_incs[i];
                if self.saw_phases[i] >= 1.0 {
                    self.saw_phases[i] -= 1.0;
                }
            }
            let sample = sum as f32 * amp;
            *l = sample;
            *r = sample;
        }
    }

    /// Xorshift64 PRNG returning a value in the range -1.0 to +1.0.
    fn next_noise(&mut self) -> f64 {
        let mut x = self.noise_state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.noise_state = x;
        // Map u64 to -1.0..+1.0
        (x as f64) / (u64::MAX as f64) * 2.0 - 1.0
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48_000.0;

    #[test]
    fn sine_output_bounded() {
        let mut sg = SignalGenerator::new(SR);
        sg.set_playing(true);
        let mut left = vec![0.0_f32; 4800];
        let mut right = vec![0.0_f32; 4800];
        sg.generate(&mut left, &mut right);

        let bound = sg.amplitude() + 1e-6;
        for (i, (&l, &r)) in left.iter().zip(right.iter()).enumerate() {
            assert!(
                l.abs() <= bound && r.abs() <= bound,
                "sample {i}: l={l}, r={r}, bound={bound}"
            );
        }
    }

    #[test]
    fn silence_when_paused() {
        let mut sg = SignalGenerator::new(SR);
        // Default is paused.
        let mut left = vec![1.0_f32; 256];
        let mut right = vec![1.0_f32; 256];
        sg.generate(&mut left, &mut right);

        assert!(left.iter().all(|&s| s == 0.0));
        assert!(right.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn white_noise_has_nonzero_variance() {
        let mut sg = SignalGenerator::new(SR);
        sg.set_signal_type(SignalType::WhiteNoise);
        sg.set_playing(true);
        let mut left = vec![0.0_f32; 4800];
        let mut right = vec![0.0_f32; 4800];
        sg.generate(&mut left, &mut right);

        let mean: f32 = left.iter().sum::<f32>() / left.len() as f32;
        let variance: f32 =
            left.iter().map(|&s| (s - mean).powi(2)).sum::<f32>() / left.len() as f32;
        assert!(
            variance > 0.01,
            "white noise variance too low: {variance}"
        );
    }

    #[test]
    fn impulse_has_correct_spacing() {
        let mut sg = SignalGenerator::new(SR);
        sg.set_signal_type(SignalType::Impulse);
        sg.set_impulse_rate(10.0);
        sg.set_playing(true);

        // 48000 Hz / 10 Hz = 4800 samples between impulses.
        let n = 9600;
        let mut left = vec![0.0_f32; n];
        let mut right = vec![0.0_f32; n];
        sg.generate(&mut left, &mut right);

        let impulse_positions: Vec<usize> = left
            .iter()
            .enumerate()
            .filter(|&(_, &s)| s > 0.0)
            .map(|(i, _)| i)
            .collect();

        assert!(
            impulse_positions.contains(&0),
            "first impulse should be at sample 0, got {impulse_positions:?}"
        );
        assert!(
            impulse_positions.contains(&4800),
            "second impulse should be at sample 4800, got {impulse_positions:?}"
        );
    }

    #[test]
    fn sweep_loops() {
        let mut sg = SignalGenerator::new(SR);
        sg.set_signal_type(SignalType::Sweep);
        sg.set_sweep_params(100.0, 10_000.0, 1.0, true);
        sg.set_playing(true);

        // Generate 2 seconds (should loop once).
        let n = SR as usize * 2;
        let mut left = vec![0.0_f32; n];
        let mut right = vec![0.0_f32; n];
        sg.generate(&mut left, &mut right);

        let energy: f32 = left.iter().map(|s| s * s).sum();
        assert!(energy > 0.0, "sweep should produce nonzero energy");
    }

    #[test]
    fn saw_chord_bounded() {
        let mut sg = SignalGenerator::new(SR);
        sg.set_signal_type(SignalType::SawChord);
        sg.set_playing(true);
        let mut left = vec![0.0_f32; 4800];
        let mut right = vec![0.0_f32; 4800];
        sg.generate(&mut left, &mut right);

        let bound = sg.amplitude() + 1e-6;
        for (i, (&l, &r)) in left.iter().zip(right.iter()).enumerate() {
            assert!(
                l.abs() <= bound && r.abs() <= bound,
                "sample {i}: l={l}, r={r}, bound={bound}"
            );
        }
    }

    #[test]
    fn stop_resets_phase() {
        let mut sg = SignalGenerator::new(SR);
        sg.set_playing(true);
        let mut left = vec![0.0_f32; 1000];
        let mut right = vec![0.0_f32; 1000];
        sg.generate(&mut left, &mut right);

        // Phase should have advanced.
        assert!(sg.phase > 0.0);

        sg.stop();
        assert!(!sg.is_playing());
        assert_eq!(sg.phase, 0.0);
    }

    #[test]
    fn pink_noise_bounded() {
        let mut sg = SignalGenerator::new(SR);
        sg.set_signal_type(SignalType::PinkNoise);
        sg.set_playing(true);
        let mut left = vec![0.0_f32; 48_000];
        let mut right = vec![0.0_f32; 48_000];
        sg.generate(&mut left, &mut right);

        for (i, (&l, &r)) in left.iter().zip(right.iter()).enumerate() {
            assert!(l.is_finite(), "sample {i} left is not finite: {l}");
            assert!(r.is_finite(), "sample {i} right is not finite: {r}");
            assert!(
                l.abs() < 2.0,
                "sample {i} left out of range: {l}"
            );
            assert!(
                r.abs() < 2.0,
                "sample {i} right out of range: {r}"
            );
        }
    }
}
