//! DAG routing engine for the Sonido DSP framework.
//!
//! The graph module replaces linear effect chains with a compiled processing graph:
//! edit the graph at mutation time (add/remove/connect), compile to a
//! [`CompiledSchedule`] snapshot, execute that schedule per audio block with zero
//! allocations.
//!
//! # Architecture
//!
//! The system uses a **two-object split** (adapted from the Tracktion pattern):
//!
//! - [`ProcessingGraph`] — owned by the mutation thread. Holds topology (nodes,
//!   edges), performs mutations, runs [`compile()`](ProcessingGraph::compile).
//!   NOT touched by the audio thread.
//! - [`CompiledSchedule`] — immutable snapshot. Holds a flat
//!   [`Vec<ProcessStep>`](ProcessStep) + [`BufferPool`] +
//!   latency map. Shared with the audio thread via `Arc`. The audio thread never
//!   sees partial state.
//!
//! # Buffer Efficiency
//!
//! Buffer assignment uses liveness analysis (register allocation): a buffer is
//! "live" from the step that writes it to the last step that reads it. After the
//! last reader, the slot is freed for reuse. A 20-node linear chain uses exactly
//! 2 buffers (ping-pong). A diamond uses 3.
//!
//! # Latency Compensation
//!
//! Each node reports [`latency_samples()`](crate::Effect::latency_samples). During
//! compilation, the engine computes the longest path latency to each Merge node
//! and inserts [`CompensationDelay`] steps for shorter
//! parallel paths.
//!
//! # Click-free Schedule Swap
//!
//! When a new schedule replaces the old one, both run simultaneously during a ~5ms
//! crossfade window (via [`SmoothedParam`](crate::SmoothedParam)). This eliminates
//! audible clicks from topology changes.
//!
//! # Example
//!
//! ```rust,ignore
//! use sonido_core::graph::{ProcessingGraph, GraphError};
//!
//! let mut graph = ProcessingGraph::new(48000.0, 256);
//! let input = graph.add_input();
//! let output = graph.add_output();
//! let dist = graph.add_effect(Box::new(Distortion::new(48000.0)));
//! let reverb = graph.add_effect(Box::new(Reverb::new(48000.0)));
//!
//! graph.connect(input, dist)?;
//! graph.connect(dist, reverb)?;
//! graph.connect(reverb, output)?;
//! graph.compile()?;
//!
//! // Process audio blocks
//! graph.process_block(&left_in, &right_in, &mut left_out, &mut right_out);
//! ```
//!
//! # no_std Support
//!
//! This module is `no_std` compatible with `alloc`. The `Arc` type comes from
//! `alloc::sync::Arc` in `no_std` mode.

pub mod buffer;
pub mod edge;
pub mod node;
mod processing;
pub mod schedule;

pub use buffer::{BufferPool, CompensationDelay, StereoBuffer};
pub use edge::EdgeId;
pub use node::{NodeId, NodeKind};
pub use processing::{GraphError, ProcessingGraph};
pub use schedule::{CompiledSchedule, ProcessStep};
