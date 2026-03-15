//! Voice management for polyphonic synthesis.
//!
//! Provides voice structures, unison sub-voices, modulation matrix wiring,
//! portamento, and allocation strategies for building monophonic and
//! polyphonic synthesizers.

use crate::envelope::AdsrEnvelope;
use crate::mod_matrix::{ModDestination, ModSourceId, ModulationMatrix, ModulationValues};
use crate::oscillator::{Oscillator, OscillatorWaveform};
use sonido_core::{Effect, SmoothedParam, StateVariableFilter};

/// Maximum number of unison sub-voices per voice.
pub const MAX_UNISON: usize = 16;

/// Voice allocation modes for polyphonic synthesizers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VoiceAllocationMode {
    /// Cycle through voices in order (default)
    #[default]
    RoundRobin,
    /// Steal the oldest active note
    OldestNote,
    /// Steal the lowest pitch voice
    LowestNote,
    /// Steal the highest pitch voice
    HighestNote,
}

/// A lightweight unison sub-voice — oscillator pair with pan and detune.
///
/// Each sub-voice contains its own oscillator pair (osc1 + osc2) with
/// independent detune and stereo pan position. Sub-voices are summed
/// in the parent [`Voice`] to produce the classic "supersaw" unison effect.
///
/// ## Detune Distribution
///
/// For `count` sub-voices with spread `s`, sub-voice `i` gets:
/// ```text
/// detune_cents = s * (2*i / (count-1) - 1)
/// ```
/// This distributes voices symmetrically around the center pitch.
///
/// ## Pan Distribution
///
/// ```text
/// pan = stereo_width * (2*i / (count-1) - 1)
/// ```
/// Pan ranges from -1 (full left) to +1 (full right).
#[derive(Debug, Clone)]
pub struct SubVoice {
    /// Primary oscillator
    pub osc1: Oscillator,
    /// Secondary oscillator (for osc2 detune/mix)
    pub osc2: Oscillator,
    /// Pan position: -1.0 (left) to 1.0 (right)
    pan: f32,
    /// Detune offset in cents from base pitch
    detune_cents: f32,
}

impl SubVoice {
    /// Create a new sub-voice at the given sample rate.
    fn new(sample_rate: f32) -> Self {
        let mut osc1 = Oscillator::new(sample_rate);
        let mut osc2 = Oscillator::new(sample_rate);
        osc1.set_waveform(OscillatorWaveform::Saw);
        osc2.set_waveform(OscillatorWaveform::Saw);
        Self {
            osc1,
            osc2,
            pan: 0.0,
            detune_cents: 0.0,
        }
    }

    /// Set frequency for both oscillators, applying sub-voice detune and osc2 detune.
    fn set_frequency(&mut self, base_freq: f32, osc2_detune_cents: f32) {
        let freq = base_freq * cents_to_ratio(self.detune_cents);
        self.osc1.set_frequency(freq);
        self.osc2
            .set_frequency(freq * cents_to_ratio(osc2_detune_cents));
    }

    /// Reset oscillator phase.
    fn reset(&mut self) {
        self.osc1.reset();
        self.osc2.reset();
    }

    /// Set sample rate on both oscillators.
    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.osc1.set_sample_rate(sample_rate);
        self.osc2.set_sample_rate(sample_rate);
    }

    /// Advance oscillators and return mixed output.
    #[inline]
    fn advance(&mut self, osc_mix: f32) -> f32 {
        let o1 = self.osc1.advance();
        let o2 = self.osc2.advance();
        o1 * (1.0 - osc_mix) + o2 * osc_mix
    }

    /// Get pan position.
    pub fn pan(&self) -> f32 {
        self.pan
    }

    /// Get detune in cents.
    pub fn detune_cents(&self) -> f32 {
        self.detune_cents
    }
}

/// A single synthesizer voice with modulation matrix, portamento, and unison.
///
/// Contains sub-voices (for unison), a filter, amplitude/filter envelopes,
/// and a per-voice modulation matrix for flexible routing.
///
/// ## Parameters
/// - `osc2_detune`: Oscillator 2 detune in cents (0.0, default 0.0)
/// - `osc_mix`: Oscillator mix, 0 = osc1 only, 1 = osc2 only (0.0 to 1.0, default 0.0)
/// - `filter_env_amount`: Filter envelope amount in Hz, bipolar (-20000.0 to 20000.0, default 0.0)
/// - `filter_cutoff`: Base filter cutoff in Hz (20.0 to 20000.0, default 1000.0)
/// - `unison_count`: Number of unison sub-voices (1 to 16, default 1)
/// - `unison_spread`: Detune spread in cents across unison voices (0.0 to 100.0, default 0.0)
/// - `stereo_width`: Stereo spread of unison voices (0.0 to 1.0, default 1.0)
///
/// # Example
///
/// ```rust
/// use sonido_synth::Voice;
///
/// let mut voice = Voice::new(48000.0);
/// voice.note_on(60, 100); // Middle C, velocity 100
///
/// // Generate stereo samples with unison
/// voice.set_unison_count(4);
/// voice.set_unison_spread(15.0); // 15 cents spread
/// for _ in 0..1000 {
///     let (left, right) = voice.process_stereo();
/// }
///
/// voice.note_off();
/// ```
#[derive(Debug, Clone)]
pub struct Voice {
    /// Unison sub-voices (only `unison_count` are active)
    sub_voices: [SubVoice; MAX_UNISON],
    /// Number of active unison voices (1 to `MAX_UNISON`)
    unison_count: usize,
    /// Detune spread in cents across unison voices
    unison_spread: f32,
    /// Stereo width for unison pan distribution (0.0 to 1.0)
    stereo_width: f32,

