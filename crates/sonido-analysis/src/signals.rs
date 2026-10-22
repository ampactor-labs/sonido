//! Test signals for calibrating effects against something like real playing.

/// The level-calibration reference: [`strummed_guitar`] peaking at
/// `sonido_core::level::NOMINAL_PEAK_DBFS`, 6 seconds long.
pub fn reference_guitar(sample_rate: f32) -> Vec<f32> {
    let peak = 10f32.powf(sonido_core::level::NOMINAL_PEAK_DBFS / 20.0);
    strummed_guitar(sample_rate, 6.0, peak)
}

/// A strummed open-E chord every second, `seconds` long, normalized to `peak`.
///
/// Six strings (82–330 Hz), each a stack of harmonics (1/h amplitude, up to
/// 8 kHz) with a 2 ms attack and a decay that shortens with the harmonic, the
/// strings 12 ms apart like a downstroke. It stands in for a guitar when
/// measuring how loud an effect plays: steady pitch content, sharp attacks
/// and decaying tails, with a crest factor near a real pick.
pub fn strummed_guitar(sample_rate: f32, seconds: f32, peak: f32) -> Vec<f32> {
    let n = (seconds * sample_rate) as usize;
    let strings = [82.41f32, 123.47, 164.81, 207.65, 246.94, 329.63];
    let mut out = vec![0.0f32; n];
    let mut strum = 0.0;
    while strum < seconds - 0.5 {
        for (s, &f) in strings.iter().enumerate() {
            let start = ((strum + 0.012 * s as f32) * sample_rate) as usize;
            let end = n.min(start + (3.0 * sample_rate) as usize);
            for h in 1..=12 {
                let fh = f * h as f32;
                if fh > 8_000.0 {
                    break;
                }
                let amp = 1.0 / h as f32;
                let tau = 0.9 / (h as f32).sqrt();
                for (i, v) in out.iter_mut().enumerate().take(end).skip(start) {
                    let t = (i - start) as f32 / sample_rate;
                    let attack = (t / 0.002).min(1.0);
                    *v += amp
                        * attack
                        * (-t / tau).exp()
                        * (2.0 * core::f32::consts::PI * fh * t).sin();
                }
            }
        }
        strum += 1.0;
    }
    let max = out.iter().fold(0.0f32, |m, x| m.max(x.abs()));
    if max > 0.0 {
        let k = peak / max;
        for v in &mut out {
            *v *= k;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peaks_at_the_requested_level() {
        let g = strummed_guitar(48_000.0, 2.0, 0.1);
        let max = g.iter().fold(0.0f32, |m, x| m.max(x.abs()));
        assert!((max - 0.1).abs() < 1e-6);
        assert_eq!(g.len(), 96_000);
    }
}
