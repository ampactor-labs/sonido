//! Toggle-driven page selection for embedded multi-effect control.
//!
//! [`ControlContext`] manages paged knob routing for hardware platforms with
//! limited physical controls (e.g., Hothouse with 6 knobs + 3 toggles).
//! Toggle positions select which page of parameters the knobs control.
//!
//! # Page Layout
//!
//! With 3 three-way toggles, there are 3 × 3 = 9 page combinations.
//! Each page addresses up to `KNOBS_PER_PAGE` parameters, giving
//! 9 × 6 = 54 addressable parameters from 6 physical knobs.
//!
//! # Example
//!
//! ```rust,ignore
//! use sonido_platform::{ControlContext, KnobPage, ControlId};
//!
//! let mut ctx = ControlContext::<6>::new();
//!
//! // Map page (0, 0) knob 0 to effect A param 0
//! ctx.page_mut(0, 0).map_knob(0, effect_a_id, 0);
//!
//! // Set toggle positions
//! ctx.set_toggles(0, 0);
//!
//! // Apply knob 0 value
//! let updated = ctx.apply_knob(0, 0.5, &mut graph);
//! ```

use crate::ControlId;
use sonido_core::graph::{NodeId, ProcessingGraph};

/// Number of toggle positions per toggle (3-way: left/center/right).
const TOGGLE_POSITIONS: usize = 3;

/// Total number of pages (3 toggles × 3 positions each = 9).
const PAGE_COUNT: usize = TOGGLE_POSITIONS * TOGGLE_POSITIONS;

/// A single page of knob-to-parameter mappings.
///
/// Each page holds up to `KNOBS` mapping entries, one per physical knob.
/// `None` means the knob is unmapped on this page.
#[derive(Debug, Clone)]
pub struct KnobPage<const KNOBS: usize> {
    /// Per-knob mapping: `(NodeId, param_index)`. `None` = unmapped.
    knobs: [Option<(NodeId, usize)>; KNOBS],
}

impl<const KNOBS: usize> KnobPage<KNOBS> {
    /// Creates an empty page (all knobs unmapped).
    const fn new() -> Self {
        Self {
            knobs: [None; KNOBS],
        }
    }

    /// Maps a knob to a graph node's parameter.
    ///
    /// `knob_index` must be `< KNOBS`, otherwise this is a no-op.
    pub fn map_knob(&mut self, knob_index: usize, node_id: NodeId, param_index: usize) {
        if knob_index < KNOBS {
            self.knobs[knob_index] = Some((node_id, param_index));
        }
    }

    /// Unmaps a knob on this page.
    pub fn unmap_knob(&mut self, knob_index: usize) {
        if knob_index < KNOBS {
            self.knobs[knob_index] = None;
        }
    }

    /// Returns the mapping for a knob, if any.
    #[inline]
    pub fn get_knob(&self, knob_index: usize) -> Option<(NodeId, usize)> {
        self.knobs.get(knob_index).copied().flatten()
    }

    /// Clears all knob mappings on this page.
    pub fn clear(&mut self) {
        self.knobs = [None; KNOBS];
    }
}

/// Toggle-driven page selector and knob applicator.
///
/// Manages 9 pages (3 × 3 toggle positions) of knob mappings. Each page
/// contains up to `KNOBS` entries. The active page is selected by two
/// toggle positions (toggle A and toggle B).
///
/// # Type Parameter
///
/// - `KNOBS`: Number of physical knobs per page (e.g., 6 for Hothouse)
///
/// # Page Addressing
///
/// Pages are indexed by `(toggle_a, toggle_b)` where each toggle value
/// is 0, 1, or 2. The flat page index is `toggle_a * 3 + toggle_b`.
#[derive(Debug, Clone)]
pub struct ControlContext<const KNOBS: usize> {
    pages: [KnobPage<KNOBS>; PAGE_COUNT],
    /// Current toggle A position (0, 1, or 2).
    toggle_a: usize,
    /// Current toggle B position (0, 1, or 2).
    toggle_b: usize,
}

impl<const KNOBS: usize> ControlContext<KNOBS> {
    /// Creates a new context with all pages empty.
    pub fn new() -> Self {
        Self {
            pages: core::array::from_fn(|_| KnobPage::new()),
            toggle_a: 0,
            toggle_b: 0,
        }
    }

    /// Sets the toggle positions, selecting the active page.
    ///
    /// Values are clamped to 0..2 (3-way toggle range).
    pub fn set_toggles(&mut self, toggle_a: usize, toggle_b: usize) {
        self.toggle_a = toggle_a.min(TOGGLE_POSITIONS - 1);
        self.toggle_b = toggle_b.min(TOGGLE_POSITIONS - 1);
    }

