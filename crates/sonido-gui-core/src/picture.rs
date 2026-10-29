//! Screen pictures: what each family's screen shows where the sun used to be.
//!
//! The design system's rule is that the screen shows the real signal, so
//! every picture is either measured from the effect's own kernel or computed
//! from the live audio tap; none is drawn from a formula that could drift
//! from the DSP.
//!
//! | Family | Picture | Source |
//! |---|---|---|
//! | HEAT | transfer curve, output against input | kernel probe: a 100 Hz full-scale sine |
//! | VOLT | level curve, dB out against dB in | kernel probe: a 1 kHz sine stepped in 3 dB levels |
//! | WAVE | frequency response | kernel probe: an impulse |
//! | ECHO | impulse response envelope (taps, decay) | kernel probe: an impulse |
//! | FLUX, RIG | spectrum, input against output | live tap |
//! | LINE | goniometer (stereo field) | live tap |
//! | tuner | the tuner's own readout | — |
//!
//! The probes run a private copy of the kernel (from the registry) with the
//! plugin's current parameter values copied in, so they show what the effect
//! does even when no audio is playing.

use sonido_core::{EffectWithParams, ParamFlags};

use crate::theme::Family;

/// Which picture a screen shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PictureKind {
    /// Output against input for a full-scale sine (HEAT).
    Transfer,
    /// Steady-state output level against input level, in dB (VOLT).
    Level,
    /// Magnitude response from 20 Hz to 20 kHz (WAVE).
    Response,
    /// Envelope of the impulse response over time (ECHO).
    Impulse,
    /// Live spectrum of the input against the output (FLUX, RIG).
    Spectrum,
    /// Live stereo field: mid against side (LINE).
    Stereo,
    /// The tuner's note and cents readout.
    Tuner,
}

impl PictureKind {
    /// The picture for registry effect `effect_id` in `family`.
    pub fn for_effect(effect_id: &str, family: Family) -> Self {
        if effect_id == "tuner" {
            return Self::Tuner;
        }
        match family {
            Family::Heat => Self::Transfer,
            Family::Volt => Self::Level,
            Family::Wave => Self::Response,
            Family::Echo => Self::Impulse,
            Family::Flux | Family::Rig => Self::Spectrum,
            Family::Line => Self::Stereo,
        }
    }

    /// Whether this picture is measured from a private kernel (as opposed to
    /// the live tap).
    pub fn is_probe(self) -> bool {
        matches!(
            self,
            Self::Transfer | Self::Level | Self::Response | Self::Impulse
        )
    }

    /// The rate the probe kernel runs at. The impulse response is long (up to
    /// [`IMPULSE_SECONDS`]), so it runs at half rate; nothing in a tap pattern
    /// or a decay needs more than 12 kHz of bandwidth to be seen.
    pub fn probe_rate(self, host_rate: f32) -> f32 {
        let host = if host_rate > 0.0 { host_rate } else { 48_000.0 };
        if self == Self::Impulse {
            24_000.0
        } else {
            host
        }
    }
}

/// Input levels of the level curve, in dBFS (peak of the probe sine).
pub const LEVELS_DB: [f32; 21] = [
    -60.0, -57.0, -54.0, -51.0, -48.0, -45.0, -42.0, -39.0, -36.0, -33.0, -30.0, -27.0, -24.0,
    -21.0, -18.0, -15.0, -12.0, -9.0, -6.0, -3.0, 0.0,
];

/// Number of log-spaced frequencies in a response or spectrum.
pub const FREQ_BINS: usize = 96;
/// Lowest frequency shown.
pub const F_LO: f32 = 20.0;
/// Highest frequency shown.
pub const F_HI: f32 = 20_000.0;

/// Length of the impulse-response window, in seconds.
pub const IMPULSE_SECONDS: f32 = 2.5;
/// Columns in the impulse envelope.
pub const IMPULSE_COLUMNS: usize = 160;

