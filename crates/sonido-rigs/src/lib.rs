//! Sonido's factory rigs: one set for the pedal, the graph-player plugin and
//! the editor.
//!
//! Each rig is an ordinary [`Patch`] built from effects in the registry. The
//! pedal firmware compiles them into its image as encoded sectors (its build
//! script calls [`factory_rigs`]); the graph player and the editor list them
//! directly. Their tests hold every rig to the pedal's CPU and memory rule
//! and level-match it to the dry guitar.
//!
//! # Knobs
//!
//! Every rig maps its six macros the pedal's way: the top row is levels, the
//! bottom row is the sound (see `sonido_platform::knob_mapping::knob_map`).
//! K1 input gain (±12 dB, 0 at noon), K2 morph time (how long a glide
//! between A and B takes: 0.2 s, 1 s at noon, 5 s), K3 master volume (±12 dB
//! around the rig's level-matching trim, generated into [`trims`] by this
//! crate's `calibrate` example), and K4–K6 the rig's own three controls. The
//! A/B morph moves the parameters the macros leave alone, so a knob never
//! fights the morph.
//!
//! # Toggles
//!
//! Every rig gives T1 and T3 a job (`Patch::toggles`): each switches part of
//! its sound three ways, such as Shimmer Cathedral's interval or Fuzz Lab's
//! clipping. A toggle sets only what the morph and the knobs leave alone, and
//! its middle position is the rig's own value, so the rig starts as its trim
//! was measured and every position plays near the same loudness.

pub mod guide;
pub mod trims;

use sonido_core::{GlobalParam, MacroTarget, MorphCurve};
use sonido_patch::{
    MacroDef, MacroMappingSpec, Patch, PatchEdge, PatchEndpoint, PatchNode, ToggleDef,
    ToggleMappingSpec,
};
use sonido_registry::{EffectRegistry, effect_uid};

/// A rig being built: nodes with A/B values set by parameter name.
struct Rig<'r> {
    registry: &'r EffectRegistry,
    patch: Patch,
    ids: Vec<&'static str>,
}

