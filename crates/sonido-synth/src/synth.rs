//! Complete synthesizer implementations.
//!
//! Provides ready-to-use monophonic and polyphonic synthesizers
//! with modulation, filtering, and voice management.

use crate::envelope::AdsrEnvelope;
use crate::oscillator::{Oscillator, OscillatorWaveform};
use crate::voice::{VoiceAllocationMode, VoiceManager, cents_to_ratio, midi_to_freq};
use sonido_core::{Effect, Lfo, LfoWaveform, StateVariableFilter, SvfOutput};

/// A monophonic synthesizer.
///
/// Single-voice synth with two oscillators, filter, envelopes, and LFOs.
/// Suitable for bass and lead sounds where only one note plays at a time.
///
/// # Features
///
/// - Two oscillators with independent waveforms and detune
/// - State variable filter with envelope modulation
/// - Two LFOs for modulation
/// - ADSR envelopes for amplitude and filter
/// - Glide/portamento between notes
///
/// # Example
///
/// ```rust
/// use sonido_synth::{MonophonicSynth, OscillatorWaveform};
///
/// let mut synth = MonophonicSynth::new(48000.0);
///
/// // Configure sound
/// synth.set_osc1_waveform(OscillatorWaveform::Saw);
/// synth.set_filter_cutoff(2000.0);
/// synth.set_filter_resonance(2.0);
///
/// // Play a note
/// synth.note_on(60, 100);
///
/// // Generate audio
/// for _ in 0..1000 {
///     let sample = synth.process();
/// }
/// ```
#[derive(Debug)]
pub struct MonophonicSynth {
    /// Oscillator 1
    osc1: Oscillator,
    /// Oscillator 2
    osc2: Oscillator,
    /// Filter
    filter: StateVariableFilter,
    /// Amplitude envelope
    amp_env: AdsrEnvelope,
    /// Filter envelope
    filter_env: AdsrEnvelope,
    /// LFO 1
    lfo1: Lfo,
    /// LFO 2
    lfo2: Lfo,

    /// Sample rate
    sample_rate: f32,
    /// Current note
    current_note: u8,
    /// Target note (for glide)
    target_note: u8,
    /// Current frequency (for glide)
    current_freq: f32,
    /// Target frequency
    target_freq: f32,
    /// Glide time in seconds
    glide_time: f32,
    /// Glide coefficient
    glide_coeff: f32,

    // Oscillator parameters
    osc2_detune: f32,
    osc_mix: f32,

    // Filter parameters
    filter_cutoff: f32,
    filter_resonance: f32,
    filter_env_amount: f32,
    filter_key_track: f32,

    // LFO parameters
    lfo1_to_pitch: f32,
    lfo1_to_filter: f32,
    lfo2_to_pitch: f32,
    lfo2_to_filter: f32,
}

impl Default for MonophonicSynth {
    fn default() -> Self {
        Self::new(48000.0)
    }
}

impl MonophonicSynth {
    /// Create a new monophonic synthesizer.
    pub fn new(sample_rate: f32) -> Self {
        let mut synth = Self {
            osc1: Oscillator::new(sample_rate),
            osc2: Oscillator::new(sample_rate),
            filter: StateVariableFilter::new(sample_rate),
            amp_env: AdsrEnvelope::new(sample_rate),
            filter_env: AdsrEnvelope::new(sample_rate),
            lfo1: Lfo::new(sample_rate, 5.0),
            lfo2: Lfo::new(sample_rate, 0.5),
            sample_rate,
            current_note: 60,
            target_note: 60,
            current_freq: 261.63,
            target_freq: 261.63,
            glide_time: 0.0,
            glide_coeff: 1.0,
            osc2_detune: 0.0,
            osc_mix: 0.0,
            filter_cutoff: 1000.0,
            filter_resonance: 1.0,
            filter_env_amount: 0.0,
            filter_key_track: 0.0,
            lfo1_to_pitch: 0.0,
            lfo1_to_filter: 0.0,
            lfo2_to_pitch: 0.0,
            lfo2_to_filter: 0.0,
        };

        // Default waveforms
        synth.osc1.set_waveform(OscillatorWaveform::Saw);
        synth.osc2.set_waveform(OscillatorWaveform::Saw);
        synth.lfo1.set_waveform(LfoWaveform::Triangle);
        synth.lfo2.set_waveform(LfoWaveform::Sine);

        synth
    }

