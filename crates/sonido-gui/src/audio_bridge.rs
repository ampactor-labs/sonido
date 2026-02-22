//! Lock-free audio↔GUI communication.
//!
//! Provides metering data transport, transport state (running flag), and
//! standalone input/master gain controls. Per-effect parameter sharing is
//! handled by [`AtomicParamBridge`](super::atomic_param_bridge) — this module
//! only owns the two global gain knobs that live outside the effect chain.

use crate::chain_manager::ChainCommand;
use crate::file_player::TransportCommand;
use crossbeam_channel::{Receiver, Sender, bounded, unbounded};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// A thread-safe atomic parameter using bit-cast f32.
///
/// GUI thread writes, audio thread reads. No locks, no allocations.
#[derive(Debug)]
pub struct AtomicParam {
    value: AtomicU32,
    min: f32,
    max: f32,
    default: f32,
}

impl AtomicParam {
    /// Create a new atomic parameter with range and default.
    pub fn new(default: f32, min: f32, max: f32) -> Self {
        Self {
            value: AtomicU32::new(default.to_bits()),
            min,
            max,
            default,
        }
    }

    /// Set the parameter value (GUI thread).
    #[inline]
    pub fn set(&self, v: f32) {
        let clamped = v.clamp(self.min, self.max);
        self.value.store(clamped.to_bits(), Ordering::Release);
    }

    /// Get the parameter value (audio thread).
    #[inline]
    pub fn get(&self) -> f32 {
        f32::from_bits(self.value.load(Ordering::Acquire))
    }

    /// Get the minimum value.
    pub fn min(&self) -> f32 {
        self.min
    }

    /// Get the maximum value.
    pub fn max(&self) -> f32 {
        self.max
    }

    /// Get the default value.
    pub fn default(&self) -> f32 {
        self.default
    }

    /// Reset to default value.
    pub fn reset(&self) {
        self.set(self.default);
    }
}

impl Clone for AtomicParam {
    fn clone(&self) -> Self {
        Self {
            value: AtomicU32::new(self.value.load(Ordering::Acquire)),
            min: self.min,
            max: self.max,
            default: self.default,
        }
    }
}

/// Metering data sent from audio thread to GUI.
#[derive(Debug, Clone, Copy, Default)]
pub struct MeteringData {
    /// Input signal peak level (linear).
    pub input_peak: f32,
    /// Input signal RMS level (linear).
    pub input_rms: f32,
    /// Output signal peak level (linear).
    pub output_peak: f32,
    /// Output signal RMS level (linear).
    pub output_rms: f32,
    /// Compressor gain reduction in dB.
    pub gain_reduction: f32,
    /// Audio thread CPU usage (0.0 to 100.0).
    pub cpu_usage: f32,
    /// File playback position in seconds (0.0 when not playing a file).
    pub playback_position_secs: f32,
}

/// Audio bridge for communication between GUI and audio threads.
///
/// Owns the two global gain controls (input gain, master volume) that sit
/// outside the per-effect parameter system, plus metering transport,
/// the running flag, and a command channel for dynamic chain mutations.
pub struct AudioBridge {
    input_gain: Arc<AtomicParam>,
    master_volume: Arc<AtomicParam>,
    running: Arc<AtomicBool>,
    metering_tx: Sender<MeteringData>,
    metering_rx: Receiver<MeteringData>,
    command_tx: Sender<ChainCommand>,
    command_rx: Receiver<ChainCommand>,
    transport_tx: Sender<TransportCommand>,
    transport_rx: Receiver<TransportCommand>,
    chain_bypass: Arc<AtomicBool>,
    /// Cumulative count of audio stream errors (output + input).
    error_count: Arc<AtomicU32>,
}

impl AudioBridge {
    /// Create a new audio bridge.
    pub fn new() -> Self {
        let (metering_tx, metering_rx) = bounded(4);
        let (command_tx, command_rx) = unbounded();
        let (transport_tx, transport_rx) = unbounded();
        Self {
            input_gain: Arc::new(AtomicParam::new(-120.0, -120.0, 20.0)),
            master_volume: Arc::new(AtomicParam::new(0.0, -40.0, 6.0)),
            running: Arc::new(AtomicBool::new(false)),
            metering_tx,
            metering_rx,
            command_tx,
            command_rx,
            transport_tx,
            transport_rx,
            chain_bypass: Arc::new(AtomicBool::new(false)),
            error_count: Arc::new(AtomicU32::new(0)),
        }
    }

    /// Get the input gain control.
    pub fn input_gain(&self) -> Arc<AtomicParam> {
        Arc::clone(&self.input_gain)
    }

    /// Get the master volume control.
    pub fn master_volume(&self) -> Arc<AtomicParam> {
        Arc::clone(&self.master_volume)
    }

