//! Core Effect trait and related types.
//!
//! The [`Effect`] trait is the foundation of the DSP framework. All audio
//! effects implement this trait, providing a consistent interface for
//! single-sample and block-based processing.
//!
//! ## Design Decisions
//!
//! - **Stereo-first**: The primary processing method is `process_stereo()`,
//!   which takes left/right input and returns left/right output. This
//!   supports true stereo effects (ping-pong delays, stereo widening, etc.)
//!   while maintaining backwards compatibility with mono effects.
//!
//! - **Backwards compatible**: The legacy `process()` method still works.
//!   Effects can implement either `process()` (for mono effects) or
//!   `process_stereo()` (for true stereo effects). Default implementations
//!   bridge between them automatically.
//!
//! - **Object-safe**: The trait is designed to be object-safe, allowing
//!   `dyn Effect` for runtime effect chaining. However, generic/static
//!   dispatch is preferred for maximum performance.
//!
//! - **No allocations**: All methods are designed to be called in real-time
//!   audio contexts with zero heap allocations.

/// Macro to verify an Effect impl doesn't infinite-recurse.
///
/// Call in your `#[cfg(test)] mod tests` after implementing Effect.
/// Panics (stack overflow) if neither `process()` nor `process_stereo()`
/// is overridden.
///
/// # Example
///
/// ```rust,ignore
/// #[cfg(test)]
/// mod tests {
///     use super::*;
///     use sonido_core::assert_effect_not_recursive;
///
///     #[test]
///     fn no_recursion() {
///         assert_effect_not_recursive!(MyEffect, 48000.0);
///     }
/// }
/// ```
#[macro_export]
macro_rules! assert_effect_not_recursive {
    ($ty:ty, $sr:expr) => {{
        let mut effect = <$ty>::new($sr);
        let _ = effect.process(0.0);
        let _ = effect.process_stereo(0.0, 0.0);
    }};
}

/// Core trait for all audio effects.
///
/// Effects process audio samples, either one at a time or in blocks.
/// The trait is designed to be object-safe while supporting efficient
/// static dispatch when used with generics.
///
/// ## Stereo-First Design
///
/// The primary processing method is [`process_stereo`](Effect::process_stereo),
/// which handles left and right channels. Effects should implement either:
///
/// - `process()` for mono effects (stereo will duplicate processing)
/// - `process_stereo()` for true stereo effects (mono will use left channel)
///
/// Default implementations bridge between mono and stereo automatically.
///
/// # Safety Contract
///
/// Implementors **must** override at least one of `process()` or
/// `process_stereo()`. Failing to override either causes infinite
/// recursion (stack overflow). This cannot be enforced at compile
/// time in stable Rust. Use [`assert_effect_not_recursive!`] in
/// your test module to verify.
///
/// # Example: Mono Effect
///
/// ```rust
/// use sonido_core::Effect;
///
/// struct Gain {
///     gain: f32,
/// }
///
/// impl Effect for Gain {
///     fn process(&mut self, input: f32) -> f32 {
///         input * self.gain
///     }
///
///     fn set_sample_rate(&mut self, _sample_rate: f32) {}
///     fn reset(&mut self) {}
/// }
/// ```
///
/// # Example: True Stereo Effect
///
/// ```rust
/// use sonido_core::Effect;
///
/// struct StereoWidener {
///     width: f32,
/// }
///
/// impl Effect for StereoWidener {
///     fn process_stereo(&mut self, left: f32, right: f32) -> (f32, f32) {
///         let mid = (left + right) * 0.5;
///         let side = (left - right) * 0.5 * self.width;
///         (mid + side, mid - side)
///     }
///
///     fn is_true_stereo(&self) -> bool { true }
///     fn set_sample_rate(&mut self, _sample_rate: f32) {}
///     fn reset(&mut self) {}
/// }
/// ```
pub trait Effect {
    /// Process a single mono sample.
    ///
    /// For mono effects, implement this method. The default stereo processing
    /// will call this independently for left and right channels.
    ///
    /// For true stereo effects, you don't need to implement this - the default
    /// implementation derives mono from stereo by using the left channel output.
    ///
    /// # Arguments
    /// * `input` - Input sample, typically in range [-1.0, 1.0]
    ///
    /// # Returns
    /// Processed output sample
    fn process(&mut self, input: f32) -> f32 {
        // Default: derive mono from stereo (use left channel)
        self.process_stereo(input, input).0
    }

