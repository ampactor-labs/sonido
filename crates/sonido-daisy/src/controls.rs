//! Lock-free shared control buffer for async control → real-time audio communication.
//!
//! [`ControlBuffer`] is the bridge between the asynchronous control task (writer, ~50 Hz)
//! and the real-time audio callback (reader, ~1500 Hz). It uses atomic operations with
//! `Relaxed` ordering — safe on single-core Cortex-M7 where all memory accesses are
//! sequentially consistent within the core.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────┐      ControlBuffer      ┌──────────────────┐
//! │  Control task    │ ──── write_knob() ────▶ │  Audio callback  │
//! │  (Embassy, 50Hz) │ ──── write_toggle() ──▶ │  (DMA, 1500Hz)   │
//! │                  │ ◀── read_led() ──────── │  write_led()     │
//! └─────────────────┘                          └──────────────────┘
//! ```
//!
//! # Direct Storage (No IIR)
//!
//! Knob values are stored directly without IIR smoothing. The 2-LSB ADC
//! jitter at `CYCLES387_5` sample time maps to sub-audible parameter
//! fluctuation across any param range. The epsilon change detection (3e-4)
//! prevents noise-triggered updates, making IIR redundant. This gives
//! instant knob response instead of the 460ms settling time that IIR imposed.
//!
//! # Change Detection
//!
//! Epsilon = 3e-4 (~10x ADC jitter at `CYCLES387_5`). The reader can check
//! whether a knob has changed since last read via [`ControlBuffer::read_knob_changed`].
//!
//! # Pure `core`
//!
//! No Embassy, no sonido-core, no alloc. Only `core::sync::atomic`.

use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};

/// Change detection epsilon: ~10x the ADC jitter at `CYCLES387_5` sample time.
///
/// Values changing by less than this are considered noise, not intentional movement.
const KNOB_EPSILON: f32 = 3e-4;

/// Maximum raw ADC value for 16-bit resolution.
const ADC_MAX_U16: f32 = 65535.0;

/// Lock-free shared state between async control task and real-time audio callback.
///
/// Generic over the number of knobs, toggles, footswitches, and LEDs. Use the
/// [`HothouseBuffer`] type alias for the Cleveland Music Co. Hothouse pedal platform.
///
/// # Memory Ordering
///
/// All atomics use `Relaxed` ordering. On single-core Cortex-M7, all memory accesses
/// are sequentially consistent within the core — the compiler fence from `Relaxed` is
/// sufficient. No `SeqCst` overhead needed.
///
/// # Const Initialization
///
/// `ControlBuffer::new()` is `const` — use it directly in `static` initializers:
///
/// ```ignore
/// use sonido_daisy::controls::HothouseBuffer;
/// static CONTROLS: HothouseBuffer = HothouseBuffer::new();
/// ```
pub struct ControlBuffer<
    const KNOBS: usize,
    const TOGGLES: usize,
    const FOOTSWITCHES: usize,
    const LEDS: usize,
> {
    /// Smoothed knob values as f32 bits (0.0–1.0 normalized).
    knobs: [AtomicU32; KNOBS],
    /// Last smoothed knob value written, for change detection.
    ///
    /// Updated on every `write_knob()` call (not just when the reader acknowledges
    /// the change). This prevents sub-epsilon ADC drift from accumulating until
    /// the total delta crosses `KNOB_EPSILON`.
    knobs_prev: [AtomicU32; KNOBS],
    /// Per-knob change flag (set by writer when value changes beyond epsilon).
    knobs_changed: [AtomicBool; KNOBS],
    /// Toggle positions: 0=UP, 1=MID, 2=DN.
    toggles: [AtomicU8; TOGGLES],
    /// Previous toggle positions for change detection.
    toggles_prev: [AtomicU8; TOGGLES],
    /// Per-toggle change flag.
    toggles_changed: [AtomicBool; TOGGLES],
    /// Footswitch states (true = pressed).
    footswitches: [AtomicBool; FOOTSWITCHES],
    /// LED brightness values as f32 bits (0.0=off, 1.0=on).
    leds: [AtomicU32; LEDS],
    /// Expression pedal value as f32 bits (0.0–1.0 normalized).
    ///
    /// Written by the hothouse control task at ~50 Hz; read by the audio
    /// callback. Non-generic: there is always 0 or 1 expression input.
    expression: AtomicU32,
}

