//! Effect factory and parameter parsing.

use sonido_core::EffectWithParams;
use sonido_effects::{
    Chorus, CleanPreamp, Compressor, Delay, Distortion, Flanger, Gate, LowPassFilter, MultiVibrato,
    ParametricEq, Phaser, Reverb, ReverbType, TapeSaturation, Tremolo, TremoloWaveform, Wah,
    WahMode, WaveShape,
};
use std::collections::HashMap;

/// Error type for effect creation.
#[derive(Debug, thiserror::Error)]
pub enum EffectError {
    #[error("Unknown effect: {0}")]
    UnknownEffect(String),

    #[error("Unknown parameter '{param}' for effect '{effect}'")]
    UnknownParameter { effect: String, param: String },

    #[error("Invalid parameter value for '{param}': {message}")]
    InvalidValue { param: String, message: String },

    #[error("Parse error: {0}")]
    ParseError(String),
}

/// Information about an available effect.
#[derive(Debug, Clone)]
pub struct EffectInfo {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: &'static [ParameterInfo],
}

/// Information about an effect parameter.
#[derive(Debug, Clone)]
pub struct ParameterInfo {
    pub name: &'static str,
    pub description: &'static str,
    pub default: &'static str,
    pub range: &'static str,
}

/// Get information about all available effects.
pub fn available_effects() -> Vec<EffectInfo> {
    vec![
        EffectInfo {
            name: "distortion",
            description: "Waveshaping distortion with multiple modes",
            parameters: &[
                ParameterInfo {
                    name: "drive",
                    description: "Drive amount in dB",
                    default: "12.0",
                    range: "0-40",
                },
                ParameterInfo {
                    name: "tone",
                    description: "Tone frequency in Hz",
                    default: "4000.0",
                    range: "500-10000",
                },
                ParameterInfo {
                    name: "level",
                    description: "Output level in dB",
                    default: "-6.0",
                    range: "-20-0",
                },
                ParameterInfo {
                    name: "waveshape",
                    description: "Waveshape type",
                    default: "softclip",
                    range: "softclip|hardclip|foldback|asymmetric",
                },
                ParameterInfo {
                    name: "foldback_threshold",
                    description: "Foldback threshold (foldback mode only)",
                    default: "0.8",
                    range: "0.1-1.0",
                },
            ],
        },
        EffectInfo {
            name: "compressor",
            description: "Dynamics compressor with soft knee",
            parameters: &[
                ParameterInfo {
                    name: "threshold",
                    description: "Threshold in dB",
                    default: "-18.0",
                    range: "-40-0",
                },
                ParameterInfo {
                    name: "ratio",
                    description: "Compression ratio",
                    default: "4.0",
                    range: "1-20",
                },
                ParameterInfo {
                    name: "attack",
                    description: "Attack time in ms",
                    default: "10.0",
                    range: "0.1-100",
                },
                ParameterInfo {
                    name: "release",
                    description: "Release time in ms",
                    default: "100.0",
                    range: "10-1000",
                },
                ParameterInfo {
                    name: "makeup",
                    description: "Makeup gain in dB",
                    default: "0.0",
                    range: "0-20",
                },
                ParameterInfo {
                    name: "knee",
                    description: "Knee width in dB",
                    default: "6.0",
                    range: "0-12",
                },
            ],
        },
        EffectInfo {
            name: "chorus",
            description: "Dual-voice modulated delay chorus",
            parameters: &[
                ParameterInfo {
                    name: "rate",
                    description: "LFO rate in Hz",
                    default: "1.0",
                    range: "0.1-10",
                },
                ParameterInfo {
                    name: "depth",
                    description: "Modulation depth (0-1)",
                    default: "0.5",
                    range: "0-1",
                },
                ParameterInfo {
                    name: "mix",
                    description: "Wet/dry mix (0-1)",
                    default: "0.5",
                    range: "0-1",
                },
                ParameterInfo {
                    name: "output",
                    description: "Output level in dB",
                    default: "0.0",
                    range: "-20-20",
                },
            ],
        },
        EffectInfo {
            name: "delay",
            description: "Tape-style feedback delay",
            parameters: &[
                ParameterInfo {
                    name: "time",
                    description: "Delay time in ms",
                    default: "300.0",
                    range: "1-2000",
                },
                ParameterInfo {
                    name: "feedback",
                    description: "Feedback amount (0-1)",
                    default: "0.4",
                    range: "0-0.95",
                },
                ParameterInfo {
                    name: "mix",
                    description: "Wet/dry mix (0-1)",
                    default: "0.5",
                    range: "0-1",
                },
                ParameterInfo {
                    name: "ping_pong",
                    description: "Ping-pong stereo mode (0=off, 1=on)",
                    default: "0",
                    range: "0-1",
                },
                ParameterInfo {
                    name: "output",
                    description: "Output level in dB",
                    default: "0.0",
                    range: "-20-20",
                },
            ],
        },
        EffectInfo {
            name: "flanger",
            description: "Classic flanger with through-zero mode and bipolar feedback",
            parameters: &[
                ParameterInfo {
                    name: "rate",
                    description: "LFO rate in Hz",
                    default: "0.5",
                    range: "0.05-5",
                },
                ParameterInfo {
                    name: "depth",
                    description: "Modulation depth (0-1)",
                    default: "0.5",
                    range: "0-1",
                },
                ParameterInfo {
                    name: "feedback",
                    description: "Feedback amount, negative = even harmonics",
                    default: "0.5",
                    range: "-0.95-0.95",
                },
                ParameterInfo {
                    name: "mix",
                    description: "Wet/dry mix (0-1)",
                    default: "0.5",
                    range: "0-1",
                },
                ParameterInfo {
                    name: "tzf",
                    description: "Through-zero flanging mode",
                    default: "false",
                    range: "true/false",
                },
                ParameterInfo {
                    name: "output",
                    description: "Output level in dB",
                    default: "0.0",
                    range: "-20-20",
                },
            ],
        },
        EffectInfo {
            name: "phaser",
            description: "Multi-stage allpass phaser with LFO",
            parameters: &[
                ParameterInfo {
                    name: "rate",
                    description: "LFO rate in Hz",
                    default: "0.3",
                    range: "0.05-5",
                },
                ParameterInfo {
                    name: "depth",
                    description: "Frequency sweep range (0-1)",
                    default: "0.5",
                    range: "0-1",
                },
                ParameterInfo {
                    name: "stages",
                    description: "Number of allpass stages",
                    default: "6",
                    range: "2-12",
                },
                ParameterInfo {
                    name: "feedback",
                    description: "Feedback/resonance (0-1)",
                    default: "0.5",
                    range: "0-0.95",
                },
                ParameterInfo {
                    name: "mix",
                    description: "Wet/dry mix (0-1)",
                    default: "0.5",
                    range: "0-1",
                },
                ParameterInfo {
                    name: "stereo_spread",
                    description: "Stereo LFO phase offset (0-0.5)",
                    default: "0.25",
                    range: "0-0.5",
                },
                ParameterInfo {
                    name: "output",
                    description: "Output level in dB",
                    default: "0.0",
                    range: "-20-20",
                },
            ],
        },
        EffectInfo {
            name: "filter",
            description: "Resonant lowpass filter",
            parameters: &[
                ParameterInfo {
                    name: "cutoff",
                    description: "Cutoff frequency in Hz",
                    default: "1000.0",
                    range: "20-20000",
                },
                ParameterInfo {
                    name: "resonance",
                    description: "Resonance (Q)",
                    default: "0.707",
                    range: "0.1-10",
                },
                ParameterInfo {
                    name: "output",
                    description: "Output level in dB",
                    default: "0.0",
                    range: "-20-20",
                },
            ],
        },
        EffectInfo {
            name: "multivibrato",
            description: "10-unit tape wow/flutter vibrato",
            parameters: &[
                ParameterInfo {
                    name: "depth",
                    description: "Overall depth (0-1)",
                    default: "0.5",
                    range: "0-1",
                },
                ParameterInfo {
                    name: "mix",
                    description: "Wet/dry mix (0-1)",
                    default: "1.0",
                    range: "0-1",
                },
                ParameterInfo {
                    name: "output",
                    description: "Output level in dB",
                    default: "0.0",
                    range: "-20-20",
                },
            ],
        },
        EffectInfo {
            name: "tape",
            description: "Tape saturation with HF rolloff",
            parameters: &[
                ParameterInfo {
                    name: "drive",
                    description: "Drive amount in dB",
                    default: "6.0",
                    range: "0-24",
                },
                ParameterInfo {
                    name: "saturation",
                    description: "Saturation amount (0-1)",
                    default: "0.5",
                    range: "0-1",
                },
                ParameterInfo {
                    name: "output",
                    description: "Output level in dB",
                    default: "0.0",
                    range: "-12-12",
                },
                ParameterInfo {
                    name: "hf_rolloff",
                    description: "HF rolloff frequency in Hz",
                    default: "12000.0",
                    range: "1000-20000",
                },
                ParameterInfo {
                    name: "bias",
                    description: "Tape bias (-0.2 to 0.2)",
                    default: "0.0",
                    range: "-0.2-0.2",
                },
            ],
        },
        EffectInfo {
            name: "preamp",
            description: "Clean preamp/gain stage",
            parameters: &[
                ParameterInfo {
                    name: "gain",
                    description: "Gain in dB",
                    default: "0.0",
                    range: "-20-20",
                },
                ParameterInfo {
                    name: "output",
                    description: "Output level in dB",
                    default: "0.0",
                    range: "-20-20",
                },
                ParameterInfo {
                    name: "headroom",
                    description: "Headroom before clipping in dB",
                    default: "20.0",
                    range: "6-40",
                },
            ],
        },
        EffectInfo {
            name: "reverb",
            description: "Freeverb-style algorithmic reverb",
            parameters: &[
                ParameterInfo {
                    name: "room_size",
                    description: "Room size (0-1)",
                    default: "0.5",
                    range: "0-1",
                },
                ParameterInfo {
                    name: "decay",
                    description: "Decay time (0-1)",
                    default: "0.5",
                    range: "0-1",
                },
                ParameterInfo {
                    name: "damping",
                    description: "HF damping (0-1, 0=bright, 1=dark)",
                    default: "0.5",
                    range: "0-1",
                },
                ParameterInfo {
                    name: "predelay",
                    description: "Pre-delay in ms",
                    default: "10.0",
                    range: "0-100",
                },
                ParameterInfo {
                    name: "mix",
                    description: "Wet/dry mix (0-1)",
                    default: "0.5",
                    range: "0-1",
                },
                ParameterInfo {
                    name: "stereo_width",
                    description: "Stereo width (0-1)",
                    default: "1.0",
                    range: "0-1",
                },
                ParameterInfo {
                    name: "type",
                    description: "Reverb type preset",
                    default: "room",
                    range: "room|hall",
                },
                ParameterInfo {
                    name: "output",
                    description: "Output level in dB",
                    default: "0.0",
                    range: "-20-20",
                },
            ],
        },
        EffectInfo {
            name: "tremolo",
            description: "Amplitude modulation with multiple waveforms",
            parameters: &[
                ParameterInfo {
                    name: "rate",
                    description: "LFO rate in Hz",
                    default: "5.0",
                    range: "0.5-20",
                },
                ParameterInfo {
                    name: "depth",
                    description: "Modulation depth (0-1)",
                    default: "0.5",
                    range: "0-1",
                },
                ParameterInfo {
                    name: "waveform",
                    description: "Waveform type",
                    default: "sine",
                    range: "sine|triangle|square|samplehold",
                },
                ParameterInfo {
                    name: "output",
                    description: "Output level in dB",
                    default: "0.0",
                    range: "-20-20",
                },
            ],
        },
        EffectInfo {
            name: "gate",
            description: "Noise gate with threshold and hold",
            parameters: &[
                ParameterInfo {
                    name: "threshold",
                    description: "Threshold in dB",
                    default: "-40.0",
                    range: "-80-0",
                },
                ParameterInfo {
                    name: "attack",
                    description: "Attack time in ms",
                    default: "1.0",
                    range: "0.1-50",
                },
                ParameterInfo {
                    name: "release",
                    description: "Release time in ms",
                    default: "100.0",
                    range: "10-1000",
                },
                ParameterInfo {
                    name: "hold",
                    description: "Hold time in ms",
                    default: "50.0",
                    range: "0-500",
                },
                ParameterInfo {
                    name: "output",
                    description: "Output level in dB",
                    default: "0.0",
                    range: "-20-20",
                },
            ],
        },
        EffectInfo {
            name: "wah",
            description: "Auto-wah and manual wah with envelope follower",
            parameters: &[
                ParameterInfo {
                    name: "frequency",
                    description: "Center frequency in Hz",
                    default: "800.0",
                    range: "200-2000",
                },
                ParameterInfo {
                    name: "resonance",
                    description: "Filter Q (sharpness)",
                    default: "5.0",
                    range: "1-10",
                },
                ParameterInfo {
                    name: "sensitivity",
                    description: "Envelope sensitivity (0-1)",
                    default: "0.5",
                    range: "0-1",
                },
                ParameterInfo {
                    name: "mode",
                    description: "Wah mode",
                    default: "auto",
                    range: "auto|manual",
                },
                ParameterInfo {
                    name: "output",
                    description: "Output level in dB",
                    default: "0.0",
                    range: "-20-20",
                },
            ],
        },
        EffectInfo {
            name: "eq",
            description: "3-band parametric equalizer",
            parameters: &[
                ParameterInfo {
                    name: "low_freq",
                    description: "Low band frequency in Hz",
                    default: "100.0",
                    range: "20-500",
                },
                ParameterInfo {
                    name: "low_gain",
                    description: "Low band gain in dB",
                    default: "0.0",
                    range: "-12-12",
                },
                ParameterInfo {
                    name: "low_q",
                    description: "Low band Q",
                    default: "1.0",
                    range: "0.5-5",
                },
                ParameterInfo {
                    name: "mid_freq",
                    description: "Mid band frequency in Hz",
                    default: "1000.0",
                    range: "200-5000",
                },
                ParameterInfo {
                    name: "mid_gain",
                    description: "Mid band gain in dB",
                    default: "0.0",
                    range: "-12-12",
                },
                ParameterInfo {
                    name: "mid_q",
                    description: "Mid band Q",
                    default: "1.0",
                    range: "0.5-5",
                },
                ParameterInfo {
                    name: "high_freq",
                    description: "High band frequency in Hz",
                    default: "5000.0",
                    range: "1000-15000",
                },
                ParameterInfo {
                    name: "high_gain",
                    description: "High band gain in dB",
                    default: "0.0",
                    range: "-12-12",
                },
                ParameterInfo {
                    name: "high_q",
                    description: "High band Q",
                    default: "1.0",
                    range: "0.5-5",
                },
                ParameterInfo {
                    name: "output",
                    description: "Output level in dB",
                    default: "0.0",
                    range: "-20-20",
                },
            ],
        },
    ]
}