    /// Set sample rate.
    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.osc1.set_sample_rate(sample_rate);
        self.osc2.set_sample_rate(sample_rate);
        self.filter.set_sample_rate(sample_rate);
        self.amp_env.set_sample_rate(sample_rate);
        self.filter_env.set_sample_rate(sample_rate);
        self.lfo1.set_sample_rate(sample_rate);
        self.lfo2.set_sample_rate(sample_rate);
        self.update_glide_coeff();
    }

    // Oscillator settings

    /// Set oscillator 1 waveform.
    pub fn set_osc1_waveform(&mut self, waveform: OscillatorWaveform) {
        self.osc1.set_waveform(waveform);
    }

    /// Set oscillator 2 waveform.
    pub fn set_osc2_waveform(&mut self, waveform: OscillatorWaveform) {
        self.osc2.set_waveform(waveform);
    }

    /// Set oscillator 2 detune in cents.
    pub fn set_osc2_detune(&mut self, cents: f32) {
        self.osc2_detune = cents;
    }

    /// Set oscillator mix (0 = osc1 only, 1 = osc2 only).
    pub fn set_osc_mix(&mut self, mix: f32) {
        self.osc_mix = mix.clamp(0.0, 1.0);
    }

    // Filter settings

    /// Set filter cutoff frequency.
    pub fn set_filter_cutoff(&mut self, freq: f32) {
        self.filter_cutoff = freq.clamp(20.0, 20000.0);
    }

    /// Set filter resonance.
    pub fn set_filter_resonance(&mut self, resonance: f32) {
        self.filter_resonance = resonance.clamp(0.5, 20.0);
        self.filter.set_resonance(self.filter_resonance);
    }

    /// Set filter type.
    pub fn set_filter_type(&mut self, filter_type: SvfOutput) {
        self.filter.set_output_type(filter_type);
    }

    /// Set filter envelope amount (in Hz).
    pub fn set_filter_env_amount(&mut self, amount: f32) {
        self.filter_env_amount = amount;
    }

    /// Set filter keyboard tracking (0 = none, 1 = full).
    pub fn set_filter_key_track(&mut self, amount: f32) {
        self.filter_key_track = amount.clamp(0.0, 1.0);
    }

    // Envelope settings

    /// Set amplitude envelope attack time.
    pub fn set_amp_attack(&mut self, ms: f32) {
        self.amp_env.set_attack_ms(ms);
    }

    /// Set amplitude envelope decay time.
    pub fn set_amp_decay(&mut self, ms: f32) {
        self.amp_env.set_decay_ms(ms);
    }

    /// Set amplitude envelope sustain level.
    pub fn set_amp_sustain(&mut self, level: f32) {
        self.amp_env.set_sustain(level);
    }

    /// Set amplitude envelope release time.
    pub fn set_amp_release(&mut self, ms: f32) {
        self.amp_env.set_release_ms(ms);
    }

    /// Set filter envelope attack time.
    pub fn set_filter_attack(&mut self, ms: f32) {
        self.filter_env.set_attack_ms(ms);
    }

    /// Set filter envelope decay time.
    pub fn set_filter_decay(&mut self, ms: f32) {
        self.filter_env.set_decay_ms(ms);
    }

    /// Set filter envelope sustain level.
    pub fn set_filter_sustain(&mut self, level: f32) {
        self.filter_env.set_sustain(level);
    }

    /// Set filter envelope release time.
    pub fn set_filter_release(&mut self, ms: f32) {
        self.filter_env.set_release_ms(ms);
    }

    // LFO settings

    /// Set LFO 1 rate.
    pub fn set_lfo1_rate(&mut self, hz: f32) {
        self.lfo1.set_frequency(hz);
    }

    /// Set LFO 1 waveform.
    pub fn set_lfo1_waveform(&mut self, waveform: LfoWaveform) {
        self.lfo1.set_waveform(waveform);
    }

    /// Set LFO 1 to pitch modulation amount (in semitones).
    pub fn set_lfo1_to_pitch(&mut self, semitones: f32) {
        self.lfo1_to_pitch = semitones;
    }

    /// Set LFO 1 to filter modulation amount (in Hz).
    pub fn set_lfo1_to_filter(&mut self, hz: f32) {
        self.lfo1_to_filter = hz;
    }

    /// Set LFO 2 rate.
    pub fn set_lfo2_rate(&mut self, hz: f32) {
        self.lfo2.set_frequency(hz);
    }

    /// Set LFO 2 waveform.
    pub fn set_lfo2_waveform(&mut self, waveform: LfoWaveform) {
        self.lfo2.set_waveform(waveform);
    }

    /// Set LFO 2 to pitch modulation amount (in semitones).
    pub fn set_lfo2_to_pitch(&mut self, semitones: f32) {
        self.lfo2_to_pitch = semitones;
    }

    /// Set LFO 2 to filter modulation amount (in Hz).
    pub fn set_lfo2_to_filter(&mut self, hz: f32) {
        self.lfo2_to_filter = hz;
    }

    // Glide settings

    /// Set glide time in milliseconds.
    pub fn set_glide_time(&mut self, ms: f32) {
        self.glide_time = ms / 1000.0;
        self.update_glide_coeff();
    }

    fn update_glide_coeff(&mut self) {
        if self.glide_time > 0.001 {
            let samples = self.glide_time * self.sample_rate;
            self.glide_coeff = libm::expf(-1.0 / samples);
        } else {
            self.glide_coeff = 0.0; // Instant
        }
    }

    /// Trigger a note.
    pub fn note_on(&mut self, note: u8, _velocity: u8) {
        self.target_note = note;
        self.target_freq = midi_to_freq(note);

        if self.glide_coeff == 0.0 || !self.amp_env.is_active() {
            self.current_freq = self.target_freq;
            self.current_note = note;
        }

        self.amp_env.gate_on();
        self.filter_env.gate_on();
    }

    /// Release the current note.
    pub fn note_off(&mut self, _note: u8) {
        self.amp_env.gate_off();
        self.filter_env.gate_off();
    }

    /// Process one sample.
    #[inline]
    pub fn process(&mut self) -> f32 {
        // Update glide
        if self.glide_coeff > 0.0 {
            self.current_freq =
                self.target_freq + (self.current_freq - self.target_freq) * self.glide_coeff;
        }

        // Get LFO values
        let lfo1_val = self.lfo1.advance();
        let lfo2_val = self.lfo2.advance();

        // Calculate pitch modulation
        let pitch_mod_semitones = lfo1_val * self.lfo1_to_pitch + lfo2_val * self.lfo2_to_pitch;
        let pitch_ratio = cents_to_ratio(pitch_mod_semitones * 100.0);

        // Set oscillator frequencies
        let base_freq = self.current_freq * pitch_ratio;
        self.osc1.set_frequency(base_freq);
        self.osc2
            .set_frequency(base_freq * cents_to_ratio(self.osc2_detune));

        // Generate oscillator output
        let osc1_out = self.osc1.advance();
        let osc2_out = self.osc2.advance();
        let osc_out = osc1_out * (1.0 - self.osc_mix) + osc2_out * self.osc_mix;

        // Get envelope values
        let amp_env = self.amp_env.advance();
        let filter_env = self.filter_env.advance();

        // Calculate filter modulation
        let key_track_offset = if self.filter_key_track > 0.0 {
            (self.current_note as f32 - 60.0) * 100.0 * self.filter_key_track
        } else {
            0.0
        };

        let filter_mod = filter_env * self.filter_env_amount
            + lfo1_val * self.lfo1_to_filter
            + lfo2_val * self.lfo2_to_filter
            + key_track_offset;

        let modulated_cutoff = (self.filter_cutoff + filter_mod).clamp(20.0, 20000.0);
        self.filter.set_cutoff(modulated_cutoff);

        // Apply filter
        let filtered = self.filter.process(osc_out);

        // Apply amplitude envelope
        filtered * amp_env
    }

    /// Reset the synthesizer.
    pub fn reset(&mut self) {
        self.osc1.reset();
        self.osc2.reset();
        self.filter.reset();
        self.amp_env.reset();
        self.filter_env.reset();
        self.lfo1.reset();
        self.lfo2.reset();
    }
}