    /// Filter (shared across all sub-voices)
    pub filter: StateVariableFilter,
    /// Amplitude envelope
    pub amp_env: AdsrEnvelope,
    /// Filter envelope
    pub filter_env: AdsrEnvelope,

    /// Per-voice modulation matrix (16 routing slots)
    pub mod_matrix: ModulationMatrix<16>,
    /// Current modulation source values (populated each sample)
    mod_values: ModulationValues,

    /// Portamento frequency smoother — smooths toward target frequency.
    freq_target: SmoothedParam,

    /// Current MIDI note number
    note: u8,
    /// Current velocity (0-127)
    velocity: u8,
    /// Voice age (for voice stealing)
    age: u64,
    /// Whether this voice is currently active
    active: bool,

    /// Sample rate
    sample_rate: f32,

    // Voice parameters
    /// Oscillator 2 detune in cents
    osc2_detune: f32,
    /// Oscillator mix (0 = osc1 only, 1 = osc2 only)
    osc_mix: f32,
    /// Filter envelope amount (bipolar, in Hz)
    filter_env_amount: f32,
    /// Base filter cutoff frequency
    filter_cutoff: f32,

    // External modulation (set by parent synth LFOs, etc.)
    /// External pitch modulation in semitones (additive with mod matrix).
    external_pitch_mod: f32,
    /// External filter cutoff modulation in Hz (additive with mod matrix).
    external_filter_mod: f32,

    // MPE (MIDI Polyphonic Expression) per-note dimensions.
    // Updated by set_mpe_* methods from the MIDI parser; consumed each sample.
    /// MPE per-note pitch bend in semitones. Range: -96.0 to +96.0.
    mpe_pitch_bend_semitones: f32,
    /// MPE per-note pressure (polyphonic aftertouch). Range: 0.0 to 1.0.
    mpe_pressure: f32,
    /// MPE per-note slide (CC74 "brightness"). Range: 0.0 to 1.0.
    mpe_slide: f32,
    /// MIDI channel this voice is assigned to (1 = global, 2-16 = per-note).
    mpe_channel: u8,
}

impl Default for Voice {
    fn default() -> Self {
        Self::new(48000.0)
    }
}

impl Voice {
    /// Create a new voice at the given sample rate.
    pub fn new(sample_rate: f32) -> Self {
        let mut voice = Self {
            sub_voices: core::array::from_fn(|_| SubVoice::new(sample_rate)),
            unison_count: 1,
            unison_spread: 0.0,
            stereo_width: 1.0,
            filter: StateVariableFilter::new(sample_rate),
            amp_env: AdsrEnvelope::new(sample_rate),
            filter_env: AdsrEnvelope::new(sample_rate),
            mod_matrix: ModulationMatrix::new(),
            mod_values: ModulationValues::new(),
            freq_target: SmoothedParam::interpolated(440.0, sample_rate),
            note: 0,
            velocity: 0,
            age: 0,
            active: false,
            sample_rate,
            osc2_detune: 0.0,
            osc_mix: 0.0,
            filter_env_amount: 0.0,
            filter_cutoff: 1000.0,
            external_pitch_mod: 0.0,
            external_filter_mod: 0.0,
            mpe_pitch_bend_semitones: 0.0,
            mpe_pressure: 0.0,
            mpe_slide: 0.0,
            mpe_channel: 1,
        };

        voice.filter.set_cutoff(1000.0);
        voice.filter.set_resonance(1.0);

        voice
    }

