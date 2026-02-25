//! Graph-based audio processing engine.
//!
//! [`GraphEngine`] wraps [`ProcessingGraph`] to provide a higher-level API for
//! audio processing with DAG-based routing. It mirrors [`ProcessingEngine`]'s
//! interface for backward compatibility while enabling parallel paths, sidechains,
//! and multiband processing.
//!
//! # Migration from `ProcessingEngine`
//!
//! Use [`from_chain()`](GraphEngine::from_chain) for drop-in replacement of linear
//! chains. The output is numerically identical to `ProcessingEngine` for linear
//! topologies.
//!
//! ```rust,ignore
//! use sonido_io::GraphEngine;
//!
//! let mut engine = GraphEngine::from_chain(
//!     vec![Box::new(Distortion::new(48000.0)), Box::new(Reverb::new(48000.0))],
//!     48000.0,
//!     256,
//! )?;
//!
//! engine.process_block_stereo(&left_in, &right_in, &mut left_out, &mut right_out);
//! ```

use crate::StereoSamples;
use sonido_core::Effect;
use sonido_core::graph::{GraphError, ProcessingGraph};

/// Graph-based processing engine for DAG audio routing.
///
/// Wraps [`ProcessingGraph`] with a convenient API for common operations.
/// Supports both linear chains (backward compatible) and arbitrary DAG topologies.
pub struct GraphEngine {
    graph: ProcessingGraph,
}

impl GraphEngine {
    /// Creates a `GraphEngine` from an already-configured [`ProcessingGraph`].
    ///
    /// The graph must already be compiled (via [`ProcessingGraph::compile()`]).
    pub fn new(graph: ProcessingGraph) -> Self {
        Self { graph }
    }

    /// Creates a linear effect chain: Input → E1 → E2 → ... → En → Output.
    ///
    /// This is a drop-in replacement for `ProcessingEngine` with identical output.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError`] if graph construction or compilation fails.
    pub fn from_chain(
        effects: Vec<Box<dyn Effect + Send>>,
        sample_rate: f32,
        block_size: usize,
    ) -> Result<Self, GraphError> {
        let graph = ProcessingGraph::linear(effects, sample_rate, block_size)?;
        Ok(Self { graph })
    }

    /// Returns a mutable reference to the underlying [`ProcessingGraph`].
    ///
    /// Use this for direct graph mutations (add/remove/connect nodes, recompile).
    pub fn graph_mut(&mut self) -> &mut ProcessingGraph {
        &mut self.graph
    }

    /// Returns a reference to the underlying [`ProcessingGraph`].
    pub fn graph(&self) -> &ProcessingGraph {
        &self.graph
    }

    /// Returns the sample rate.
    pub fn sample_rate(&self) -> f32 {
        self.graph.sample_rate()
    }