/// Create an effect with custom parameters.
pub fn create_effect_with_params(
    name: &str,
    sample_rate: f32,
    params: &HashMap<String, String>,
) -> Result<Box<dyn EffectWithParams + Send>, EffectError> {
    match name.to_lowercase().as_str() {
        "distortion" => {
            let mut effect = Distortion::new(sample_rate);
            for (key, value) in params {
                match key.as_str() {
                    "drive" => effect.set_drive_db(parse_f32(key, value)?),
                    "tone" => effect.set_tone_db(parse_f32(key, value)?),
                    "mix" => effect.set_mix(parse_f32(key, value)? / 100.0),
                    "waveshape" | "shape" => effect.set_waveshape(parse_waveshape(value)?),
                    "foldback_threshold" | "foldback" => {
                        effect.set_foldback_threshold(parse_f32(key, value)?);
                    }
                    _ => {
                        return Err(EffectError::UnknownParameter {
                            effect: name.to_string(),
                            param: key.to_string(),
                        });
                    }
                }
            }
            Ok(Box::new(effect))
        }
        "compressor" => {
            let mut effect = Compressor::new(sample_rate);
            for (key, value) in params {
                match key.as_str() {
                    "threshold" => effect.set_threshold_db(parse_f32(key, value)?),
                    "ratio" => effect.set_ratio(parse_f32(key, value)?),
                    "attack" => effect.set_attack_ms(parse_f32(key, value)?),
                    "release" => effect.set_release_ms(parse_f32(key, value)?),
                    "makeup" => effect.set_makeup_gain_db(parse_f32(key, value)?),
                    "knee" => effect.set_knee_db(parse_f32(key, value)?),
                    _ => {
                        return Err(EffectError::UnknownParameter {
                            effect: name.to_string(),
                            param: key.to_string(),
                        });
                    }
                }
            }
            Ok(Box::new(effect))
        }
        "chorus" => {
            let mut effect = Chorus::new(sample_rate);
            for (key, value) in params {
                match key.as_str() {
                    "rate" => effect.set_rate(parse_f32(key, value)?),
                    "depth" => effect.set_depth(parse_f32(key, value)?),
                    "mix" => effect.set_mix(parse_f32(key, value)?),
                    "output" => {
                        use sonido_core::ParameterInfo as _;
                        let db = parse_f32(key, value)?;
                        let last = effect.param_count() - 1;
                        effect.set_param(last, db);
                    }
                    _ => {
                        return Err(EffectError::UnknownParameter {
                            effect: name.to_string(),
                            param: key.to_string(),
                        });
                    }
                }
            }
            Ok(Box::new(effect))
        }
        "delay" => {
            let mut effect = Delay::new(sample_rate);
            for (key, value) in params {
                match key.as_str() {
                    "time" => effect.set_delay_time_ms(parse_f32(key, value)?),
                    "feedback" => effect.set_feedback(parse_f32(key, value)?),
                    "mix" => effect.set_mix(parse_f32(key, value)?),
                    "ping_pong" | "pingpong" => {
                        effect.set_ping_pong(parse_f32(key, value)? > 0.5);
                    }
                    "output" => {
                        use sonido_core::ParameterInfo as _;
                        let db = parse_f32(key, value)?;
                        let last = effect.param_count() - 1;
                        effect.set_param(last, db);
                    }
                    _ => {
                        return Err(EffectError::UnknownParameter {
                            effect: name.to_string(),
                            param: key.to_string(),
                        });
                    }
                }
            }
            Ok(Box::new(effect))
        }
        "flanger" => {
            let mut effect = Flanger::new(sample_rate);
            for (key, value) in params {
                match key.as_str() {
                    "rate" => effect.set_rate(parse_f32(key, value)?),
                    "depth" => effect.set_depth(parse_f32(key, value)?),
                    "feedback" | "fdbk" => effect.set_feedback(parse_f32(key, value)?),
                    "mix" => effect.set_mix(parse_f32(key, value)?),
                    "tzf" => effect.set_tzf(parse_f32(key, value)? > 0.5),
                    "output" => {
                        use sonido_core::ParameterInfo as _;
                        let db = parse_f32(key, value)?;
                        let last = effect.param_count() - 1;
                        effect.set_param(last, db);
                    }
                    _ => {
                        return Err(EffectError::UnknownParameter {
                            effect: name.to_string(),
                            param: key.to_string(),
                        });
                    }
                }
            }
            Ok(Box::new(effect))
        }
        "phaser" => {
            let mut effect = Phaser::new(sample_rate);
            for (key, value) in params {
                match key.as_str() {
                    "rate" => effect.set_rate(parse_f32(key, value)?),
                    "depth" => effect.set_depth(parse_f32(key, value)?),
                    "stages" | "stg" => effect.set_stages(parse_f32(key, value)? as usize),
                    "feedback" | "fdbk" => effect.set_feedback(parse_f32(key, value)?),
                    "mix" => effect.set_mix(parse_f32(key, value)?),
                    "stereo_spread" | "spread" => {
                        effect.set_stereo_spread(parse_f32(key, value)?);
                    }
                    "min_freq" => effect.set_min_freq(parse_f32(key, value)?),
                    "max_freq" => effect.set_max_freq(parse_f32(key, value)?),
                    "output" => {
                        use sonido_core::ParameterInfo as _;
                        let db = parse_f32(key, value)?;
                        let last = effect.param_count() - 1;
                        effect.set_param(last, db);
                    }
                    _ => {
                        return Err(EffectError::UnknownParameter {
                            effect: name.to_string(),
                            param: key.to_string(),
                        });
                    }
                }
            }
            Ok(Box::new(effect))
        }
        "filter" | "lowpass" => {
            let mut effect = LowPassFilter::new(sample_rate);
            for (key, value) in params {
                match key.as_str() {
                    "cutoff" => effect.set_cutoff_hz(parse_f32(key, value)?),
                    "resonance" | "q" => effect.set_q(parse_f32(key, value)?),
                    "output" => {
                        use sonido_core::ParameterInfo as _;
                        let db = parse_f32(key, value)?;
                        let last = effect.param_count() - 1;
                        effect.set_param(last, db);
                    }
                    _ => {
                        return Err(EffectError::UnknownParameter {
                            effect: name.to_string(),
                            param: key.to_string(),
                        });
                    }
                }
            }
            Ok(Box::new(effect))
        }
        "multivibrato" | "vibrato" => {
            let mut effect = MultiVibrato::new(sample_rate);
            for (key, value) in params {
                match key.as_str() {
                    "depth" => effect.set_depth(parse_f32(key, value)?),
                    "mix" => effect.set_mix(parse_f32(key, value)?),
                    "output" => {
                        use sonido_core::ParameterInfo as _;
                        let db = parse_f32(key, value)?;
                        let last = effect.param_count() - 1;
                        effect.set_param(last, db);
                    }
                    _ => {
                        return Err(EffectError::UnknownParameter {
                            effect: name.to_string(),
                            param: key.to_string(),
                        });
                    }
                }
            }
            Ok(Box::new(effect))
        }
        "tape" | "tapesaturation" => {
            let mut effect = TapeSaturation::new(sample_rate);
            for (key, value) in params {
                match key.as_str() {
                    "drive" => effect.set_drive(parse_f32(key, value)?),
                    "saturation" => effect.set_saturation(parse_f32(key, value)?),
                    "output" => {
                        let db = parse_f32(key, value)?;
                        effect.set_output(10.0f32.powf(db / 20.0));
                    }
                    "hf_rolloff" | "hf" => effect.set_hf_rolloff(parse_f32(key, value)?),
                    "bias" => effect.set_bias(parse_f32(key, value)?),
                    _ => {
                        return Err(EffectError::UnknownParameter {
                            effect: name.to_string(),
                            param: key.to_string(),
                        });
                    }
                }
            }
            Ok(Box::new(effect))
        }
        "preamp" | "cleanpreamp" => {
            let mut effect = CleanPreamp::new(sample_rate);
            for (key, value) in params {
                match key.as_str() {
                    "gain" => effect.set_gain_db(parse_f32(key, value)?),
                    "output" => effect.set_output_db(parse_f32(key, value)?),
                    "headroom" | "headroom_db" => {
                        effect.set_headroom_db(parse_f32(key, value)?);
                    }
                    _ => {
                        return Err(EffectError::UnknownParameter {
                            effect: name.to_string(),
                            param: key.to_string(),
                        });
                    }
                }
            }
            Ok(Box::new(effect))
        }
        "reverb" => {
            let mut effect = Reverb::new(sample_rate);
            for (key, value) in params {
                match key.as_str() {
                    "room_size" | "room" | "size" => effect.set_room_size(parse_f32(key, value)?),
                    "decay" => effect.set_decay(parse_f32(key, value)?),
                    "damping" | "damp" => effect.set_damping(parse_f32(key, value)?),
                    "predelay" | "pre" => effect.set_predelay_ms(parse_f32(key, value)?),
                    "mix" => effect.set_mix(parse_f32(key, value)?),
                    "stereo_width" | "width" => {
                        effect.set_stereo_width(parse_f32(key, value)?);
                    }
                    "type" | "preset" => effect.set_reverb_type(parse_reverb_type(value)?),
                    "output" => {
                        use sonido_core::ParameterInfo as _;
                        let db = parse_f32(key, value)?;
                        let last = effect.param_count() - 1;
                        effect.set_param(last, db);
                    }
                    _ => {
                        return Err(EffectError::UnknownParameter {
                            effect: name.to_string(),
                            param: key.to_string(),
                        });
                    }
                }
            }
            Ok(Box::new(effect))
        }
        "tremolo" => {
            let mut effect = Tremolo::new(sample_rate);
            for (key, value) in params {
                match key.as_str() {
                    "rate" => effect.set_rate(parse_f32(key, value)?),
                    "depth" => effect.set_depth(parse_f32(key, value)?),
                    "waveform" | "wave" => effect.set_waveform(parse_tremolo_waveform(value)?),
                    "stereo_spread" | "spread" => {
                        effect.set_stereo_spread(parse_f32(key, value)?);
                    }
                    "output" => {
                        use sonido_core::ParameterInfo as _;
                        let db = parse_f32(key, value)?;
                        let last = effect.param_count() - 1;
                        effect.set_param(last, db);
                    }
                    _ => {
                        return Err(EffectError::UnknownParameter {
                            effect: name.to_string(),
                            param: key.to_string(),
                        });
                    }
                }
            }
            Ok(Box::new(effect))
        }
        "gate" | "noisegate" => {
            let mut effect = Gate::new(sample_rate);
            for (key, value) in params {
                match key.as_str() {
                    "threshold" | "thresh" => effect.set_threshold_db(parse_f32(key, value)?),
                    "attack" | "atk" => effect.set_attack_ms(parse_f32(key, value)?),
                    "release" | "rel" => effect.set_release_ms(parse_f32(key, value)?),
                    "hold" => effect.set_hold_ms(parse_f32(key, value)?),
                    "output" => {
                        use sonido_core::ParameterInfo as _;
                        let db = parse_f32(key, value)?;
                        let last = effect.param_count() - 1;
                        effect.set_param(last, db);
                    }
                    _ => {
                        return Err(EffectError::UnknownParameter {
                            effect: name.to_string(),
                            param: key.to_string(),
                        });
                    }
                }
            }
            Ok(Box::new(effect))
        }
        "wah" | "autowah" => {
            let mut effect = Wah::new(sample_rate);
            for (key, value) in params {
                match key.as_str() {
                    "frequency" | "freq" => effect.set_frequency(parse_f32(key, value)?),
                    "resonance" | "reso" | "q" => effect.set_resonance(parse_f32(key, value)?),
                    "sensitivity" | "sens" => effect.set_sensitivity(parse_f32(key, value)?),
                    "mode" => effect.set_mode(parse_wah_mode(value)?),
                    "output" => {
                        use sonido_core::ParameterInfo as _;
                        let db = parse_f32(key, value)?;
                        let last = effect.param_count() - 1;
                        effect.set_param(last, db);
                    }
                    _ => {
                        return Err(EffectError::UnknownParameter {
                            effect: name.to_string(),
                            param: key.to_string(),
                        });
                    }
                }
            }
            Ok(Box::new(effect))
        }
        "eq" | "parametriceq" | "peq" => {
            let mut effect = ParametricEq::new(sample_rate);
            for (key, value) in params {
                match key.as_str() {
                    "low_freq" | "lf" => effect.set_low_freq(parse_f32(key, value)?),
                    "low_gain" | "lg" => effect.set_low_gain(parse_f32(key, value)?),
                    "low_q" | "lq" => effect.set_low_q(parse_f32(key, value)?),
                    "mid_freq" | "mf" => effect.set_mid_freq(parse_f32(key, value)?),
                    "mid_gain" | "mg" => effect.set_mid_gain(parse_f32(key, value)?),
                    "mid_q" | "mq" => effect.set_mid_q(parse_f32(key, value)?),
                    "high_freq" | "hf" => effect.set_high_freq(parse_f32(key, value)?),
                    "high_gain" | "hg" => effect.set_high_gain(parse_f32(key, value)?),
                    "high_q" | "hq" => effect.set_high_q(parse_f32(key, value)?),
                    "output" => {
                        use sonido_core::ParameterInfo as _;
                        let db = parse_f32(key, value)?;
                        let last = effect.param_count() - 1;
                        effect.set_param(last, db);
                    }
                    _ => {
                        return Err(EffectError::UnknownParameter {
                            effect: name.to_string(),
                            param: key.to_string(),
                        });
                    }
                }
            }
            Ok(Box::new(effect))
        }
        _ => Err(EffectError::UnknownEffect(name.to_string())),
    }
}

