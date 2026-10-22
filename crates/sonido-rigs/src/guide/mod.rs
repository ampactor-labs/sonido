//! How to play the pedal, drawn from the code that runs it.
//!
//! Two forms of one guide: [`html_guide`], an infographic of the pedal with
//! every control annotated (what `sonido pedal controls` opens, and what the
//! editor's pedal panel shows), and [`text_card`], the same facts as an
//! 80-column terminal card. Every word in both comes from code: what each
//! footswitch press, toggle, knob page, LED and power-on gesture does from
//! `sonido_patch::controls` (the functions the firmware dispatches through);
//! each jam effect's knobs from `sonido_platform::knob_mapping` and the
//! effect's own parameters; the factory rigs from [`crate::factory_rigs`].
//! Only the drawing's geometry is laid out by hand.

mod html;
mod text;

pub use html::html_guide;
pub use text::{Style, effect_card, text_card};

use sonido_patch::Patch;
use sonido_patch::controls::{self, KnobPage, Mode, SETUP_LEVELS};
use sonido_platform::knob_mapping::{NULL_KNOB, knob_map};
use sonido_registry::{EffectRegistry, JAM_EFFECT_IDS};

/// The sample rate effects are built at to read their parameters.
const SR: f32 = 48_000.0;

/// T2's three positions, top to bottom.
const POSITIONS: [u8; 3] = [0, 1, 2];

/// Each knob's parameter name for `id` ("—" for an unmapped knob).
pub(crate) fn knob_labels(registry: &EffectRegistry, id: &str) -> Option<[String; 6]> {
    let map = knob_map(id)?;
    let effect = registry.create(id, SR)?;
    Some(core::array::from_fn(|k| {
        if map[k] == NULL_KNOB {
            return "—".to_owned();
        }
        effect.effect_param_info(usize::from(map[k])).map_or_else(
            || "?".to_owned(),
            |d| {
                if d.name.chars().count() > 11 {
                    d.short_name.to_owned()
                } else {
                    d.name.to_owned()
                }
            },
        )
    }))
}

/// The jam effects with their six knob labels, in pick-knob order.
pub(crate) fn jam_effect_knobs(registry: &EffectRegistry) -> Vec<(String, [String; 6])> {
    JAM_EFFECT_IDS
        .iter()
        .filter_map(|&id| Some((id.to_owned(), knob_labels(registry, id)?)))
        .collect()
}

/// A rig's six knob names ("—" for an unmapped knob).
pub(crate) fn rig_knobs(rig: &Patch) -> [String; 6] {
    core::array::from_fn(|k| {
        let name = &rig.macros[k].name;
        if name.is_empty() {
            "—".to_owned()
        } else {
            name.clone()
        }
    })
}

/// One way of reading the knobs: the page, where it applies, and each
/// knob's job there.
pub(crate) struct KnobMode {
    /// The page (`controls::knob_page`).
    pub page: KnobPage,
    /// Where it applies, e.g. "In the jam, T2 up or down".
    pub when: String,
    /// What it's for, e.g. "the T1 slot's effect, its A or B sound".
    pub what: &'static str,
    /// Each knob's job, K1–K6.
    pub knobs: [String; 6],
}

/// The knobs' three pages (the jam's sound, the jam's setup page, a rig),
/// each with where it applies and every knob's job, all derived from code.
pub(crate) fn knob_modes(registry: &EffectRegistry, rigs: &[Patch]) -> Vec<KnobMode> {
    let mut modes: Vec<KnobMode> = Vec::new();
    for jam in [true, false] {
        for p in POSITIONS {
            let mode = Mode::from_toggle(p);
            let page = controls::knob_page(jam, mode);
            if let Some(existing) = modes.iter_mut().find(|m| m.page == page) {
                if jam && !existing.when.contains(mode.label()) {
                    existing.when.push_str(&format!(" or {}", t2_place(p)));
                }
                continue;
            }
            let when = if jam {
                format!("In the jam, T2 {}", t2_place(p))
            } else {
                "In any rig".to_owned()
            };
            let knobs = match page {
                KnobPage::Sound => sound_page_knobs(registry),
                KnobPage::Setup => core::array::from_fn(|k| match SETUP_LEVELS.get(k) {
                    Some((desc, _)) => desc.name.to_owned(),
                    None => format!("slot {} effect", k - SETUP_LEVELS.len() + 1),
                }),
                KnobPage::Macros => rig_page_knobs(rigs),
            };
            modes.push(KnobMode {
                page,
                when,
                what: page.describe(),
                knobs,
            });
        }
    }
    modes
}

/// "up (A)", "middle (Morph)", "down (B)".
pub(crate) fn t2_place(position: u8) -> String {
    let place = ["up", "middle", "down"][usize::from(position.min(2))];
    format!("{place} ({})", Mode::from_toggle(position).label())
}

/// The jam's sound page: a knob's parameter name when most jam effects share
/// it (K2 Mix, K3 Output), else its role in the knob rule.
fn sound_page_knobs(registry: &EffectRegistry) -> [String; 6] {
    let effects = jam_effect_knobs(registry);
    core::array::from_fn(|k| {
        let mut counts: Vec<(&str, usize)> = Vec::new();
        for (_, knobs) in &effects {
            match counts.iter_mut().find(|(name, _)| *name == knobs[k]) {
                Some((_, n)) => *n += 1,
                None => counts.push((&knobs[k], 1)),
            }
        }
        let shared = counts
            .into_iter()
            .filter(|(name, _)| *name != "—")
            .max_by_key(|(_, n)| *n)
            .filter(|(_, n)| *n * 4 >= effects.len() * 3)
            .map(|(name, _)| name.to_owned());
        shared.unwrap_or_else(|| {
            match k {
                0 => "an extra",
                3 => "the main control",
                4 => "the 2nd control",
                _ => "the 3rd control",
            }
            .to_owned()
        })
    })
}

/// A rig's page: a knob's name when every factory rig shares it (K1–K3:
/// Input, Morph Time, Master), else "the rig's own".
fn rig_page_knobs(rigs: &[Patch]) -> [String; 6] {
    core::array::from_fn(|k| {
        let first = rigs.first().map(|r| r.macros[k].name.clone());
        match first {
            Some(name) if !name.is_empty() && rigs.iter().all(|r| r.macros[k].name == name) => name,
            _ => "the rig's own".to_owned(),
        }
    })
}
