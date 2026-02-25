//! Compiled schedule types for the DAG routing engine.
//!
//! A [`CompiledSchedule`] is an immutable snapshot produced by
//! [`ProcessingGraph::compile()`](super::ProcessingGraph::compile). It contains a
//! flat list of [`ProcessStep`] instructions that the audio thread executes
//! sequentially, plus the [`BufferPool`] and
//! compensation delay lines needed for execution.
//!
//! The schedule is shared with the audio thread via `Arc` — the audio thread
//! never sees partial state.

#[cfg(not(feature = "std"))]
use alloc::vec::Vec;

use super::buffer::{BufferPool, CompensationDelay};

/// A single instruction in the compiled processing schedule.
///
/// Steps are executed sequentially by the audio thread. The instruction set
/// is minimal: write external input, process an effect, copy/accumulate buffers,
/// apply latency compensation, and read final output.
#[derive(Debug)]
pub enum ProcessStep {
    /// Write external audio input into a buffer slot.
    WriteInput {
        /// Buffer slot to write the external input into.
        buffer_idx: usize,
    },

    /// Process audio through an effect node.
    ///
    /// Reads from `input_buf`, writes to `output_buf`. The node index
    /// references into `ProcessingGraph.nodes`.
    ProcessEffect {
        /// Index into the graph's node storage.
        node_idx: usize,
        /// Buffer slot containing the input signal.
        input_buf: usize,
        /// Buffer slot to write the processed output into.
        output_buf: usize,
    },

    /// Copy a source buffer to one or more destination buffers (fan-out from Split node).
    SplitCopy {
        /// Buffer slot to copy from.
        source_buf: usize,
        /// Buffer slots to copy into.
        dest_bufs: Vec<usize>,
    },

    /// Clear a buffer to silence (used before accumulation at Merge nodes).
    ClearBuffer {
        /// Buffer slot to zero out.
        buffer_idx: usize,
    },

    /// Add (accumulate) a source buffer into a destination buffer.
    ///
    /// Used for fan-in at Merge nodes: clear dest, then accumulate each input.
    AccumulateBuffer {
        /// Buffer slot to read from.
        source_buf: usize,
        /// Buffer slot to add into.
        dest_buf: usize,
    },

    /// Apply latency compensation delay to a buffer.
    ///
    /// Inserted automatically during compilation for shorter parallel paths
    /// feeding the same Merge node.
    DelayCompensate {
        /// Buffer slot to delay in-place.
        buffer_idx: usize,
        /// Index into the `CompiledSchedule.delay_lines` array.
        delay_line_idx: usize,
    },

    /// Read the final output from a buffer slot.
    ReadOutput {
        /// Buffer slot containing the final mixed output.
        buffer_idx: usize,
    },
}

/// Immutable compiled snapshot of the processing graph.
///
/// Shared with the audio thread via `Arc`. Never mutated after creation.
/// The audio thread sees complete state or nothing — no partial updates.
pub struct CompiledSchedule {
    /// Flat list of processing instructions, in execution order.
    pub(crate) steps: Vec<ProcessStep>,
    /// Audio buffer pool sized by liveness analysis.
    pub(crate) pool: BufferPool,
    /// Compensation delay lines for latency-mismatched parallel paths.
    pub(crate) delay_lines: Vec<CompensationDelay>,
    /// Total graph latency in samples (longest path from input to output).
    pub(crate) total_latency: usize,
}

impl CompiledSchedule {
    /// Returns the number of processing steps.
    pub fn step_count(&self) -> usize {
        self.steps.len()
    }

    /// Returns the number of buffer slots allocated.
    pub fn buffer_count(&self) -> usize {
        self.pool.count()
    }

    /// Returns the total graph latency in samples.
    pub fn total_latency(&self) -> usize {
        self.total_latency
    }
}