/// A polyphonic synthesizer with configurable voice count.
///
/// Multi-voice synth with global LFOs and per-voice processing.
///
/// # Example
///
/// ```rust
/// use sonido_synth::{PolyphonicSynth, OscillatorWaveform, VoiceAllocationMode};
///
/// // Create an 8-voice synth
/// let mut synth: PolyphonicSynth<8> = PolyphonicSynth::new(48000.0);
///
/// synth.set_allocation_mode(VoiceAllocationMode::OldestNote);
///
/// // Play a chord
/// synth.note_on(60, 100);
/// synth.note_on(64, 100);
/// synth.note_on(67, 100);
///
/// // Generate audio
/// for _ in 0..1000 {
///     let sample = synth.process();
/// }
/// ```
#[derive(Debug)]
pub struct PolyphonicSynth<const VOICES: usize> {
    /// Voice manager
    voices: VoiceManager<VOICES>,
    /// LFO 1 (global)
    lfo1: Lfo,
    /// LFO 2 (global)
    lfo2: Lfo,
    /// Sample rate
    sample_rate: f32,

    // Global parameters
    osc1_waveform: OscillatorWaveform,
    osc2_waveform: OscillatorWaveform,
    osc2_detune: f32,
    osc_mix: f32,
    filter_cutoff: f32,
    filter_resonance: f32,
    filter_env_amount: f32,

