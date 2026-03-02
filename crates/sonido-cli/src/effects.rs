//! Effect factory and parameter parsing.
//!
//! All effect creation goes through [`sonido_registry::EffectRegistry`], which
//! returns `Box<dyn EffectWithParams + Send>` backed by `KernelAdapter<XxxKernel>`.
//! CLI-specific aliases for effect names, parameter names, and enum values are
//! normalized here before being passed to the registry.

use sonido_core::EffectWithParams;
use sonido_registry::EffectRegistry;
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

/// Information about an available effect (for display in `sonido effects`).
#[derive(Debug, Clone)]
pub struct EffectInfo {
    pub name: String,
    pub description: String,
    pub parameters: Vec<ParameterInfo>,
}

/// Information about an effect parameter (for display).
#[derive(Debug, Clone)]
pub struct ParameterInfo {
    pub name: String,
    pub description: String,
    pub default: String,
    pub range: String,
}

/// Get information about all available effects from the registry.
pub fn available_effects() -> Vec<EffectInfo> {
    let registry = EffectRegistry::new();
    registry
        .all_effects()
        .iter()
        .map(|desc| {
            let effect = registry.create(desc.id, 48000.0).unwrap();
            let params = (0..effect.effect_param_count())
                .filter_map(|i| {
                    let d = effect.effect_param_info(i)?;
                    Some(ParameterInfo {
                        name: d.short_name.to_lowercase(),
                        description: format!(
                            "{} ({} to {})",
                            d.name,
                            d.format_value(d.min),
                            d.format_value(d.max),
                        ),
                        default: d.format_value(d.default),
                        range: format!("{} .. {}", d.format_value(d.min), d.format_value(d.max)),
                    })
                })
                .collect();
            EffectInfo {
                name: desc.id.to_string(),
                description: desc.description.to_string(),
                parameters: params,
            }
        })
        .collect()
}

/// Map CLI effect name aliases to canonical registry IDs.
///
/// Returns a `&'static str` for known aliases, or the original `name` for
/// pass-through (the registry will reject unknown names downstream).
fn resolve_effect_name<'a>(name: &'a str) -> &'a str {
    let lower = name.to_lowercase();
    match lower.as_str() {
        "lowpass" | "filter" => "filter",
        "vibrato" | "multivibrato" | "multi_vibrato" => "vibrato",
        "tape" | "tapesaturation" | "tape_saturation" => "tape",
        "noisegate" | "noise_gate" | "gate" => "gate",
        "autowah" | "auto_wah" | "wah" => "wah",
        "cleanpreamp" | "clean_preamp" | "preamp" => "preamp",
        "parametriceq" | "parametric_eq" | "peq" | "eq" => "eq",
        "crusher" | "bitcrusher" => "bitcrusher",
        "ring" | "ring_mod" | "ringmod" => "ringmod",
        "distortion" => "distortion",
        "compressor" => "compressor",
        "chorus" => "chorus",
        "flanger" => "flanger",
        "phaser" => "phaser",
        "tremolo" => "tremolo",
        "delay" => "delay",
        "reverb" => "reverb",
        "limiter" => "limiter",
        "stage" => "stage",
        // Pass through as-is — the registry will reject unknown names
        _ => name,
    }
}