    /// Process a stereo sample pair.
    ///
    /// This is the PRIMARY processing method. For true stereo effects
    /// (ping-pong delays, stereo widening, etc.), implement this method.
    ///
    /// For mono effects, the default implementation calls `process()` twice,
    /// once for each channel, which is correct for independent processing.
    ///
    /// # Mutual Recursion Safety
    ///
    /// `process()` and `process_stereo()` have default implementations that
    /// call each other. This is safe because every concrete effect must
    /// implement at least one of the two methods, breaking the recursion:
    ///
    /// - **Mono effects** implement `process()`. The default `process_stereo()`
    ///   calls `process()` twice (once per channel) -- no recursion.
    /// - **True stereo effects** implement `process_stereo()`. The default
    ///   `process()` calls `process_stereo(input, input).0` -- no recursion.
    ///
    /// An effect that overrides neither method would infinite-loop, but this
    /// is a logic error (an effect must do something).
    ///
    /// # Arguments
    /// * `left` - Left channel input sample
    /// * `right` - Right channel input sample
    ///
    /// # Returns
    /// Tuple of (left_output, right_output)
    fn process_stereo(&mut self, left: f32, right: f32) -> (f32, f32) {
        // Default: process channels independently (mono effect behavior)
        // Note: This works because mono effects implement process() directly
        // and don't override process_stereo(), breaking the recursion.
        (self.process(left), self.process(right))
    }

    /// Process a block of samples.
    ///
    /// Default implementation calls `process()` for each sample. Effects
    /// may override this for SIMD optimization or more efficient block
    /// processing.
    ///
    /// # Arguments
    /// * `input` - Input sample buffer
    /// * `output` - Output sample buffer (must be same length as input)
    ///
    /// # Panics
    /// Default implementation panics if `input.len() != output.len()`
    fn process_block(&mut self, input: &[f32], output: &mut [f32]) {
        debug_assert_eq!(
            input.len(),
            output.len(),
            "Input and output buffers must have same length"
        );
        for (inp, out) in input.iter().zip(output.iter_mut()) {
            *out = self.process(*inp);
        }
    }

    /// Process a block of samples in-place.
    ///
    /// Convenience method for when input and output are the same buffer.
    /// Default implementation processes each sample in place.
    ///
    /// # Arguments
    /// * `buffer` - Buffer to process in-place
    fn process_block_inplace(&mut self, buffer: &mut [f32]) {
        for sample in buffer.iter_mut() {
            *sample = self.process(*sample);
        }
    }

    /// Process a block of stereo samples.
    ///
    /// Default implementation calls `process_stereo()` for each sample pair.
    /// Effects may override this for SIMD optimization or more efficient
    /// block processing.
    ///
    /// # Arguments
    /// * `left_in` - Left channel input buffer
    /// * `right_in` - Right channel input buffer
    /// * `left_out` - Left channel output buffer
    /// * `right_out` - Right channel output buffer
    ///
    /// # Panics
    /// Default implementation panics if buffers have different lengths
    fn process_block_stereo(
        &mut self,
        left_in: &[f32],
        right_in: &[f32],
        left_out: &mut [f32],
        right_out: &mut [f32],
    ) {
        debug_assert_eq!(
            left_in.len(),
            right_in.len(),
            "Left and right input buffers must have same length"
        );
        debug_assert_eq!(
            left_in.len(),
            left_out.len(),
            "Input and output buffers must have same length"
        );
        debug_assert_eq!(
            left_out.len(),
            right_out.len(),
            "Left and right output buffers must have same length"
        );

        for i in 0..left_in.len() {
            let (l, r) = self.process_stereo(left_in[i], right_in[i]);
            left_out[i] = l;
            right_out[i] = r;
        }
    }