    /// Returns the current toggle positions.
    pub fn toggles(&self) -> (usize, usize) {
        (self.toggle_a, self.toggle_b)
    }

    /// Returns the flat page index for the current toggle positions.
    #[inline]
    fn active_page_index(&self) -> usize {
        self.toggle_a * TOGGLE_POSITIONS + self.toggle_b
    }

    /// Returns a reference to the active page.
    pub fn active_page(&self) -> &KnobPage<KNOBS> {
        &self.pages[self.active_page_index()]
    }

    /// Returns a mutable reference to a page by toggle coordinates.
    ///
    /// Values are clamped to 0..2.
    pub fn page_mut(&mut self, toggle_a: usize, toggle_b: usize) -> &mut KnobPage<KNOBS> {
        let a = toggle_a.min(TOGGLE_POSITIONS - 1);
        let b = toggle_b.min(TOGGLE_POSITIONS - 1);
        &mut self.pages[a * TOGGLE_POSITIONS + b]
    }

    /// Returns a reference to a page by toggle coordinates.
    pub fn page(&self, toggle_a: usize, toggle_b: usize) -> &KnobPage<KNOBS> {
        let a = toggle_a.min(TOGGLE_POSITIONS - 1);
        let b = toggle_b.min(TOGGLE_POSITIONS - 1);
        &self.pages[a * TOGGLE_POSITIONS + b]
    }

    /// Applies a knob value on the active page to the graph.
    ///
    /// Reads the active page's mapping for `knob_index`, denormalizes the
    /// value using the effect's parameter descriptor, and sets it.
    ///
    /// Returns `true` if a parameter was updated, `false` if the knob
    /// is unmapped or the parameter was not found.
    pub fn apply_knob(
        &self,
        knob_index: usize,
        normalized_value: f32,
        graph: &mut ProcessingGraph,
    ) -> bool {
        let page = self.active_page();
        if let Some((node_id, param_index)) = page.get_knob(knob_index)
            && let Some(ewp) = graph.effect_with_params_mut(node_id)
            && let Some(desc) = ewp.effect_param_info(param_index)
        {
            let value = desc.denormalize(normalized_value);
            ewp.effect_set_param(param_index, value);
            return true;
        }
        false
    }

    /// Applies all knob values on the active page from hardware control states.
    ///
    /// `knob_values` should be a slice of normalized values (0.0-1.0), one
    /// per physical knob. Returns the number of parameters updated.
    pub fn apply_all_knobs(&self, knob_values: &[f32], graph: &mut ProcessingGraph) -> usize {
        let mut updated = 0;
        for (i, &value) in knob_values.iter().take(KNOBS).enumerate() {
            if self.apply_knob(i, value, graph) {
                updated += 1;
            }
        }
        updated
    }

    /// Converts a hardware [`ControlId`] knob index to a page-relative knob index.
    ///
    /// Assumes hardware knobs use sequential indices starting from 0.
    /// Returns `None` if the control isn't a hardware control or index >= KNOBS.
    pub fn knob_index_from_control(&self, id: ControlId) -> Option<usize> {
        if id.is_hardware() {
            let idx = id.index() as usize;
            if idx < KNOBS {
                return Some(idx);
            }
        }
        None
    }

    /// Total number of addressable parameters across all pages.
    pub const fn total_addressable(&self) -> usize {
        PAGE_COUNT * KNOBS
    }

    /// Clears all pages.
    pub fn clear_all(&mut self) {
        for page in &mut self.pages {
            page.clear();
        }
    }
}