    // LFO modulation amounts
    lfo1_to_pitch: f32,
    lfo1_to_filter: f32,
}

impl<const VOICES: usize> PolyphonicSynth<VOICES> {
    /// Create a new polyphonic synthesizer.
    pub fn new(sample_rate: f32) -> Self {
        let mut synth = Self {
            voices: VoiceManager::new(sample_rate),
            lfo1: Lfo::new(sample_rate, 5.0),
            lfo2: Lfo::new(sample_rate, 0.5),
            sample_rate,
            osc1_waveform: OscillatorWaveform::Saw,
            osc2_waveform: OscillatorWaveform::Saw,
            osc2_detune: 0.0,
            osc_mix: 0.0,
            filter_cutoff: 1000.0,
            filter_resonance: 1.0,
            filter_env_amount: 0.0,
            lfo1_to_pitch: 0.0,
            lfo1_to_filter: 0.0,
        };

        // Apply default waveforms to all voices
        synth.update_voice_params();
        synth
    }

    /// Set sample rate.
    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.voices.set_sample_rate(sample_rate);
        self.lfo1.set_sample_rate(sample_rate);
        self.lfo2.set_sample_rate(sample_rate);
    }

    /// Set voice allocation mode.
    pub fn set_allocation_mode(&mut self, mode: VoiceAllocationMode) {
        self.voices.set_allocation_mode(mode);
    }

    /// Set oscillator 1 waveform for all voices.
    pub fn set_osc1_waveform(&mut self, waveform: OscillatorWaveform) {
        self.osc1_waveform = waveform;
        for voice in self.voices.voices_mut() {
            voice.set_osc1_waveform(waveform);
        }
    }

    /// Set oscillator 2 waveform for all voices.
    pub fn set_osc2_waveform(&mut self, waveform: OscillatorWaveform) {
        self.osc2_waveform = waveform;
        for voice in self.voices.voices_mut() {
            voice.set_osc2_waveform(waveform);
        }
    }

    /// Set oscillator 2 detune for all voices.
    pub fn set_osc2_detune(&mut self, cents: f32) {
        self.osc2_detune = cents;
        for voice in self.voices.voices_mut() {
            voice.set_osc2_detune(cents);
        }
    }

    /// Set oscillator mix for all voices.
    pub fn set_osc_mix(&mut self, mix: f32) {
        self.osc_mix = mix;
        for voice in self.voices.voices_mut() {
            voice.set_osc_mix(mix);
        }
    }

    /// Set filter cutoff for all voices.
    pub fn set_filter_cutoff(&mut self, freq: f32) {
        self.filter_cutoff = freq;
        for voice in self.voices.voices_mut() {
            voice.set_filter_cutoff(freq);
        }
    }

    /// Set filter resonance for all voices.
    pub fn set_filter_resonance(&mut self, resonance: f32) {
        self.filter_resonance = resonance;
        for voice in self.voices.voices_mut() {
            voice.filter.set_resonance(resonance);
        }
    }

    /// Set filter envelope amount for all voices.
    pub fn set_filter_env_amount(&mut self, amount: f32) {
        self.filter_env_amount = amount;
        for voice in self.voices.voices_mut() {
            voice.set_filter_env_amount(amount);
        }
    }

    /// Set amplitude envelope attack for all voices.
    pub fn set_amp_attack(&mut self, ms: f32) {
        for voice in self.voices.voices_mut() {
            voice.amp_env.set_attack_ms(ms);
        }
    }

    /// Set amplitude envelope decay for all voices.
    pub fn set_amp_decay(&mut self, ms: f32) {
        for voice in self.voices.voices_mut() {
            voice.amp_env.set_decay_ms(ms);
        }
    }

    /// Set amplitude envelope sustain for all voices.
    pub fn set_amp_sustain(&mut self, level: f32) {
        for voice in self.voices.voices_mut() {
            voice.amp_env.set_sustain(level);
        }
    }

    /// Set amplitude envelope release for all voices.
    pub fn set_amp_release(&mut self, ms: f32) {
        for voice in self.voices.voices_mut() {
            voice.amp_env.set_release_ms(ms);
        }
    }

    /// Set LFO 1 rate.
    pub fn set_lfo1_rate(&mut self, hz: f32) {
        self.lfo1.set_frequency(hz);
    }

    /// Set LFO 1 waveform.
    pub fn set_lfo1_waveform(&mut self, waveform: LfoWaveform) {
        self.lfo1.set_waveform(waveform);
    }

    /// Set LFO 1 to pitch modulation.
    pub fn set_lfo1_to_pitch(&mut self, semitones: f32) {
        self.lfo1_to_pitch = semitones;
    }

    /// Set LFO 1 to filter modulation.
    pub fn set_lfo1_to_filter(&mut self, hz: f32) {
        self.lfo1_to_filter = hz;
    }

    fn update_voice_params(&mut self) {
        for voice in self.voices.voices_mut() {
            voice.set_osc1_waveform(self.osc1_waveform);
            voice.set_osc2_waveform(self.osc2_waveform);
            voice.set_osc2_detune(self.osc2_detune);
            voice.set_osc_mix(self.osc_mix);
            voice.set_filter_cutoff(self.filter_cutoff);
            voice.filter.set_resonance(self.filter_resonance);
            voice.set_filter_env_amount(self.filter_env_amount);
        }
    }

    /// Trigger a note.
    pub fn note_on(&mut self, note: u8, velocity: u8) {
        self.voices.note_on(note, velocity);
    }

    /// Release a note.
    pub fn note_off(&mut self, note: u8) {
        self.voices.note_off(note);
    }

    /// Stop all notes.
    pub fn all_notes_off(&mut self) {
        self.voices.all_notes_off();
    }

    /// Get number of active voices.
    pub fn active_voice_count(&self) -> usize {
        self.voices.active_voice_count()
    }

    /// Process one sample.
    #[inline]
    pub fn process(&mut self) -> f32 {
        // Get global LFO values
        let lfo1_val = self.lfo1.advance();
        let _lfo2_val = self.lfo2.advance();

        // Apply LFO modulation via Voice external mod fields
        let pitch_mod = lfo1_val * self.lfo1_to_pitch; // semitones
        let filter_mod = lfo1_val * self.lfo1_to_filter; // Hz

        for voice in self.voices.voices_mut() {
            voice.set_external_pitch_mod(pitch_mod);
            voice.set_external_filter_mod(filter_mod);
        }

        // Sum all voices
        self.voices.process()
    }

    /// Process stereo output from all voices.
    #[inline]
    pub fn process_stereo(&mut self) -> (f32, f32) {
        let lfo1_val = self.lfo1.advance();
        let _lfo2_val = self.lfo2.advance();

        let pitch_mod = lfo1_val * self.lfo1_to_pitch;
        let filter_mod = lfo1_val * self.lfo1_to_filter;

        for voice in self.voices.voices_mut() {
            voice.set_external_pitch_mod(pitch_mod);
            voice.set_external_filter_mod(filter_mod);
        }

        self.voices.process_stereo()
    }

    /// Reset the synthesizer.
    pub fn reset(&mut self) {
        self.voices.reset();
        self.lfo1.reset();
        self.lfo2.reset();
    }
}