/// Parse an effect chain specification.
///
/// Format: "effect1:param1=value1,param2=value2|effect2:param=value"
///
/// Examples:
/// - "distortion:drive=15"
/// - "preamp:gain=6|distortion:drive=12|delay:time=300,feedback=0.4"
pub fn parse_chain(
    spec: &str,
    sample_rate: f32,
) -> Result<Vec<Box<dyn EffectWithParams + Send>>, EffectError> {
    let mut effects = Vec::new();

    for effect_spec in spec.split('|') {
        let effect_spec = effect_spec.trim();
        if effect_spec.is_empty() {
            continue;
        }

        let (name, params) = parse_effect_spec(effect_spec)?;
        let effect = create_effect_with_params(&name, sample_rate, &params)?;
        effects.push(effect);
    }

    Ok(effects)
}

/// Parse a single effect specification.
///
/// Format: "effect_name:param1=value1,param2=value2"
fn parse_effect_spec(spec: &str) -> Result<(String, HashMap<String, String>), EffectError> {
    let parts: Vec<&str> = spec.splitn(2, ':').collect();
    let name = parts[0].trim().to_string();

    let params = if parts.len() > 1 {
        parse_params(parts[1])?
    } else {
        HashMap::new()
    };

    Ok((name, params))
}

/// Parse parameter string into a map.
fn parse_params(params_str: &str) -> Result<HashMap<String, String>, EffectError> {
    let mut params = HashMap::new();

    for param in params_str.split(',') {
        let param = param.trim();
        if param.is_empty() {
            continue;
        }

        let kv: Vec<&str> = param.splitn(2, '=').collect();
        if kv.len() != 2 {
            return Err(EffectError::ParseError(format!(
                "Invalid parameter format: '{}' (expected key=value)",
                param
            )));
        }

        params.insert(kv[0].trim().to_string(), kv[1].trim().to_string());
    }

    Ok(params)
}

