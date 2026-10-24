//! Cross-correlation — time-domain (direct) and frequency-domain (FFT) implementations
//! with normalization and peak lag detection.
//!
//! Cross-correlation measures the similarity between two signals as a function of the
//! time-shift (lag) applied to one of them. It is fundamental for delay estimation,
//! pitch detection, and characterizing the relationship between two signals.
//!
//! # Mathematical Definition
//!
//! The cross-correlation of signals x and y at lag τ is:
//!
//! ```text
//! R_xy(τ) = Σ_{n} x[n] · y[n + τ]
//! ```
//!
//! When τ > 0, R_xy peaks at lag τ₀ when y is a delayed version of x by τ₀ samples,
//! i.e., y\[n\] = x\[n - τ₀\] → peak at τ = τ₀ (y leads x by τ₀).
//!
//! # FFT-based Computation
//!
//! For long signals the O(n · max_lag) direct sum is expensive. The FFT-based method
//! exploits the cross-correlation theorem:
//!
//! ```text
//! R_xy(τ) = IFFT( conj(X(f)) · Y(f) )
//! ```
//!
//! where X, Y are the DFTs of x and y zero-padded to avoid circular wrap-around.
//!
//! # References
//!
//! - Oppenheim & Schafer, "Discrete-Time Signal Processing" (3rd ed.), section 2.8.
//! - Proakis & Manolakis, "Digital Signal Processing" (4th ed.), section 6.4.

use crate::fft::Fft;
use rustfft::num_complex::Complex;

/// Compute the direct time-domain cross-correlation.
///
/// Time complexity O(n · max_lag). For large signals or large max_lag, prefer
/// [`xcorr_fft`] which is O(n log n).
///
/// # Arguments
///
/// * `x` — first signal
/// * `y` — second signal
/// * `max_lag` — maximum lag to evaluate (inclusive). The output has length
///   `2 * max_lag + 1`, with index 0 corresponding to lag `-max_lag` and
///   index `max_lag` corresponding to lag 0.
///
/// # Returns
///
/// `Vec<f32>` of length `2 * max_lag + 1`. Entry `i` contains R_xy at lag
/// `i as i32 - max_lag as i32`, i.e. layout `[R(-max_lag), …, R(0), …, R(max_lag)]`.
///
/// ```text
/// R_xy[τ] = Σ_{n} x[n] · y[n + τ]
/// ```
///
/// Reference: Oppenheim & Schafer, "Discrete-Time Signal Processing" (3rd ed.), §2.8.
pub fn xcorr_direct(x: &[f32], y: &[f32], max_lag: usize) -> Vec<f32> {
    let n = x.len().max(y.len());
    let len = 2 * max_lag + 1;
    let mut result = vec![0.0f32; len];

    for (out_i, lag) in (-(max_lag as i32)..=(max_lag as i32)).enumerate() {
        let mut sum = 0.0f32;
        for n_idx in 0..n {
            let m = n_idx as i32 + lag;
            if m >= 0 && (m as usize) < y.len() && n_idx < x.len() {
                sum += x[n_idx] * y[m as usize];
            }
        }
        result[out_i] = sum;
    }

    result
}