/// Synthesizer wrapped as an `Effect` node for use in a `ProcessingGraph`.
///
/// `SynthNode` exposes a fixed 8-voice polyphonic synthesizer as a graph node.
/// The incoming audio signal is **ignored** — the synth is a source, not a
/// processor. Notes are triggered via [`note_on`](SynthNode::note_on) and
/// [`note_off`](SynthNode::note_off) externally (e.g., from a MIDI parser).
///
/// ## Graph Placement
///
/// Place at the head of a linear chain or as a source node in a DAG:
///
/// ```rust,ignore
/// let mut engine = GraphEngine::new_linear(48000.0, 256);
/// let slot = engine.add_effect_named(Box::new(SynthNode::new(48000.0)), "synth");
/// ```
///
/// ## True Stereo
///
/// `SynthNode` returns `true` from `is_true_stereo()` because the polyphonic
/// synth spreads unison voices across the stereo field.
#[derive(Debug)]
pub struct SynthNode {
    /// Internal polyphonic synthesizer (8 voices).
    synth: PolyphonicSynth<8>,
}

impl SynthNode {
    /// Create a new synthesizer graph node at the given sample rate.
    pub fn new(sample_rate: f32) -> Self {
        Self {
            synth: PolyphonicSynth::new(sample_rate),
        }
    }