fn parse_f32(param: &str, value: &str) -> Result<f32, EffectError> {
    value.parse().map_err(|_| EffectError::InvalidValue {
        param: param.to_string(),
        message: format!("'{}' is not a valid number", value),
    })
}

fn parse_waveshape(value: &str) -> Result<WaveShape, EffectError> {
    match value.to_lowercase().as_str() {
        "softclip" | "soft" => Ok(WaveShape::SoftClip),
        "hardclip" | "hard" => Ok(WaveShape::HardClip),
        "foldback" | "fold" => Ok(WaveShape::Foldback),
        "asymmetric" | "asym" => Ok(WaveShape::Asymmetric),
        _ => Err(EffectError::InvalidValue {
            param: "waveshape".to_string(),
            message: format!(
                "'{}' is not a valid waveshape (use: softclip, hardclip, foldback, asymmetric)",
                value
            ),
        }),
    }
}

fn parse_reverb_type(value: &str) -> Result<ReverbType, EffectError> {
    match value.to_lowercase().as_str() {
        "room" => Ok(ReverbType::Room),
        "hall" => Ok(ReverbType::Hall),
        _ => Err(EffectError::InvalidValue {
            param: "type".to_string(),
            message: format!("'{}' is not a valid reverb type (use: room, hall)", value),
        }),
    }
}