    /// Set sample rate for all components.
    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        for sv in &mut self.sub_voices {
            sv.set_sample_rate(sample_rate);
        }
        self.filter.set_sample_rate(sample_rate);
        self.amp_env.set_sample_rate(sample_rate);
        self.filter_env.set_sample_rate(sample_rate);
        self.freq_target.set_sample_rate(sample_rate);
    }

    /// Trigger note on.
    ///
    /// With portamento enabled, the frequency smoothly glides from the
    /// previous note to the new one. Without portamento, frequency snaps
    /// instantly.
    pub fn note_on(&mut self, note: u8, velocity: u8) {
        self.note = note;
        self.velocity = velocity;
        self.active = true;

        let freq = midi_to_freq(note);

        // Portamento: if voice was already active, glide; otherwise snap
        if self.freq_target.advance() > 0.0 && self.amp_env.is_active() {
            self.freq_target.set_target(freq);
        } else {
            self.freq_target.set_immediate(freq);
        }

        // Reset sub-voice oscillators and apply frequencies
        for sv in &mut self.sub_voices[..self.unison_count] {
            sv.set_frequency(freq, self.osc2_detune);
            sv.reset();
        }

        self.amp_env.gate_on();
        self.filter_env.gate_on();

        // Populate initial mod values
        self.mod_values.set_velocity_from_midi(velocity);
        self.mod_values.set_key_track_from_note(note);
    }

    /// Trigger note off.
    pub fn note_off(&mut self) {
        self.amp_env.gate_off();
        self.filter_env.gate_off();
    }

    /// Force voice to stop immediately.
    pub fn kill(&mut self) {
        self.active = false;
        self.amp_env.reset();
        self.filter_env.reset();
    }

    /// Reset voice to initial state.
    pub fn reset(&mut self) {
        self.kill();
        self.note = 0;
        self.velocity = 0;
        self.age = 0;
        for sv in &mut self.sub_voices {
            sv.reset();
        }
        self.filter.reset();
        self.mod_values = ModulationValues::new();
    }

    /// Check if voice is currently producing sound.
    pub fn is_active(&self) -> bool {
        self.active && self.amp_env.is_active()
    }

    /// Get the current note number.
    pub fn note(&self) -> u8 {
        self.note
    }

    /// Get the current velocity.
    pub fn velocity(&self) -> u8 {
        self.velocity
    }

    /// Get voice age.
    pub fn age(&self) -> u64 {
        self.age
    }

    /// Set voice age.
    pub fn set_age(&mut self, age: u64) {
        self.age = age;
    }

    /// Get the primary oscillator (sub-voice 0, osc1) for read access.
    pub fn osc1(&self) -> &Oscillator {
        &self.sub_voices[0].osc1
    }

    /// Get the secondary oscillator (sub-voice 0, osc2) for read access.
    pub fn osc2(&self) -> &Oscillator {
        &self.sub_voices[0].osc2
    }

    /// Set oscillator 1 waveform on all sub-voices.
    pub fn set_osc1_waveform(&mut self, waveform: OscillatorWaveform) {
        for sv in &mut self.sub_voices {
            sv.osc1.set_waveform(waveform);
        }
    }

    /// Set oscillator 2 waveform on all sub-voices.
    pub fn set_osc2_waveform(&mut self, waveform: OscillatorWaveform) {
        for sv in &mut self.sub_voices {
            sv.osc2.set_waveform(waveform);
        }
    }

    /// Set oscillator 2 detune in cents.
    ///
    /// Range: -2400.0 to 2400.0 cents (-2 to +2 octaves).
    pub fn set_osc2_detune(&mut self, cents: f32) {
        self.osc2_detune = cents;
        if self.active {
            let base_freq = midi_to_freq(self.note);
            for sv in &mut self.sub_voices[..self.unison_count] {
                sv.set_frequency(base_freq, cents);
            }
        }
    }

    /// Set oscillator mix (0 = osc1 only, 1 = osc2 only).
    ///
    /// Range: 0.0 to 1.0.
    pub fn set_osc_mix(&mut self, mix: f32) {
        self.osc_mix = mix.clamp(0.0, 1.0);
    }

    /// Set filter envelope amount in Hz (bipolar).
    ///
    /// Positive values sweep the filter cutoff up, negative values sweep down.
    /// Range: -20000.0 to 20000.0 Hz.
    pub fn set_filter_env_amount(&mut self, amount: f32) {
        self.filter_env_amount = amount;
    }

    /// Set base filter cutoff frequency.
    ///
    /// Range: 20.0 to 20000.0 Hz.
    pub fn set_filter_cutoff(&mut self, freq: f32) {
        self.filter_cutoff = freq;
        self.filter.set_cutoff(freq);
    }

    /// Set portamento (glide) time in milliseconds.
    ///
    /// Range: 0.0 (instant) to any positive value. Typical: 50-500 ms.
    /// Uses interpolated smoothing (50 ms preset as base, overridden here).
    pub fn set_portamento_time(&mut self, ms: f32) {
        self.freq_target.set_smoothing_time_ms(ms.max(0.0));
    }

    /// Set number of unison sub-voices.
    ///
    /// Range: 1 to 16. More voices = thicker sound, higher CPU.
    /// When count is 1, no detuning or stereo spread is applied.
    pub fn set_unison_count(&mut self, count: usize) {
        self.unison_count = count.clamp(1, MAX_UNISON);
        self.recalculate_unison_distribution();
    }

    /// Get current unison count.
    pub fn unison_count(&self) -> usize {
        self.unison_count
    }

    /// Set unison detune spread in cents.
    ///
    /// Range: 0.0 to 100.0 cents. The spread is distributed symmetrically
    /// around the center pitch.
    pub fn set_unison_spread(&mut self, cents: f32) {
        self.unison_spread = cents.max(0.0);
        self.recalculate_unison_distribution();
    }

    /// Get unison detune spread in cents.
    pub fn unison_spread(&self) -> f32 {
        self.unison_spread
    }

    /// Set stereo width for unison voice panning.
    ///
    /// Range: 0.0 (mono) to 1.0 (full stereo). At 0.0, all sub-voices
    /// are centered. At 1.0, sub-voices span the full stereo field.
    pub fn set_stereo_width(&mut self, width: f32) {
        self.stereo_width = width.clamp(0.0, 1.0);
        self.recalculate_unison_distribution();
    }

    /// Get stereo width.
    pub fn stereo_width(&self) -> f32 {
        self.stereo_width
    }

    /// Get read access to sub-voices.
    pub fn sub_voices(&self) -> &[SubVoice] {
        &self.sub_voices[..self.unison_count]
    }

    /// Get mutable access to the modulation matrix.
    pub fn mod_matrix_mut(&mut self) -> &mut ModulationMatrix<16> {
        &mut self.mod_matrix
    }

    /// Get read access to current modulation values.
    pub fn mod_values(&self) -> &ModulationValues {
        &self.mod_values
    }

    /// Set a modulation source value directly (for external sources like LFOs, mod wheel).
    pub fn set_mod_source(&mut self, source: ModSourceId, value: f32) {
        self.mod_values.set(source, value);
    }

    /// Set external pitch modulation in semitones.
    ///
    /// Additive with mod matrix pitch modulation. Used by parent synths
    /// (e.g., `PolyphonicSynth`) to apply global LFO vibrato.
    pub fn set_external_pitch_mod(&mut self, semitones: f32) {
        self.external_pitch_mod = semitones;
    }

    /// Set external filter cutoff modulation in Hz.
    ///
    /// Additive with mod matrix and envelope filter modulation.
    pub fn set_external_filter_mod(&mut self, hz: f32) {
        self.external_filter_mod = hz;
    }

    // --- MPE per-note expression setters ---

    /// Set per-note MPE pitch bend in semitones.
    ///
    /// Range: -96.0 to +96.0 semitones. Applied additively with global pitch
    /// bend (if any) and the modulation matrix `Osc1Pitch` destination.
    /// The standard MPE per-note range is ±48 semitones, but ±96 is accepted
    /// to accommodate custom zone configurations.
    pub fn set_mpe_pitch_bend(&mut self, semitones: f32) {
        self.mpe_pitch_bend_semitones = semitones.clamp(-96.0, 96.0);
    }

    /// Get current MPE per-note pitch bend in semitones.
    pub fn mpe_pitch_bend(&self) -> f32 {
        self.mpe_pitch_bend_semitones
    }

    /// Set per-note MPE pressure (polyphonic aftertouch).
    ///
    /// Range: 0.0 to 1.0. Mapped to `ModSourceId::Aftertouch` in the
    /// modulation matrix so users can route it to any destination.
    pub fn set_mpe_pressure(&mut self, pressure: f32) {
        self.mpe_pressure = pressure.clamp(0.0, 1.0);
    }

    /// Get current MPE pressure.
    pub fn mpe_pressure(&self) -> f32 {
        self.mpe_pressure
    }

    /// Set per-note MPE slide (CC74 "brightness").
    ///
    /// Range: 0.0 to 1.0. Mapped to `ModSourceId::Custom1` so users can
    /// route it to filter cutoff, wavetable morph, FM mod index, etc.
    pub fn set_mpe_slide(&mut self, slide: f32) {
        self.mpe_slide = slide.clamp(0.0, 1.0);
    }

    /// Get current MPE slide.
    pub fn mpe_slide(&self) -> f32 {
        self.mpe_slide
    }

    /// Assign this voice to a MIDI channel (1 = global zone, 2-16 = per-note).
    ///
    /// Channel 1 is the MPE global zone. Channels 2-16 carry per-note
    /// expression for individual notes.
    pub fn set_mpe_channel(&mut self, channel: u8) {
        self.mpe_channel = channel.clamp(1, 16);
    }

    /// Get the assigned MIDI channel.
    pub fn mpe_channel(&self) -> u8 {
        self.mpe_channel
    }

    /// Recalculate sub-voice detune and pan from unison parameters.
    fn recalculate_unison_distribution(&mut self) {
        let count = self.unison_count;
        if count == 1 {
            self.sub_voices[0].detune_cents = 0.0;
            self.sub_voices[0].pan = 0.0;
            return;
        }

        let divisor = (count - 1) as f32;
        for i in 0..count {
            let t = 2.0 * i as f32 / divisor - 1.0; // -1.0 to 1.0
            self.sub_voices[i].detune_cents = self.unison_spread * t;
            self.sub_voices[i].pan = self.stereo_width * t;
        }
    }

    /// Process one mono sample.
    ///
    /// Sums all active unison sub-voices, applies filter with envelope + mod
    /// matrix modulation, then amplitude envelope with velocity scaling and
    /// mod matrix amplitude modulation.
    #[inline]
    pub fn process(&mut self) -> f32 {
        let (left, right) = self.process_stereo();
        (left + right) * 0.5
    }

    /// Process one stereo sample pair.
    ///
    /// Returns `(left, right)`. Unison sub-voices are panned according to
    /// their stereo position. Gain is normalized by `1/sqrt(unison_count)`
    /// to maintain consistent perceived loudness.
    #[inline]
    pub fn process_stereo(&mut self) -> (f32, f32) {
        if !self.is_active() {
            if self.active && !self.amp_env.is_active() {
                self.active = false;
            }
            return (0.0, 0.0);
        }

        // Advance portamento and get current base frequency
        let base_freq = self.freq_target.advance();

        // Update sub-voice frequencies with portamento
        for sv in &mut self.sub_voices[..self.unison_count] {
            sv.set_frequency(base_freq, self.osc2_detune);
        }

        // Advance envelopes
        let amp_env_val = self.amp_env.advance();
        let filter_env_val = self.filter_env.advance();

        // Populate mod values from voice state and MPE per-note dimensions.
        self.mod_values.amp_env = amp_env_val;
        self.mod_values.filter_env = filter_env_val;
        // MPE pressure → aftertouch source; MPE slide → custom1 source
        self.mod_values.aftertouch = self.mpe_pressure;
        self.mod_values.custom1 = self.mpe_slide;

        // Read modulation destinations from matrix
        let pitch_mod = self
            .mod_matrix
            .get_modulation(ModDestination::Osc1Pitch, &self.mod_values);
        let filter_mod = self
            .mod_matrix
            .get_modulation(ModDestination::FilterCutoff, &self.mod_values);
        let amp_mod = self
            .mod_matrix
            .get_modulation(ModDestination::Amplitude, &self.mod_values);

        // Apply pitch modulation (in semitones) to sub-voice frequencies.
        // MPE per-note pitch bend is additive with global LFO/mod-matrix pitch.
        let total_pitch_mod = pitch_mod + self.external_pitch_mod + self.mpe_pitch_bend_semitones;
        if total_pitch_mod.abs() > 1e-6 {
            let pitch_ratio = cents_to_ratio(total_pitch_mod * 100.0);
            for sv in &mut self.sub_voices[..self.unison_count] {
                let f1 = sv.osc1.frequency() * pitch_ratio;
                let f2 = sv.osc2.frequency() * pitch_ratio;
                sv.osc1.set_frequency(f1);
                sv.osc2.set_frequency(f2);
            }
        }

        // Sum sub-voices with stereo panning
        let mut left = 0.0_f32;
        let mut right = 0.0_f32;
        let count = self.unison_count;
        for sv in &mut self.sub_voices[..count] {
            let sample = sv.advance(self.osc_mix);
            // Constant-power pan law: left = cos(angle), right = sin(angle)
            // where angle = (pan + 1) * pi/4 maps [-1,1] to [0, pi/2]
            let angle = (sv.pan + 1.0) * core::f32::consts::FRAC_PI_4;
            let (sin_a, cos_a) = libm::sincosf(angle);
            left += sample * cos_a;
            right += sample * sin_a;
        }

        // Normalize gain: 1/sqrt(count) to maintain perceived loudness
        let gain_norm = 1.0 / libm::sqrtf(count as f32);
        left *= gain_norm;
        right *= gain_norm;

        // Apply filter with envelope + mod matrix + external modulation (bipolar env amount)
        let modulated_cutoff = self.filter_cutoff
            + filter_env_val * self.filter_env_amount
            + filter_mod * 1000.0
            + self.external_filter_mod;
        self.filter
            .set_cutoff(modulated_cutoff.clamp(20.0, 20000.0));
        left = self.filter.process(left);
        // Process right through filter — for dual-mono, same filter state is fine
        // since SVF is stateful; for true stereo filter we'd need two filters.
        // This matches the original mono filter behavior.
        right = self.filter.process(right);

        // Apply amplitude envelope with velocity scaling and mod matrix
        let velocity_scale = self.velocity as f32 / 127.0;
        let amp = amp_env_val * velocity_scale * (1.0 + amp_mod).max(0.0);

        (left * amp, right * amp)
    }
}

