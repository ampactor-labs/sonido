//! Modulation matrix for flexible parameter routing.
//!
//! Provides a system for routing modulation sources to destinations
//! with configurable amounts and bipolar/unipolar scaling.

/// Modulation source identifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModSourceId {
    /// LFO 1
    Lfo1,
    /// LFO 2
    Lfo2,
    /// Amplitude envelope
    AmpEnv,
    /// Filter envelope
    FilterEnv,
    /// Modulation envelope
    ModEnv,
    /// Velocity
    Velocity,
    /// Aftertouch / channel pressure
    Aftertouch,
    /// Mod wheel (CC1)
    ModWheel,
    /// Pitch bend
    PitchBend,
    /// Audio input (envelope follower)
    AudioIn,
    /// Note number (for keyboard tracking)
    KeyTrack,
    /// Custom source 1
    Custom1,
    /// Custom source 2
    Custom2,
}

/// Modulation destination identifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModDestination {
    /// Oscillator 1 pitch (in semitones)
    Osc1Pitch,
    /// Oscillator 2 pitch (in semitones)
    Osc2Pitch,
    /// Oscillator 1 pulse width
    Osc1PulseWidth,
    /// Oscillator 2 pulse width
    Osc2PulseWidth,
    /// Oscillator mix
    OscMix,
    /// Filter cutoff frequency
    FilterCutoff,
    /// Filter resonance
    FilterResonance,
    /// Amplitude / VCA level
    Amplitude,
    /// Pan position
    Pan,
    /// LFO 1 rate
    Lfo1Rate,
    /// LFO 2 rate
    Lfo2Rate,
    /// Effect parameter 1
    EffectParam1,
    /// Effect parameter 2
    EffectParam2,
}

/// A single modulation route.
#[derive(Clone, Copy, Debug)]
pub struct ModulationRoute {
    /// Source of modulation
    pub source: ModSourceId,
    /// Destination parameter
    pub destination: ModDestination,
    /// Modulation amount (-1.0 to 1.0, negative inverts)
    pub amount: f32,
    /// Whether the source is bipolar (centered at 0)
    pub bipolar: bool,
}

impl ModulationRoute {
    /// Create a new modulation route.
    pub fn new(source: ModSourceId, destination: ModDestination, amount: f32) -> Self {
        Self {
            source,
            destination,
            amount: amount.clamp(-1.0, 1.0),
            bipolar: true,
        }
    }

    /// Create a unipolar modulation route (source is 0 to 1).
    pub fn unipolar(source: ModSourceId, destination: ModDestination, amount: f32) -> Self {
        Self {
            source,
            destination,
            amount: amount.clamp(-1.0, 1.0),
            bipolar: false,
        }
    }
}

/// Modulation matrix with a fixed number of routing slots.
///
/// Manages modulation routing from sources to destinations.
/// Each route has an amount that scales the modulation signal.
///
/// # Example
///
/// ```rust
/// use sonido_synth::{ModulationMatrix, ModulationRoute, ModSourceId, ModDestination};
///
/// let mut matrix: ModulationMatrix<8> = ModulationMatrix::new();
///
/// // Route LFO1 to filter cutoff
/// matrix.add_route(ModulationRoute::new(
///     ModSourceId::Lfo1,
///     ModDestination::FilterCutoff,
///     0.5,
/// ));
///
/// // Route filter envelope to filter cutoff
/// matrix.add_route(ModulationRoute::unipolar(
///     ModSourceId::FilterEnv,
///     ModDestination::FilterCutoff,
///     0.8,
/// ));
/// ```
#[derive(Debug, Clone)]
pub struct ModulationMatrix<const N: usize> {
    routes: [Option<ModulationRoute>; N],
    route_count: usize,
}

impl<const N: usize> Default for ModulationMatrix<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> ModulationMatrix<N> {
    /// Create a new empty modulation matrix.
    pub fn new() -> Self {
        Self {
            routes: [None; N],
            route_count: 0,
        }
    }

    /// Add a modulation route.
    ///
    /// Returns `true` if the route was added, `false` if the matrix is full.
    pub fn add_route(&mut self, route: ModulationRoute) -> bool {
        if self.route_count >= N {
            return false;
        }

        self.routes[self.route_count] = Some(route);
        self.route_count += 1;
        true
    }

    /// Remove a modulation route by index.
    pub fn remove_route(&mut self, index: usize) -> Option<ModulationRoute> {
        if index >= self.route_count {
            return None;
        }

        let route = self.routes[index].take();

        // Shift remaining routes down
        for i in index..self.route_count - 1 {
            self.routes[i] = self.routes[i + 1].take();
        }
        self.route_count -= 1;

        route
    }

