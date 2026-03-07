//! Adapter that bridges [`DspKernel`] to the existing [`Effect`] + [`ParameterInfo`] system.
//!
//! [`KernelAdapter`] wraps a kernel with per-parameter smoothers and implements
//! the standard Sonido traits. The rest of the system (DAG graph, registry,
//! CLAP plugin, GUI) sees a normal `Effect` — they don't know or care that the
//! underlying implementation is a separated kernel.
//!
//! This is the "platform layer" for desktop/plugin deployment. Embedded targets
//! bypass the adapter entirely and call `DspKernel::process_stereo()` directly
//! with values from hardware ADCs.

#[cfg(not(feature = "std"))]
use alloc::vec::Vec;

use crate::effect::Effect;
use crate::math::flush_denormal;
use crate::param::SmoothedParam;
use crate::param_info::{ParamDescriptor, ParameterInfo};
use crate::tempo::TempoContext;

use super::traits::{DspKernel, KernelParams};

/// Wraps a [`DspKernel`] into the [`Effect`] + [`ParameterInfo`] interface.
///
/// The adapter owns:
/// - The kernel (DSP state)
/// - Per-parameter [`SmoothedParam`] instances (configured from [`KernelParams::smoothing()`])
/// - A params snapshot (filled each sample from smoothed values and passed to the kernel)
///
/// # Processing Flow
///
/// Each sample:
/// 1. Advance all smoothers → get current smoothed values
/// 2. Write values into the `Params` snapshot struct
/// 3. Pass snapshot to `kernel.process_stereo()`
/// 4. Return kernel output
///
/// # Thread Safety
///
/// `KernelAdapter` implements `Send` (required by the DAG graph) because both
/// `DspKernel: Send` and `SmoothedParam: Send` (plain f32 fields).
///
/// # Example
///
/// ```rust,ignore
/// use sonido_core::kernel::KernelAdapter;
/// use sonido_core::Effect;
///
/// let adapter = KernelAdapter::new(DistortionKernel::new(48000.0), 48000.0);
///
/// // Use as a standard Effect
/// let mut effect: Box<dyn Effect> = Box::new(adapter);
/// let output = effect.process(0.5);
///
/// // Or in the DAG graph
/// graph.add_effect(Box::new(adapter));
/// ```
pub struct KernelAdapter<K: DspKernel> {
    /// The DSP kernel — owns only processing state.
    kernel: K,

    /// Per-parameter smoothers. Indexed by parameter index (0..COUNT).
    /// Targets are in user-facing units (dB, %, etc.).
    smoothers: Vec<SmoothedParam>,

    /// Snapshot of current (smoothed) parameter values, passed to the kernel
    /// on each processing call. Rebuilt each sample from smoother outputs.
    snapshot: K::Params,

    /// Sample rate — needed for smoother reconfiguration.
    sample_rate: f32,

    /// When `true`, all smoothers have reached their targets and
    /// [`advance_params()`](Self::advance_params) can skip iteration entirely.
    /// Cleared on [`set_param()`](Self::set_param) and
    /// [`load_snapshot()`](Self::load_snapshot).
    all_settled: bool,
}

impl<K: DspKernel> KernelAdapter<K> {
    /// Create a new adapter wrapping the given kernel.
    ///
    /// Initializes per-parameter smoothers from `KernelParams::smoothing()` and
    /// sets all values to their descriptor defaults.
    pub fn new(kernel: K, sample_rate: f32) -> Self {
        let defaults = K::Params::from_defaults();
        let count = K::Params::COUNT;

        let mut smoothers = Vec::with_capacity(count);
        for i in 0..count {
            let default_val = defaults.get(i);
            let style = K::Params::smoothing(i);
            let smoother = SmoothedParam::with_config(default_val, sample_rate, style.time_ms());
            smoothers.push(smoother);
        }

        Self {
            kernel,
            smoothers,
            snapshot: defaults,
            sample_rate,
            all_settled: true,
        }
    }

    /// Direct access to the underlying kernel (for testing, inspection).
    pub fn kernel(&self) -> &K {
        &self.kernel
    }

    /// Mutable access to the underlying kernel.
    pub fn kernel_mut(&mut self) -> &mut K {
        &mut self.kernel
    }