/// The `i`-th of [`FREQ_BINS`] log-spaced frequencies from [`F_LO`] to [`F_HI`].
pub fn bin_freq(i: usize) -> f32 {
    let u = i as f32 / (FREQ_BINS - 1) as f32;
    F_LO * (F_HI / F_LO).powf(u)
}

/// A measurement, ready to draw.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Measured {
    /// Nothing measured yet (or no kernel).
    #[default]
    None,
    /// `(input, output)` pairs over one cycle of the probe sine.
    Transfer(Vec<[f32; 2]>),
    /// Output level in dB for each of [`LEVELS_DB`].
    Level(Vec<f32>),
    /// Gain in dB at each of the [`FREQ_BINS`] frequencies.
    Response(Vec<f32>),
    /// Peak level in dB (relative to the impulse) per column over
    /// [`IMPULSE_SECONDS`].
    Impulse(Vec<f32>),
}

const BLOCK: usize = 256;

fn db(x: f32) -> f32 {
    20.0 * x.max(1e-9).log10()
}

/// Run `input` through `fx` in blocks, returning the mono output.
fn run(fx: &mut dyn EffectWithParams, input: &[f32]) -> Vec<f32> {
    let mut out = Vec::with_capacity(input.len());
    let mut l = [0.0_f32; BLOCK];
    let mut r = [0.0_f32; BLOCK];
    for chunk in input.chunks(BLOCK) {
        let n = chunk.len();
        l[..n].copy_from_slice(chunk);
        r[..n].copy_from_slice(chunk);
        fx.process_block_stereo_inplace(&mut l[..n], &mut r[..n]);
        out.extend(l[..n].iter().zip(&r[..n]).map(|(a, b)| (a + b) * 0.5));
    }
    out
}

/// Silence long enough for parameter smoothers to land on their targets.
fn warm(fx: &mut dyn EffectWithParams, sample_rate: f32) {
    let n = (sample_rate * 0.15) as usize;
    run(fx, &vec![0.0; n]);
}

/// Copy `values` (one per parameter, in index order) into `fx`, skipping the
/// read-only diagnostics, and clear its state.
pub fn prepare(fx: &mut dyn EffectWithParams, values: &[f32]) {
    fx.reset();
    for (i, &v) in values.iter().enumerate().take(fx.effect_param_count()) {
        let writable = fx
            .effect_param_info(i)
            .is_some_and(|d| !d.flags.contains(ParamFlags::READ_ONLY));
        if writable {
            fx.effect_set_param(i, v);
        }
    }
}

/// Output against input over one cycle of a full-scale 100 Hz sine, after a
/// cycle to settle. A memoryless shaper draws a line; tone filters open it
/// into a loop, which is the truth about them.
pub fn transfer_curve(fx: &mut dyn EffectWithParams, sample_rate: f32) -> Vec<[f32; 2]> {
    warm(fx, sample_rate);
    let period = (sample_rate / 100.0).round() as usize;
    let input: Vec<f32> = (0..period * 2)
        .map(|i| (std::f32::consts::TAU * i as f32 / period as f32).sin())
        .collect();
    let output = run(fx, &input);
    input[period..]
        .iter()
        .zip(&output[period..])
        .map(|(&x, &y)| [x, y])
        .collect()
}

/// Output level (dB, sine peak) for each of [`LEVELS_DB`]: a 1 kHz sine held
/// 50 ms at each level, rising, measured over the last 10 ms of each step.
pub fn level_curve(fx: &mut dyn EffectWithParams, sample_rate: f32) -> Vec<f32> {
    warm(fx, sample_rate);
    let step = (sample_rate * 0.05) as usize;
    let tail = (sample_rate * 0.01) as usize;
    let w = std::f32::consts::TAU * 1000.0 / sample_rate;
    let mut phase = 0usize;
    LEVELS_DB
        .iter()
        .map(|&level| {
            let a = 10f32.powf(level / 20.0);
            let input: Vec<f32> = (0..step)
                .map(|i| a * (w * (phase + i) as f32).sin())
                .collect();
            phase += step;
            let out = run(fx, &input);
            let ms = out[step - tail..].iter().map(|v| v * v).sum::<f32>() / tail as f32;
            // RMS of a sine × √2 = its peak, so in and out share a scale.
            db(ms.sqrt() * std::f32::consts::SQRT_2)
        })
        .collect()
}