    /// Clear all routes.
    pub fn clear(&mut self) {
        self.routes.fill(None);
        self.route_count = 0;
    }

    /// Get number of active routes.
    pub fn route_count(&self) -> usize {
        self.route_count
    }

    /// Get maximum number of routes.
    pub fn capacity(&self) -> usize {
        N
    }

    /// Get a route by index.
    pub fn get_route(&self, index: usize) -> Option<&ModulationRoute> {
        if index < self.route_count {
            self.routes[index].as_ref()
        } else {
            None
        }
    }

    /// Get a mutable route by index.
    pub fn get_route_mut(&mut self, index: usize) -> Option<&mut ModulationRoute> {
        if index < self.route_count {
            self.routes[index].as_mut()
        } else {
            None
        }
    }

    /// Iterate over active routes.
    pub fn iter(&self) -> impl Iterator<Item = &ModulationRoute> {
        self.routes[..self.route_count]
            .iter()
            .filter_map(|r| r.as_ref())
    }

    /// Calculate total modulation for a destination.
    ///
    /// Returns the sum of all modulation amounts targeting the specified destination.
    pub fn get_modulation(&self, destination: ModDestination, sources: &ModulationValues) -> f32 {
        let mut total = 0.0;

        for route in self.iter() {
            if route.destination == destination {
                let source_value = sources.get(route.source);
                let scaled = if route.bipolar {
                    source_value * route.amount
                } else {
                    // Unipolar: map 0-1 to -1-1 if amount is negative
                    let unipolar = (source_value + 1.0) * 0.5;
                    unipolar * route.amount
                };
                total += scaled;
            }
        }

        total
    }
}

/// Container for current modulation source values.
#[derive(Debug, Clone, Default)]
pub struct ModulationValues {
    /// LFO 1 value (-1 to 1)
    pub lfo1: f32,
    /// LFO 2 value (-1 to 1)
    pub lfo2: f32,
    /// Amplitude envelope value (0 to 1)
    pub amp_env: f32,
    /// Filter envelope value (0 to 1)
    pub filter_env: f32,
    /// Modulation envelope value (0 to 1)
    pub mod_env: f32,
    /// Velocity (0 to 1)
    pub velocity: f32,
    /// Aftertouch (0 to 1)
    pub aftertouch: f32,
    /// Mod wheel (0 to 1)
    pub mod_wheel: f32,
    /// Pitch bend (-1 to 1)
    pub pitch_bend: f32,
    /// Audio input envelope (0 to 1)
    pub audio_in: f32,
    /// Key tracking (-1 to 1, centered at middle C)
    pub key_track: f32,
    /// Custom source 1
    pub custom1: f32,
    /// Custom source 2
    pub custom2: f32,
}

impl ModulationValues {
    /// Create new modulation values with all sources at zero.
    pub fn new() -> Self {
        Self::default()
    }

    /// Get value for a specific source.
    pub fn get(&self, source: ModSourceId) -> f32 {
        match source {
            ModSourceId::Lfo1 => self.lfo1,
            ModSourceId::Lfo2 => self.lfo2,
            ModSourceId::AmpEnv => self.amp_env,
            ModSourceId::FilterEnv => self.filter_env,
            ModSourceId::ModEnv => self.mod_env,
            ModSourceId::Velocity => self.velocity,
            ModSourceId::Aftertouch => self.aftertouch,
            ModSourceId::ModWheel => self.mod_wheel,
            ModSourceId::PitchBend => self.pitch_bend,
            ModSourceId::AudioIn => self.audio_in,
            ModSourceId::KeyTrack => self.key_track,
            ModSourceId::Custom1 => self.custom1,
            ModSourceId::Custom2 => self.custom2,
        }
    }

    /// Set value for a specific source.
    pub fn set(&mut self, source: ModSourceId, value: f32) {
        match source {
            ModSourceId::Lfo1 => self.lfo1 = value,
            ModSourceId::Lfo2 => self.lfo2 = value,
            ModSourceId::AmpEnv => self.amp_env = value,
            ModSourceId::FilterEnv => self.filter_env = value,
            ModSourceId::ModEnv => self.mod_env = value,
            ModSourceId::Velocity => self.velocity = value,
            ModSourceId::Aftertouch => self.aftertouch = value,
            ModSourceId::ModWheel => self.mod_wheel = value,
            ModSourceId::PitchBend => self.pitch_bend = value,
            ModSourceId::AudioIn => self.audio_in = value,
            ModSourceId::KeyTrack => self.key_track = value,
            ModSourceId::Custom1 => self.custom1 = value,
            ModSourceId::Custom2 => self.custom2 = value,
        }
    }