/// Compute FFT-based cross-correlation.
///
/// Uses the cross-correlation theorem R_xy = IFFT(conj(X) · Y) with zero-padding
/// to avoid circular wrap-around. Time complexity O(n log n).
///
/// # Arguments
///
/// * `x` — first signal
/// * `y` — second signal
/// * `max_lag` — maximum lag to extract from the full circular result
///
/// # Returns
///
/// `Vec<f32>` of length `2 * max_lag + 1` arranged as
/// `[R(-max_lag), …, R(0), …, R(max_lag)]`.
///
/// ```text
/// R_xy = IFFT( conj(FFT(x)) · FFT(y) )
/// ```
///
/// Reference: Oppenheim & Schafer, "Discrete-Time Signal Processing" (3rd ed.), §2.8.
pub fn xcorr_fft(x: &[f32], y: &[f32], max_lag: usize) -> Vec<f32> {
    let sig_len = x.len().max(y.len());
    // Zero-pad to next power of 2 >= len(x) + len(y) - 1 to avoid circular aliasing.
    let min_fft_size = x.len() + y.len().saturating_sub(1).max(1);
    let fft_size = min_fft_size.next_power_of_two().max(2);

    let fft = Fft::new(fft_size);

    // Build complex buffers
    let mut buf_x: Vec<Complex<f32>> = x.iter().map(|&v| Complex::new(v, 0.0)).collect();
    buf_x.resize(fft_size, Complex::new(0.0, 0.0));

    let mut buf_y: Vec<Complex<f32>> = y.iter().map(|&v| Complex::new(v, 0.0)).collect();
    buf_y.resize(fft_size, Complex::new(0.0, 0.0));

    fft.forward_complex(&mut buf_x);
    fft.forward_complex(&mut buf_y);

    // Multiply conj(X) * Y element-wise
    for (cx, cy) in buf_x.iter_mut().zip(buf_y.iter()) {
        *cx = cx.conj() * cy;
    }

    // Inverse FFT
    fft.inverse_complex(&mut buf_x);

    // The circular result has:
    //   Positive lags τ = 0, 1, …  at indices 0, 1, …
    //   Negative lags τ = -1, -2, … at indices fft_size-1, fft_size-2, …
    //
    // Assemble output: [R(-max_lag), …, R(-1), R(0), R(1), …, R(max_lag)]
    let actual_max_lag = max_lag.min(sig_len.saturating_sub(1)).min(fft_size / 2);
    let out_len = 2 * max_lag + 1;
    let mut result = vec![0.0f32; out_len];

    for (out_i, lag) in (-(max_lag as i32)..=(max_lag as i32)).enumerate() {
        let fft_idx = if lag >= 0 {
            lag as usize
        } else {
            (fft_size as i32 + lag) as usize
        };
        // Guard against out-of-range indices when max_lag >= fft_size
        if fft_idx < fft_size {
            result[out_i] = buf_x[fft_idx].re;
        }
    }

    // Suppress unused variable warning from `actual_max_lag` guard above
    let _ = actual_max_lag;

    result
}

/// Normalized cross-correlation (Pearson correlation coefficient per lag).
///
/// Each lag value is divided by sqrt(Σ x² · Σ y²), scaling the output to [-1, 1].
/// A value of 1.0 at lag τ means x and y are perfectly correlated when y is shifted
/// by τ samples.
///
/// # Arguments
///
/// * `x` — first signal
/// * `y` — second signal
/// * `max_lag` — maximum lag
///
/// # Returns
///
/// `Vec<f32>` of length `2 * max_lag + 1` with values in [-1, 1].
///
/// ```text
/// R̂_xy(τ) = R_xy(τ) / sqrt(Σ x[n]² · Σ y[n]²)
/// ```
pub fn xcorr_normalized(x: &[f32], y: &[f32], max_lag: usize) -> Vec<f32> {
    let raw = xcorr_direct(x, y, max_lag);

    let norm_x: f32 = x.iter().map(|&v| v * v).sum::<f32>().sqrt();
    let norm_y: f32 = y.iter().map(|&v| v * v).sum::<f32>().sqrt();
    let denom = norm_x * norm_y;

    if denom < 1e-12 {
        return raw; // both signals near-zero — normalization undefined, return as-is
    }

    raw.iter().map(|&r| r / denom).collect()
}