    /// Process a block of stereo samples in-place.
    ///
    /// Convenience method for when input and output are the same buffers.
    ///
    /// # Arguments
    /// * `left` - Left channel buffer (in-place)
    /// * `right` - Right channel buffer (in-place)
    ///
    /// # Panics
    /// Panics if buffers have different lengths
    fn process_block_stereo_inplace(&mut self, left: &mut [f32], right: &mut [f32]) {
        debug_assert_eq!(
            left.len(),
            right.len(),
            "Left and right buffers must have same length"
        );

        for i in 0..left.len() {
            let (l, r) = self.process_stereo(left[i], right[i]);
            left[i] = l;
            right[i] = r;
        }
    }

    /// Process a block of stereo samples with an external sidechain input.
    ///
    /// The `sc_left` / `sc_right` buffers carry the sidechain signal for
    /// effects that support it (e.g., sidechain compressor, ducking gate).
    /// The default implementation ignores the sidechain signal and delegates
    /// to [`process_block_stereo`](Self::process_block_stereo).
    ///
    /// # Arguments
    /// * `left_in`  - Main-chain left input
    /// * `right_in` - Main-chain right input
    /// * `left_out` - Left output
    /// * `right_out` - Right output
    /// * `sc_left`  - Sidechain left input (may be ignored)
    /// * `sc_right` - Sidechain right input (may be ignored)
    #[allow(unused_variables)]
    fn process_block_stereo_with_sidechain(
        &mut self,
        left_in: &[f32],
        right_in: &[f32],
        left_out: &mut [f32],
        right_out: &mut [f32],
        sc_left: &[f32],
        sc_right: &[f32],
    ) {
        self.process_block_stereo(left_in, right_in, left_out, right_out);
    }

    /// Process a block of stereo samples in-place with an external sidechain input.
    ///
    /// Same as [`process_block_stereo_with_sidechain`](Self::process_block_stereo_with_sidechain)
    /// but operates in-place on `left`/`right`. The default implementation
    /// ignores the sidechain signal and delegates to
    /// [`process_block_stereo_inplace`](Self::process_block_stereo_inplace).
    ///
    /// # Arguments
    /// * `left`    - Main-chain left buffer (in-place)
    /// * `right`   - Main-chain right buffer (in-place)
    /// * `sc_left`  - Sidechain left input (may be ignored)
    /// * `sc_right` - Sidechain right input (may be ignored)
    #[allow(unused_variables)]
    fn process_block_stereo_inplace_with_sidechain(
        &mut self,
        left: &mut [f32],
        right: &mut [f32],
        sc_left: &[f32],
        sc_right: &[f32],
    ) {
        self.process_block_stereo_inplace(left, right);
    }

    /// Returns whether this effect is a true stereo effect.
    ///
    /// True stereo effects have cross-channel interaction (e.g., ping-pong
    /// delays, stereo wideners, mid-side processors). Mono effects process
    /// each channel independently.
    ///
    /// This metadata helps effect hosts optimize processing and provides
    /// useful information for users.
    ///
    /// Default returns `false` (mono effect).
    fn is_true_stereo(&self) -> bool {
        false
    }

    /// Update the sample rate.
    ///
    /// Called when the sample rate changes. Effects should recalculate
    /// any sample-rate-dependent coefficients (filter coefficients,
    /// delay times in samples, LFO increments, etc.).
    ///
    /// # Arguments
    /// * `sample_rate` - New sample rate in Hz (e.g., 44100.0, 48000.0)
    fn set_sample_rate(&mut self, sample_rate: f32);

    /// Reset internal state.
    ///
    /// Clears all internal state (delay lines, filter history, etc.)
    /// without changing parameters. Called when playback stops/starts
    /// or when the effect is bypassed to prevent artifacts.
    fn reset(&mut self);

    /// Report processing latency in samples.
    ///
    /// Returns the number of samples of latency introduced by this effect.
    /// Used for latency compensation in DAWs. Most effects have zero latency;
    /// lookahead limiters and linear-phase filters are exceptions.
    ///
    /// Default returns 0 (no latency).
    fn latency_samples(&self) -> usize {
        0
    }

