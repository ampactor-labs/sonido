//! Counter-based random numbers.
//!
//! A [`CounterRng`] is a pure function of a 64-bit key and a 64-bit counter.
//! Draw `n` doesn't depend on any earlier draw, so a generative piece can
//! compute any bar without replaying the bars before it, and two machines that
//! agree on the key agree on every number. That is what a sequential generator
//! (xorshift, PCG) can't offer without carrying state from the start.
//!
//! The mixer is SplitMix64's output function (Steele, Lea and Flood, "Fast
//! Splittable Pseudorandom Number Generators", OOPSLA 2014), a bijection on
//! `u64` that passes BigCrush when fed a counter.
//!
//! ```rust
//! use sonido_core::rng::CounterRng;
//!
//! let section = CounterRng::new(20261023).stream(417);
//! let a = section.unit(3);
//! assert_eq!(a, CounterRng::new(20261023).stream(417).unit(3));
//! assert!((0.0..1.0).contains(&a));
//! ```

/// SplitMix64's output function: a bijective 64-bit mixer.
#[inline]
pub const fn mix64(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// The 64-bit golden-ratio increment SplitMix64 uses (`2^64 / φ`).
const GOLDEN: u64 = 0x9E37_79B9_7F4A_7C15;

/// FNV-1a over bytes: a stable 64-bit key for a label such as a date or a name.
pub const fn key_from_bytes(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xCBF2_9CE4_8422_2325;
    let mut i = 0;
    while i < bytes.len() {
        h ^= bytes[i] as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01B3);
        i += 1;
    }
    h
}

/// Random numbers addressed by counter instead of by sequence.
///
/// `Copy` and stateless: every method takes the counter it should answer for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CounterRng {
    key: u64,
}

impl CounterRng {
    /// A generator for a key.
    pub const fn new(key: u64) -> Self {
        Self {
            key: mix64(key ^ GOLDEN),
        }
    }

    /// A generator keyed by a label's bytes (see [`key_from_bytes`]).
    pub const fn from_label(label: &[u8]) -> Self {
        Self::new(key_from_bytes(label))
    }

    /// An independent stream for a label: a section, a voice, a family.
    ///
    /// Streams nest, so `rng.stream(section).stream(voice)` is a stream of its
    /// own, unrelated to `rng.stream(voice).stream(section)`.
    pub const fn stream(self, label: u64) -> Self {
        Self {
            key: mix64(self.key ^ mix64(label.wrapping_add(GOLDEN))),
        }
    }

    /// The raw 64-bit draw for a counter.
    pub const fn u64(self, counter: u64) -> u64 {
        mix64(self.key.wrapping_add(counter.wrapping_mul(GOLDEN)))
    }

    /// A uniform `f32` in `[0, 1)` with 24 bits of resolution.
    pub fn unit(self, counter: u64) -> f32 {
        (self.u64(counter) >> 40) as f32 * (1.0 / 16_777_216.0)
    }

    /// A uniform integer in `[0, n)`; `0` when `n` is `0`.
    ///
    /// Lemire's multiply-shift on the top 32 bits: the bias is below
    /// `n / 2^32`, which no musical use can hear.
    pub const fn below(self, counter: u64, n: u32) -> u32 {
        (((self.u64(counter) >> 32) * n as u64) >> 32) as u32
    }

    /// `true` with probability `p` (clamped to `[0, 1]`).
    pub fn chance(self, counter: u64, p: f32) -> bool {
        self.unit(counter) < p
    }

    /// A uniform `f32` in `[lo, hi)`.
    pub fn range(self, counter: u64, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.unit(counter)
    }

    /// One item of a slice, or `None` for an empty slice.
    pub fn pick<T>(self, counter: u64, items: &[T]) -> Option<&T> {
        if items.is_empty() {
            return None;
        }
        items.get(self.below(counter, items.len() as u32) as usize)
    }

    /// An index chosen in proportion to `weights`.
    ///
    /// Negative and non-finite weights count as zero. If every weight is zero
    /// the draw is uniform over the indices; an empty slice returns `0`.
    pub fn weighted(self, counter: u64, weights: &[f32]) -> usize {
        let clean = |w: f32| if w.is_finite() && w > 0.0 { w } else { 0.0 };
        let total: f32 = weights.iter().map(|&w| clean(w)).sum();
        if weights.is_empty() {
            return 0;
        }
        if total <= 0.0 {
            return self.below(counter, weights.len() as u32) as usize;
        }
        let mut target = self.unit(counter) * total;
        for (i, &w) in weights.iter().enumerate() {
            let w = clean(w);
            if target < w {
                return i;
            }
            target -= w;
        }
        // Rounding can leave a sliver past the end; give it to the last
        // index with weight.
        weights.iter().rposition(|&w| clean(w) > 0.0).unwrap_or(0)
    }