    /// Sets the sample rate for all effect nodes. Requires recompilation.
    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.graph.set_sample_rate(sample_rate);
    }

    /// Resets all effect nodes and clears delay lines.
    pub fn reset(&mut self) {
        self.graph.reset();
    }

    /// Returns the total graph latency in samples.
    pub fn latency_samples(&self) -> usize {
        self.graph.latency_samples()
    }

    /// Processes a block of stereo audio through the graph.
    ///
    /// Output buffers must be at least as large as input buffers.
    pub fn process_block_stereo(
        &mut self,
        left_in: &[f32],
        right_in: &[f32],
        left_out: &mut [f32],
        right_out: &mut [f32],
    ) {
        self.graph
            .process_block(left_in, right_in, left_out, right_out);
    }

    /// Processes an entire stereo file through the graph.
    ///
    /// Splits the input into blocks of the graph's block size and processes each.
    pub fn process_file_stereo(
        &mut self,
        input: &StereoSamples,
        block_size: usize,
    ) -> StereoSamples {
        let len = input.len();
        let mut left_out = vec![0.0; len];
        let mut right_out = vec![0.0; len];

        for i in (0..len).step_by(block_size) {
            let chunk_len = block_size.min(len - i);
            let end = i + chunk_len;

            self.graph.process_block(
                &input.left[i..end],
                &input.right[i..end],
                &mut left_out[i..end],
                &mut right_out[i..end],
            );
        }

        StereoSamples::new(left_out, right_out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProcessingEngine;

    /// Simple test effect that multiplies by a constant.
    struct Gain {
        factor: f32,
    }

    impl Effect for Gain {
        fn process(&mut self, input: f32) -> f32 {
            input * self.factor
        }
        fn set_sample_rate(&mut self, _sample_rate: f32) {}
        fn reset(&mut self) {}
    }

    #[test]
    fn test_from_chain_passthrough() {
        let effects: Vec<Box<dyn Effect + Send>> = vec![Box::new(Gain { factor: 1.0 })];
        let mut engine = GraphEngine::from_chain(effects, 48000.0, 256).unwrap();

        let left_in = [1.0, 2.0, 3.0, 4.0];
        let right_in = [0.5, 1.0, 1.5, 2.0];
        let mut left_out = [0.0; 4];
        let mut right_out = [0.0; 4];

        engine.process_block_stereo(&left_in, &right_in, &mut left_out, &mut right_out);

        assert_eq!(left_out, [1.0, 2.0, 3.0, 4.0]);
        assert_eq!(right_out, [0.5, 1.0, 1.5, 2.0]);
    }

    #[test]
    fn test_graph_matches_processing_engine() {
        // Build identical chains in both engines.
        let sr = 48000.0;
        let block_size = 64;

        let mut legacy = ProcessingEngine::new(sr);
        legacy.add_effect(Box::new(Gain { factor: 2.0 }));
        legacy.add_effect(Box::new(Gain { factor: 0.5 }));

        let effects: Vec<Box<dyn Effect + Send>> = vec![
            Box::new(Gain { factor: 2.0 }),
            Box::new(Gain { factor: 0.5 }),
        ];
        let mut graph_engine = GraphEngine::from_chain(effects, sr, block_size).unwrap();

        // Generate test input.
        let left_in: Vec<f32> = (0..block_size).map(|i| (i as f32) * 0.01).collect();
        let right_in: Vec<f32> = (0..block_size).map(|i| (i as f32) * -0.01).collect();

        // Process with legacy engine.
        let mut legacy_left = vec![0.0; block_size];
        let mut legacy_right = vec![0.0; block_size];
        legacy.process_block_stereo(&left_in, &right_in, &mut legacy_left, &mut legacy_right);

        // Process with graph engine.
        let mut graph_left = vec![0.0; block_size];
        let mut graph_right = vec![0.0; block_size];
        graph_engine.process_block_stereo(&left_in, &right_in, &mut graph_left, &mut graph_right);

        // Outputs must match exactly.
        for i in 0..block_size {
            assert!(
                (legacy_left[i] - graph_left[i]).abs() < 1e-6,
                "left mismatch at {i}: legacy={}, graph={}",
                legacy_left[i],
                graph_left[i]
            );
            assert!(
                (legacy_right[i] - graph_right[i]).abs() < 1e-6,
                "right mismatch at {i}: legacy={}, graph={}",
                legacy_right[i],
                graph_right[i]
            );
        }
    }

    #[test]
    fn test_process_file_stereo() {
        let effects: Vec<Box<dyn Effect + Send>> = vec![Box::new(Gain { factor: 0.5 })];
        let mut engine = GraphEngine::from_chain(effects, 48000.0, 64).unwrap();

        let left: Vec<f32> = (0..1000).map(|i| i as f32).collect();
        let right: Vec<f32> = (0..1000).map(|i| (i as f32) * 2.0).collect();
        let input = StereoSamples::new(left.clone(), right.clone());

        let output = engine.process_file_stereo(&input, 64);

        assert_eq!(output.len(), 1000);
        for i in 0..output.len() {
            assert!(
                (output.left[i] - left[i] * 0.5).abs() < 1e-6,
                "left mismatch at {i}"
            );
            assert!(
                (output.right[i] - right[i] * 0.5).abs() < 1e-6,
                "right mismatch at {i}"
            );
        }
    }

    #[test]
    fn test_graph_engine_latency() {
        struct LatentGain {
            factor: f32,
            latency: usize,
        }

        impl Effect for LatentGain {
            fn process(&mut self, input: f32) -> f32 {
                input * self.factor
            }
            fn set_sample_rate(&mut self, _: f32) {}
            fn reset(&mut self) {}
            fn latency_samples(&self) -> usize {
                self.latency
            }
        }

        let effects: Vec<Box<dyn Effect + Send>> = vec![
            Box::new(LatentGain {
                factor: 1.0,
                latency: 64,
            }),
            Box::new(LatentGain {
                factor: 1.0,
                latency: 128,
            }),
        ];
        let engine = GraphEngine::from_chain(effects, 48000.0, 256).unwrap();
        assert_eq!(engine.latency_samples(), 192);
    }
}
