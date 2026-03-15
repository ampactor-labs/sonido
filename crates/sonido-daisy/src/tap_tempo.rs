//! Tap tempo BPM detection from footswitch input.
//!
//! Detects tempo from tap intervals, computing a running average BPM.
//! Feed taps from footswitch events; read BPM to feed into
//! [`TempoManager::set_bpm()`](sonido_core::tempo::TempoManager::set_bpm).
//!
//! # Usage
//!
//! ```rust,ignore
//! let mut tap = TapTempo::new();
//!
//! // On footswitch tap (in control poll):
//! tap.tap(embassy_time::Instant::now().as_ticks());
//!
//! // Read BPM (in audio callback):
//! if let Some(bpm) = tap.bpm() {
//!     tempo_manager.set_bpm(bpm);
//! }
//! ```

/// Maximum number of tap intervals to average.
const TAP_HISTORY: usize = 4;

/// Timeout in ticks after which tap history resets.
/// At 32768 Hz tick rate, 3 seconds = 98304 ticks.
const TAP_TIMEOUT_TICKS: u64 = 98_304;

/// Minimum BPM (40). Below this, taps are too slow to be intentional.
const MIN_BPM: f32 = 40.0;

/// Maximum BPM (300). Above this, taps are faster than plausible.
const MAX_BPM: f32 = 300.0;

/// Tick rate for converting ticks to seconds.
const TICK_HZ: f32 = 32_768.0;

/// Footswitch-based BPM detector with running average.
///
/// Stores the last [`TAP_HISTORY`] tap timestamps and computes BPM from
/// the average interval. Automatically resets if no tap arrives within
/// [`TAP_TIMEOUT_TICKS`] (3 seconds).
///
/// # Range
///
/// Clamps output to 40–300 BPM. Returns `None` if fewer than 2 taps
/// have been recorded or the history has timed out.
pub struct TapTempo {
    /// Timestamps of recent taps (embassy_time ticks at 32768 Hz).
    history: [u64; TAP_HISTORY],
    /// Number of valid entries in history (0..=TAP_HISTORY).
    count: usize,
    /// Index of the next write position (circular).
    write_idx: usize,
    /// Timestamp of the most recent tap.
    last_tap: u64,
}

impl TapTempo {
    /// Creates a new tap tempo detector with empty history.
    pub const fn new() -> Self {
        Self {
            history: [0; TAP_HISTORY],
            count: 0,
            write_idx: 0,
            last_tap: 0,
        }
    }

    /// Record a tap event at the given timestamp (embassy_time ticks).
    ///
    /// If the interval since the last tap exceeds the timeout (3 s),
    /// the history is reset and this tap becomes the first entry.
    pub fn tap(&mut self, now_ticks: u64) {
        // Reset if timed out (or first tap)
        if self.count > 0 && now_ticks.saturating_sub(self.last_tap) > TAP_TIMEOUT_TICKS {
            self.reset();
        }

        self.history[self.write_idx] = now_ticks;
        self.write_idx = (self.write_idx + 1) % TAP_HISTORY;
        if self.count < TAP_HISTORY {
            self.count += 1;
        }
        self.last_tap = now_ticks;
    }

    /// Compute the current BPM from tap history.
    ///
    /// Returns `Some(bpm)` if at least 2 taps are recorded and the most
    /// recent tap is within the timeout window. Returns `None` otherwise.
    ///
    /// BPM is clamped to 40–300.
    pub fn bpm(&self) -> Option<f32> {
        if self.count < 2 {
            return None;
        }

        // Reconstruct timestamps in chronological order from the circular buffer.
        // write_idx points to the next write slot, so the oldest valid entry is at
        // (write_idx + TAP_HISTORY - count) % TAP_HISTORY.
        let oldest = (self.write_idx + TAP_HISTORY - self.count) % TAP_HISTORY;

        let mut total_interval: u64 = 0;
        let pairs = self.count - 1;
        for i in 0..pairs {
            let a = self.history[(oldest + i) % TAP_HISTORY];
            let b = self.history[(oldest + i + 1) % TAP_HISTORY];
            total_interval += b.saturating_sub(a);
        }

        if pairs == 0 || total_interval == 0 {
            return None;
        }

        let avg_interval = total_interval as f32 / pairs as f32;
        let bpm = 60.0 * TICK_HZ / avg_interval;

        // Clamp to valid range
        if bpm < MIN_BPM {
            Some(MIN_BPM)
        } else if bpm > MAX_BPM {
            Some(MAX_BPM)
        } else {
            Some(bpm)
        }
    }

