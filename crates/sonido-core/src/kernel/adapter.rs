//! Generic adapter that bridges [`DspKernel`] to the [`Effect`] + [`ParameterInfo`] system.
//!
//! The adapter is parameterised by a [`SmoothingPolicy`] that controls how
//! parameter changes are applied:
//!
//! - **[`SmoothedPolicy`]** — per-sample [`SmoothedParam`] advancement (desktop / plugin).
//! - **[`DirectPolicy`]** — zero-overhead immediate writes (embedded targets).
//!
//! Two type aliases keep backward compatibility:
//!
//! - [`KernelAdapter<K>`] — `Adapter<K, SmoothedPolicy>`, the original desktop adapter.
//! - [`EmbeddedAdapter<K>`] (re-exported from `sonido-daisy`) — `Adapter<K, DirectPolicy>`.
//!
//! # Processing Flow (SmoothedPolicy)
//!
//! Each sample:
//! 1. Advance all smoothers → current smoothed values
//! 2. Write values into the `Params` snapshot
//! 3. Pass snapshot to `kernel.process_stereo()`
//! 4. Track input/output peaks for observability
//!
//! # Thread Safety
//!
//! `Adapter<K, S>` implements `Send` because both `DspKernel: Send` and
//! `SmoothedParam: Send` (plain f32 fields).

#[cfg(not(feature = "std"))]
use alloc::vec::Vec;

use crate::effect::{Effect, TailReporting};
use crate::math::flush_denormal;
use crate::param::SmoothedParam;
use crate::param_info::{ParamDescriptor, ParamFlags, ParameterInfo};
use crate::tempo::TempoContext;

use super::traits::{DspKernel, KernelParams};

// ── SmoothingPolicy trait ────────────────────────────────────────────────────

/// Strategy for parameter smoothing in [`Adapter`].
///
/// Implementors control how parameter targets are stored, advanced, and
/// applied to the kernel's params snapshot each sample.
///
/// Two implementations are provided:
/// - [`SmoothedPolicy`] — desktop/plugin, per-sample [`SmoothedParam`] advancement.
/// - [`DirectPolicy`] — embedded, zero-overhead immediate application.
pub trait SmoothingPolicy: Send {
    /// Create storage for `count` parameters at the given sample rate.
    ///
    /// Called once in [`Adapter::new`]. `count` equals `K::Params::COUNT`.
    fn create<P: KernelParams>(count: usize, sample_rate: f32) -> Self;

    /// Set the target value for parameter at `index`.
    ///
    /// For [`SmoothedPolicy`], queues the value for gradual approach.
    /// For [`DirectPolicy`], this is a no-op — the adapter writes directly to the snapshot.
    fn set_target(&mut self, index: usize, value: f32);

    /// Get the current target value for parameter at `index`.
    ///
    /// For [`SmoothedPolicy`], returns the smoother's target (what the user set).
    /// For [`DirectPolicy`], this is not the authoritative value — read from the snapshot.
    fn get_target(&self, index: usize) -> f32;

    /// Advance all smoothers and write current values into `snapshot`.
    ///
    /// Returns `true` when all smoothers have reached their targets (steady state).
    /// For [`DirectPolicy`], always returns `true` (snapshot IS the live state).
    fn advance_all<P: KernelParams>(&mut self, snapshot: &mut P) -> bool;

    /// Snap all smoothers to their targets immediately.
    ///
    /// Called by [`Adapter::reset`] to apply params without interpolation.
    /// For [`DirectPolicy`], this is a no-op.
    fn snap_all(&mut self);

    /// Reconfigure all smoothers for a new sample rate.
    fn set_sample_rate<P: KernelParams>(&mut self, sample_rate: f32);

    /// Whether all smoothers have reached their targets.
    ///
    /// For [`DirectPolicy`], always returns `true`.
    fn is_settled(&self) -> bool;

    /// Whether this policy writes directly to the snapshot on `set_param`.
    ///
    /// When `true`, [`Adapter::set_param`] and [`Adapter::get_param`] bypass
    /// the policy and work directly on the snapshot. When `false`, set/get
    /// go through the policy's target storage.
    ///
    /// Returns `false` by default (SmoothedPolicy behaviour).
    fn direct_mode() -> bool {
        false
    }
}