/// Map CLI parameter aliases to canonical kernel parameter names.
///
/// The registry's `param_index_by_name()` already matches case-insensitively on
/// the descriptor `name` and `short_name`. This function only handles aliases that
/// don't match either.
fn normalize_param_name(effect_id: &str, key: &str) -> String {
    let k = key.to_lowercase();
    match (effect_id, k.as_str()) {
        // ── Delay ─────────────────────────────────────────────────────────
        ("delay", "time") => "delay time".to_string(),
        ("delay", "pingpong" | "ping_pong") => "ping pong".to_string(),
        ("delay", "fb_lp") => "feedback lp".to_string(),
        ("delay", "fb_hp") => "feedback hp".to_string(),

        // ── Reverb ────────────────────────────────────────────────────────
        ("reverb", "room" | "size") => "room size".to_string(),
        ("reverb", "damp") => "damping".to_string(),
        ("reverb", "predelay" | "pre") => "pre-delay".to_string(),
        ("reverb", "stereo_width" | "width") => "stereo width".to_string(),
        ("reverb", "er" | "er_level") => "er level".to_string(),

        // ── Parametric EQ ─────────────────────────────────────────────────
        ("eq", "lf" | "low_freq") => "low frequency".to_string(),
        ("eq", "lg" | "low_gain") => "low gain".to_string(),
        ("eq", "lq" | "low_q") => "low q".to_string(),
        ("eq", "mf" | "mid_freq") => "mid frequency".to_string(),
        ("eq", "mg" | "mid_gain") => "mid gain".to_string(),
        ("eq", "mq" | "mid_q") => "mid q".to_string(),
        ("eq", "hf" | "high_freq") => "high frequency".to_string(),
        ("eq", "hg" | "high_gain") => "high gain".to_string(),
        ("eq", "hq" | "high_q") => "high q".to_string(),

        // ── Bitcrusher ────────────────────────────────────────────────────
        ("bitcrusher", "bits" | "bit_depth") => "bit depth".to_string(),
        ("bitcrusher", "ds") => "downsample".to_string(),

        // ── Stage ─────────────────────────────────────────────────────────
        ("stage", "haas" | "haas_delay") => "haas".to_string(),
        ("stage", "bass_mono") => "bass mono".to_string(),
        ("stage", "phase_l") => "phase l".to_string(),
        ("stage", "phase_r") => "phase r".to_string(),
        ("stage", "dc_block") => "dc block".to_string(),
        ("stage", "bass_freq") => "bass freq".to_string(),
        ("stage", "haas_side") => "haas side".to_string(),
        ("stage", "bal") => "balance".to_string(),
        ("stage", "channel") => "channel".to_string(),

        // ── Distortion ────────────────────────────────────────────────────
        ("distortion", "waveshape" | "shape") => "waveshape".to_string(),
        ("distortion", "level") => "output".to_string(),

        // ── Compressor ────────────────────────────────────────────────────
        ("compressor", "thresh") => "threshold".to_string(),
        ("compressor", "makeup" | "makeup_gain") => "makeup gain".to_string(),
        ("compressor", "detect" | "detection") => "detection".to_string(),
        ("compressor", "sc_hpf" | "sidechain_hpf") => "sc hpf freq".to_string(),
        ("compressor", "auto_makeup" | "automu") => "auto makeup".to_string(),

        // ── Gate ──────────────────────────────────────────────────────────
        ("gate", "thresh") => "threshold".to_string(),
        ("gate", "atk") => "attack".to_string(),
        ("gate", "rel") => "release".to_string(),
        ("gate", "hyst") => "hysteresis".to_string(),
        ("gate", "sc_hpf") => "sc hpf freq".to_string(),

        // ── Limiter ───────────────────────────────────────────────────────
        ("limiter", "thresh") => "threshold".to_string(),
        ("limiter", "ceil") => "ceiling".to_string(),
        ("limiter", "rel") => "release".to_string(),
        ("limiter", "la") => "lookahead".to_string(),

        // ── Flanger ───────────────────────────────────────────────────────
        ("flanger", "fdbk") => "feedback".to_string(),

        // ── Phaser ────────────────────────────────────────────────────────
        ("phaser", "stg") => "stages".to_string(),
        ("phaser", "fdbk") => "feedback".to_string(),
        ("phaser", "spread" | "stereo_spread") => "stereo spread".to_string(),

        // ── Tremolo ───────────────────────────────────────────────────────
        ("tremolo", "wave" | "waveform") => "waveform".to_string(),
        ("tremolo", "spread" | "stereo_spread") => "stereo spread".to_string(),

        // ── Wah ───────────────────────────────────────────────────────────
        ("wah", "freq") => "frequency".to_string(),
        ("wah", "reso" | "q") => "resonance".to_string(),
        ("wah", "sens") => "sensitivity".to_string(),

        // ── Ring Mod ──────────────────────────────────────────────────────
        ("ringmod", "freq") => "frequency".to_string(),
        ("ringmod", "wave" | "waveform") => "waveform".to_string(),

        // ── Tape ──────────────────────────────────────────────────────────
        ("tape", "hf" | "hf_rolloff") => "hf rolloff".to_string(),

        // ── Preamp ────────────────────────────────────────────────────────
        ("preamp", "headroom_db") => "tone".to_string(),

        // ── Chorus ────────────────────────────────────────────────────────
        ("chorus", "base_delay" | "basedelay") => "base delay".to_string(),

        // ── Filter ────────────────────────────────────────────────────────
        ("filter", "q") => "resonance".to_string(),

        // Default: replace underscores with spaces (kernel descriptors use spaces,
        // CLI users type underscores) and pass through for case-insensitive matching
        _ => key.replace('_', " "),
    }
}