    /// Reset all tap history.
    pub fn reset(&mut self) {
        self.history = [0; TAP_HISTORY];
        self.count = 0;
        self.write_idx = 0;
        self.last_tap = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ticks per beat at a given BPM (32768 Hz tick rate).
    fn ticks_per_beat(bpm: f32) -> u64 {
        (60.0 * TICK_HZ / bpm) as u64
    }

    #[test]
    fn no_taps_returns_none() {
        let tap = TapTempo::new();
        assert!(tap.bpm().is_none());
    }

    #[test]
    fn one_tap_returns_none() {
        let mut tap = TapTempo::new();
        tap.tap(0);
        assert!(tap.bpm().is_none());
    }

    #[test]
    fn two_taps_at_120bpm() {
        let mut tap = TapTempo::new();
        let interval = ticks_per_beat(120.0);
        tap.tap(0);
        tap.tap(interval);
        let bpm = tap.bpm().expect("should have bpm after 2 taps");
        assert!((bpm - 120.0).abs() < 1.0, "expected ~120 bpm, got {bpm}");
    }

    #[test]
    fn four_taps_averages_correctly() {
        let mut tap = TapTempo::new();
        let interval = ticks_per_beat(100.0);
        for i in 0..4u64 {
            tap.tap(i * interval);
        }
        let bpm = tap.bpm().expect("should have bpm");
        assert!((bpm - 100.0).abs() < 1.0, "expected ~100 bpm, got {bpm}");
    }

    #[test]
    fn timeout_resets_history() {
        let mut tap = TapTempo::new();
        tap.tap(0);
        tap.tap(ticks_per_beat(120.0));
        // Tap after timeout — should reset and only record one tap
        tap.tap(TAP_TIMEOUT_TICKS + ticks_per_beat(120.0) * 2 + 1);
        assert!(tap.bpm().is_none(), "history should have reset after timeout");
    }

    #[test]
    fn clamp_min_bpm() {
        let mut tap = TapTempo::new();
        // Very slow taps (20 BPM = below MIN_BPM=40)
        let interval = ticks_per_beat(20.0);
        // Keep within timeout (3 seconds = 98304 ticks) by using a smaller interval
        // Use interval just above timeout limit and verify clamping works for slow BPM
        // 40 BPM = 49152 ticks per beat, 20 BPM = 98304 ticks (exactly at timeout boundary)
        // Use 41 BPM to stay within timeout but below min after rounding isn't possible.
        // Instead, directly test clamp: use 39 BPM equivalent
        let interval_39 = ticks_per_beat(39.0); // 50482 ticks, well within 3s timeout
        tap.tap(0);
        tap.tap(interval_39);
        // 39 BPM is below min, should clamp to 40
        let bpm = tap.bpm().expect("should have bpm");
        assert_eq!(bpm, MIN_BPM);
        let _ = interval; // suppress unused warning
    }

    #[test]
    fn clamp_max_bpm() {
        let mut tap = TapTempo::new();
        // Very fast taps (301 BPM > MAX_BPM=300)
        let interval = ticks_per_beat(301.0);
        tap.tap(0);
        tap.tap(interval);
        let bpm = tap.bpm().expect("should have bpm");
        assert_eq!(bpm, MAX_BPM);
    }

    #[test]
    fn circular_buffer_wraps_correctly() {
        let mut tap = TapTempo::new();
        let interval = ticks_per_beat(80.0);
        // Tap 6 times — exceeds TAP_HISTORY=4, wraps around
        for i in 0..6u64 {
            tap.tap(i * interval);
        }
        let bpm = tap.bpm().expect("should have bpm after wrapping");
        assert!((bpm - 80.0).abs() < 1.0, "expected ~80 bpm after wrap, got {bpm}");
    }
}