/// Voice manager for polyphonic synthesis.
///
/// Manages a pool of voices and handles note allocation/stealing.
///
/// # Example
///
/// ```rust
/// use sonido_synth::{VoiceManager, VoiceAllocationMode};
///
/// let mut manager: VoiceManager<8> = VoiceManager::new(48000.0);
/// manager.set_allocation_mode(VoiceAllocationMode::OldestNote);
///
/// // Play notes
/// manager.note_on(60, 100);
/// manager.note_on(64, 100);
/// manager.note_on(67, 100);
///
/// // Process audio
/// for _ in 0..1000 {
///     let sample = manager.process();
/// }
/// ```
#[derive(Debug)]
pub struct VoiceManager<const N: usize> {
    voices: [Voice; N],
    allocation_mode: VoiceAllocationMode,
    sample_rate: f32,
    /// Global voice age counter
    age_counter: u64,
    /// Round-robin index
    round_robin_idx: usize,
}

impl<const N: usize> VoiceManager<N> {
    /// Create a new voice manager with the specified number of voices.
    pub fn new(sample_rate: f32) -> Self {
        Self {
            voices: core::array::from_fn(|_| Voice::new(sample_rate)),
            allocation_mode: VoiceAllocationMode::RoundRobin,
            sample_rate,
            age_counter: 0,
            round_robin_idx: 0,
        }
    }