    /// Load a complete parameter snapshot.
    ///
    /// Sets all smoother targets from the snapshot values. Use with `reset()`
    /// for instant preset recall (snap smoothers), or without for smooth
    /// morphing into the new state.
    ///
    /// ```rust,ignore
    /// // Instant preset load (snap)
    /// adapter.load_snapshot(&saved_preset);
    /// adapter.reset();
    ///
    /// // Smooth transition to new preset
    /// adapter.load_snapshot(&new_preset);
    /// // Smoothers will glide to new values over their configured time
    /// ```
    pub fn load_snapshot(&mut self, params: &K::Params) {
        for i in 0..self.smoothers.len() {
            if let Some(desc) = K::Params::descriptor(i) {
                let clamped = desc.clamp(params.get(i));
                self.smoothers[i].set_target(clamped);
            }
        }
        self.all_settled = false;
    }

    /// Get a snapshot of current parameter targets (what the user set).
    ///
    /// Returns a `Params` struct that can be saved as a preset, used as
    /// a morph endpoint, or restored later via [`load_snapshot()`](Self::load_snapshot).
    pub fn snapshot(&self) -> K::Params {
        let mut params = K::Params::from_defaults();
        for i in 0..self.smoothers.len() {
            params.set(i, self.smoothers[i].target());
        }
        params
    }

    /// Advance all smoothers and rebuild the parameter snapshot.
    ///
    /// Called once per sample before `kernel.process_stereo()`.
    /// Skips iteration entirely when all smoothers have settled, avoiding
    /// 30-80 wasted function calls per sample in steady state for a typical chain.
    #[inline]
    fn advance_params(&mut self) {
        if !self.all_settled {
            for i in 0..self.smoothers.len() {
                let value = self.smoothers[i].advance();
                self.snapshot.set(i, flush_denormal(value));
            }
            self.all_settled = self.smoothers.iter().all(SmoothedParam::is_settled);
        }
    }
}

impl<K: DspKernel> Effect for KernelAdapter<K> {
    #[inline]
    fn process(&mut self, input: f32) -> f32 {
        self.advance_params();
        self.kernel.process(input, &self.snapshot)
    }

    #[inline]
    fn process_stereo(&mut self, left: f32, right: f32) -> (f32, f32) {
        self.advance_params();
        self.kernel.process_stereo(left, right, &self.snapshot)
    }

    fn process_block(&mut self, input: &[f32], output: &mut [f32]) {
        debug_assert_eq!(input.len(), output.len());
        for (inp, out) in input.iter().zip(output.iter_mut()) {
            self.advance_params();
            *out = self.kernel.process(*inp, &self.snapshot);
        }
    }

    fn process_block_stereo(
        &mut self,
        left_in: &[f32],
        right_in: &[f32],
        left_out: &mut [f32],
        right_out: &mut [f32],
    ) {
        debug_assert_eq!(left_in.len(), right_in.len());
        debug_assert_eq!(left_in.len(), left_out.len());
        debug_assert_eq!(left_out.len(), right_out.len());

        for i in 0..left_in.len() {
            self.advance_params();
            let (l, r) = self
                .kernel
                .process_stereo(left_in[i], right_in[i], &self.snapshot);
            left_out[i] = l;
            right_out[i] = r;
        }
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.kernel.set_sample_rate(sample_rate);
        for smoother in &mut self.smoothers {
            smoother.set_sample_rate(sample_rate);
        }
    }

    fn reset(&mut self) {
        self.kernel.reset();
        for smoother in &mut self.smoothers {
            smoother.snap_to_target();
        }
    }

    fn latency_samples(&self) -> usize {
        self.kernel.latency_samples()
    }

    fn is_true_stereo(&self) -> bool {
        self.kernel.is_true_stereo()
    }

    fn set_tempo_context(&mut self, ctx: &TempoContext) {
        self.kernel.set_tempo_context(ctx);
    }
}

impl<K: DspKernel> ParameterInfo for KernelAdapter<K> {
    fn param_count(&self) -> usize {
        K::Params::COUNT
    }

    fn param_info(&self, index: usize) -> Option<ParamDescriptor> {
        K::Params::descriptor(index)
    }

    fn get_param(&self, index: usize) -> f32 {
        if index < self.smoothers.len() {
            // Return the TARGET value (what the user set), not the smoothed current.
            self.smoothers[index].target()
        } else {
            0.0
        }
    }