    /// Get the running flag.
    pub fn running(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.running)
    }

    /// Send metering data from audio thread (non-blocking).
    pub fn send_metering(&self, data: MeteringData) {
        // Try to send, drop if buffer is full (OK for metering)
        let _ = self.metering_tx.try_send(data);
    }

    /// Get metering sender for audio thread.
    pub fn metering_sender(&self) -> Sender<MeteringData> {
        self.metering_tx.clone()
    }

    /// Receive latest metering data (GUI thread).
    pub fn receive_metering(&self) -> Option<MeteringData> {
        // Get the most recent metering data, discarding older ones
        let mut latest = None;
        while let Ok(data) = self.metering_rx.try_recv() {
            latest = Some(data);
        }
        latest
    }

    /// Set the running state.
    pub fn set_running(&self, running: bool) {
        self.running.store(running, Ordering::SeqCst);
    }

    /// Check if audio is running.
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// Send a chain mutation command to the audio thread (non-blocking).
    ///
    /// Commands are drained by the audio thread at the start of each buffer
    /// via the receiver obtained from [`command_receiver`](Self::command_receiver).
    pub fn send_command(&self, cmd: ChainCommand) {
        let _ = self.command_tx.send(cmd);
    }

    /// Get a clone of the command receiver for the audio thread.
    ///
    /// `crossbeam` receivers are cheaply cloneable. In practice only one
    /// audio thread should drain the channel.
    pub fn command_receiver(&self) -> Receiver<ChainCommand> {
        self.command_rx.clone()
    }

    /// Get a clone of the transport command sender for the file player.
    pub fn transport_sender(&self) -> Sender<TransportCommand> {
        self.transport_tx.clone()
    }

    /// Get a clone of the transport command receiver for the audio thread.
    pub fn transport_receiver(&self) -> Receiver<TransportCommand> {
        self.transport_rx.clone()
    }

    /// Get the chain bypass flag.
    ///
    /// When true, the audio processor passes dry signal through with a
    /// click-free crossfade, bypassing all effects.
    pub fn chain_bypass(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.chain_bypass)
    }

    /// Get the cumulative audio stream error count.
    ///
    /// Incremented by cpal error callbacks on both input and output streams.
    /// The GUI reads this to display a non-intrusive error indicator.
    pub fn error_count(&self) -> Arc<AtomicU32> {
        Arc::clone(&self.error_count)
    }
}

impl Default for AudioBridge {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_atomic_param_clamping() {
        let param = AtomicParam::new(0.5, 0.0, 1.0);

        param.set(1.5);
        assert_eq!(param.get(), 1.0);

        param.set(-0.5);
        assert_eq!(param.get(), 0.0);

        param.set(0.75);
        assert_eq!(param.get(), 0.75);
    }

    #[test]
    fn test_atomic_param_reset() {
        let param = AtomicParam::new(0.5, 0.0, 1.0);
        param.set(0.8);
        assert_eq!(param.get(), 0.8);

        param.reset();
        assert_eq!(param.get(), 0.5);
    }

    #[test]
    fn test_command_channel() {
        use sonido_registry::EffectRegistry;

        let bridge = AudioBridge::new();
        let rx = bridge.command_receiver();

        let registry = EffectRegistry::new();
        let effect = registry.create("distortion", 48000.0).unwrap();
        bridge.send_command(ChainCommand::Add {
            id: "distortion",
            effect,
        });
        bridge.send_command(ChainCommand::Remove {
            slot: sonido_gui_core::SlotIndex(0),
        });

        // Drain the channel
        let cmd1 = rx.try_recv().unwrap();
        assert!(matches!(
            cmd1,
            ChainCommand::Add {
                id: "distortion",
                ..
            }
        ));

        let cmd2 = rx.try_recv().unwrap();
        assert!(matches!(
            cmd2,
            ChainCommand::Remove { slot } if slot == sonido_gui_core::SlotIndex(0)
        ));

        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn test_audio_bridge_metering() {
        let bridge = AudioBridge::new();

        bridge.send_metering(MeteringData {
            input_peak: 0.5,
            input_rms: 0.3,
            output_peak: 0.6,
            output_rms: 0.4,
            gain_reduction: 3.0,
            cpu_usage: 12.5,
            playback_position_secs: 0.0,
        });

        let data = bridge.receive_metering();
        assert!(data.is_some());
        let data = data.unwrap();
        assert_eq!(data.input_peak, 0.5);
    }

    #[test]
    fn test_audio_bridge_gain_accessors() {
        let bridge = AudioBridge::new();

        let input = bridge.input_gain();
        assert_eq!(input.get(), 0.0);
        assert_eq!(input.min(), -20.0);
        assert_eq!(input.max(), 20.0);

        let master = bridge.master_volume();
        assert_eq!(master.get(), 0.0);
        assert_eq!(master.min(), -40.0);
        assert_eq!(master.max(), 6.0);

        // Verify Arc identity — same underlying allocation
        input.set(6.0);
        assert_eq!(bridge.input_gain().get(), 6.0);

        master.set(-10.0);
        assert_eq!(bridge.master_volume().get(), -10.0);
    }
}