/// Normalize CLI shorthand for stepped/enum parameter values into the
/// canonical step-label strings that `ParamDescriptor::parse_value()` expects.
fn normalize_enum_value(param_name: &str, value: &str) -> String {
    let lower = value.to_lowercase();
    match lower.as_str() {
        // ── Waveshape ─────────────────────────────────────────────────────
        "soft" | "softclip" | "soft_clip" => "Soft Clip".to_string(),
        "hard" | "hardclip" | "hard_clip" => "Hard Clip".to_string(),
        "fold" | "foldback" => "Foldback".to_string(),
        "asym" | "asymmetric" => "Asymmetric".to_string(),

        // ── Waveform (tremolo, ring mod) ──────────────────────────────────
        "sin" | "sine" => "Sine".to_string(),
        "tri" | "triangle" => "Triangle".to_string(),
        "sq" | "square" => "Square".to_string(),
        "sh" | "s&h" | "samplehold" | "sample_hold" => "SampleHold".to_string(),

        // ── Wah mode ──────────────────────────────────────────────────────
        "auto" => "Auto".to_string(),
        "manual" => "Manual".to_string(),

        // ── Filter types ──────────────────────────────────────────────────
        "lp" | "lowpass" | "low_pass" => "Lowpass".to_string(),
        "hp" | "highpass" | "high_pass" => "Highpass".to_string(),
        "bp" | "bandpass" | "band_pass" => "Bandpass".to_string(),

        // ── Boolean on/off (ping-pong, dc_block, etc.) ────────────────────
        "true" | "on" | "yes" => "On".to_string(),
        "false" | "off" | "no" => "Off".to_string(),

        // Pass through: might be numeric or already matches a step label
        _ => {
            // If it looks like a param name check is useful, use the param_name
            // to provide context, but for now just return as-is
            let _ = param_name;
            value.to_string()
        }
    }
}