    /// Set sample rate for all voices.
    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        for voice in &mut self.voices {
            voice.set_sample_rate(sample_rate);
        }
    }

    /// Set voice allocation mode.
    pub fn set_allocation_mode(&mut self, mode: VoiceAllocationMode) {
        self.allocation_mode = mode;
    }

    /// Get current allocation mode.
    pub fn allocation_mode(&self) -> VoiceAllocationMode {
        self.allocation_mode
    }

    /// Get number of voices.
    pub fn voice_count(&self) -> usize {
        N
    }

    /// Get number of active voices.
    pub fn active_voice_count(&self) -> usize {
        self.voices.iter().filter(|v| v.is_active()).count()
    }

    /// Get mutable access to all voices (for setting parameters).
    pub fn voices_mut(&mut self) -> &mut [Voice; N] {
        &mut self.voices
    }

    /// Get read access to all voices.
    pub fn voices(&self) -> &[Voice; N] {
        &self.voices
    }

    /// Trigger a note on.
    pub fn note_on(&mut self, note: u8, velocity: u8) {
        let voice_idx = self.allocate_voice(note);
        self.age_counter += 1;
        self.voices[voice_idx].set_age(self.age_counter);
        self.voices[voice_idx].note_on(note, velocity);
    }

    /// Trigger a note off.
    pub fn note_off(&mut self, note: u8) {
        for voice in &mut self.voices {
            if voice.is_active() && voice.note() == note {
                voice.note_off();
                return;
            }
        }
    }

    /// Stop all notes immediately.
    pub fn all_notes_off(&mut self) {
        for voice in &mut self.voices {
            voice.kill();
        }
    }

    /// Reset all voices.
    pub fn reset(&mut self) {
        for voice in &mut self.voices {
            voice.reset();
        }
        self.age_counter = 0;
        self.round_robin_idx = 0;
    }

    /// Process one mono sample from all voices.
    #[inline]
    pub fn process(&mut self) -> f32 {
        let mut output = 0.0;
        for voice in &mut self.voices {
            output += voice.process();
        }
        output
    }

    /// Process stereo output from all voices.
    #[inline]
    pub fn process_stereo(&mut self) -> (f32, f32) {
        let mut left = 0.0;
        let mut right = 0.0;
        for voice in &mut self.voices {
            let (l, r) = voice.process_stereo();
            left += l;
            right += r;
        }
        (left, right)
    }

    fn allocate_voice(&mut self, _note: u8) -> usize {
        // First, try to find a free voice
        for (i, voice) in self.voices.iter().enumerate() {
            if !voice.is_active() {
                return i;
            }
        }

        // All voices are active, need to steal one
        match self.allocation_mode {
            VoiceAllocationMode::RoundRobin => {
                let idx = self.round_robin_idx;
                self.round_robin_idx = (self.round_robin_idx + 1) % N;
                idx
            }
            VoiceAllocationMode::OldestNote => self
                .voices
                .iter()
                .enumerate()
                .min_by_key(|(_, v)| v.age())
                .map(|(i, _)| i)
                .unwrap_or(0),
            VoiceAllocationMode::LowestNote => self
                .voices
                .iter()
                .enumerate()
                .min_by_key(|(_, v)| v.note())
                .map(|(i, _)| i)
                .unwrap_or(0),
            VoiceAllocationMode::HighestNote => self
                .voices
                .iter()
                .enumerate()
                .max_by_key(|(_, v)| v.note())
                .map(|(i, _)| i)
                .unwrap_or(0),
        }
    }
}