    /// Receive tempo and transport information from the host.
    ///
    /// Called once per buffer before processing begins. Effects that support
    /// tempo sync (delays, LFO-based modulation) should override this to
    /// update their internal timing. The default implementation is a no-op.
    ///
    /// # Arguments
    /// * `ctx` - Current tempo, transport state, and musical position
    fn set_tempo_context(&mut self, _ctx: &crate::tempo::TempoContext) {}

    /// Report the effect's ring-out tail in samples.
    ///
    /// Used by the graph engine for spillover — effects with non-zero tails
    /// are kept alive after removal, fed silence, and faded out over their
    /// tail duration. Default: 0 (no tail).
    ///
    /// Override for reverb, delay, and looper effects.
    fn tail_samples(&self) -> usize {
        0
    }
}

/// Extension trait for effects that report their tail/ring-out duration.
///
/// Separate from [`Effect`] to keep the base trait clean forever (Pattern P5:
/// Extension Traits). New capabilities are added via independent traits, not
/// by growing the base trait.
///
/// Effects with tails (reverb, delay, looper) implement this to report how
/// long audio continues after input stops. Used by:
/// - Spillover architecture (Phase 4) to determine crossfade duration
/// - Plugin hosts via `clap_plugin_tail`
/// - Graph engine for intelligent buffer management
///
/// Query via trait object downcast or via [`DspKernel::tail_samples()`](crate::DspKernel::tail_samples).
///
/// # Example
///
/// ```rust,ignore
/// use sonido_core::TailReporting;
///
/// if let Some(tail) = effect_ref.downcast_ref::<dyn TailReporting>() {
///     println!("Tail: {} samples", tail.tail_samples());
/// }
/// ```
pub trait TailReporting {
    /// Duration of the effect's ring-out tail in samples.
    ///
    /// Returns 0 for effects with no tail (distortion, EQ, compressor, etc.).
    /// Returns the ring-out duration for reverb, delay, looper.
    fn tail_samples(&self) -> usize;
}

/// Extension trait for chaining effects.
///
/// Provides a fluent interface for building effect chains with static dispatch.
/// For dynamic chains, use `Vec<Box<dyn Effect>>` instead.
pub trait EffectExt: Effect + Sized {
    /// Chain this effect with another, creating a composite effect.
    ///
    /// The output of `self` feeds into the input of `next`.
    ///
    /// # Example
    /// ```rust,ignore
    /// let chain = distortion.chain(chorus).chain(delay);
    /// ```
    fn chain<E: Effect>(self, next: E) -> Chain<Self, E> {
        Chain {
            first: self,
            second: next,
        }
    }
}

// Blanket implementation for all Effects
impl<T: Effect> EffectExt for T {}

/// Two effects chained in series.
///
/// Created by [`EffectExt::chain`]. The first effect's output feeds
/// into the second effect's input.
pub struct Chain<A, B> {
    first: A,
    second: B,
}

impl<A: Effect, B: Effect> Effect for Chain<A, B> {
    #[inline]
    fn process(&mut self, input: f32) -> f32 {
        let mid = self.first.process(input);
        self.second.process(mid)
    }

    #[inline]
    fn process_stereo(&mut self, left: f32, right: f32) -> (f32, f32) {
        let (mid_l, mid_r) = self.first.process_stereo(left, right);
        self.second.process_stereo(mid_l, mid_r)
    }

    fn process_block(&mut self, input: &[f32], output: &mut [f32]) {
        // Process first effect into output buffer
        self.first.process_block(input, output);
        // Process second effect in-place
        self.second.process_block_inplace(output);
    }

    fn process_block_stereo(
        &mut self,
        left_in: &[f32],
        right_in: &[f32],
        left_out: &mut [f32],
        right_out: &mut [f32],
    ) {
        // Process first effect into output buffers
        self.first
            .process_block_stereo(left_in, right_in, left_out, right_out);
        // Process second effect in-place
        self.second
            .process_block_stereo_inplace(left_out, right_out);
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.first.set_sample_rate(sample_rate);
        self.second.set_sample_rate(sample_rate);
    }