    /// Set key tracking from MIDI note number.
    ///
    /// Centers at middle C (note 60), ranges from -1 to 1.
    pub fn set_key_track_from_note(&mut self, note: u8) {
        // Center at note 60, full range over ~5 octaves
        self.key_track = ((note as f32 - 60.0) / 60.0).clamp(-1.0, 1.0);
    }

    /// Set velocity from MIDI velocity (0-127).
    pub fn set_velocity_from_midi(&mut self, velocity: u8) {
        self.velocity = velocity as f32 / 127.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_modulation_route_creation() {
        let route = ModulationRoute::new(ModSourceId::Lfo1, ModDestination::FilterCutoff, 0.5);
        assert_eq!(route.source, ModSourceId::Lfo1);
        assert_eq!(route.destination, ModDestination::FilterCutoff);
        assert!((route.amount - 0.5).abs() < 0.001);
        assert!(route.bipolar);
    }

    #[test]
    fn test_modulation_route_unipolar() {
        let route =
            ModulationRoute::unipolar(ModSourceId::FilterEnv, ModDestination::FilterCutoff, 0.8);
        assert!(!route.bipolar);
    }

    #[test]
    fn test_modulation_matrix_add_route() {
        let mut matrix: ModulationMatrix<4> = ModulationMatrix::new();

        assert_eq!(matrix.route_count(), 0);

        let added = matrix.add_route(ModulationRoute::new(
            ModSourceId::Lfo1,
            ModDestination::Osc1Pitch,
            0.5,
        ));
        assert!(added);
        assert_eq!(matrix.route_count(), 1);
    }

    #[test]
    fn test_modulation_matrix_full() {
        let mut matrix: ModulationMatrix<2> = ModulationMatrix::new();

        matrix.add_route(ModulationRoute::new(
            ModSourceId::Lfo1,
            ModDestination::Osc1Pitch,
            0.5,
        ));
        matrix.add_route(ModulationRoute::new(
            ModSourceId::Lfo2,
            ModDestination::Osc2Pitch,
            0.5,
        ));

        // Third should fail
        let added = matrix.add_route(ModulationRoute::new(
            ModSourceId::AmpEnv,
            ModDestination::Amplitude,
            0.5,
        ));
        assert!(!added);
        assert_eq!(matrix.route_count(), 2);
    }

    #[test]
    fn test_modulation_matrix_remove_route() {
        let mut matrix: ModulationMatrix<4> = ModulationMatrix::new();

        matrix.add_route(ModulationRoute::new(
            ModSourceId::Lfo1,
            ModDestination::Osc1Pitch,
            0.5,
        ));
        matrix.add_route(ModulationRoute::new(
            ModSourceId::Lfo2,
            ModDestination::Osc2Pitch,
            0.3,
        ));

        let removed = matrix.remove_route(0);
        assert!(removed.is_some());
        assert_eq!(matrix.route_count(), 1);

        // Remaining route should now be at index 0
        let route = matrix.get_route(0).unwrap();
        assert_eq!(route.source, ModSourceId::Lfo2);
    }

    #[test]
    fn test_modulation_matrix_get_modulation() {
        let mut matrix: ModulationMatrix<4> = ModulationMatrix::new();

        matrix.add_route(ModulationRoute::new(
            ModSourceId::Lfo1,
            ModDestination::FilterCutoff,
            0.5,
        ));
        matrix.add_route(ModulationRoute::new(
            ModSourceId::FilterEnv,
            ModDestination::FilterCutoff,
            0.3,
        ));

        let mut values = ModulationValues::new();
        values.lfo1 = 1.0;
        values.filter_env = 0.5;

        let mod_amount = matrix.get_modulation(ModDestination::FilterCutoff, &values);

        // LFO1: 1.0 * 0.5 = 0.5
        // FilterEnv: 0.5 * 0.3 = 0.15
        // Total: 0.65
        assert!(
            (mod_amount - 0.65).abs() < 0.001,
            "Expected 0.65, got {}",
            mod_amount
        );
    }

    #[test]
    fn test_modulation_values() {
        let mut values = ModulationValues::new();

        values.set_velocity_from_midi(127);
        assert!((values.velocity - 1.0).abs() < 0.001);

        values.set_velocity_from_midi(64);
        assert!((values.velocity - 0.504).abs() < 0.01);

        values.set_key_track_from_note(60); // Middle C
        assert!(values.key_track.abs() < 0.001);

        values.set_key_track_from_note(72); // One octave up
        assert!(values.key_track > 0.0);

        values.set_key_track_from_note(48); // One octave down
        assert!(values.key_track < 0.0);
    }
}