/// Create an effect with custom parameters using the registry.
///
/// Effect names and parameter names are resolved through CLI alias tables,
/// so users can type `distortion:drive=15,shape=soft` or
/// `eq:lf=200,lg=3,hf=8000,hg=-2`.
pub fn create_effect_with_params(
    name: &str,
    sample_rate: f32,
    params: &HashMap<String, String>,
) -> Result<Box<dyn EffectWithParams + Send>, EffectError> {
    let registry = EffectRegistry::new();
    let effect_id = resolve_effect_name(name);

    let mut effect = registry
        .create(effect_id, sample_rate)
        .ok_or_else(|| EffectError::UnknownEffect(name.to_string()))?;

    for (key, value) in params {
        let norm_key = normalize_param_name(effect_id, key);

        let idx = registry
            .param_index_by_name(effect_id, &norm_key)
            .ok_or_else(|| EffectError::UnknownParameter {
                effect: name.to_string(),
                param: key.clone(),
            })?;

        let desc = effect.effect_param_info(idx).unwrap();

        // Try enum normalization first, then parse_value
        let norm_value = normalize_enum_value(&norm_key, value);
        let parsed = desc
            .parse_value(&norm_value)
            .ok_or_else(|| EffectError::InvalidValue {
                param: key.clone(),
                message: format!("'{}' is not a valid value for '{}'", value, desc.name),
            })?;

        effect.effect_set_param(idx, parsed);
    }

    Ok(effect)
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
pub(crate) fn parse_effect_spec(
    spec: &str,
) -> Result<(String, HashMap<String, String>), EffectError> {
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
        params.insert("decay".to_string(), "80".to_string());
        params.insert("room_size".to_string(), "70".to_string());
        params.insert("damping".to_string(), "30".to_string());
        params.insert("mix".to_string(), "50".to_string());
        let effect = create_effect_with_params("reverb", 48000.0, &params);
        assert!(effect.is_ok());
    }

    #[test]
    fn test_parse_chain_with_reverb() {
        let chain = parse_chain("delay:time=300|reverb:decay=90,mix=60", 48000.0);
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
            assert!(
                create_effect_with_params("distortion", 48000.0, &params).is_ok(),
                "waveshape '{}' should be accepted",
                shape
            );
        }
        let mut params = HashMap::new();
        params.insert("waveshape".to_string(), "invalid".to_string());
        assert!(create_effect_with_params("distortion", 48000.0, &params).is_err());
    }

    #[test]
    fn create_effect_tremolo_waveform_enum() {
        for wf in &["sine", "triangle", "square", "samplehold"] {
            let mut params = HashMap::new();
            params.insert("waveform".to_string(), wf.to_string());
            assert!(
                create_effect_with_params("tremolo", 48000.0, &params).is_ok(),
                "waveform '{}' should be accepted",
                wf
            );
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
            assert!(
                create_effect_with_params("wah", 48000.0, &params).is_ok(),
                "wah mode '{}' should be accepted",
                mode
            );
        }
    }

    #[test]
    fn available_effects_count() {
        assert_eq!(available_effects().len(), 19);
    }

    #[test]
    fn available_effects_names_match_registry() {
        let effects = available_effects();
        let names: Vec<&str> = effects.iter().map(|e| e.name.as_str()).collect();
        let expected = [
            "distortion",
            "compressor",
            "chorus",
            "delay",
            "flanger",
            "phaser",
            "filter",
            "vibrato",
            "tape",
            "preamp",
            "reverb",
            "tremolo",
            "gate",
            "wah",
            "eq",
            "limiter",
            "bitcrusher",
            "ringmod",
            "stage",
        ];
        for name in &expected {
            assert!(names.contains(name), "missing effect: {name}");
        }
    }

    #[test]
    fn effect_name_aliases_resolve() {
        let aliases = vec![
            ("lowpass", "filter"),
            ("vibrato", "vibrato"),
            ("multivibrato", "vibrato"),
            ("tapesaturation", "tape"),
            ("noisegate", "gate"),
            ("autowah", "wah"),
            ("cleanpreamp", "preamp"),
            ("parametriceq", "eq"),
            ("peq", "eq"),
            ("crusher", "bitcrusher"),
            ("ring", "ringmod"),
        ];
        for (alias, expected) in aliases {
            let params = HashMap::new();
            let result = create_effect_with_params(alias, 48000.0, &params);
            assert!(
                result.is_ok(),
                "alias '{}' should resolve to '{}'",
                alias,
                expected
            );
        }
    }

    #[test]
    fn all_effects_creatable_with_no_params() {
        let registry = EffectRegistry::new();
        for desc in registry.all_effects() {
            let params = HashMap::new();
            let result = create_effect_with_params(desc.id, 48000.0, &params);
            assert!(
                result.is_ok(),
                "effect '{}' should be creatable with no params",
                desc.id
            );
        }
    }

    #[test]
    fn eq_param_aliases() {
        let cases = vec![
            ("lf", "200"),
            ("lg", "3"),
            ("mf", "1000"),
            ("mg", "-2"),
            ("hf", "8000"),
            ("hg", "1"),
            ("lq", "2"),
            ("mq", "1.5"),
            ("hq", "3"),
        ];
        for (alias, val) in cases {
            let mut params = HashMap::new();
            params.insert(alias.to_string(), val.to_string());
            let result = create_effect_with_params("eq", 48000.0, &params);
            assert!(result.is_ok(), "eq param alias '{}' should work", alias);
        }
    }

    #[test]
    fn delay_param_aliases() {
        let mut params = HashMap::new();
        params.insert("time".to_string(), "500".to_string());
        assert!(create_effect_with_params("delay", 48000.0, &params).is_ok());

        let mut params = HashMap::new();
        params.insert("pingpong".to_string(), "on".to_string());
        assert!(create_effect_with_params("delay", 48000.0, &params).is_ok());
    }

    #[test]
    fn reverb_param_aliases() {
        let mut params = HashMap::new();
        params.insert("room".to_string(), "70".to_string());
        params.insert("damp".to_string(), "30".to_string());
        assert!(create_effect_with_params("reverb", 48000.0, &params).is_ok());
    }

    #[test]
    fn reverb_type_param_is_gone() {
        // Kernel-based reverb no longer has a "type" / "preset" param
        let mut params = HashMap::new();
        params.insert("type".to_string(), "hall".to_string());
        let result = create_effect_with_params("reverb", 48000.0, &params);
        assert!(matches!(result, Err(EffectError::UnknownParameter { .. })));
    }
}