/// Gain in dB at each of the [`FREQ_BINS`] frequencies, from the kernel's
/// response to an impulse (4096 samples, DFT at each frequency).
pub fn frequency_response(fx: &mut dyn EffectWithParams, sample_rate: f32) -> Vec<f32> {
    warm(fx, sample_rate);
    // Small enough that a level-dependent filter (an auto-wah) stays at rest.
    const AMP: f32 = 0.1;
    let mut input = vec![0.0_f32; 4096];
    input[0] = AMP;
    let ir = run(fx, &input);
    (0..FREQ_BINS)
        .map(|k| {
            let w = std::f32::consts::TAU * bin_freq(k) / sample_rate;
            let (mut re, mut im) = (0.0_f32, 0.0_f32);
            for (n, &y) in ir.iter().enumerate() {
                let a = w * n as f32;
                re += y * a.cos();
                im -= y * a.sin();
            }
            db(re.hypot(im) / AMP)
        })
        .collect()
}

/// Peak level per column (dB relative to the impulse) over
/// [`IMPULSE_SECONDS`]: delay taps stand up as spikes, a reverb as a slope.
pub fn impulse_envelope(fx: &mut dyn EffectWithParams, sample_rate: f32) -> Vec<f32> {
    warm(fx, sample_rate);
    const AMP: f32 = 0.5;
    let n = (sample_rate * IMPULSE_SECONDS) as usize;
    let mut input = vec![0.0_f32; n];
    input[0] = AMP;
    let out = run(fx, &input);
    let per = n.div_ceil(IMPULSE_COLUMNS);
    out.chunks(per)
        .map(|c| db(c.iter().fold(0.0_f32, |m, v| m.max(v.abs())) / AMP))
        .collect()
}

/// Spectrum of `samples` in dBFS at the [`FREQ_BINS`] frequencies, Hann
/// windowed (the window's coherent gain is taken back out, so a full-scale
/// sine reads about 0 dB).
pub fn spectrum(samples: &[f32], sample_rate: f32) -> Vec<f32> {
    let n = samples.len().max(1);
    let hann = |i: usize| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n as f32).cos();
    let windowed: Vec<f32> = samples
        .iter()
        .enumerate()
        .map(|(i, &s)| s * hann(i))
        .collect();
    (0..FREQ_BINS)
        .map(|k| {
            let w = std::f32::consts::TAU * bin_freq(k) / sample_rate;
            let (mut re, mut im) = (0.0_f32, 0.0_f32);
            for (i, &s) in windowed.iter().enumerate() {
                let a = w * i as f32;
                re += s * a.cos();
                im -= s * a.sin();
            }
            // Hann's coherent gain is 0.5; a sine of peak A sums to A·n/4.
            db(re.hypot(im) * 4.0 / n as f32)
        })
        .collect()
}

/// A private kernel and its last measurement, re-measured when the
/// parameters change, at most every [`Probe::MIN_INTERVAL`] seconds while
/// they keep changing.
pub struct Probe {
    kind: PictureKind,
    effect_id: String,
    kernel: Option<Box<dyn EffectWithParams + Send>>,
    rate: f32,
    key: Vec<f32>,
    last_run: f64,
    measured: Measured,
}

impl Probe {
    /// Seconds between measurements while a knob is moving.
    pub const MIN_INTERVAL: f64 = 0.1;

    /// A probe for `effect_id` showing `kind`. Nothing is built until the
    /// first [`update`](Self::update).
    pub fn new(effect_id: &str, kind: PictureKind) -> Self {
        Self {
            kind,
            effect_id: effect_id.to_owned(),
            kernel: None,
            rate: 0.0,
            key: Vec::new(),
            last_run: f64::NEG_INFINITY,
            measured: Measured::None,
        }
    }