    /// A Fisher–Yates shuffle of `items`, decided entirely by `counter`.
    pub fn shuffle<T>(self, counter: u64, items: &mut [T]) {
        let shuffler = self.stream(counter);
        for i in (1..items.len()).rev() {
            let j = shuffler.below(i as u64, (i + 1) as u32) as usize;
            items.swap(i, j);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draws_are_pure_functions_of_key_and_counter() {
        let a = CounterRng::new(7).stream(3);
        let b = CounterRng::new(7).stream(3);
        for n in 0..1000 {
            assert_eq!(a.u64(n), b.u64(n));
        }
        // Order of access doesn't matter: draw 999 before draw 0.
        assert_eq!(a.u64(999), CounterRng::new(7).stream(3).u64(999));
    }

    #[test]
    fn streams_are_distinct() {
        let base = CounterRng::new(42);
        let s1 = base.stream(1);
        let s2 = base.stream(2);
        let same = (0..256).filter(|&n| s1.u64(n) == s2.u64(n)).count();
        assert_eq!(same, 0);
        assert_ne!(base.stream(1).stream(2), base.stream(2).stream(1));
    }

    #[test]
    fn unit_is_in_range_and_centered() {
        let rng = CounterRng::from_label(b"2026-10-23");
        let mut sum = 0.0f64;
        for n in 0..100_000 {
            let u = rng.unit(n);
            assert!((0.0..1.0).contains(&u));
            sum += f64::from(u);
        }
        let mean = sum / 100_000.0;
        assert!((mean - 0.5).abs() < 0.005, "mean {mean}");
    }

    #[test]
    fn below_is_roughly_uniform() {
        let rng = CounterRng::new(1);
        let mut counts = [0u32; 7];
        for n in 0..70_000 {
            counts[rng.below(n, 7) as usize] += 1;
        }
        for &c in &counts {
            assert!((9_400..10_600).contains(&c), "{counts:?}");
        }
        assert_eq!(rng.below(5, 0), 0);
    }

    #[test]
    fn weighted_respects_weights() {
        let rng = CounterRng::new(9);
        let weights = [0.0, 3.0, 1.0, -2.0, f32::NAN];
        let mut counts = [0u32; 5];
        for n in 0..40_000 {
            counts[rng.weighted(n, &weights)] += 1;
        }
        assert_eq!(counts[0], 0);
        assert_eq!(counts[3], 0);
        assert_eq!(counts[4], 0);
        let ratio = f64::from(counts[1]) / f64::from(counts[2]);
        assert!((2.7..3.3).contains(&ratio), "{counts:?}");
        // All-zero weights fall back to uniform.
        let zeros = [0.0f32; 4];
        let mut seen = [false; 4];
        for n in 0..200 {
            seen[rng.weighted(n, &zeros)] = true;
        }
        assert!(seen.iter().all(|&s| s));
        assert_eq!(rng.weighted(0, &[]), 0);
    }

    #[test]
    fn shuffle_is_a_deterministic_permutation() {
        let rng = CounterRng::new(5);
        let mut a = [0, 1, 2, 3, 4, 5, 6, 7];
        let mut b = a;
        rng.shuffle(11, &mut a);
        rng.shuffle(11, &mut b);
        assert_eq!(a, b);
        let mut sorted = a;
        sorted.sort_unstable();
        assert_eq!(sorted, [0, 1, 2, 3, 4, 5, 6, 7]);
        let mut c = [0, 1, 2, 3, 4, 5, 6, 7];
        rng.shuffle(12, &mut c);
        assert_ne!(a, c, "different counters should shuffle differently");
    }

    #[test]
    fn pick_and_range() {
        let rng = CounterRng::new(3);
        assert_eq!(rng.pick::<u8>(0, &[]), None);
        assert!(matches!(rng.pick(0, &[10, 20, 30]), Some(10 | 20 | 30)));
        for n in 0..1000 {
            let v = rng.range(n, -2.0, 5.0);
            assert!((-2.0..5.0).contains(&v));
        }
    }

    #[test]
    fn labels_hash_stably() {
        // FNV-1a test vectors.
        assert_eq!(key_from_bytes(b""), 0xCBF2_9CE4_8422_2325);
        assert_eq!(key_from_bytes(b"a"), 0xAF63_DC4C_8601_EC8C);
        assert_eq!(key_from_bytes(b"foobar"), 0x8594_4171_F739_67E8);
    }
}