/// Control buffer sized for the Cleveland Music Co. Hothouse pedal:
/// 6 knobs, 3 toggles (3-position), 2 footswitches, 2 LEDs.
pub type HothouseBuffer = ControlBuffer<6, 3, 2, 2>;

// ── Const array initialization helpers ────────────────────────────────────

/// Creates a const array of `AtomicU32::new(0)`.
macro_rules! atomic_u32_array {
    ($n:expr) => {{
        const ZERO: AtomicU32 = AtomicU32::new(0);
        [ZERO; $n]
    }};
}

/// Creates a const array of `AtomicU8::new(0)`.
macro_rules! atomic_u8_array {
    ($n:expr) => {{
        const ZERO: AtomicU8 = AtomicU8::new(0);
        [ZERO; $n]
    }};
}

/// Creates a const array of `AtomicBool::new(false)`.
macro_rules! atomic_bool_array {
    ($n:expr) => {{
        const FALSE: AtomicBool = AtomicBool::new(false);
        [FALSE; $n]
    }};
}

impl<const KNOBS: usize, const TOGGLES: usize, const FOOTSWITCHES: usize, const LEDS: usize>
    ControlBuffer<KNOBS, TOGGLES, FOOTSWITCHES, LEDS>
{
    /// Creates a new buffer with all values zeroed.
    ///
    /// Usable in `static` initializers:
    ///
    /// ```ignore
    /// static BUF: HothouseBuffer = HothouseBuffer::new();
    /// ```
    pub const fn new() -> Self {
        Self {
            knobs: atomic_u32_array!(KNOBS),
            knobs_prev: atomic_u32_array!(KNOBS),
            knobs_changed: atomic_bool_array!(KNOBS),
            toggles: atomic_u8_array!(TOGGLES),
            toggles_prev: atomic_u8_array!(TOGGLES),
            toggles_changed: atomic_bool_array!(TOGGLES),
            footswitches: atomic_bool_array!(FOOTSWITCHES),
            leds: atomic_u32_array!(LEDS),
            expression: AtomicU32::new(0),
        }
    }

    // ── Writer methods (control task, ~50 Hz) ─────────────────────────────

    /// Writes a raw 16-bit ADC reading.
    ///
    /// Normalizes `raw_u16` to 0.0–1.0 and stores it directly.
    ///
    /// # Parameters
    ///
    /// - `index`: Knob index (0-based). Panics if `>= KNOBS`.
    /// - `raw_u16`: Raw ADC value (0–65535).
    pub fn write_knob_raw(&self, index: usize, raw_u16: u16) {
        let normalized = raw_u16 as f32 / ADC_MAX_U16;
        self.write_knob(index, normalized);
    }

    /// Writes a pre-normalized knob value (0.0–1.0).
    ///
    /// ADC jitter (2 LSB at CYCLES387_5) maps to sub-audible param fluctuation
    /// across any param range. The epsilon change detection (3e-4) prevents
    /// noise-triggered updates. IIR smoothing was removed because it added
    /// 460ms latency solving a non-problem — hardware oversampling is the
    /// correct fix if noise ever becomes an issue.
    ///
    /// # Parameters
    ///
    /// - `index`: Knob index (0-based). Panics if `>= KNOBS`.
    /// - `normalized`: Input value in 0.0–1.0 range.
    pub fn write_knob(&self, index: usize, normalized: f32) {
        // Edge snap: avoid dead zones at pot extremes.
        let smoothed = if normalized < 0.005 {
            0.0
        } else if normalized > 0.995 {
            1.0
        } else {
            normalized
        };

        self.knobs[index].store(smoothed.to_bits(), Ordering::Relaxed);

        // Change detection: compare against last written smoothed value.
        let write_prev = f32::from_bits(self.knobs_prev[index].load(Ordering::Relaxed));
        let delta = if smoothed > write_prev {
            smoothed - write_prev
        } else {
            write_prev - smoothed
        };
        if delta > KNOB_EPSILON {
            self.knobs_changed[index].store(true, Ordering::Relaxed);
        }
        // Always update — prevents sub-epsilon drift accumulation.
        self.knobs_prev[index].store(smoothed.to_bits(), Ordering::Relaxed);
    }

    /// Writes a 3-position toggle state.
    ///
    /// # Parameters
    ///
    /// - `index`: Toggle index (0-based). Panics if `>= TOGGLES`.
    /// - `position`: 0=UP, 1=MID, 2=DN.
    pub fn write_toggle(&self, index: usize, position: u8) {
        let prev = self.toggles[index].swap(position, Ordering::Relaxed);
        if prev != position {
            self.toggles_changed[index].store(true, Ordering::Relaxed);
        }
    }

    /// Writes a footswitch state.
    ///
    /// # Parameters
    ///
    /// - `index`: Footswitch index (0-based). Panics if `>= FOOTSWITCHES`.
    /// - `pressed`: `true` if the switch is currently pressed.
    pub fn write_footswitch(&self, index: usize, pressed: bool) {
        self.footswitches[index].store(pressed, Ordering::Relaxed);
    }

    // ── Reader methods (audio callback, ~1500 Hz) ─────────────────────────

    /// Reads the current smoothed knob value (0.0–1.0).
    ///
    /// # Parameters
    ///
    /// - `index`: Knob index (0-based). Panics if `>= KNOBS`.
    pub fn read_knob(&self, index: usize) -> f32 {
        f32::from_bits(self.knobs[index].load(Ordering::Relaxed))
    }

    /// Reads the current knob value and whether it changed since the last call.
    ///
    /// The change flag is cleared after reading. The previous value is updated
    /// to the current value for future change detection.
    ///
    /// # Returns
    ///
    /// `(value, changed)` where `value` is 0.0–1.0 and `changed` is true if
    /// the value moved more than `KNOB_EPSILON` since the last read.
    pub fn read_knob_changed(&self, index: usize) -> (f32, bool) {
        let val = f32::from_bits(self.knobs[index].load(Ordering::Relaxed));
        let changed = self.knobs_changed[index].swap(false, Ordering::Relaxed);
        if changed {
            self.knobs_prev[index].store(val.to_bits(), Ordering::Relaxed);
        }
        (val, changed)
    }

    /// Reads the current toggle position.
    ///
    /// # Returns
    ///
    /// 0=UP, 1=MID, 2=DN.
    pub fn read_toggle(&self, index: usize) -> u8 {
        self.toggles[index].load(Ordering::Relaxed)
    }

    /// Reads the current toggle position and whether it changed since the last call.
    ///
    /// The change flag is cleared after reading.
    ///
    /// # Returns
    ///
    /// `(position, changed)` where position is 0=UP, 1=MID, 2=DN.
    pub fn read_toggle_changed(&self, index: usize) -> (u8, bool) {
        let val = self.toggles[index].load(Ordering::Relaxed);
        let changed = self.toggles_changed[index].swap(false, Ordering::Relaxed);
        if changed {
            self.toggles_prev[index].store(val, Ordering::Relaxed);
        }
        (val, changed)
    }

    /// Reads the current footswitch state.
    ///
    /// # Returns
    ///
    /// `true` if the footswitch is currently pressed.
    pub fn read_footswitch(&self, index: usize) -> bool {
        self.footswitches[index].load(Ordering::Relaxed)
    }

    // ── Expression pedal bridge (control task writes, audio callback reads) ─

    /// Writes a normalized expression pedal value (0.0–1.0).
    ///
    /// Called by the hothouse control task at ~50 Hz with the pre-processed
    /// output from [`ExpressionInput::value()`](crate::expression::ExpressionInput::value).
    ///
    /// # Parameters
    ///
    /// - `value`: Smoothed pedal position in 0.0–1.0 range.
    pub fn write_expression(&self, value: f32) {
        self.expression.store(value.to_bits(), Ordering::Relaxed);
    }

    /// Reads the current expression pedal value (0.0–1.0).
    ///
    /// Returns 0.0 when no expression pedal is connected (default after reset).
    pub fn read_expression(&self) -> f32 {
        f32::from_bits(self.expression.load(Ordering::Relaxed))
    }

    // ── LED bridge (audio writes, control task reads + drives GPIO) ───────

    /// Sets an LED value from the audio callback.
    ///
    /// The control task reads this value and drives the physical GPIO.
    ///
    /// # Parameters
    ///
    /// - `index`: LED index (0-based). Panics if `>= LEDS`.
    /// - `value`: 0.0 = off, 1.0 = on. Intermediate values for PWM dimming.
    pub fn write_led(&self, index: usize, value: f32) {
        self.leds[index].store(value.to_bits(), Ordering::Relaxed);
    }

    /// Reads an LED value (for the control task to drive GPIO).
    ///
    /// # Returns
    ///
    /// LED brightness: 0.0 = off, 1.0 = on.
    pub fn read_led(&self, index: usize) -> f32 {
        f32::from_bits(self.leds[index].load(Ordering::Relaxed))
    }
}