// ── SmoothedPolicy ───────────────────────────────────────────────────────────

/// Desktop/plugin smoothing — per-sample [`SmoothedParam`] advancement.
///
/// Each parameter gets its own [`SmoothedParam`] configured from
/// [`KernelParams::smoothing()`]. Targets are stored in the smoothers;
/// the adapter's snapshot is rebuilt each sample from smoother outputs.
pub struct SmoothedPolicy {
    smoothers: Vec<SmoothedParam>,
    /// Tracks steady state: cleared on `set_target`, set when all settled.
    all_settled: bool,
}

impl SmoothingPolicy for SmoothedPolicy {
    fn create<P: KernelParams>(count: usize, sample_rate: f32) -> Self {
        let defaults = P::from_defaults();
        let mut smoothers = Vec::with_capacity(count);
        for i in 0..count {
            let default_val = defaults.get(i);
            let style = P::smoothing(i);
            smoothers.push(SmoothedParam::with_config(
                default_val,
                sample_rate,
                style.time_ms(),
            ));
        }
        Self {
            smoothers,
            all_settled: true,
        }
    }

    fn set_target(&mut self, index: usize, value: f32) {
        if index < self.smoothers.len() {
            self.smoothers[index].set_target(value);
            self.all_settled = false;
        }
    }

    fn get_target(&self, index: usize) -> f32 {
        if index < self.smoothers.len() {
            self.smoothers[index].target()
        } else {
            0.0
        }
    }

    fn advance_all<P: KernelParams>(&mut self, snapshot: &mut P) -> bool {
        if !self.all_settled {
            for i in 0..self.smoothers.len() {
                // Skip READ_ONLY params — their values are written by update_diagnostics(),
                // not by smoothers. Advancing would overwrite the live diagnostic values.
                if P::descriptor(i).is_some_and(|d| d.flags.contains(ParamFlags::READ_ONLY)) {
                    let _ = self.smoothers[i].advance(); // advance but discard
                    continue;
                }
                let value = self.smoothers[i].advance();
                snapshot.set(i, flush_denormal(value));
            }
            self.all_settled = self.smoothers.iter().all(SmoothedParam::is_settled);
        }
        self.all_settled
    }

    fn snap_all(&mut self) {
        for smoother in &mut self.smoothers {
            smoother.snap_to_target();
        }
        self.all_settled = true;
    }

    fn set_sample_rate<P: KernelParams>(&mut self, sample_rate: f32) {
        for smoother in &mut self.smoothers {
            smoother.set_sample_rate(sample_rate);
        }
    }

    fn is_settled(&self) -> bool {
        self.all_settled
    }
}

// ── DirectPolicy ─────────────────────────────────────────────────────────────

/// Embedded zero-smoothing — parameters applied immediately.
///
/// Zero-overhead: `set_param` writes directly to the adapter's snapshot
/// (bypassing the policy entirely via [`SmoothingPolicy::direct_mode`]).
/// All policy methods are no-ops.
///
/// # Why this is zero-sized
///
/// On embedded targets (Daisy Seed / Hothouse), ADC readings are
/// hardware-filtered by analog RC circuits and IIR-smoothed in the
/// control task. Per-sample software smoothing would be redundant overhead.
/// See ADR-028: "smoothing belongs to the platform layer."
pub struct DirectPolicy;

impl SmoothingPolicy for DirectPolicy {
    fn create<P: KernelParams>(_count: usize, _sample_rate: f32) -> Self {
        DirectPolicy
    }

    /// No-op — the adapter writes directly to the snapshot.
    fn set_target(&mut self, _index: usize, _value: f32) {}

    /// Always returns 0.0 — for DirectPolicy, read from the adapter's snapshot instead.
    fn get_target(&self, _index: usize) -> f32 {
        0.0
    }

    /// No-op — snapshot already contains live values.
    fn advance_all<P: KernelParams>(&mut self, _snapshot: &mut P) -> bool {
        true
    }

