//! Compiled schedule types for the DAG routing engine.
//!
//! A [`CompiledSchedule`] is an immutable snapshot produced by
//! [`ProcessingGraph::compile()`](super::ProcessingGraph::compile). It contains a
//! flat list of [`ProcessStep`] instructions that the audio thread executes
//! sequentially, plus metadata about buffer requirements and latency compensation.
//!
//! The schedule is shared with the audio thread via `Arc` — the audio thread
//! never sees partial state.

#[cfg(not(feature = "std"))]
use alloc::vec::Vec;

/// Maximum number of fan-out targets for a single Split node.
///
/// Fixed-size array eliminates the `Vec<usize>` heap allocation from `SplitCopy`,
/// making the `ProcessStep` enum entirely stack-allocated.
pub const MAX_SPLIT_TARGETS: usize = 8;

/// A single instruction in the compiled processing schedule.
///
/// Steps are executed sequentially by the audio thread. The instruction set
/// is minimal: write external input, process an effect, copy/accumulate buffers,
/// apply latency compensation, and read final output.
///
/// All variants are stack-allocated (no heap pointers) for RT-safety.
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
    ///
    /// Uses a fixed-size array to avoid heap allocation. `dest_count` indicates
    /// how many entries in `dest_bufs` are valid.
    SplitCopy {
        /// Buffer slot to copy from.
        source_buf: usize,
        /// Buffer slots to copy into (first `dest_count` entries are valid).
        dest_bufs: [usize; MAX_SPLIT_TARGETS],
        /// Number of valid entries in `dest_bufs`.
        dest_count: usize,
    },

    /// Clear a buffer to silence (used before accumulation at Merge nodes).
    ClearBuffer {
        /// Buffer slot to zero out.
        buffer_idx: usize,
    },

    /// Add (accumulate) a source buffer into a destination buffer, scaled by `gain`.
    ///
    /// Used for fan-in at Merge nodes: clear dest, then accumulate each input.
    /// Gain is `1.0 / path_count` so that N paths sum to unity, preventing
    /// amplitude doubling on split→merge topologies.
    AccumulateBuffer {
        /// Buffer slot to read from.
        source_buf: usize,
        /// Buffer slot to add into.
        dest_buf: usize,
        /// Gain applied to each accumulated sample (typically `1.0 / path_count`).
        gain: f32,
    },

    /// Apply latency compensation delay to a buffer.
    ///
    /// Inserted automatically during compilation for shorter parallel paths
    /// feeding the same Merge node.
    DelayCompensate {
        /// Buffer slot to delay in-place.
        buffer_idx: usize,
        /// Index into the persistent `ProcessingGraph.audio_delay_lines` array.
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
///
/// Buffer pool and compensation delay lines are owned by [`ProcessingGraph`](super::ProcessingGraph)
/// as persistent fields for RT-safe execution (zero per-block allocations).
/// This schedule stores only the counts/sizes needed to validate or rebuild them.
pub struct CompiledSchedule {
    /// Flat list of processing instructions, in execution order.
    pub(crate) steps: Vec<ProcessStep>,
    /// Number of buffer slots required (determined by liveness analysis).
    pub(crate) buffer_count: usize,
    /// Delay sample counts for latency compensation on parallel paths.
    /// Each entry corresponds to a `DelayCompensate` step's `delay_line_idx`.
    pub(crate) delay_sample_counts: Vec<usize>,
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
        self.buffer_count
    }

    /// Returns the total graph latency in samples.
    pub fn total_latency(&self) -> usize {
        self.total_latency
    }

    /// Returns the number of compensation delay lines.
    pub fn delay_line_count(&self) -> usize {
        self.delay_sample_counts.len()
    }
}