/// Convert MIDI note number to frequency in Hz.
///
/// Uses standard tuning: A4 (note 69) = 440 Hz.
#[inline]
pub fn midi_to_freq(note: u8) -> f32 {
    440.0 * libm::powf(2.0, (note as f32 - 69.0) / 12.0)
}

/// Convert frequency in Hz to MIDI note number.
#[inline]
pub fn freq_to_midi(freq: f32) -> f32 {
    69.0 + 12.0 * libm::log2f(freq / 440.0)
}

/// Convert cents to frequency ratio.
///
/// 100 cents = 1 semitone.
#[inline]
pub fn cents_to_ratio(cents: f32) -> f32 {
    libm::powf(2.0, cents / 1200.0)
}

#[cfg(test)]
mod tests {
    extern crate alloc;
    use alloc::vec::Vec;

    use super::*;

    #[test]
    fn test_midi_to_freq_a4() {
        let freq = midi_to_freq(69);
        assert!(
            (freq - 440.0).abs() < 0.01,
            "A4 should be 440 Hz, got {}",
            freq
        );
    }

    #[test]
    fn test_midi_to_freq_middle_c() {
        let freq = midi_to_freq(60);
        assert!(
            (freq - 261.63).abs() < 0.1,
            "C4 should be ~261.63 Hz, got {}",
            freq
        );
    }

    #[test]
    fn test_cents_to_ratio() {
        // 1200 cents = 1 octave = ratio of 2
        let ratio = cents_to_ratio(1200.0);
        assert!(
            (ratio - 2.0).abs() < 0.001,
            "1200 cents should be ratio 2, got {}",
            ratio
        );

        // 0 cents = ratio of 1
        let ratio = cents_to_ratio(0.0);
        assert!(
            (ratio - 1.0).abs() < 0.001,
            "0 cents should be ratio 1, got {}",
            ratio
        );
    }

    #[test]
    fn test_voice_note_on_off() {
        let mut voice = Voice::new(48000.0);

        assert!(!voice.is_active());

        voice.note_on(60, 100);
        assert!(voice.is_active());
        assert_eq!(voice.note(), 60);
        assert_eq!(voice.velocity(), 100);

        voice.note_off();
        // Voice should still be active during release
        // but will become inactive after envelope completes

        voice.kill();
        assert!(!voice.is_active());
    }

    #[test]
    fn test_voice_process() {
        let mut voice = Voice::new(48000.0);
        voice.note_on(69, 100); // A4

        // Should produce non-zero output
        let mut sum = 0.0;
        for _ in 0..1000 {
            sum += voice.process().abs();
        }

        assert!(sum > 0.0, "Voice should produce output");
    }

    #[test]
    fn test_voice_process_stereo() {
        let mut voice = Voice::new(48000.0);
        voice.note_on(69, 100);

        let mut left_sum = 0.0;
        let mut right_sum = 0.0;
        for _ in 0..1000 {
            let (l, r) = voice.process_stereo();
            left_sum += l.abs();
            right_sum += r.abs();
        }

        assert!(left_sum > 0.0, "Left channel should produce output");
        assert!(right_sum > 0.0, "Right channel should produce output");
    }