    /// No-op.
    fn snap_all(&mut self) {}

    /// No-op.
    fn set_sample_rate<P: KernelParams>(&mut self, _sample_rate: f32) {}

    /// Always settled.
    fn is_settled(&self) -> bool {
        true
    }

    fn direct_mode() -> bool {
        true
    }
}

// ── Adapter<K, S> ────────────────────────────────────────────────────────────

/// Generic kernel adapter — bridges [`DspKernel`] to [`Effect`] + [`ParameterInfo`].
///
/// Parameterised by `K` (the kernel) and `S` (the smoothing policy). Use the
/// type aliases [`KernelAdapter`] (desktop/plugin) or `EmbeddedAdapter` (embedded).
///
/// # Peak Tracking (Pattern P1 — Observable Kernel)
///
/// Input and output peaks are tracked per `process_stereo` call and
/// readable via [`peak_in`](Self::peak_in) / [`peak_out`](Self::peak_out).
/// Reset with [`reset_peaks`](Self::reset_peaks). Use for level meters,
/// clip detection, or observability dashboards.
///
/// # Thread Safety
///
/// `Adapter<K, S>` implements `Send` because `DspKernel: Send` and
/// `SmoothedParam: Send` (plain f32 fields). `S: Send` is required by
/// the `SmoothingPolicy: Send` bound.
pub struct Adapter<K: DspKernel, S: SmoothingPolicy> {
    /// The DSP kernel — owns only processing state.
    kernel: K,

    /// Smoothing policy — owns parameter target storage and smoothers.
    policy: S,

    /// Snapshot of current (smoothed or direct) parameter values.
    /// Passed to the kernel on each processing call.
    snapshot: K::Params,

    /// Sample rate — needed for policy reconfiguration.
    sample_rate: f32,

    /// Input peak levels: (left, right). Updated each `process_stereo` call.
    peak_in: (f32, f32),

    /// Output peak levels: (left, right). Updated each `process_stereo` call.
    peak_out: (f32, f32),
}

// ── SmoothedPolicy constructor ────────────────────────────────────────────────

impl<K: DspKernel> Adapter<K, SmoothedPolicy> {
    /// Create a new adapter with per-parameter smoothing (desktop/plugin).
    ///
    /// Initialises per-parameter smoothers from `KernelParams::smoothing()` and
    /// sets all values to their descriptor defaults.
    pub fn new(kernel: K, sample_rate: f32) -> Self {
        let defaults = K::Params::from_defaults();
        let count = K::Params::COUNT;
        let policy = SmoothedPolicy::create::<K::Params>(count, sample_rate);

        Self {
            kernel,
            policy,
            snapshot: defaults,
            sample_rate,
            peak_in: (0.0, 0.0),
            peak_out: (0.0, 0.0),
        }
    }
}

// ── DirectPolicy constructor ──────────────────────────────────────────────────

impl<K: DspKernel> Adapter<K, DirectPolicy> {
    /// Create a new adapter with zero-smoothing direct parameter access (embedded).
    ///
    /// Parameters written via [`set_param`](ParameterInfo::set_param) are live
    /// on the very next processing call.
    pub fn new_direct(kernel: K) -> Self {
        let defaults = K::Params::from_defaults();
        Self {
            kernel,
            policy: DirectPolicy,
            snapshot: defaults,
            sample_rate: 48000.0,
            peak_in: (0.0, 0.0),
            peak_out: (0.0, 0.0),
        }
    }
}

// ── Shared impl ──────────────────────────────────────────────────────────────

