//! Effect factory and parameter parsing.
//!
//! All effect creation goes through [`sonido_registry::EffectRegistry`], which
//! returns `Box<dyn EffectWithParams + Send>` backed by `Adapter<XxxKernel, SmoothedPolicy>`.
//! CLI and GUI aliases for effect names, parameter names, and enum values are
//! normalized here before being passed to the registry.

use sonido_core::EffectWithParams;
use sonido_registry::EffectRegistry;
use std::collections::HashMap;

/// Error type for effect creation.
#[derive(Debug, thiserror::Error)]
pub enum EffectError {
    /// Unknown effect name.
    #[error("Unknown effect: {0}")]
    UnknownEffect(String),

    /// Unknown parameter name for a known effect.
    #[error("Unknown parameter '{param}' for effect '{effect}'")]
    UnknownParameter {
        /// Effect name.
        effect: String,
        /// Parameter name.
        param: String,
    },

    /// Invalid parameter value.
    #[error("Invalid parameter value for '{param}': {message}")]
    InvalidValue {
        /// Parameter name.
        param: String,
        /// Error description.
        message: String,
    },

    /// Generic parse error.
    #[error("Parse error: {0}")]
    ParseError(String),
}

/// Map user-facing effect name aliases to canonical registry IDs.
///
/// Returns the canonical `&'static str` ID for known aliases, or the original
/// `name` for pass-through (the registry will reject unknown names downstream).
///
/// Shared between CLI and GUI to ensure consistent behavior.
pub fn resolve_effect_name(name: &str) -> &str {
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

/// Map parameter aliases to canonical kernel parameter names.
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
        // users type underscores) and pass through for case-insensitive matching
        _ => key.replace('_', " "),
    }
}

/// Normalize shorthand for stepped/enum parameter values into the
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
            let _ = param_name;
            value.to_string()
        }
    }
}

/// Create an effect with custom parameters using the registry.
///
/// Effect names and parameter names are resolved through alias tables,
/// so users can type `distortion:drive=15,shape=soft` or
/// `eq:lf=200,lg=3,hf=8000,hg=-2`.
///
/// Returns the created effect and the resolved canonical effect ID
/// (as a `&'static str` from the registry).
pub fn create_effect_with_params(
    name: &str,
    sample_rate: f32,
    params: &HashMap<String, String>,
) -> Result<(Box<dyn EffectWithParams + Send>, &'static str), EffectError> {
    let registry = EffectRegistry::new();
    let effect_id = resolve_effect_name(name);

    let resolved_static_id = registry
        .get(effect_id)
        .map(|d| d.id)
        .ok_or_else(|| EffectError::UnknownEffect(name.to_string()))?;

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

    Ok((effect, resolved_static_id))
}

/// Parse an effect chain specification.
///
/// Format: `"effect1:param1=value1,param2=value2|effect2:param=value"`
///
/// Examples:
/// - `"distortion:drive=15"`
/// - `"preamp:gain=6|distortion:drive=12|delay:time=300,feedback=0.4"`
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
        let (effect, _) = create_effect_with_params(&name, sample_rate, &params)?;
        effects.push(effect);
    }

    Ok(effects)
}

/// Parse a single effect specification.
///
/// Format: `"effect_name:param1=value1,param2=value2"`
pub fn parse_effect_spec(spec: &str) -> Result<(String, HashMap<String, String>), EffectError> {
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
        let result = create_effect_with_params("distortion", 48000.0, &params);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().1, "distortion");

        let result = create_effect_with_params("unknown", 48000.0, &params);
        assert!(result.is_err());
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
            assert_eq!(
                resolve_effect_name(alias),
                expected,
                "alias '{}' should resolve to '{}'",
                alias,
                expected
            );
        }
    }

    #[test]
    fn create_effect_unknown_param() {
        let mut params = HashMap::new();
        params.insert("bogus".to_string(), "1.0".to_string());
        let result = create_effect_with_params("distortion", 48000.0, &params);
        assert!(matches!(result, Err(EffectError::UnknownParameter { .. })));
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
    }
}