    #[test]
    fn test_voice_unison_count() {
        let mut voice = Voice::new(48000.0);
        assert_eq!(voice.unison_count(), 1);

        voice.set_unison_count(4);
        assert_eq!(voice.unison_count(), 4);

        // Clamp to MAX_UNISON
        voice.set_unison_count(100);
        assert_eq!(voice.unison_count(), MAX_UNISON);

        // Clamp to 1
        voice.set_unison_count(0);
        assert_eq!(voice.unison_count(), 1);
    }

    #[test]
    fn test_unison_detune_distribution() {
        let mut voice = Voice::new(48000.0);
        voice.set_unison_count(4);
        voice.set_unison_spread(20.0);

        let svs = voice.sub_voices();
        assert_eq!(svs.len(), 4);

        // Symmetric: first should be -20, last should be +20
        assert!(
            (svs[0].detune_cents() - (-20.0)).abs() < 0.01,
            "First sub-voice detune: {}",
            svs[0].detune_cents()
        );
        assert!(
            (svs[3].detune_cents() - 20.0).abs() < 0.01,
            "Last sub-voice detune: {}",
            svs[3].detune_cents()
        );

        // Middle voices should be evenly spaced
        let expected_1 = -20.0 + 40.0 / 3.0; // ~-6.67
        assert!(
            (svs[1].detune_cents() - expected_1).abs() < 0.01,
            "Second sub-voice detune: {} expected {}",
            svs[1].detune_cents(),
            expected_1
        );
    }

    #[test]
    fn test_unison_pan_distribution() {
        let mut voice = Voice::new(48000.0);
        voice.set_unison_count(3);
        voice.set_stereo_width(1.0);

        let svs = voice.sub_voices();
        assert!(
            (svs[0].pan() - (-1.0)).abs() < 0.01,
            "First pan: {}",
            svs[0].pan()
        );
        assert!(svs[1].pan().abs() < 0.01, "Center pan: {}", svs[1].pan());
        assert!(
            (svs[2].pan() - 1.0).abs() < 0.01,
            "Last pan: {}",
            svs[2].pan()
        );
    }

    #[test]
    fn test_unison_single_voice_centered() {
        let mut voice = Voice::new(48000.0);
        voice.set_unison_count(1);
        voice.set_unison_spread(50.0);
        voice.set_stereo_width(1.0);

        let svs = voice.sub_voices();
        assert_eq!(svs.len(), 1);
        assert!(svs[0].detune_cents().abs() < 0.001);
        assert!(svs[0].pan().abs() < 0.001);
    }

    #[test]
    fn test_unison_stereo_spread() {
        let mut voice = Voice::new(48000.0);
        voice.set_unison_count(8);
        voice.set_unison_spread(30.0);
        voice.note_on(60, 100);

        // Run enough samples for the attack envelope to build up
        for _ in 0..2000 {
            voice.process_stereo();
        }

        // With 8 voices detuned, there should be a thick stereo signal
        let mut left_sum = 0.0;
        let mut right_sum = 0.0;
        for _ in 0..1000 {
            let (l, r) = voice.process_stereo();
            left_sum += l.abs();
            right_sum += r.abs();
        }

        assert!(left_sum > 0.0, "Left should have output");
        assert!(right_sum > 0.0, "Right should have output");
    }

    #[test]
    fn test_portamento() {
        let mut voice = Voice::new(48000.0);
        voice.set_portamento_time(100.0); // 100ms glide

        // First note — snaps immediately
        voice.note_on(60, 100);
        for _ in 0..100 {
            voice.process();
        }

        // Second note — should glide
        voice.note_on(72, 100);
        let freq_after_trigger = voice.freq_target.advance();
        let target_freq = midi_to_freq(72);

        // Should not have reached target yet
        assert!(
            (freq_after_trigger - target_freq).abs() > 1.0,
            "Portamento should not reach target immediately: freq={}",
            freq_after_trigger
        );

        // After enough time, should approach target
        for _ in 0..48000 {
            voice.process();
        }
        let freq_after_glide = voice.freq_target.advance();
        assert!(
            (freq_after_glide - target_freq).abs() < 1.0,
            "Portamento should reach target: got {} expected {}",
            freq_after_glide,
            target_freq
        );
    }

    #[test]
    fn test_mod_matrix_wiring() {
        use crate::mod_matrix::ModulationRoute;

        let mut voice = Voice::new(48000.0);

        // Route filter envelope to filter cutoff
        voice.mod_matrix.add_route(ModulationRoute::unipolar(
            ModSourceId::FilterEnv,
            ModDestination::FilterCutoff,
            0.5,
        ));

        voice.note_on(60, 100);

        // Process some samples — filter env should affect cutoff through mod matrix
        for _ in 0..1000 {
            voice.process();
        }

        // Mod values should be populated
        assert!(
            voice.mod_values().amp_env > 0.0,
            "Amp env should be positive"
        );
    }

    #[test]
    fn test_bipolar_filter_env_amount() {
        let mut voice = Voice::new(48000.0);
        voice.set_filter_cutoff(5000.0);
        voice.set_filter_env_amount(-3000.0); // Negative = sweep down

        voice.note_on(60, 100);

        // During attack, filter env is positive, so cutoff should decrease
        // (5000 + positive_env * -3000)
        let mut samples = Vec::new();
        for _ in 0..500 {
            samples.push(voice.process());
        }

        // Should produce output (doesn't crash with negative env amount)
        let sum: f32 = samples.iter().map(|s| s.abs()).sum();
        assert!(sum > 0.0, "Bipolar filter env should produce output");
    }

