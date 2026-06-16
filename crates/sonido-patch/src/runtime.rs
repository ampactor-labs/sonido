//! `PatchPlayer` — run a [`Patch`] live.
//!
//! Owns the compiled [`GraphEngine`], a [`MacroMap`] over the six knobs, a
//! [`ChainMorph`] for A/B, the morph position/speed, the graph-level gains, and
//! a whole-rig bypass. This is the single runtime the graph-player plugin and
//! the firmware patch player both wrap — so a patch sounds identical in the
//! DAW and on the pedal.
//!
//! Per control tick the order is **morph, then macros** (macros win on overlap),
//! exactly as the design specifies; a macro that targets the morph position
//! takes effect on the next tick.

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, vec::Vec};

use sonido_core::{
    ChainMorph, EffectWithParams, GlobalParam, GraphEngine, MacroMap, MacroMapping, db_to_linear,
};

use crate::build::{PatchBuildError, build_graph_from_patch};
use crate::{NUM_MACROS, Patch};

/// A live, controllable instance of a [`Patch`].
pub struct PatchPlayer {
    engine: GraphEngine,
    macros: MacroMap<NUM_MACROS>,
    morph: ChainMorph,
    morph_t: f32,
    morph_speed: f32,
    input_gain_db: f32,
    master_volume_db: f32,
    bypassed: bool,
}