    fn set_param(&mut self, index: usize, value: f32) {
        if index < self.smoothers.len() {
            // Clamp to descriptor bounds, then set the smoother target.
            if let Some(desc) = K::Params::descriptor(index) {
                let clamped = desc.clamp(value);
                self.smoothers[index].set_target(clamped);
                self.all_settled = false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::{KernelParams, SmoothingStyle};
    use crate::param_info::{ParamDescriptor, ParamId};

    // ── Minimal test kernel: simple gain ──

    #[derive(Debug, Clone, Copy, Default)]
    struct TestGainParams {
        gain_db: f32,
    }

    impl KernelParams for TestGainParams {
        const COUNT: usize = 1;

        fn descriptor(index: usize) -> Option<ParamDescriptor> {
            match index {
                0 => Some(
                    ParamDescriptor::gain_db("Gain", "Gain", -60.0, 12.0, 0.0)
                        .with_id(ParamId(100), "gain_level"),
                ),
                _ => None,
            }
        }

        fn smoothing(index: usize) -> SmoothingStyle {
            match index {
                0 => SmoothingStyle::Standard,
                _ => SmoothingStyle::Standard,
            }
        }

        fn get(&self, index: usize) -> f32 {
            match index {
                0 => self.gain_db,
                _ => 0.0,
            }
        }

        fn set(&mut self, index: usize, value: f32) {
            if index == 0 {
                self.gain_db = value;
            }
        }
    }

    struct TestGainKernel;

    impl DspKernel for TestGainKernel {
        type Params = TestGainParams;

        fn process(&mut self, input: f32, params: &TestGainParams) -> f32 {
            input * crate::fast_db_to_linear(params.gain_db)
        }

        fn reset(&mut self) {}
        fn set_sample_rate(&mut self, _sample_rate: f32) {}
    }

    // ── Tests ──

    #[test]
    fn adapter_implements_effect() {
        let mut adapter = KernelAdapter::new(TestGainKernel, 48000.0);

        // Default gain is 0 dB → unity gain
        // Snap smoother so we get exact values
        adapter.reset();
        let output = adapter.process(1.0);
        assert!(
            (output - 1.0).abs() < 0.01,
            "Expected ~1.0 at 0dB gain, got {output}"
        );
    }

    #[test]
    fn adapter_implements_parameter_info() {
        let adapter = KernelAdapter::new(TestGainKernel, 48000.0);
        assert_eq!(adapter.param_count(), 1);

        let desc = adapter.param_info(0).unwrap();
        assert_eq!(desc.name, "Gain");
        assert_eq!(desc.min, -60.0);
        assert_eq!(desc.max, 12.0);
    }

    #[test]
    fn adapter_set_param_clamps() {
        let mut adapter = KernelAdapter::new(TestGainKernel, 48000.0);

        // Try to set beyond max
        adapter.set_param(0, 100.0);
        assert!(
            (adapter.get_param(0) - 12.0).abs() < 0.01,
            "Should clamp to max 12.0"
        );

        // Try to set below min
        adapter.set_param(0, -200.0);
        assert!(
            (adapter.get_param(0) - (-60.0)).abs() < 0.01,
            "Should clamp to min -60.0"
        );
    }

    #[test]
    fn adapter_stereo_processing() {
        let mut adapter = KernelAdapter::new(TestGainKernel, 48000.0);
        adapter.reset(); // snap smoothers

        let (left, right) = adapter.process_stereo(1.0, 0.5);
        // At 0 dB, output should equal input
        assert!((left - 1.0).abs() < 0.01);
        assert!((right - 0.5).abs() < 0.01);
    }

    #[test]
    fn adapter_block_stereo_processing() {
        let mut adapter = KernelAdapter::new(TestGainKernel, 48000.0);
        adapter.reset();

        let left_in = [1.0, 0.5, 0.25];
        let right_in = [0.5, 0.25, 0.125];
        let mut left_out = [0.0; 3];
        let mut right_out = [0.0; 3];

        adapter.process_block_stereo(&left_in, &right_in, &mut left_out, &mut right_out);

        for i in 0..3 {
            assert!(
                (left_out[i] - left_in[i]).abs() < 0.01,
                "Left sample {i}: expected {}, got {}",
                left_in[i],
                left_out[i]
            );
        }
    }

    #[test]
    fn adapter_param_change_affects_output() {
        let mut adapter = KernelAdapter::new(TestGainKernel, 48000.0);
        adapter.reset();

        // Set gain to 6 dB (~2x)
        adapter.set_param(0, 6.0);
        // Snap to target so we get exact value
        for smoother in &mut adapter.smoothers {
            smoother.snap_to_target();
        }

        let output = adapter.process(0.5);
        let expected = 0.5 * crate::fast_db_to_linear(6.0);
        assert!(
            (output - expected).abs() < 0.05,
            "Expected ~{expected}, got {output}"
        );
    }

    #[test]
    fn adapter_sample_rate_propagates() {
        let mut adapter = KernelAdapter::new(TestGainKernel, 44100.0);
        adapter.set_sample_rate(96000.0);
        // Should not panic or produce NaN
        let output = adapter.process(1.0);
        assert!(!output.is_nan());
    }

    #[test]
    fn adapter_load_snapshot_sets_targets() {
        let mut adapter = KernelAdapter::new(TestGainKernel, 48000.0);

        let preset = TestGainParams { gain_db: -12.0 };

        adapter.load_snapshot(&preset);
        assert!(
            (adapter.get_param(0) - (-12.0)).abs() < 0.01,
            "load_snapshot should set target"
        );
    }

    #[test]
    fn adapter_snapshot_roundtrip() {
        let mut adapter = KernelAdapter::new(TestGainKernel, 48000.0);
        adapter.set_param(0, 6.0);

        let saved = adapter.snapshot();
        assert!((saved.gain_db - 6.0).abs() < 0.01);

        // Load into a fresh adapter
        let mut adapter2 = KernelAdapter::new(TestGainKernel, 48000.0);
        adapter2.load_snapshot(&saved);
        assert!((adapter2.get_param(0) - 6.0).abs() < 0.01);
    }

    #[test]
    fn params_lerp_continuous() {
        let a = TestGainParams { gain_db: -60.0 };
        let b = TestGainParams { gain_db: 12.0 };

        let mid = TestGainParams::lerp(&a, &b, 0.5);
        let expected = -60.0 + (12.0 - (-60.0)) * 0.5; // -24.0
        assert!(
            (mid.gain_db - expected).abs() < 0.01,
            "Expected {expected}, got {}",
            mid.gain_db
        );

        let at_a = TestGainParams::lerp(&a, &b, 0.0);
        assert!((at_a.gain_db - (-60.0)).abs() < 0.01);

        let at_b = TestGainParams::lerp(&a, &b, 1.0);
        assert!((at_b.gain_db - 12.0).abs() < 0.01);
    }

    #[test]
    fn params_from_defaults() {
        let params = TestGainParams::from_defaults();
        assert!(
            (params.gain_db - 0.0).abs() < 0.01,
            "Default should be 0 dB"
        );
    }

    // ── True stereo kernel test ──

    #[derive(Debug, Clone, Copy, Default)]
    struct SwapParams;

    impl KernelParams for SwapParams {
        const COUNT: usize = 0;
        fn descriptor(_: usize) -> Option<ParamDescriptor> {
            None
        }
        fn smoothing(_: usize) -> SmoothingStyle {
            SmoothingStyle::None
        }
        fn get(&self, _: usize) -> f32 {
            0.0
        }
        fn set(&mut self, _: usize, _: f32) {}
    }

    struct StereoSwapKernel;

    impl DspKernel for StereoSwapKernel {
        type Params = SwapParams;

        fn process_stereo(&mut self, left: f32, right: f32, _params: &SwapParams) -> (f32, f32) {
            (right, left) // Swap channels
        }

        fn is_true_stereo(&self) -> bool {
            true
        }
        fn reset(&mut self) {}
        fn set_sample_rate(&mut self, _: f32) {}
    }

    #[test]
    fn true_stereo_kernel() {
        let mut adapter = KernelAdapter::new(StereoSwapKernel, 48000.0);
        assert!(adapter.is_true_stereo());

        let (l, r) = adapter.process_stereo(1.0, 2.0);
        assert_eq!(l, 2.0);
        assert_eq!(r, 1.0);
    }
}