impl<K: DspKernel, S: SmoothingPolicy> Adapter<K, S> {
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
    /// Sets all smoother targets (or snapshot values for DirectPolicy) from
    /// the snapshot values. Use with `reset()` for instant preset recall, or
    /// without for smooth morphing into the new state.
    ///
    /// ```rust,ignore
    /// // Instant preset load (snap)
    /// adapter.load_snapshot(&saved_preset);
    /// adapter.reset();
    ///
    /// // Smooth transition to new preset (SmoothedPolicy only)
    /// adapter.load_snapshot(&new_preset);
    /// ```
    pub fn load_snapshot(&mut self, params: &K::Params) {
        for i in 0..K::Params::COUNT {
            if let Some(desc) = K::Params::descriptor(i) {
                let clamped = desc.clamp(params.get(i));
                if S::direct_mode() {
                    self.snapshot.set(i, clamped);
                } else {
                    self.policy.set_target(i, clamped);
                }
            }
        }
    }

    /// Get a snapshot of current parameter targets (what the user set).
    ///
    /// Returns a `Params` struct that can be saved as a preset, used as
    /// a morph endpoint, or restored later via [`load_snapshot`](Self::load_snapshot).
    pub fn snapshot(&self) -> K::Params {
        if S::direct_mode() {
            self.snapshot.clone()
        } else {
            let mut params = K::Params::from_defaults();
            for i in 0..K::Params::COUNT {
                params.set(i, self.policy.get_target(i));
            }
            params
        }
    }

    /// Current input peak levels since last [`reset_peaks`](Self::reset_peaks).
    ///
    /// Returns `(left_peak, right_peak)` in absolute amplitude.
    pub fn peak_in(&self) -> (f32, f32) {
        self.peak_in
    }

    /// Current output peak levels since last [`reset_peaks`](Self::reset_peaks).
    ///
    /// Returns `(left_peak, right_peak)` in absolute amplitude.
    pub fn peak_out(&self) -> (f32, f32) {
        self.peak_out
    }

    /// Clear input and output peak accumulators.
    pub fn reset_peaks(&mut self) {
        self.peak_in = (0.0, 0.0);
        self.peak_out = (0.0, 0.0);
    }

    /// Advance policy smoothers and rebuild the snapshot (SmoothedPolicy only).
    ///
    /// Called once per sample before `kernel.process_stereo()`.
    #[inline]
    fn advance_params(&mut self) {
        self.policy.advance_all::<K::Params>(&mut self.snapshot);
    }
}

// ── Effect impl ──────────────────────────────────────────────────────────────

impl<K: DspKernel, S: SmoothingPolicy> Effect for Adapter<K, S> {
    #[inline]
    fn process(&mut self, input: f32) -> f32 {
        self.advance_params();
        let out = self.kernel.process(input, &self.snapshot);
        self.kernel.update_diagnostics(&mut self.snapshot);
        out
    }

    #[inline]
    fn process_stereo(&mut self, left: f32, right: f32) -> (f32, f32) {
        // Track input peaks
        let abs_l = left.abs();
        let abs_r = right.abs();
        if abs_l > self.peak_in.0 {
            self.peak_in.0 = abs_l;
        }
        if abs_r > self.peak_in.1 {
            self.peak_in.1 = abs_r;
        }

        self.advance_params();
        let (l, r) = self.kernel.process_stereo(left, right, &self.snapshot);
        // Write READ_ONLY diagnostic values (gain reduction, gate state, LFO phase, etc.)
        // back into the snapshot so get_param() can return live values.
        self.kernel.update_diagnostics(&mut self.snapshot);

        // Track output peaks
        let abs_l_out = l.abs();
        let abs_r_out = r.abs();
        if abs_l_out > self.peak_out.0 {
            self.peak_out.0 = abs_l_out;
        }
        if abs_r_out > self.peak_out.1 {
            self.peak_out.1 = abs_r_out;
        }

        (l, r)
    }

    fn process_block(&mut self, input: &[f32], output: &mut [f32]) {
        debug_assert_eq!(input.len(), output.len());
        for (inp, out) in input.iter().zip(output.iter_mut()) {
            self.advance_params();
            *out = self.kernel.process(*inp, &self.snapshot);
            self.kernel.update_diagnostics(&mut self.snapshot);
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
            self.kernel.update_diagnostics(&mut self.snapshot);
            left_out[i] = l;
            right_out[i] = r;
        }
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.kernel.set_sample_rate(sample_rate);
        self.policy.set_sample_rate::<K::Params>(sample_rate);
    }