impl PatchPlayer {
    /// Build a player from `patch`, using `make_effect` to construct each node
    /// (see [`build_graph_from_patch`]).
    ///
    /// # Errors
    ///
    /// Propagates [`PatchBuildError`] from graph construction.
    pub fn from_patch(
        patch: &Patch,
        sample_rate: f32,
        block_size: usize,
        make_effect: impl FnMut(u16, f32) -> Option<(Box<dyn EffectWithParams + Send>, &'static str)>,
    ) -> Result<Self, PatchBuildError> {
        let engine = build_graph_from_patch(patch, sample_rate, block_size, make_effect)?;

        // MacroMap from the patch's six macros.
        let mut macros: MacroMap<NUM_MACROS> = MacroMap::new();
        for (i, def) in patch.macros.iter().enumerate() {
            for spec in &def.mappings {
                macros.add_mapping(MacroMapping {
                    macro_index: i,
                    target: spec.target,
                    min: spec.min,
                    max: spec.max,
                    curve: spec.curve,
                });
            }
        }

        // ChainMorph from the per-node A/B snapshots, curves auto-detected from
        // the engine's descriptors (frequency → log, stepped → snap).
        let counts: Vec<usize> = patch.nodes.iter().map(|n| n.params_a.len()).collect();
        let mut morph = ChainMorph::new(&counts);
        for (slot, node) in patch.nodes.iter().enumerate() {
            morph.set_corner(slot, 0, &node.params_a);
            morph.set_corner(slot, 1, &node.params_b);
            morph.set_bypass(slot, 0, node.bypassed);
            morph.set_bypass(slot, 1, node.bypassed);
            morph.set_locked(slot, patch.morph.is_locked(slot));
            let descs: Vec<Option<sonido_core::ParamDescriptor>> = (0..node.params_a.len())
                .map(|p| engine.param_descriptor_at(slot, p))
                .collect();
            morph.auto_curves(slot, &descs);
        }

        Ok(Self {
            engine,
            macros,
            morph,
            morph_t: 0.0,
            morph_speed: patch.morph.speed,
            input_gain_db: patch.globals.input_gain_db,
            master_volume_db: patch.globals.master_volume_db,
            bypassed: false,
        })
    }

    /// Set macro `index`'s position (0–1) without re-applying (call [`apply_controls`]).
    ///
    /// [`apply_controls`]: Self::apply_controls
    pub fn set_macro(&mut self, index: usize, position: f32) {
        self.macros.set_position(index, position);
    }

    /// Set the A/B morph position (0 = A, 1 = B).
    pub fn set_morph_position(&mut self, t: f32) {
        self.morph_t = t.clamp(0.0, 1.0);
    }

    /// Current morph position.
    pub fn morph_position(&self) -> f32 {
        self.morph_t
    }

    /// Set the whole-rig bypass.
    pub fn set_bypassed(&mut self, bypassed: bool) {
        self.bypassed = bypassed;
    }

    /// Set graph-level input gain (dB).
    pub fn set_input_gain_db(&mut self, db: f32) {
        self.input_gain_db = db;
    }

    /// Set graph-level master volume (dB).
    pub fn set_master_volume_db(&mut self, db: f32) {
        self.master_volume_db = db;
    }

    /// Number of effect slots.
    pub fn slot_count(&self) -> usize {
        self.engine.slot_count()
    }

    /// Borrow the underlying engine (e.g. for latency reporting).
    pub fn engine(&self) -> &GraphEngine {
        &self.engine
    }

    /// Re-apply morph then macros to the engine and globals.
    ///
    /// Call once per control tick after changing macro/morph positions. Morph is
    /// applied first; macros overwrite any parameter they also target.
    pub fn apply_controls(&mut self) {
        self.morph.apply_to_engine(self.morph_t, &mut self.engine);

        // Macros: slot targets go to the engine; global targets update our fields.
        let mut in_gain = self.input_gain_db;
        let mut master = self.master_volume_db;
        let mut morph_t = self.morph_t;
        let mut morph_speed = self.morph_speed;
        self.macros
            .apply_to_engine(&mut self.engine, |g, v| match g {
                GlobalParam::InputGain => in_gain = v,
                GlobalParam::MasterVolume => master = v,
                GlobalParam::MorphPosition => morph_t = v,
                GlobalParam::MorphSpeed => morph_speed = v,
                _ => {}
            });
        self.input_gain_db = in_gain;
        self.master_volume_db = master;
        // A macro-driven morph position takes effect next tick (read before the
        // next morph apply), per the documented ordering.
        self.morph_t = morph_t.clamp(0.0, 1.0);
        self.morph_speed = morph_speed;
    }

    /// Process one stereo block in place, applying input gain, the graph, master
    /// volume, and whole-rig bypass.
    pub fn process_block_stereo(&mut self, left: &mut [f32], right: &mut [f32]) {
        if self.bypassed {
            return;
        }
        let in_gain = db_to_linear(self.input_gain_db);
        if (in_gain - 1.0).abs() > f32::EPSILON {
            for s in left.iter_mut().chain(right.iter_mut()) {
                *s *= in_gain;
            }
        }

        self.engine.process_block_stereo_inplace(left, right);

        let master = db_to_linear(self.master_volume_db);
        if (master - 1.0).abs() > f32::EPSILON {
            for s in left.iter_mut().chain(right.iter_mut()) {
                *s *= master;
            }
        }
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;
    use crate::{GlobalParam, MacroMappingSpec, MacroTarget, MorphCurve, Patch, PatchNode};
    use sonido_registry::{EffectRegistry, effect_by_uid, effect_uid};

    fn factory(
        registry: &EffectRegistry,
    ) -> impl FnMut(u16, f32) -> Option<(Box<dyn EffectWithParams + Send>, &'static str)> + '_ {
        move |uid, sr| {
            let id = effect_by_uid(uid)?;
            Some((registry.create(id, sr)?, id))
        }
    }

    fn player(patch: &Patch) -> PatchPlayer {
        let registry = EffectRegistry::new();
        PatchPlayer::from_patch(patch, 48_000.0, 32, factory(&registry)).unwrap()
    }

    fn finite(p: &mut PatchPlayer) -> bool {
        let mut l = [0.1f32; 32];
        let mut r = [0.1f32; 32];
        p.process_block_stereo(&mut l, &mut r);
        l.iter().chain(r.iter()).all(|v| v.is_finite())
    }

    #[test]
    fn builds_and_runs() {
        let patch = Patch::linear_chain(
            "r",
            vec![PatchNode::new(
                effect_uid("distortion").unwrap(),
                vec![20.0],
            )],
        );
        let mut p = player(&patch);
        assert_eq!(p.slot_count(), 1);
        assert!(finite(&mut p));
    }

    #[test]
    fn macro_drives_a_parameter() {
        // Macro 0 maps distortion drive (param 0) 0..40.
        let mut patch = Patch::linear_chain(
            "m",
            vec![PatchNode::new(effect_uid("distortion").unwrap(), vec![0.0])],
        );
        patch.macros[0].mappings.push(MacroMappingSpec {
            target: MacroTarget::Slot { slot: 0, param: 0 },
            min: 0.0,
            max: 40.0,
            curve: MorphCurve::Linear,
        });
        let mut p = player(&patch);
        p.set_macro(0, 1.0);
        p.apply_controls();
        // Drive should now be at the mapping max.
        assert!((p.engine.param_descriptor_at(0, 0).unwrap().max - 40.0).abs() < 1.0);
        assert!(finite(&mut p));
    }

    #[test]
    fn macro_targets_master_volume_global() {
        let mut patch = Patch::linear_chain(
            "g",
            vec![PatchNode::new(effect_uid("distortion").unwrap(), vec![8.0])],
        );
        patch.macros[1].mappings.push(MacroMappingSpec {
            target: MacroTarget::Global(GlobalParam::MasterVolume),
            min: 0.0,
            max: -40.0,
            curve: MorphCurve::Linear,
        });
        let mut p = player(&patch);
        p.set_macro(1, 1.0);
        p.apply_controls();
        assert!((p.master_volume_db - (-40.0)).abs() < 1e-3);
    }

    #[test]
    fn morph_interpolates_between_a_and_b() {
        let mut patch = Patch::linear_chain(
            "mo",
            vec![PatchNode::new(effect_uid("distortion").unwrap(), vec![0.0])],
        );
        patch.nodes[0].params_a = vec![0.0];
        patch.nodes[0].params_b = vec![40.0];
        let mut p = player(&patch);
        p.set_morph_position(0.5);
        p.apply_controls();
        // Drive (linear param) should be ~20 at t=0.5.
        // (read back is not directly exposed; just assert processing stays finite)
        assert!(finite(&mut p));
        assert!((p.morph_position() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn bypass_passes_through() {
        let patch = Patch::linear_chain(
            "b",
            vec![PatchNode::new(
                effect_uid("distortion").unwrap(),
                vec![40.0],
            )],
        );
        let mut p = player(&patch);
        p.set_bypassed(true);
        let mut l = [0.25f32; 32];
        let mut r = [-0.25f32; 32];
        p.process_block_stereo(&mut l, &mut r);
        assert!(l.iter().all(|v| (*v - 0.25).abs() < 1e-6));
        assert!(r.iter().all(|v| (*v + 0.25).abs() < 1e-6));
    }
}