/// Parameter values by name (name, short name or string id, loosely matched).
type Values<'a> = &'a [(&'a str, f32)];

impl<'r> Rig<'r> {
    fn new(registry: &'r EffectRegistry, name: &str) -> Self {
        Self {
            registry,
            patch: Patch::new(name),
            ids: Vec::new(),
        }
    }

    fn param(&self, node: usize, name: &str) -> usize {
        let id = self.ids[node];
        let effect = self
            .registry
            .create(id, 48_000.0)
            .expect("registered effect");
        (0..effect.effect_param_count())
            .find(|&i| {
                effect
                    .effect_param_info(i)
                    .is_some_and(|d| d.matches_key(name))
            })
            .unwrap_or_else(|| panic!("{}: {id} has no parameter {name:?}", self.patch.name))
    }

    /// Add `id` with its defaults, then `a` (both states) and `b` (B only).
    fn fx(&mut self, id: &'static str, a: Values, b: Values) -> PatchEndpoint {
        let effect = self
            .registry
            .create(id, 48_000.0)
            .unwrap_or_else(|| panic!("{}: no effect {id:?}", self.patch.name));
        let defaults: Vec<f32> = (0..effect.effect_param_count())
            .map(|i| effect.effect_get_param(i))
            .collect();
        let uid = effect_uid(id).expect("registered uid");
        self.ids.push(id);
        let node = self.ids.len() - 1;
        let mut patch_node = PatchNode::new(uid, defaults);
        for (states, values) in [(&[0usize, 1][..], a), (&[1usize][..], b)] {
            for &(name, value) in values {
                let i = self.param(node, name);
                let desc = effect.effect_param_info(i).unwrap();
                assert!(
                    (desc.min..=desc.max).contains(&value),
                    "{}: {id} {name} = {value} is outside {}..{}",
                    self.patch.name,
                    desc.min,
                    desc.max
                );
                for &s in states {
                    let snapshot = if s == 0 {
                        &mut patch_node.params_a
                    } else {
                        &mut patch_node.params_b
                    };
                    snapshot[i] = value;
                }
            }
        }
        self.patch.nodes.push(patch_node);
        PatchEndpoint::Node(node as u8)
    }

    fn wire(&mut self, path: &[PatchEndpoint]) {
        for pair in path.windows(2) {
            self.patch.edges.push(PatchEdge::new(pair[0], pair[1]));
        }
    }

    /// Macro `k` (0-based; 3–5 are the rig's own K4–K6) moves node `node`'s
    /// `param` across `min..=max`.
    fn knob(
        &mut self,
        k: usize,
        label: &str,
        node: PatchEndpoint,
        param: &str,
        (min, max): (f32, f32),
        curve: MorphCurve,
    ) {
        let PatchEndpoint::Node(n) = node else {
            panic!("macros target effect nodes");
        };
        let p = self.param(n as usize, param);
        self.patch.macros[k] = MacroDef {
            name: label.into(),
            mappings: vec![MacroMappingSpec {
                target: MacroTarget::Slot {
                    slot: n,
                    param: p as u8,
                },
                min,
                max,
                curve,
            }],
        };
    }

    /// Toggle `t` (0 = T1, 1 = T3) named `name`, with a label per position
    /// (up, middle, down), setting each node's `param` to its value there.
    ///
    /// Call after the knobs. Panics unless each param holds still through the
    /// morph, no knob turns it, the middle value is the rig's own and every
    /// value is in range (the crate rule: see the crate docs).
    fn toggle(
        &mut self,
        t: usize,
        name: &str,
        labels: [&str; 3],
        sets: &[(PatchEndpoint, &str, [f32; 3])],
    ) {
        let mappings = sets
            .iter()
            .map(|&(node, param, values)| {
                let PatchEndpoint::Node(n) = node else {
                    panic!("toggles target effect nodes");
                };
                let p = self.param(n as usize, param);
                let rig = &self.patch.name;
                let at = &self.patch.nodes[n as usize];
                assert_eq!(
                    at.params_a[p], at.params_b[p],
                    "{rig}: {name} sets {param}, which the morph moves"
                );
                assert!(
                    (at.params_a[p] - values[1]).abs() < 1e-6,
                    "{rig}: {name}'s middle {param} is {}, not the rig's {}",
                    values[1],
                    at.params_a[p]
                );
                let target = MacroTarget::Slot {
                    slot: n,
                    param: p as u8,
                };
                assert!(
                    !self
                        .patch
                        .macros
                        .iter()
                        .any(|m| m.mappings.iter().any(|m| m.target == target)),
                    "{rig}: {name} sets {param}, which a knob turns"
                );
                let effect = self.registry.create(self.ids[n as usize], 48_000.0);
                let desc = effect.and_then(|e| e.effect_param_info(p)).unwrap();
                for v in values {
                    assert!(
                        (desc.min..=desc.max).contains(&v),
                        "{rig}: {name} {param} = {v} is outside {}..{}",
                        desc.min,
                        desc.max
                    );
                }
                ToggleMappingSpec { target, values }
            })
            .collect();
        self.patch.toggles[t] = ToggleDef {
            name: name.into(),
            positions: labels.map(String::from),
            mappings,
        };
    }

    /// K1–K3 (see the crate docs); `trim_db` is the master volume at noon.
    fn finish(mut self, trim_db: f32) -> Patch {
        let global = |label: &str, target, min, max, curve| MacroDef {
            name: String::from(label),
            mappings: vec![MacroMappingSpec {
                target: MacroTarget::Global(target),
                min,
                max,
                curve,
            }],
        };
        self.patch.macros[0] = global(
            "Input",
            GlobalParam::InputGain,
            -12.0,
            12.0,
            MorphCurve::Linear,
        );
        // Speed is sweeps per second: 5 → 0.2 s, 1 at noon (geometric), 0.2 → 5 s.
        self.patch.macros[1] = global(
            "Morph Time",
            GlobalParam::MorphSpeed,
            5.0,
            0.2,
            MorphCurve::Logarithmic,
        );
        self.patch.macros[2] = global(
            "Master",
            GlobalParam::MasterVolume,
            trim_db - 12.0,
            trim_db + 12.0,
            MorphCurve::Linear,
        );
        self.patch.globals.master_volume_db = trim_db;
        self.patch
    }
}

use MorphCurve::{Linear, Logarithmic};
use PatchEndpoint::{Input, Merge, Output, Split};

/// An octave-up shimmer into a huge plate, beside a diffused delay.
fn shimmer_cathedral(r: &EffectRegistry) -> Patch {
    let mut rig = Rig::new(r, "Shimmer Cathedral");
    let pitch = rig.fx(
        "pitch_shift",
        &[("Semitones", 12.0), ("Grain Size", 40.0), ("Mix", 100.0)],
        &[],
    );
    let plate = rig.fx(
        "plate_reverb",
        &[
            ("Decay", 2.5),
            ("Size", 55.0),
            ("Damping", 40.0),
            ("Diffusion", 70.0),
            ("Pre-Delay", 30.0),
            ("Mix", 100.0),
        ],
        &[
            ("Decay", 9.5),
            ("Size", 100.0),
            ("Damping", 12.0),
            ("Diffusion", 95.0),
        ],
    );
    let delay = rig.fx(
        "delay",
        &[
            ("Delay Time", 420.0),
            ("Feedback", 25.0),
            ("Feedback LP", 7000.0),
            ("Feedback HP", 120.0),
            ("Diffusion", 20.0),
        ],
        &[("Feedback", 62.0), ("Diffusion", 75.0)],
    );
    rig.wire(&[Input, Split(0), pitch, plate, Merge(0), Output]);
    rig.wire(&[Split(0), delay, Merge(0)]);
    rig.knob(3, "Shimmer", pitch, "Mix", (0.0, 100.0), Linear);
    rig.knob(4, "Wash", plate, "Output", (-6.0, 6.0), Linear);
    rig.knob(5, "Echo", delay, "Mix", (0.0, 70.0), Linear);
    rig.toggle(
        0,
        "Interval",
        ["+19", "+12", "+7"],
        &[(pitch, "Semitones", [19.0, 12.0, 7.0])],
    );
    rig.toggle(
        1,
        "Echo",
        ["Long", "Mid", "Short"],
        &[(delay, "Delay Time", [630.0, 420.0, 280.0])],
    );
    rig.finish(trims::SHIMMER_CATHEDRAL)
}

/// Asymmetric fuzz through a resonant filter, beside a crusher into a ring
/// modulator, all into tape.
fn fuzz_lab(r: &EffectRegistry) -> Patch {
    let mut rig = Rig::new(r, "Fuzz Lab");
    let fuzz = rig.fx(
        "distortion",
        &[
            ("Drive", 30.0),
            ("Waveshape", 3.0),
            ("Tone", 2.0),
            ("Output", -10.0),
        ],
        &[],
    );
    let filter = rig.fx(
        "filter",
        &[("Cutoff", 1500.0), ("Resonance", 4.0), ("Type", 0.0)],
        &[("Resonance", 12.0)],
    );
    let crush = rig.fx(
        "bitcrusher",
        &[("Bit Depth", 8.0), ("Downsample", 2.0)],
        &[("Bit Depth", 5.0), ("Downsample", 16.0)],
    );
    let ring = rig.fx(
        "ringmod",
        &[
            ("Frequency", 180.0),
            ("Depth", 100.0),
            ("Waveform", 0.0),
            ("Mix", 60.0),
        ],
        &[("Frequency", 900.0), ("Waveform", 1.0)],
    );
    let tape = rig.fx(
        "tape",
        &[
            ("Drive", 4.0),
            ("Saturation", 40.0),
            ("Wow", 0.1),
            ("Flutter", 0.1),
            ("Output", -2.0),
        ],
        &[
            ("Wow", 0.8),
            ("Flutter", 0.6),
            ("Saturation", 80.0),
            ("Output", -5.5),
        ],
    );
    rig.wire(&[Input, Split(0), fuzz, filter, Merge(0), tape, Output]);
    rig.wire(&[Split(0), crush, ring, Merge(0)]);
    rig.knob(3, "Fuzz", fuzz, "Drive", (12.0, 40.0), Linear);
    rig.knob(4, "Filter", filter, "Cutoff", (300.0, 8000.0), Logarithmic);
    rig.knob(5, "Crush", crush, "Mix", (0.0, 100.0), Linear);
    rig.toggle(
        0,
        "Clip",
        ["Hard", "Asym", "Fold"],
        &[(fuzz, "Waveshape", [1.0, 3.0, 2.0])],
    );
    rig.toggle(
        1,
        "Filter",
        ["Band", "Low", "High"],
        &[(filter, "Type", [2.0, 0.0, 1.0])],
    );
    rig.finish(trims::FUZZ_LAB)
}

/// A tape echo: preamp, tape, delay, spring.
fn space_echo(r: &EffectRegistry) -> Patch {
    let mut rig = Rig::new(r, "Space Echo");
    let pre = rig.fx("preamp", &[("Tone", 2.0)], &[]);
    let tape = rig.fx(
        "tape",
        &[
            ("Drive", 6.0),
            ("Saturation", 35.0),
            ("HF Rolloff", 9000.0),
            ("Wow", 0.25),
            ("Flutter", 0.2),
            ("Head Bump", 0.4),
            ("Output", -4.0),
        ],
        &[("Wow", 0.7), ("Flutter", 0.5), ("Saturation", 60.0)],
    );
    let delay = rig.fx(
        "delay",
        &[
            ("Delay Time", 180.0),
            ("Feedback", 25.0),
            ("Feedback LP", 4500.0),
            ("Feedback HP", 120.0),
            ("Diffusion", 10.0),
        ],
        &[
            ("Delay Time", 480.0),
            ("Feedback", 88.0),
            ("Feedback LP", 2500.0),
        ],
    );
    let spring = rig.fx(
        "spring_reverb",
        &[("Decay", 2.0), ("Tension", 55.0), ("Drip", 50.0)],
        &[("Decay", 4.5)],
    );
    rig.wire(&[Input, pre, tape, delay, spring, Output]);
    rig.knob(3, "Echo", delay, "Mix", (0.0, 80.0), Linear);
    rig.knob(4, "Spring", spring, "Mix", (0.0, 60.0), Linear);
    rig.knob(5, "Grit", pre, "Gain", (0.0, 30.0), Linear);
    rig.toggle(
        0,
        "Tone",
        ["Bright", "Mid", "Dark"],
        &[(pre, "Tone", [6.0, 2.0, -3.0])],
    );
    rig.toggle(
        1,
        "Spring",
        ["Tight", "Med", "Loose"],
        &[(spring, "Tension", [85.0, 55.0, 25.0])],
    );
    rig.finish(trims::SPACE_ECHO)
}

/// Stutters beside a pitch-stretched grain cloud, into a plate.
fn granular_drift(r: &EffectRegistry) -> Patch {
    let mut rig = Rig::new(r, "Granular Drift");
    let glitch = rig.fx(
        "glitch",
        &[
            ("Mode", 0.0),
            ("Rate", 8.0),
            ("Depth", 40.0),
            ("Probability", 30.0),
        ],
        &[("Rate", 16.0), ("Probability", 80.0)],
    );
    let stretch = rig.fx(
        "time_stretch",
        &[
            ("Pitch Ratio", 2.0),
            ("Grain Size", 60.0),
            ("Density", 50.0),
            ("Randomize", 30.0),
        ],
        &[
            ("Pitch Ratio", 0.5),
            ("Grain Size", 90.0),
            ("Randomize", 80.0),
        ],
    );
    let grains = rig.fx(
        "texture",
        &[
            ("Density", 6.0),
            ("Size", 250.0),
            ("Scatter", 50.0),
            ("Pitch Var", 3.0),
            ("Mix", 100.0),
        ],
        &[("Density", 16.0), ("Scatter", 95.0), ("Pitch Var", 12.0)],
    );
    let plate = rig.fx(
        "plate_reverb",
        &[("Decay", 3.0), ("Mix", 40.0)],
        &[("Decay", 7.0)],
    );
    rig.wire(&[Input, Split(0), glitch, Merge(0), plate, Output]);
    rig.wire(&[Split(0), stretch, grains, Merge(0)]);
    rig.knob(3, "Cloud", grains, "Output", (-36.0, 6.0), Linear);
    rig.knob(4, "Stretch", stretch, "Output", (-36.0, 6.0), Linear);
    rig.knob(5, "Stutter", glitch, "Mix", (0.0, 100.0), Linear);
    rig.toggle(
        0,
        "Glitch",
        ["Reverse", "Stutter", "Shuffle"],
        &[(glitch, "Mode", [2.0, 0.0, 3.0])],
    );
    rig.toggle(
        1,
        "Grains",
        ["Big", "Mid", "Small"],
        &[(grains, "Size", [450.0, 250.0, 100.0])],
    );
    rig.finish(trims::GRANULAR_DRIFT)
}

/// The harmonic habitat, then chorus and delay.
fn harmonic_habitat(r: &EffectRegistry) -> Patch {
    let mut rig = Rig::new(r, "Harmonic Habitat");
    let habitat = rig.fx(
        "harmonic_habitat",
        &[
            ("Room Size", 50.0),
            ("Decay", 50.0),
            ("Harmonicity", 40.0),
            ("Tracking", 60.0),
            ("Memory", 50.0),
        ],
        &[
            ("Room Size", 90.0),
            ("Decay", 90.0),
            ("Harmonicity", 90.0),
            ("Memory", 95.0),
        ],
    );
    let chorus = rig.fx(
        "chorus",
        &[("Rate", 0.4), ("Depth", 40.0)],
        &[("Depth", 80.0)],
    );
    let delay = rig.fx(
        "delay",
        &[("Delay Time", 360.0), ("Feedback", 30.0)],
        &[("Feedback", 65.0)],
    );
    rig.wire(&[Input, habitat, chorus, delay, Output]);
    rig.knob(3, "Habitat", habitat, "Mix", (0.0, 100.0), Linear);
    rig.knob(4, "Chorus", chorus, "Mix", (0.0, 80.0), Linear);
    rig.knob(5, "Echo", delay, "Mix", (0.0, 70.0), Linear);
    rig.toggle(
        0,
        "Tail",
        ["Bright", "Mid", "Dark"],
        &[(habitat, "Damping", [15.0, 45.0, 80.0])],
    );
    rig.toggle(
        1,
        "Swirl",
        ["Fast", "Slow", "Drift"],
        &[(chorus, "Rate", [1.2, 0.4, 0.1])],
    );
    rig.finish(trims::HARMONIC_HABITAT)
}

/// Gate, amp and cabinet: a direct rig, edge of breakup to high-gain lead.
fn amp_stack(r: &EffectRegistry) -> Patch {
    let mut rig = Rig::new(r, "Amp Stack");
    let gate = rig.fx("gate", &[("Threshold", -62.0)], &[]);
    let amp = rig.fx(
        "amp",
        &[("Gain", 28.0), ("Sag", 20.0), ("Presence", 45.0)],
        // The master comes down as the gain goes up, like a real amp's.
        &[
            ("Gain", 85.0),
            ("Sag", 50.0),
            ("Presence", 65.0),
            ("Master", -16.4),
        ],
    );
    let cab = rig.fx("cabinet", &[("Mix", 100.0)], &[]);
    rig.wire(&[Input, gate, amp, cab, Output]);
    rig.knob(3, "Bass", amp, "Bass", (0.0, 100.0), Linear);
    rig.knob(4, "Mid", amp, "Mid", (0.0, 100.0), Linear);
    rig.knob(5, "Treble", amp, "Treble", (0.0, 100.0), Linear);
    rig.toggle(
        0,
        "Gate",
        ["Tight", "Normal", "Open"],
        &[(gate, "Threshold", [-50.0, -62.0, -80.0])],
    );
    rig.toggle(
        1,
        "Bright",
        ["On", "Off", "Off"],
        &[(amp, "Bright", [1.0, 0.0, 0.0])],
    );
    rig.finish(trims::AMP_STACK)
}

/// The factory rigs, in pedal order.
pub fn factory_rigs() -> Vec<Patch> {
    let registry = EffectRegistry::new();
    vec![
        shimmer_cathedral(&registry),
        fuzz_lab(&registry),
        space_echo(&registry),
        granular_drift(&registry),
        harmonic_habitat(&registry),
        amp_stack(&registry),
    ]
}