    fn reset(&mut self) {
        self.kernel.reset();
        self.policy.snap_all();
        // Re-apply snapshot from policy targets after snap (SmoothedPolicy),
        // or leave snapshot as-is (DirectPolicy — it's already live).
        if !S::direct_mode() {
            for i in 0..K::Params::COUNT {
                self.snapshot.set(i, self.policy.get_target(i));
            }
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

// ── TailReporting impl ───────────────────────────────────────────────────────

impl<K: DspKernel, S: SmoothingPolicy> TailReporting for Adapter<K, S> {
    /// Delegate to the kernel's tail duration.
    ///
    /// Returns 0 for most effects. Reverb, delay, and looper kernels override
    /// [`DspKernel::tail_samples()`] to report their ring-out duration.
    fn tail_samples(&self) -> usize {
        self.kernel.tail_samples()
    }
}

// ── ParameterInfo impl ───────────────────────────────────────────────────────

impl<K: DspKernel, S: SmoothingPolicy> ParameterInfo for Adapter<K, S> {
    fn param_count(&self) -> usize {
        K::Params::COUNT
    }

    fn param_info(&self, index: usize) -> Option<ParamDescriptor> {
        K::Params::descriptor(index)
    }

    fn get_param(&self, index: usize) -> f32 {
        if index >= K::Params::COUNT {
            return 0.0;
        }
        // READ_ONLY params are written by the kernel's update_diagnostics() into the
        // snapshot after each process_stereo() call. Always read them from the snapshot
        // regardless of smoothing policy — they have no user-settable target.
        if K::Params::descriptor(index).is_some_and(|d| d.flags.contains(ParamFlags::READ_ONLY)) {
            return self.snapshot.get(index);
        }
        if S::direct_mode() {
            // DirectPolicy: snapshot IS the live state
            self.snapshot.get(index)
        } else {
            // SmoothedPolicy: return the TARGET (what the user set), not the smoothed current
            self.policy.get_target(index)
        }
    }

    fn set_param(&mut self, index: usize, value: f32) {
        if index < K::Params::COUNT
            && let Some(desc) = K::Params::descriptor(index)
        {
            let clamped = desc.clamp(value);
            if S::direct_mode() {
                // DirectPolicy: write directly to snapshot, live immediately
                self.snapshot.set(index, clamped);
            } else {
                // SmoothedPolicy: set the smoother target, glide to value
                self.policy.set_target(index, clamped);
            }
        }
    }
}

// ── Type aliases ─────────────────────────────────────────────────────────────

/// Desktop/plugin adapter with per-parameter smoothing.
///
/// Wraps a [`DspKernel`] into [`Effect`] + [`ParameterInfo`] with per-sample
/// [`SmoothedParam`] advancement. The rest of the system (DAG graph, registry,
/// CLAP plugin, GUI) sees a standard `Effect`.
///
/// # Example
///
/// ```rust,ignore
/// use sonido_core::kernel::KernelAdapter;
/// use sonido_core::Effect;
///
/// let adapter = KernelAdapter::new(DistortionKernel::new(48000.0), 48000.0);
/// let mut effect: Box<dyn Effect> = Box::new(adapter);
/// let output = effect.process(0.5);
/// ```
pub type KernelAdapter<K> = Adapter<K, SmoothedPolicy>;

// ── Tests ────────────────────────────────────────────────────────────────────

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

    // ── Tail-reporting kernel ──

    struct TailKernel {
        tail: usize,
    }

    impl DspKernel for TailKernel {
        type Params = TestGainParams;

        fn process(&mut self, input: f32, _params: &TestGainParams) -> f32 {
            input
        }

        fn reset(&mut self) {}
        fn set_sample_rate(&mut self, _: f32) {}

        fn tail_samples(&self) -> usize {
            self.tail
        }
    }

    // ── Existing KernelAdapter tests (unchanged) ──

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
        adapter.policy.snap_all();
        // Re-sync snapshot after snap
        for i in 0..TestGainParams::COUNT {
            adapter.snapshot.set(i, adapter.policy.get_target(i));
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

    // ── DirectPolicy (EmbeddedAdapter) tests ──

    #[test]
    fn direct_policy_param_change_is_instant() {
        let mut adapter = Adapter::<TestGainKernel, DirectPolicy>::new_direct(TestGainKernel);
        // Default is 0 dB
        adapter.set_param(0, 6.0);
        // No need to snap — DirectPolicy is immediate
        let output = adapter.process(0.5);
        let expected = 0.5 * crate::fast_db_to_linear(6.0);
        assert!(
            (output - expected).abs() < 0.05,
            "DirectPolicy: expected ~{expected}, got {output}"
        );
    }

    #[test]
    fn direct_policy_get_param_reads_snapshot() {
        let mut adapter = Adapter::<TestGainKernel, DirectPolicy>::new_direct(TestGainKernel);
        adapter.set_param(0, -6.0);
        assert!(
            (adapter.get_param(0) - (-6.0)).abs() < 0.01,
            "DirectPolicy get_param should return snapshot value"
        );
    }

    #[test]
    fn direct_policy_clamps() {
        let mut adapter = Adapter::<TestGainKernel, DirectPolicy>::new_direct(TestGainKernel);
        adapter.set_param(0, 999.0);
        assert!(
            (adapter.get_param(0) - 12.0).abs() < 0.01,
            "DirectPolicy should clamp to max"
        );
    }

    // ── Peak tracking tests ──

    #[test]
    fn peak_tracking_input_and_output() {
        let mut adapter = KernelAdapter::new(TestGainKernel, 48000.0);
        adapter.reset();

        adapter.process_stereo(0.8, 0.4);
        let (pi_l, pi_r) = adapter.peak_in();
        let (po_l, po_r) = adapter.peak_out();

        assert!((pi_l - 0.8).abs() < 0.01, "peak_in left: {pi_l}");
        assert!((pi_r - 0.4).abs() < 0.01, "peak_in right: {pi_r}");
        // At 0 dB gain, output equals input
        assert!((po_l - 0.8).abs() < 0.01, "peak_out left: {po_l}");
        assert!((po_r - 0.4).abs() < 0.01, "peak_out right: {po_r}");
    }

    #[test]
    fn peak_tracking_accumulates_max() {
        let mut adapter = KernelAdapter::new(TestGainKernel, 48000.0);
        adapter.reset();

        adapter.process_stereo(0.3, 0.1);
        adapter.process_stereo(0.9, 0.5);
        adapter.process_stereo(0.2, 0.3);

        let (pi_l, pi_r) = adapter.peak_in();
        assert!((pi_l - 0.9).abs() < 0.01, "Should accumulate max: {pi_l}");
        assert!((pi_r - 0.5).abs() < 0.01, "Should accumulate max: {pi_r}");
    }

    #[test]
    fn peak_reset_clears_accumulators() {
        let mut adapter = KernelAdapter::new(TestGainKernel, 48000.0);
        adapter.reset();

        adapter.process_stereo(0.9, 0.9);
        adapter.reset_peaks();

        let (pi_l, pi_r) = adapter.peak_in();
        let (po_l, po_r) = adapter.peak_out();
        assert_eq!(pi_l, 0.0);
        assert_eq!(pi_r, 0.0);
        assert_eq!(po_l, 0.0);
        assert_eq!(po_r, 0.0);
    }

    // ── TailReporting tests ──

    #[test]
    fn tail_reporting_delegates_to_kernel() {
        let adapter = KernelAdapter::new(TailKernel { tail: 96000 }, 48000.0);
        assert_eq!(adapter.tail_samples(), 96000);
    }

    #[test]
    fn tail_reporting_default_is_zero() {
        let adapter = KernelAdapter::new(TestGainKernel, 48000.0);
        assert_eq!(adapter.tail_samples(), 0);
    }

    #[test]
    fn tail_reporting_direct_policy() {
        let adapter = Adapter::<TailKernel, DirectPolicy>::new_direct(TailKernel { tail: 48000 });
        assert_eq!(adapter.tail_samples(), 48000);
    }
}