/// Find the lag of maximum absolute correlation and its value.
///
/// # Arguments
///
/// * `correlation` — output of [`xcorr_direct`], [`xcorr_fft`], or [`xcorr_normalized`]
/// * `max_lag` — the `max_lag` used when computing `correlation`; used to convert
///   array index to signed lag
///
/// # Returns
///
/// `(lag, value)` where `lag` is the signed lag in samples and `value` is the
/// correlation at that lag. Positive lag means y leads x (`y[n] ≈ x[n - lag]`).
///
/// Uses maximum absolute value so that strongly negative correlations (anti-phase)
/// are also found.
pub fn peak_lag(correlation: &[f32], max_lag: usize) -> (i32, f32) {
    if correlation.is_empty() {
        return (0, 0.0);
    }

    let (best_idx, &best_val) = correlation
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| {
            a.abs()
                .partial_cmp(&b.abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .unwrap();

    let lag = best_idx as i32 - max_lag as i32;
    (lag, best_val)
}

/// Generalized cross-correlation with phase-transform weighting (GCC-PHAT).
///
/// Plain cross-correlation is dominated by whatever frequencies are loudest,
/// so in a room its peak smears across reflections. GCC-PHAT divides the
/// cross-spectrum by its own magnitude before the inverse FFT, keeping only
/// phase, which turns a pure delay into a sharp spike (Knapp and Carter,
/// "The generalized correlation method for estimation of time delay", IEEE
/// Trans. ASSP 24(4), 1976). Full whitening is fragile in noise, so `beta`
/// weights the cross-spectrum by `1 / |X*·Y|^beta`: `1.0` is classic PHAT,
/// `0.0` is plain cross-correlation, and about `0.4`–`0.7` (PHAT-β) is the
/// usual compromise for noisy, reverberant rooms.
///
/// The output has the same layout as [`xcorr_fft`]:
/// `[R(-max_lag), …, R(0), …, R(max_lag)]`, so [`peak_lag`] reads it, and a
/// positive lag means `y` lags `x`.
pub fn gcc_phat(x: &[f32], y: &[f32], max_lag: usize, beta: f32) -> Vec<f32> {
    let min_fft_size = x.len() + y.len().saturating_sub(1).max(1);
    let fft_size = min_fft_size.next_power_of_two().max(2);
    let fft = Fft::new(fft_size);

    let mut buf_x: Vec<Complex<f32>> = x.iter().map(|&v| Complex::new(v, 0.0)).collect();
    buf_x.resize(fft_size, Complex::new(0.0, 0.0));
    let mut buf_y: Vec<Complex<f32>> = y.iter().map(|&v| Complex::new(v, 0.0)).collect();
    buf_y.resize(fft_size, Complex::new(0.0, 0.0));
    fft.forward_complex(&mut buf_x);
    fft.forward_complex(&mut buf_y);

    // Weighted cross-spectrum. The floor keeps silent bins from dividing by
    // zero; it sits far below any bin with signal in it.
    let beta = beta.clamp(0.0, 1.0);
    let floor = 1e-12f32;
    for (cx, cy) in buf_x.iter_mut().zip(buf_y.iter()) {
        let g = cx.conj() * cy;
        let mag = g.norm().max(floor);
        *cx = g / mag.powf(beta);
    }
    fft.inverse_complex(&mut buf_x);

    (-(max_lag as i64)..=(max_lag as i64))
        .map(|lag| {
            let idx = if lag >= 0 {
                lag as usize
            } else {
                (fft_size as i64 + lag) as usize
            };
            buf_x.get(idx).map_or(0.0, |c| c.re)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    fn sine(freq: f32, sr: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (2.0 * PI * freq * i as f32 / sr).sin())
            .collect()
    }

    /// Simple reproducible PRNG for white noise.
    fn white_noise(n: usize, seed: u32) -> Vec<f32> {
        let mut state = seed;
        (0..n)
            .map(|_| {
                state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                (state as i32 as f32) / (i32::MAX as f32)
            })
            .collect()
    }

    #[test]
    fn test_autocorrelation_peak_at_zero() {
        let x = sine(10.0, 1000.0, 512);
        let max_lag = 50;
        let corr = xcorr_direct(&x, &x, max_lag);

        // Zero-lag is at index max_lag
        let zero_lag_val = corr[max_lag];
        for (i, &v) in corr.iter().enumerate() {
            if i != max_lag {
                assert!(
                    zero_lag_val.abs() >= v.abs(),
                    "Autocorrelation peak should be at lag 0, but index {i} (lag {}) has larger value {} > {}",
                    i as i32 - max_lag as i32,
                    v.abs(),
                    zero_lag_val.abs()
                );
            }
        }
    }

    #[test]
    fn test_delayed_sine_peak_at_delay() {
        let delay = 100usize;
        let sr = 1000.0;
        let n = 512;
        let x = sine(10.0, sr, n);

        // y is x delayed by `delay` samples
        let mut y = vec![0.0f32; n];
        y[delay..n].copy_from_slice(&x[..(n - delay)]);

        let max_lag = 150;
        let corr = xcorr_direct(&x, &y, max_lag);
        let (lag, _) = peak_lag(&corr, max_lag);

        // y[n] = x[n - delay] → y leads at positive lag = delay
        assert_eq!(lag, delay as i32, "Expected peak at lag {delay}, got {lag}");
    }

    #[test]
    fn test_direct_matches_fft() {
        let x = sine(5.0, 500.0, 128);
        let y = sine(5.0, 500.0, 128);
        let max_lag = 30;

        let direct = xcorr_direct(&x, &y, max_lag);
        let fft_based = xcorr_fft(&x, &y, max_lag);

        assert_eq!(direct.len(), fft_based.len());
        for (i, (&d, &f)) in direct.iter().zip(fft_based.iter()).enumerate() {
            assert!(
                (d - f).abs() < 0.5,
                "Mismatch at index {i}: direct={d:.4}, fft={f:.4}"
            );
        }
    }

    #[test]
    fn test_normalized_range() {
        let x = sine(7.0, 1000.0, 256);
        let y = sine(13.0, 1000.0, 256);
        let corr = xcorr_normalized(&x, &y, 50);

        for (i, &v) in corr.iter().enumerate() {
            assert!(
                (-1.0 - 1e-5..=1.0 + 1e-5).contains(&v),
                "Normalized correlation out of [-1,1] at index {i}: {v}"
            );
        }
    }

    #[test]
    fn test_uncorrelated_signals_near_zero() {
        // White noise vs a sine wave: peak normalized correlation should be small.
        let noise = white_noise(256, 0xDEAD_BEEF);
        let tone = sine(10.0, 1000.0, 256);
        let corr = xcorr_normalized(&noise, &tone, 50);

        let peak = corr.iter().map(|&v| v.abs()).fold(0.0f32, f32::max);
        assert!(
            peak < 0.3,
            "Expected peak correlation < 0.3 for uncorrelated signals, got {peak:.3}"
        );
    }

    #[test]
    fn test_peak_lag_sign_convention() {
        // y = x delayed by D → y[n] = x[n - D]
        // R_xy(τ) = Σ x[n] · y[n + τ] peaks when τ = D (y leads x)
        let delay = 20usize;
        let n = 200;
        let x: Vec<f32> = (0..n).map(|i| (i as f32 * 0.1).sin()).collect();
        let mut y = vec![0.0f32; n];
        y[delay..n].copy_from_slice(&x[..(n - delay)]);

        let max_lag = 40;
        let corr = xcorr_direct(&x, &y, max_lag);
        let (lag, _) = peak_lag(&corr, max_lag);

        assert_eq!(
            lag, delay as i32,
            "Positive lag should indicate y leads x: expected {delay}, got {lag}"
        );
    }
    /// A room in miniature: a delayed copy, a strong reflection and noise.
    fn room(delay: usize, reflection: usize, noise: f32) -> (Vec<f32>, Vec<f32>) {
        let n = 8192;
        let src = white_noise(n + 400, 7);
        let x: Vec<f32> = src[400..].to_vec();
        let hiss = white_noise(n, 99);
        let y: Vec<f32> = (0..n)
            .map(|i| 0.6 * src[400 + i - delay] + 0.5 * src[400 + i - reflection] + noise * hiss[i])
            .collect();
        (x, y)
    }

    #[test]
    fn gcc_phat_finds_the_delay_in_a_room() {
        let (x, y) = room(37, 140, 0.3);
        for beta in [1.0f32, 0.7, 0.4] {
            let r = gcc_phat(&x, &y, 300, beta);
            let (lag, _) = peak_lag(&r, 300);
            assert_eq!(lag, 37, "beta {beta}");
        }
    }

    #[test]
    fn gcc_phat_peak_is_sharper_than_plain_xcorr() {
        // Low-passed noise makes plain cross-correlation broad; PHAT whitens it.
        let raw = white_noise(4096 + 64, 3);
        let mut lp = vec![0.0f32; raw.len()];
        for i in 1..raw.len() {
            lp[i] = 0.95 * lp[i - 1] + 0.05 * raw[i];
        }
        let x = lp[64..].to_vec();
        let y: Vec<f32> = (0..4096).map(|i| lp[64 + i - 21]).collect();
        let width = |r: &[f32]| {
            let (_, peak) = peak_lag(r, 100);
            r.iter().filter(|v| v.abs() > 0.5 * peak.abs()).count()
        };
        let plain = xcorr_fft(&x, &y, 100);
        let phat = gcc_phat(&x, &y, 100, 1.0);
        assert_eq!(peak_lag(&phat, 100).0, 21);
        assert!(
            width(&phat) * 4 < width(&plain),
            "{} vs {}",
            width(&phat),
            width(&plain)
        );
    }

    #[test]
    fn gcc_phat_with_beta_zero_is_plain_xcorr() {
        let x = white_noise(512, 5);
        let y = white_noise(512, 6);
        let a = gcc_phat(&x, &y, 20, 0.0);
        let b = xcorr_fft(&x, &y, 20);
        for (p, q) in a.iter().zip(b.iter()) {
            assert!((p - q).abs() < 1e-3, "{p} vs {q}");
        }
    }
}