impl<const KNOBS: usize> Default for ControlContext<KNOBS> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    extern crate alloc;
    use super::*;
    use alloc::boxed::Box;
    use sonido_core::effect::Effect;
    use sonido_core::param_info::{ParamDescriptor, ParameterInfo};

    struct TestEffect {
        value: f32,
    }

    impl TestEffect {
        fn new() -> Self {
            Self { value: 0.0 }
        }
    }

    impl Effect for TestEffect {
        fn process(&mut self, input: f32) -> f32 {
            input
        }
        fn set_sample_rate(&mut self, _: f32) {}
        fn reset(&mut self) {}
    }

    impl ParameterInfo for TestEffect {
        fn param_count(&self) -> usize {
            1
        }
        fn param_info(&self, index: usize) -> Option<ParamDescriptor> {
            match index {
                0 => Some(ParamDescriptor::gain_db("Level", "Level", -60.0, 12.0, 0.0)),
                _ => None,
            }
        }
        fn get_param(&self, index: usize) -> f32 {
            match index {
                0 => self.value,
                _ => 0.0,
            }
        }
        fn set_param(&mut self, index: usize, value: f32) {
            if index == 0 {
                self.value = value.clamp(-60.0, 12.0);
            }
        }
    }

    fn build_test_graph() -> (ProcessingGraph, NodeId) {
        let mut graph = ProcessingGraph::new(48000.0, 64);
        let input = graph.add_input();
        let effect = graph.add_effect(Box::new(TestEffect::new()));
        let output = graph.add_output();
        graph.connect(input, effect).unwrap();
        graph.connect(effect, output).unwrap();
        graph.compile().unwrap();
        (graph, effect)
    }

    #[test]
    fn test_control_context_new() {
        let ctx = ControlContext::<6>::new();
        assert_eq!(ctx.toggles(), (0, 0));
        assert_eq!(ctx.total_addressable(), 54); // 9 pages × 6 knobs
    }

    #[test]
    fn test_page_mapping() {
        let mut ctx = ControlContext::<6>::new();
        let node = NodeId::sentinel();

        ctx.page_mut(0, 0).map_knob(0, node, 0);
        assert_eq!(ctx.page(0, 0).get_knob(0), Some((node, 0)));
        assert_eq!(ctx.page(0, 0).get_knob(1), None);
        assert_eq!(ctx.page(0, 1).get_knob(0), None); // Different page.
    }

    #[test]
    fn test_toggle_selection() {
        let mut ctx = ControlContext::<6>::new();
        let node = NodeId::sentinel();

        ctx.page_mut(1, 2).map_knob(0, node, 5);

        // Page (1, 2) is not active.
        ctx.set_toggles(0, 0);
        assert_eq!(ctx.active_page().get_knob(0), None);

        // Switch to page (1, 2).
        ctx.set_toggles(1, 2);
        assert_eq!(ctx.active_page().get_knob(0), Some((node, 5)));
    }

    #[test]
    fn test_apply_knob() {
        let (mut graph, effect) = build_test_graph();

        let mut ctx = ControlContext::<6>::new();
        ctx.page_mut(0, 0).map_knob(0, effect, 0);

        // Apply normalized 0.5 → -24 dB.
        let applied = ctx.apply_knob(0, 0.5, &mut graph);
        assert!(applied);

        let value = graph
            .effect_with_params_ref(effect)
            .unwrap()
            .effect_get_param(0);
        assert!(
            (value - (-24.0)).abs() < 0.01,
            "expected -24.0, got {value}"
        );
    }

    #[test]
    fn test_apply_knob_wrong_page() {
        let (mut graph, effect) = build_test_graph();

        let mut ctx = ControlContext::<6>::new();
        ctx.page_mut(1, 0).map_knob(0, effect, 0);

        // Active page is (0, 0) — knob 0 is unmapped there.
        ctx.set_toggles(0, 0);
        assert!(!ctx.apply_knob(0, 0.5, &mut graph));
    }

    #[test]
    fn test_apply_all_knobs() {
        let (mut graph, effect) = build_test_graph();

        let mut ctx = ControlContext::<6>::new();
        ctx.page_mut(0, 0).map_knob(0, effect, 0);
        // Knobs 1-5 unmapped.

        let values = [0.0, 0.5, 0.5, 0.5, 0.5, 0.5];
        let updated = ctx.apply_all_knobs(&values, &mut graph);
        assert_eq!(updated, 1); // Only knob 0 is mapped.
    }

    #[test]
    fn test_knob_index_from_control() {
        let ctx = ControlContext::<6>::new();

        assert_eq!(ctx.knob_index_from_control(ControlId::hardware(0)), Some(0));
        assert_eq!(ctx.knob_index_from_control(ControlId::hardware(5)), Some(5));
        assert_eq!(ctx.knob_index_from_control(ControlId::hardware(6)), None); // >= KNOBS
        assert_eq!(ctx.knob_index_from_control(ControlId::midi(0)), None); // Wrong namespace
    }

    #[test]
    fn test_toggle_clamping() {
        let mut ctx = ControlContext::<6>::new();
        ctx.set_toggles(99, 99);
        assert_eq!(ctx.toggles(), (2, 2)); // Clamped to max.
    }

    #[test]
    fn test_clear_all() {
        let mut ctx = ControlContext::<6>::new();
        let node = NodeId::sentinel();

        ctx.page_mut(0, 0).map_knob(0, node, 0);
        ctx.page_mut(2, 2).map_knob(5, node, 3);
        ctx.clear_all();

        assert_eq!(ctx.page(0, 0).get_knob(0), None);
        assert_eq!(ctx.page(2, 2).get_knob(5), None);
    }
}