    fn reset(&mut self) {
        self.first.reset();
        self.second.reset();
    }

    fn latency_samples(&self) -> usize {
        self.first.latency_samples() + self.second.latency_samples()
    }

    fn is_true_stereo(&self) -> bool {
        // Chain is true stereo if either component is true stereo
        self.first.is_true_stereo() || self.second.is_true_stereo()
    }

    fn set_tempo_context(&mut self, ctx: &crate::tempo::TempoContext) {
        self.first.set_tempo_context(ctx);
        self.second.set_tempo_context(ctx);
    }

    fn tail_samples(&self) -> usize {
        self.first.tail_samples().max(self.second.tail_samples())
    }
}

impl<A, B> Chain<A, B> {
    /// Get a reference to the first effect in the chain.
    pub fn first(&self) -> &A {
        &self.first
    }

    /// Get a mutable reference to the first effect in the chain.
    pub fn first_mut(&mut self) -> &mut A {
        &mut self.first
    }

    /// Get a reference to the second effect in the chain.
    pub fn second(&self) -> &B {
        &self.second
    }

    /// Get a mutable reference to the second effect in the chain.
    pub fn second_mut(&mut self) -> &mut B {
        &mut self.second
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Mono effect - implements process() only
    struct Gain(f32);

    impl Effect for Gain {
        fn process(&mut self, input: f32) -> f32 {
            input * self.0
        }
        fn set_sample_rate(&mut self, _: f32) {}
        fn reset(&mut self) {}
    }

    // True stereo effect - implements process_stereo() only
    struct StereoSwap;

    impl Effect for StereoSwap {
        fn process_stereo(&mut self, left: f32, right: f32) -> (f32, f32) {
            (right, left) // Swap channels
        }
        fn is_true_stereo(&self) -> bool {
            true
        }
        fn set_sample_rate(&mut self, _: f32) {}
        fn reset(&mut self) {}
    }

    // True stereo effect - stereo widener
    struct StereoWidener {
        width: f32,
    }

    impl Effect for StereoWidener {
        fn process_stereo(&mut self, left: f32, right: f32) -> (f32, f32) {
            let mid = (left + right) * 0.5;
            let side = (left - right) * 0.5 * self.width;
            (mid + side, mid - side)
        }
        fn is_true_stereo(&self) -> bool {
            true
        }
        fn set_sample_rate(&mut self, _: f32) {}
        fn reset(&mut self) {}
    }

    #[test]
    fn test_chain() {
        let mut chain = Gain(2.0).chain(Gain(3.0));
        assert_eq!(chain.process(1.0), 6.0);
    }

    #[test]
    fn test_chain_block() {
        let mut chain = Gain(2.0).chain(Gain(0.5));
        let input = [1.0, 2.0, 3.0];
        let mut output = [0.0; 3];
        chain.process_block(&input, &mut output);
        assert_eq!(output, [1.0, 2.0, 3.0]);
    }

    #[test]
    fn test_chain_latency() {
        struct LatentEffect(usize);
        impl Effect for LatentEffect {
            fn process(&mut self, input: f32) -> f32 {
                input
            }
            fn set_sample_rate(&mut self, _: f32) {}
            fn reset(&mut self) {}
            fn latency_samples(&self) -> usize {
                self.0
            }
        }

        let chain = LatentEffect(10).chain(LatentEffect(5));
        assert_eq!(chain.latency_samples(), 15);
    }

    // New stereo tests

    #[test]
    fn test_mono_effect_stereo_processing() {
        // Mono effect should process channels independently
        let mut gain = Gain(2.0);
        let (left, right) = gain.process_stereo(1.0, 0.5);
        assert_eq!(left, 2.0);
        assert_eq!(right, 1.0);
    }

    #[test]
    fn test_mono_effect_is_not_true_stereo() {
        let gain = Gain(1.0);
        assert!(!gain.is_true_stereo());
    }

    #[test]
    fn test_true_stereo_effect() {
        let mut swap = StereoSwap;
        let (left, right) = swap.process_stereo(1.0, 2.0);
        assert_eq!(left, 2.0); // Was right
        assert_eq!(right, 1.0); // Was left
        assert!(swap.is_true_stereo());
    }

    #[test]
    fn test_true_stereo_mono_derivation() {
        // True stereo effect's mono processing should derive from stereo
        let mut swap = StereoSwap;
        // process() uses left channel of process_stereo(input, input)
        // StereoSwap returns (right, left), so with same input it's (input, input)
        let output = swap.process(1.0);
        assert_eq!(output, 1.0);
    }

    #[test]
    fn test_stereo_widener() {
        let mut widener = StereoWidener { width: 2.0 };
        // With width=2.0: mid=0.5*(L+R), side=0.5*(L-R)*2 = L-R
        // output_L = mid + side = 0.5*(L+R) + (L-R) = 1.5L - 0.5R
        // output_R = mid - side = 0.5*(L+R) - (L-R) = -0.5L + 1.5R
        let (left, right) = widener.process_stereo(1.0, 0.0);
        assert!((left - 1.5).abs() < 1e-6);
        assert!((right - (-0.5)).abs() < 1e-6);
    }

    #[test]
    fn test_process_block_stereo() {
        let mut gain = Gain(2.0);
        let left_in = [1.0, 2.0, 3.0];
        let right_in = [0.5, 1.0, 1.5];
        let mut left_out = [0.0; 3];
        let mut right_out = [0.0; 3];

        gain.process_block_stereo(&left_in, &right_in, &mut left_out, &mut right_out);

        assert_eq!(left_out, [2.0, 4.0, 6.0]);
        assert_eq!(right_out, [1.0, 2.0, 3.0]);
    }

    #[test]
    fn test_process_block_stereo_inplace() {
        let mut gain = Gain(0.5);
        let mut left = [2.0, 4.0, 6.0];
        let mut right = [1.0, 2.0, 3.0];

        gain.process_block_stereo_inplace(&mut left, &mut right);

        assert_eq!(left, [1.0, 2.0, 3.0]);
        assert_eq!(right, [0.5, 1.0, 1.5]);
    }

    #[test]
    fn test_chain_stereo() {
        // Chain mono effects and test stereo processing
        let mut chain = Gain(2.0).chain(Gain(3.0));
        let (left, right) = chain.process_stereo(1.0, 0.5);
        assert_eq!(left, 6.0); // 1.0 * 2.0 * 3.0
        assert_eq!(right, 3.0); // 0.5 * 2.0 * 3.0
    }

    #[test]
    fn test_chain_with_true_stereo() {
        // Chain mono gain with stereo swap
        let mut chain = Gain(2.0).chain(StereoSwap);
        let (left, right) = chain.process_stereo(1.0, 0.5);
        // After gain: (2.0, 1.0), after swap: (1.0, 2.0)
        assert_eq!(left, 1.0);
        assert_eq!(right, 2.0);
        assert!(chain.is_true_stereo());
    }

    #[test]
    fn test_chain_is_true_stereo_propagation() {
        // Chain of mono effects is not true stereo
        let mono_chain = Gain(1.0).chain(Gain(1.0));
        assert!(!mono_chain.is_true_stereo());

        // Chain with one true stereo effect is true stereo
        let stereo_chain1 = Gain(1.0).chain(StereoSwap);
        assert!(stereo_chain1.is_true_stereo());

        let stereo_chain2 = StereoSwap.chain(Gain(1.0));
        assert!(stereo_chain2.is_true_stereo());
    }

    #[test]
    fn test_chain_block_stereo() {
        let mut chain = Gain(2.0).chain(Gain(0.5));
        let left_in = [1.0, 2.0, 3.0];
        let right_in = [0.5, 1.0, 1.5];
        let mut left_out = [0.0; 3];
        let mut right_out = [0.0; 3];

        chain.process_block_stereo(&left_in, &right_in, &mut left_out, &mut right_out);

        // 2.0 * 0.5 = 1.0 overall gain
        assert_eq!(left_out, [1.0, 2.0, 3.0]);
        assert_eq!(right_out, [0.5, 1.0, 1.5]);
    }
}