    /// Get mutable access to the inner synthesizer for configuration.
    pub fn synth_mut(&mut self) -> &mut PolyphonicSynth<8> {
        &mut self.synth
    }

    /// Get read access to the inner synthesizer.
    pub fn synth(&self) -> &PolyphonicSynth<8> {
        &self.synth
    }

    /// Trigger a note on event.
    ///
    /// # Arguments
    /// * `note` — MIDI note number (0-127)
    /// * `velocity` — MIDI velocity (0-127)
    pub fn note_on(&mut self, note: u8, velocity: u8) {
        self.synth.note_on(note, velocity);
    }

    /// Trigger a note off event.
    ///
    /// # Arguments
    /// * `note` — MIDI note number (0-127)
    pub fn note_off(&mut self, note: u8) {
        self.synth.note_off(note);
    }

    /// Stop all active notes immediately.
    pub fn all_notes_off(&mut self) {
        self.synth.all_notes_off();
    }

    /// Get number of active voices.
    pub fn active_voice_count(&self) -> usize {
        self.synth.active_voice_count()
    }
}

impl Effect for SynthNode {
    /// Ignore the input signal; return synthesized stereo output.
    ///
    /// The incoming `left`/`right` values are discarded. The synth generates
    /// audio from its internal voice pool only.
    fn process_stereo(&mut self, _left: f32, _right: f32) -> (f32, f32) {
        self.synth.process_stereo()
    }

    fn is_true_stereo(&self) -> bool {
        true
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.synth.set_sample_rate(sample_rate);
    }