    /// The last measurement.
    pub fn measured(&self) -> &Measured {
        &self.measured
    }

    /// The rate the probe kernel runs at (0.0 before the first measurement).
    pub fn rate(&self) -> f32 {
        self.rate
    }

    /// Re-measure if `values` (every parameter, in index order) or the host
    /// rate changed since the last measurement and the interval has passed.
    pub fn update(&mut self, values: &[f32], host_rate: f32, now: f64) {
        if !self.kind.is_probe() {
            return;
        }
        let rate = self.kind.probe_rate(host_rate);
        if self.kernel.is_none() || (rate - self.rate).abs() > 0.5 {
            self.kernel = sonido_registry::EffectRegistry::new().create(&self.effect_id, rate);
            self.rate = rate;
            self.key.clear();
        }
        let changed = self.key.len() != values.len()
            || self
                .key
                .iter()
                .zip(values)
                .any(|(a, b)| (a - b).abs() > 1e-6);
        if !changed || now - self.last_run < Self::MIN_INTERVAL {
            return;
        }
        let Some(fx) = self.kernel.as_deref_mut() else {
            return;
        };
        prepare(fx, values);
        self.measured = match self.kind {
            PictureKind::Transfer => Measured::Transfer(transfer_curve(fx, rate)),
            PictureKind::Level => Measured::Level(level_curve(fx, rate)),
            PictureKind::Response => Measured::Response(frequency_response(fx, rate)),
            PictureKind::Impulse => Measured::Impulse(impulse_envelope(fx, rate)),
            _ => Measured::None,
        };
        self.key = values.to_vec();
        self.last_run = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sonido_registry::EffectRegistry;

    const SR: f32 = 48_000.0;

    /// A kernel at its defaults, with `name = value` overrides.
    fn kernel(id: &str, set: &[(&str, f32)]) -> Box<dyn EffectWithParams + Send> {
        let registry = EffectRegistry::new();
        let mut fx = registry.create(id, SR).expect("registry effect");
        let mut values: Vec<f32> = (0..fx.effect_param_count())
            .map(|i| fx.effect_get_param(i))
            .collect();
        for (name, v) in set {
            let i = registry
                .param_index_by_name(id, name)
                .unwrap_or_else(|| panic!("{id} has no {name}"));
            values[i] = *v;
        }
        prepare(fx.as_mut(), &values);
        fx
    }

    #[test]
    fn every_registry_effect_has_a_picture() {
        let registry = EffectRegistry::new();
        for d in registry.all_effects() {
            let kind = PictureKind::for_effect(d.id, Family::of_effect(d.id));
            assert_eq!(kind == PictureKind::Tuner, d.id == "tuner", "{}", d.id);
        }
        assert_eq!(
            PictureKind::for_effect("graph-player", Family::Rig),
            PictureKind::Spectrum
        );
    }

    #[test]
    fn distortion_bends_and_stays_under_full_scale() {
        let mut fx = kernel("distortion", &[("Drive", 24.0)]);
        let curve = transfer_curve(fx.as_mut(), SR);
        let at = |x: f32| {
            curve
                .iter()
                .min_by(|a, b| (a[0] - x).abs().total_cmp(&(b[0] - x).abs()))
                .unwrap()[1]
        };
        // Rising, odd, and flatter at the top than through the middle.
        assert!(at(0.5) > at(0.0) && at(0.0) > at(-0.5));
        let slope_mid = (at(0.1) - at(-0.1)) / 0.2;
        let slope_top = (at(1.0) - at(0.8)) / 0.2;
        assert!(slope_top < slope_mid * 0.5, "{slope_top} vs {slope_mid}");
        assert!(curve.iter().all(|p| p[1].abs() <= 1.0 + 1e-3));
    }

    #[test]
    fn compressor_slope_above_threshold_is_about_one_over_ratio() {
        let mut fx = kernel(
            "compressor",
            &[
                ("Threshold", -30.0),
                ("Ratio", 4.0),
                ("Knee", 0.0),
                ("Makeup", 0.0),
                ("Auto Makeup", 0.0),
                ("Mix", 100.0),
            ],
        );
        let curve = level_curve(fx.as_mut(), SR);
        let at = |db_in: f32| curve[LEVELS_DB.iter().position(|&l| l == db_in).unwrap()];
        let slope = (at(-6.0) - at(-18.0)) / 12.0;
        assert!((slope - 0.25).abs() < 0.08, "slope {slope}");
        // Well under the threshold it passes level unchanged.
        assert!((at(-51.0) + 51.0).abs() < 1.5, "{}", at(-51.0));
    }

    #[test]
    fn lowpass_falls_away_above_its_cutoff() {
        let mut fx = kernel("filter", &[("Cutoff", 1000.0), ("Resonance", 0.707)]);
        let r = frequency_response(fx.as_mut(), SR);
        let at = |f: f32| {
            let k = (0..FREQ_BINS)
                .min_by(|&a, &b| (bin_freq(a) - f).abs().total_cmp(&(bin_freq(b) - f).abs()))
                .unwrap();
            r[k]
        };
        assert!(
            at(100.0) - at(4000.0) > 20.0,
            "{} {}",
            at(100.0),
            at(4000.0)
        );
    }

    #[test]
    fn delay_tap_lands_at_its_time() {
        let rate = PictureKind::Impulse.probe_rate(SR);
        let registry = EffectRegistry::new();
        let mut fx = registry.create("delay", rate).unwrap();
        let mut values: Vec<f32> = (0..fx.effect_param_count())
            .map(|i| fx.effect_get_param(i))
            .collect();
        let set = |values: &mut Vec<f32>, name: &str, v: f32| {
            values[registry.param_index_by_name("delay", name).unwrap()] = v;
        };
        set(&mut values, "Time", 500.0);
        set(&mut values, "Feedback", 0.0);
        set(&mut values, "Mix", 100.0);
        prepare(fx.as_mut(), &values);
        let env = impulse_envelope(fx.as_mut(), rate);
        let column = 0.5 / IMPULSE_SECONDS * IMPULSE_COLUMNS as f32;
        let loudest = env
            .iter()
            .enumerate()
            .skip(1)
            .max_by(|a, b| a.1.total_cmp(b.1))
            .unwrap()
            .0;
        assert!(
            (loudest as f32 - column).abs() <= 1.0,
            "tap at column {loudest}, expected {column}"
        );
    }

    #[test]
    fn a_sine_peaks_in_its_own_bin_at_full_scale() {
        let f = bin_freq(60);
        let samples: Vec<f32> = (0..1024)
            .map(|i| (std::f32::consts::TAU * f * i as f32 / SR).sin())
            .collect();
        let s = spectrum(&samples, SR);
        let peak = (0..FREQ_BINS)
            .max_by(|&a, &b| s[a].total_cmp(&s[b]))
            .unwrap();
        assert!((peak as i32 - 60).abs() <= 1, "peak bin {peak}");
        assert!(s[peak].abs() < 1.5, "{} dB", s[peak]);
    }

    #[test]
    fn a_probe_waits_out_the_interval_and_then_catches_up() {
        let mut probe = Probe::new("distortion", PictureKind::Transfer);
        let fx = EffectRegistry::new().create("distortion", SR).unwrap();
        let mut values: Vec<f32> = (0..fx.effect_param_count())
            .map(|i| fx.effect_get_param(i))
            .collect();
        probe.update(&values, SR, 0.0);
        let first = probe.measured().clone();
        assert!(matches!(first, Measured::Transfer(_)));
        values[0] += 12.0;
        probe.update(&values, SR, 0.05);
        assert_eq!(
            probe.measured(),
            &first,
            "measured again inside the interval"
        );
        probe.update(&values, SR, 0.2);
        assert_ne!(probe.measured(), &first);
    }
}