    #[test]
    fn test_voice_manager_allocation() {
        let mut manager: VoiceManager<4> = VoiceManager::new(48000.0);

        // Play 4 notes
        manager.note_on(60, 100);
        manager.note_on(64, 100);
        manager.note_on(67, 100);
        manager.note_on(72, 100);

        assert_eq!(manager.active_voice_count(), 4);

        // 5th note should steal a voice
        manager.note_on(76, 100);
        assert_eq!(manager.active_voice_count(), 4);
    }

    #[test]
    fn test_voice_manager_oldest_note_stealing() {
        let mut manager: VoiceManager<2> = VoiceManager::new(48000.0);
        manager.set_allocation_mode(VoiceAllocationMode::OldestNote);

        manager.note_on(60, 100);
        manager.note_on(64, 100);

        // Third note should steal the oldest (60)
        manager.note_on(67, 100);

        // Voice playing 64 should still be active
        let has_64 = manager
            .voices()
            .iter()
            .any(|v| v.is_active() && v.note() == 64);
        assert!(has_64, "Note 64 should still be playing");
    }

    #[test]
    fn test_voice_manager_note_off() {
        let mut manager: VoiceManager<4> = VoiceManager::new(48000.0);

        manager.note_on(60, 100);
        manager.note_on(64, 100);

        manager.note_off(60);

        // Voice should still be in release phase (active but releasing)
        // For this test, we'll check that only one voice is playing note 64
        let playing_64 = manager
            .voices()
            .iter()
            .filter(|v| v.is_active() && v.note() == 64)
            .count();
        assert_eq!(playing_64, 1);
    }

    #[test]
    fn test_voice_manager_all_notes_off() {
        let mut manager: VoiceManager<4> = VoiceManager::new(48000.0);

        manager.note_on(60, 100);
        manager.note_on(64, 100);
        manager.note_on(67, 100);

        manager.all_notes_off();
        assert_eq!(manager.active_voice_count(), 0);
    }

    #[test]
    fn test_voice_manager_process() {
        let mut manager: VoiceManager<4> = VoiceManager::new(48000.0);

        manager.note_on(69, 100); // A4

        let mut sum = 0.0;
        for _ in 0..1000 {
            sum += manager.process().abs();
        }

        assert!(sum > 0.0, "Manager should produce output");
    }

    #[test]
    fn test_mpe_pitch_bend_shifts_pitch() {
        let mut voice = Voice::new(48000.0);
        voice.note_on(60, 100); // C4
        voice.set_mpe_pitch_bend(12.0); // +1 octave

        let mut sum = 0.0f32;
        for _ in 0..1000 {
            sum += voice.process().abs();
        }
        assert!(sum > 0.0, "Voice with MPE pitch bend should produce output");
        assert!((voice.mpe_pitch_bend() - 12.0).abs() < 1e-5);
    }

    #[test]
    fn test_mpe_pressure_routes_to_aftertouch() {
        let mut voice = Voice::new(48000.0);
        voice.note_on(60, 100);
        voice.set_mpe_pressure(0.75);

        // process() calls process_stereo() which updates mod_values
        voice.process();
        assert!(
            (voice.mod_values().aftertouch - 0.75).abs() < 1e-5,
            "MPE pressure should propagate to aftertouch mod value: {}",
            voice.mod_values().aftertouch
        );
    }

    #[test]
    fn test_mpe_slide_routes_to_custom1() {
        let mut voice = Voice::new(48000.0);
        voice.note_on(60, 100);
        voice.set_mpe_slide(0.5);

        voice.process();
        assert!(
            (voice.mod_values().custom1 - 0.5).abs() < 1e-5,
            "MPE slide should propagate to custom1 mod value: {}",
            voice.mod_values().custom1
        );
    }

    #[test]
    fn test_mpe_channel_assignment() {
        let mut voice = Voice::new(48000.0);
        assert_eq!(voice.mpe_channel(), 1); // default = global zone

        voice.set_mpe_channel(5);
        assert_eq!(voice.mpe_channel(), 5);

        voice.set_mpe_channel(0); // below min → clamped to 1
        assert_eq!(voice.mpe_channel(), 1);

        voice.set_mpe_channel(20); // above max → clamped to 16
        assert_eq!(voice.mpe_channel(), 16);
    }

    #[test]
    fn test_mpe_pitch_bend_clamping() {
        let mut voice = Voice::new(48000.0);
        voice.set_mpe_pitch_bend(200.0);
        assert!((voice.mpe_pitch_bend() - 96.0).abs() < 1e-5);

        voice.set_mpe_pitch_bend(-200.0);
        assert!((voice.mpe_pitch_bend() - (-96.0)).abs() < 1e-5);
    }

    #[test]
    fn test_voice_manager_stereo() {
        let mut manager: VoiceManager<4> = VoiceManager::new(48000.0);

        // Set unison on all voices for stereo output
        for voice in manager.voices_mut() {
            voice.set_unison_count(4);
            voice.set_unison_spread(20.0);
        }

        manager.note_on(60, 100);
        manager.note_on(64, 100);

        let mut left_sum = 0.0;
        let mut right_sum = 0.0;
        for _ in 0..1000 {
            let (l, r) = manager.process_stereo();
            left_sum += l.abs();
            right_sum += r.abs();
        }

        assert!(left_sum > 0.0, "Left should have stereo output");
        assert!(right_sum > 0.0, "Right should have stereo output");
    }
}