fn parse_tremolo_waveform(value: &str) -> Result<TremoloWaveform, EffectError> {
    match value.to_lowercase().as_str() {
        "sine" | "sin" => Ok(TremoloWaveform::Sine),
        "triangle" | "tri" => Ok(TremoloWaveform::Triangle),
        "square" | "sq" => Ok(TremoloWaveform::Square),
        "samplehold" | "sh" | "sample_hold" | "s&h" => Ok(TremoloWaveform::SampleHold),
        _ => Err(EffectError::InvalidValue {
            param: "waveform".to_string(),
            message: format!(
                "'{}' is not a valid waveform (use: sine, triangle, square, samplehold)",
                value
            ),
        }),
    }
}

fn parse_wah_mode(value: &str) -> Result<WahMode, EffectError> {
    match value.to_lowercase().as_str() {
        "auto" | "0" => Ok(WahMode::Auto),
        "manual" | "1" => Ok(WahMode::Manual),
        _ => Err(EffectError::InvalidValue {
            param: "mode".to_string(),
            message: format!("'{}' is not a valid wah mode (use: auto, manual)", value),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_effect_with_params() {
        let params = HashMap::new();
        let effect = create_effect_with_params("distortion", 48000.0, &params);
        assert!(effect.is_ok());

        let effect = create_effect_with_params("unknown", 48000.0, &params);
        assert!(effect.is_err());
    }

    #[test]
    fn test_parse_params() {
        let params = parse_params("drive=15,tone=4000").unwrap();
        assert_eq!(params.get("drive"), Some(&"15".to_string()));
        assert_eq!(params.get("tone"), Some(&"4000".to_string()));
    }

    #[test]
    fn test_parse_effect_spec() {
        let (name, params) = parse_effect_spec("distortion:drive=15,level=-6").unwrap();
        assert_eq!(name, "distortion");
        assert_eq!(params.get("drive"), Some(&"15".to_string()));
        assert_eq!(params.get("level"), Some(&"-6".to_string()));
    }

    #[test]
    fn test_parse_chain() {
        let chain = parse_chain("preamp:gain=6|distortion:drive=12|delay:time=300", 48000.0);
        assert!(chain.is_ok());
        assert_eq!(chain.unwrap().len(), 3);
    }

    #[test]
    fn test_parse_chain_simple() {
        let chain = parse_chain("distortion", 48000.0);
        assert!(chain.is_ok());
        assert_eq!(chain.unwrap().len(), 1);
    }

    #[test]
    fn test_create_reverb() {
        let params = HashMap::new();
        let effect = create_effect_with_params("reverb", 48000.0, &params);
        assert!(effect.is_ok());

        // Test with parameters
        let mut params = HashMap::new();
        params.insert("decay".to_string(), "0.8".to_string());
        params.insert("room_size".to_string(), "0.7".to_string());
        params.insert("damping".to_string(), "0.3".to_string());
        params.insert("mix".to_string(), "0.5".to_string());
        params.insert("type".to_string(), "hall".to_string());
        let effect = create_effect_with_params("reverb", 48000.0, &params);
        assert!(effect.is_ok());
    }

    #[test]
    fn test_parse_chain_with_reverb() {
        let chain = parse_chain("delay:time=300|reverb:decay=0.9,mix=0.6", 48000.0);
        assert!(chain.is_ok());
        assert_eq!(chain.unwrap().len(), 2);
    }

    #[test]
    fn parse_params_empty_string() {
        let params = parse_params("").unwrap();
        assert!(params.is_empty());
    }

    #[test]
    fn parse_params_trailing_comma() {
        let params = parse_params("drive=15,").unwrap();
        assert_eq!(params.len(), 1);
        assert_eq!(params.get("drive"), Some(&"15".to_string()));
    }

    #[test]
    fn parse_params_spaces_around_equals() {
        let params = parse_params("drive = 15, tone = 4000").unwrap();
        assert_eq!(params.get("drive"), Some(&"15".to_string()));
        assert_eq!(params.get("tone"), Some(&"4000".to_string()));
    }

    #[test]
    fn parse_effect_spec_no_params() {
        let (name, params) = parse_effect_spec("chorus").unwrap();
        assert_eq!(name, "chorus");
        assert!(params.is_empty());
    }

    #[test]
    fn parse_effect_spec_whitespace() {
        let (name, params) = parse_effect_spec("  delay  ").unwrap();
        assert_eq!(name, "delay");
        assert!(params.is_empty());
    }

    #[test]
    fn parse_effect_spec_missing_colon() {
        // Missing colon is equivalent to no params — not an error
        let (name, params) = parse_effect_spec("reverb").unwrap();
        assert_eq!(name, "reverb");
        assert!(params.is_empty());
    }

    #[test]
    fn create_effect_unknown_name() {
        let params = HashMap::new();
        let result = create_effect_with_params("nonexistent", 48000.0, &params);
        assert!(matches!(result, Err(EffectError::UnknownEffect(_))));
    }

    #[test]
    fn create_effect_unknown_param() {
        let mut params = HashMap::new();
        params.insert("bogus".to_string(), "1.0".to_string());
        let result = create_effect_with_params("distortion", 48000.0, &params);
        assert!(matches!(result, Err(EffectError::UnknownParameter { .. })));
    }

    #[test]
    fn create_effect_invalid_numeric_value() {
        let mut params = HashMap::new();
        params.insert("drive".to_string(), "notanumber".to_string());
        let result = create_effect_with_params("distortion", 48000.0, &params);
        assert!(matches!(result, Err(EffectError::InvalidValue { .. })));
    }

    #[test]
    fn create_effect_waveshape_enum() {
        for shape in &["softclip", "hardclip", "foldback", "asymmetric"] {
            let mut params = HashMap::new();
            params.insert("waveshape".to_string(), shape.to_string());
            assert!(create_effect_with_params("distortion", 48000.0, &params).is_ok());
        }
        let mut params = HashMap::new();
        params.insert("waveshape".to_string(), "invalid".to_string());
        assert!(create_effect_with_params("distortion", 48000.0, &params).is_err());
    }

    #[test]
    fn create_effect_reverb_type_enum() {
        for rt in &["room", "hall"] {
            let mut params = HashMap::new();
            params.insert("type".to_string(), rt.to_string());
            assert!(create_effect_with_params("reverb", 48000.0, &params).is_ok());
        }
        let mut params = HashMap::new();
        params.insert("type".to_string(), "cathedral".to_string());
        assert!(create_effect_with_params("reverb", 48000.0, &params).is_err());
    }

    #[test]
    fn create_effect_tremolo_waveform_enum() {
        for wf in &["sine", "triangle", "square", "samplehold"] {
            let mut params = HashMap::new();
            params.insert("waveform".to_string(), wf.to_string());
            assert!(create_effect_with_params("tremolo", 48000.0, &params).is_ok());
        }
        let mut params = HashMap::new();
        params.insert("waveform".to_string(), "sawtooth".to_string());
        assert!(create_effect_with_params("tremolo", 48000.0, &params).is_err());
    }

    #[test]
    fn create_effect_wah_mode_enum() {
        for mode in &["auto", "manual"] {
            let mut params = HashMap::new();
            params.insert("mode".to_string(), mode.to_string());
            assert!(create_effect_with_params("wah", 48000.0, &params).is_ok());
        }
        let mut params = HashMap::new();
        params.insert("mode".to_string(), "expression".to_string());
        assert!(create_effect_with_params("wah", 48000.0, &params).is_err());
    }

    #[test]
    fn available_effects_count() {
        assert_eq!(available_effects().len(), 15);
    }

    #[test]
    fn available_effects_names_match_registry() {
        let effects = available_effects();
        let names: Vec<&str> = effects.iter().map(|e| e.name).collect();
        let expected = [
            "distortion",
            "compressor",
            "chorus",
            "delay",
            "flanger",
            "phaser",
            "filter",
            "multivibrato",
            "tape",
            "preamp",
            "reverb",
            "tremolo",
            "gate",
            "wah",
            "eq",
        ];
        for name in &expected {
            assert!(names.contains(name), "missing effect: {name}");
        }
    }
}