// Safety: ControlBuffer is Send+Sync because all fields are atomic.
// The AtomicXxx types are Send+Sync, and the struct contains only those.
// This is needed because the buffer is shared between Embassy tasks via &'static.
unsafe impl<const KNOBS: usize, const TOGGLES: usize, const FOOTSWITCHES: usize, const LEDS: usize>
    Sync for ControlBuffer<KNOBS, TOGGLES, FOOTSWITCHES, LEDS>
{
}

// Tests require host compilation (x86). Currently blocked by embassy/cortex-m deps
// at the crate level. Run if controls is ever extracted to a standalone crate.
#[cfg(test)]
mod tests {
    use super::*;

    type TestBuffer = ControlBuffer<2, 1, 1, 1>;

    #[test]
    fn sub_epsilon_writes_do_not_trigger_change() {
        let buf = TestBuffer::new();

        // Set initial value.
        buf.write_knob(0, 0.5);
        // Clear any change flag from the initial write.
        buf.read_knob_changed(0);

        // Write 50 times with sub-epsilon drift (~1e-5 per step).
        // Total drift = 50 * 1e-5 = 5e-4, which crosses KNOB_EPSILON (3e-4)
        // if drift accumulates against the initial value. With the fix,
        // knobs_prev updates every write, so no single step crosses epsilon.
        for i in 0..50 {
            let drift = 0.5 + (i as f32) * 1e-5;
            buf.write_knob(0, drift);
        }

        let (_, changed) = buf.read_knob_changed(0);
        assert!(
            !changed,
            "sub-epsilon writes should not trigger knobs_changed"
        );
    }

    #[test]
    fn large_knob_change_triggers_flag() {
        let buf = TestBuffer::new();
        buf.write_knob(0, 0.0);
        buf.read_knob_changed(0);

        buf.write_knob(0, 0.5);
        let (val, changed) = buf.read_knob_changed(0);
        assert!(changed, "large change should trigger knobs_changed");
        assert!((val - 0.5).abs() < 1e-6);
    }

    #[test]
    fn toggle_change_detection() {
        let buf = TestBuffer::new();
        buf.write_toggle(0, 0);
        let (_, changed) = buf.read_toggle_changed(0);
        assert!(
            !changed,
            "initial write to 0 should not trigger change (was 0)"
        );

        buf.write_toggle(0, 2);
        let (pos, changed) = buf.read_toggle_changed(0);
        assert!(changed);
        assert_eq!(pos, 2);
    }
}
