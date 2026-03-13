//! Zero-smoothing `DspKernel` wrapper for embedded deployment.
//!
//! [`EmbeddedAdapter`] is the embedded counterpart to `KernelAdapter`: same
//! `Effect + ParameterInfo` interface, zero smoothing overhead. Parameters
//! written via `set_param()` are live on the very next `process_stereo()` call.
//!
//! # Why no smoothing?
//!
//! On embedded targets (Daisy Seed / Hothouse), ADC readings are hardware-filtered
//! by analog RC circuits on the PCB and IIR-smoothed in the control task
//! ([`ControlBuffer`](crate::controls::ControlBuffer)). Adding per-sample
//! `SmoothedParam` advancement would be redundant CPU overhead. See ADR-028:
//! "smoothing belongs to the platform layer."
//!
//! # Usage
//!
//! ```ignore
//! use sonido_daisy::EmbeddedAdapter;
//! use sonido_effects::DistortionKernel;
//! use sonido_core::{Effect, ParameterInfo};
//!
//! let mut adapter = EmbeddedAdapter::new(DistortionKernel::new(48000.0));
//! adapter.set_param(0, 20.0);  // Drive = 20 dB — live immediately
//! let (l, r) = adapter.process_stereo(0.5, 0.5);
//! ```

use sonido_core::{DspKernel, Effect, KernelParams, ParamDescriptor, ParameterInfo};

/// Direct kernel wrapper with zero smoothing overhead.
///
/// Implements `Effect + ParameterInfo` (and thus `EffectWithParams` via blanket
/// impl) by delegating directly to the kernel. `set_param()` writes to the
/// kernel's typed params struct immediately — the value is live on the next
/// `process_stereo()` call.
pub struct EmbeddedAdapter<K: DspKernel> {
    kernel: K,
    params: K::Params,
}

impl<K: DspKernel> EmbeddedAdapter<K> {
    /// Creates a new adapter with default parameter values.
    pub fn new(kernel: K) -> Self {
        Self {
            params: K::Params::from_defaults(),
            kernel,
        }
    }

    /// Returns a reference to the inner kernel.
    pub fn kernel(&self) -> &K {
        &self.kernel
    }

    /// Returns a mutable reference to the inner kernel.
    pub fn kernel_mut(&mut self) -> &mut K {
        &mut self.kernel
    }

    /// Returns a reference to the current parameters.
    pub fn params(&self) -> &K::Params {
        &self.params
    }

    /// Returns a mutable reference to the current parameters.
    pub fn params_mut(&mut self) -> &mut K::Params {
        &mut self.params
    }
}

impl<K: DspKernel> Effect for EmbeddedAdapter<K> {
    fn process(&mut self, input: f32) -> f32 {
        self.kernel.process(input, &self.params)
    }

    fn process_stereo(&mut self, left: f32, right: f32) -> (f32, f32) {
        self.kernel.process_stereo(left, right, &self.params)
    }

    fn process_block(&mut self, input: &[f32], output: &mut [f32]) {
        self.kernel.process_block(input, output, &self.params);
    }

    fn process_block_stereo(
        &mut self,
        left_in: &[f32],
        right_in: &[f32],
        left_out: &mut [f32],
        right_out: &mut [f32],
    ) {
        self.kernel
            .process_block_stereo(left_in, right_in, left_out, right_out, &self.params);
    }

    fn is_true_stereo(&self) -> bool {
        self.kernel.is_true_stereo()
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.kernel.set_sample_rate(sample_rate);
    }

    fn reset(&mut self) {
        self.kernel.reset();
        self.params = K::Params::from_defaults();
    }

    fn latency_samples(&self) -> usize {
        self.kernel.latency_samples()
    }
}

impl<K: DspKernel> ParameterInfo for EmbeddedAdapter<K> {
    fn param_count(&self) -> usize {
        K::Params::COUNT
    }

    fn param_info(&self, index: usize) -> Option<ParamDescriptor> {
        K::Params::descriptor(index)
    }

    fn get_param(&self, index: usize) -> f32 {
        self.params.get(index)
    }

    fn set_param(&mut self, index: usize, value: f32) {
        self.params.set(index, value);
    }
}