    fn reset(&mut self) {
        self.synth.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_monophonic_synth_basic() {
        let mut synth = MonophonicSynth::new(48000.0);

        synth.note_on(69, 100); // A4

        let mut sum = 0.0;
        for _ in 0..1000 {
            sum += synth.process().abs();
        }

        assert!(sum > 0.0, "Synth should produce output");
    }

    #[test]
    fn test_monophonic_synth_filter() {
        let mut synth = MonophonicSynth::new(48000.0);
        synth.set_filter_cutoff(500.0);
        synth.set_filter_resonance(4.0);

        synth.note_on(60, 100);

        // Just verify it doesn't crash and produces output
        let mut sum = 0.0;
        for _ in 0..1000 {
            sum += synth.process().abs();
        }
        assert!(sum > 0.0);
    }

    #[test]
    fn test_monophonic_synth_glide() {
        let mut synth = MonophonicSynth::new(48000.0);
        synth.set_glide_time(100.0); // 100ms glide

        synth.note_on(60, 100);

        // Process some samples
        for _ in 0..1000 {
            synth.process();
        }

        // Play another note - should glide
        synth.note_on(72, 100);

        // Frequency should start gliding
        let initial_freq = synth.current_freq;

        for _ in 0..100 {
            synth.process();
        }

        // Frequency should have changed
        assert!(
            (synth.current_freq - initial_freq).abs() > 1.0,
            "Frequency should be gliding"
        );
    }

    #[test]
    fn test_polyphonic_synth_basic() {
        let mut synth: PolyphonicSynth<4> = PolyphonicSynth::new(48000.0);

        synth.note_on(60, 100);
        synth.note_on(64, 100);
        synth.note_on(67, 100);

        assert_eq!(synth.active_voice_count(), 3);

        let mut sum = 0.0;
        for _ in 0..1000 {
            sum += synth.process().abs();
        }

        assert!(sum > 0.0, "Synth should produce output");
    }

    #[test]
    fn test_polyphonic_synth_voice_stealing() {
        let mut synth: PolyphonicSynth<2> = PolyphonicSynth::new(48000.0);

        synth.note_on(60, 100);
        synth.note_on(64, 100);
        synth.note_on(67, 100); // Should steal a voice

        assert_eq!(synth.active_voice_count(), 2);
    }

    #[test]
    fn test_polyphonic_synth_lfo_modulation() {
        let mut synth: PolyphonicSynth<4> = PolyphonicSynth::new(48000.0);
        synth.set_lfo1_rate(5.0);
        synth.set_lfo1_to_pitch(0.5); // Half semitone vibrato

        synth.note_on(69, 100);

        // Process and verify no crashes
        for _ in 0..10000 {
            let sample = synth.process();
            assert!(sample.is_finite());
        }
    }

    #[test]
    fn test_polyphonic_synth_reset() {
        let mut synth: PolyphonicSynth<4> = PolyphonicSynth::new(48000.0);

        synth.note_on(60, 100);
        synth.note_on(64, 100);

        synth.reset();

        assert_eq!(synth.active_voice_count(), 0);
    }

    // --- SynthNode tests ---

    #[test]
    fn test_synth_node_produces_output_when_playing() {
        let mut node = SynthNode::new(48000.0);
        node.note_on(60, 100);

        // Effect::process_stereo: input is ignored; output is from synth
        let mut sum = 0.0f32;
        for _ in 0..1000 {
            let (l, r) = node.process_stereo(0.0, 0.0);
            sum += l.abs() + r.abs();
        }
        assert!(
            sum > 0.0,
            "SynthNode should produce output when a note is on"
        );
    }

    #[test]
    fn test_synth_node_ignores_input() {
        let mut node_zero_in = SynthNode::new(48000.0);
        let mut node_loud_in = SynthNode::new(48000.0);

        node_zero_in.note_on(60, 100);
        node_loud_in.note_on(60, 100);

        // Feed silence vs. loud signal to both
        let mut diff = 0.0f32;
        for _ in 0..1000 {
            let (l0, r0) = node_zero_in.process_stereo(0.0, 0.0);
            let (l1, r1) = node_loud_in.process_stereo(1.0, 1.0);
            diff += (l0 - l1).abs() + (r0 - r1).abs();
        }
        assert!(
            diff < 1e-5,
            "SynthNode output must not depend on input signal (diff={diff})"
        );
    }

    #[test]
    fn test_synth_node_silent_when_no_notes() {
        let mut node = SynthNode::new(48000.0);
        // No note on — output should be silent
        let mut sum = 0.0f32;
        for _ in 0..100 {
            let (l, r) = node.process_stereo(0.5, 0.5);
            sum += l.abs() + r.abs();
        }
        assert!(
            sum < 1e-5,
            "SynthNode with no notes should be silent (sum={sum})"
        );
    }

    #[test]
    fn test_synth_node_is_true_stereo() {
        let node = SynthNode::new(48000.0);
        assert!(node.is_true_stereo());
    }

    #[test]
    fn test_synth_node_reset_stops_audio() {
        let mut node = SynthNode::new(48000.0);
        node.note_on(60, 100);
        for _ in 0..100 {
            node.process_stereo(0.0, 0.0);
        }

        node.reset();
        assert_eq!(node.active_voice_count(), 0);

        let (l, r) = node.process_stereo(0.0, 0.0);
        assert!(
            l.abs() < 1e-5 && r.abs() < 1e-5,
            "After reset, output should be silent"
        );
    }

    #[test]
    fn test_synth_node_note_off() {
        let mut node = SynthNode::new(48000.0);
        node.note_on(60, 100);
        assert_eq!(node.active_voice_count(), 1);

        node.note_off(60);
        // Voice is in release phase but still counts as active
        // After all_notes_off it should be 0
        node.all_notes_off();
        assert_eq!(node.active_voice_count(), 0);
    }
}
