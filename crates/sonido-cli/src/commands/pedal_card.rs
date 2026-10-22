//! `sonido pedal controls`: the pedal's controls, drawn from the code that
//! runs them, so the card can't fall out of date.
//!
//! The footswitches, toggles, knob pages, LEDs and power-on gestures come from
//! `sonido_patch::controls`, the functions the firmware itself dispatches
//! through; each jam effect's knobs from `sonido_platform::knob_mapping` and
//! the effect's own parameters; the factory rigs' knobs from `sonido_rigs`.

use std::fmt::Write as _;
use std::io::IsTerminal;

use sonido_patch::controls::{
    self, Footswitch, KnobPage, LedShows, Mode, PowerOn, Press, SETUP_LEVELS, Toggle,
};
use sonido_patch::pedal::{JAM_SLOTS, Topology};
use sonido_platform::knob_mapping::{NULL_KNOB, knob_map, knob_to_param};
use sonido_registry::{EffectRegistry, JAM_EFFECT_IDS};

const SR: f32 = 48_000.0;
/// The card's width.
const WIDTH: usize = 80;
/// T2's three positions, top to bottom.
const POSITIONS: [u8; 3] = [0, 1, 2];
const PLACES: [&str; 3] = ["up", "middle", "down"];

/// ANSI styling, on only for a terminal that hasn't asked for `NO_COLOR`.
struct Style(bool);

impl Style {
    fn detect() -> Self {
        Self(std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none())
    }
    fn paint(&self, code: &str, s: &str) -> String {
        if self.0 {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_owned()
        }
    }
    fn head(&self, s: &str) -> String {
        self.paint("1", s)
    }
    fn dim(&self, s: &str) -> String {
        self.paint("2", s)
    }
    fn key(&self, s: &str) -> String {
        self.paint("36", s)
    }
}

/// The whole card.
pub fn card() -> String {
    let st = Style::detect();
    let mut out = String::new();
    let o = &mut out;
    let _ = writeln!(
        o,
        "{}  {}",
        st.head("SONIDO PEDAL · controls"),
        st.dim(&format!(
            "drawn from this build's source (link protocol {})",
            sonido_patch::link::PROTOCOL_VERSION
        ))
    );
    face(o);
    footswitches(o, &st);
    leds(o, &st);
    knobs(o, &st);
    setup_page(o, &st);
    jam_effects(o, &st);
    factory_rigs(o, &st);
    rig_list_and_power_on(o, &st);
    let _ = writeln!(
        o,
        "\n{}",
        st.dim("`sonido pedal controls <effect>` shows one effect's knobs with their ranges.")
    );
    out
}

/// The pedal's face: knobs, the toggles with what each position selects, the
/// LEDs over their footswitches and each footswitch's tap.
fn face(o: &mut String) {
    const W: usize = 56;
    let line = |o: &mut String, s: &str| {
        let pad = W.saturating_sub(s.chars().count());
        let _ = writeln!(o, " │{s}{}│", " ".repeat(pad));
    };
    let _ = writeln!(o, "\n ╭{}╮", "─".repeat(W));
    line(o, "   ◯ K1        ◯ K2        ◯ K3      levels (K3 volume)");
    line(o, "   ◯ K4        ◯ K5        ◯ K6      the sound");
    line(o, "");
    line(o, "   T1          T2          T3");
    for (p, arrow) in POSITIONS.into_iter().zip(['▲', '●', '▼']) {
        line(
            o,
            &format!(
                "   {arrow} {:<10}{arrow} {:<10}{arrow} {}",
                format!("slot {}", controls::slot(p) + 1),
                Mode::from_toggle(p).label(),
                Topology::from_toggle(p).label(),
            ),
        );
    }
    line(o, "");
    line(o, "   ● LED 1                              ● LED 2");
    line(o, "   ( FS1 )                              ( FS2 )");
    let tap = |fs| controls::footswitch(fs, Press::Tap, Mode::A).describe();
    line(
        o,
        &format!("   {:<37}{}", tap(Footswitch::Fs1), tap(Footswitch::Fs2)),
    );
    let _ = writeln!(o, " ╰{}╯", "─".repeat(W));
}

/// A table with one column per T2 position.
fn t2_table(o: &mut String, st: &Style, rows: &[(String, [String; 3])]) {
    let heads: Vec<String> = POSITIONS
        .iter()
        .map(|&p| {
            format!(
                "T2 {} ({})",
                PLACES[usize::from(p)],
                Mode::from_toggle(p).label()
            )
        })
        .collect();
    let _ = writeln!(
        o,
        "{}",
        st.dim(&format!(
            "   {:<12}{:<20}{:<20}{}",
            "", heads[0], heads[1], heads[2]
        ))
    );
    for (name, cells) in rows {
        let _ = writeln!(
            o,
            "   {}{:<20}{:<20}{}",
            st.key(&format!("{name:<12}")),
            cells[0],
            cells[1],
            cells[2]
        );
    }
}

fn footswitches(o: &mut String, st: &Style) {
    let _ = writeln!(
        o,
        "\n{}  {}",
        st.head("FOOTSWITCHES"),
        st.dim(&format!(
            "tap: acted on at release · hold: {:.1} s, acted on while held",
            f64::from(controls::HOLD_MS) / 1000.0
        ))
    );
    let mut rows = Vec::new();
    for (fs, name) in [(Footswitch::Fs1, "FS1"), (Footswitch::Fs2, "FS2")] {
        for (press, how) in [(Press::Tap, "tap"), (Press::Hold, "hold")] {
            let cells = POSITIONS.map(|p| {
                controls::footswitch(fs, press, Mode::from_toggle(p))
                    .describe()
                    .to_owned()
            });
            rows.push((format!("{name} {how}"), cells));
        }
    }
    t2_table(o, st, &rows);
}

fn leds(o: &mut String, st: &Style) {
    let _ = writeln!(
        o,
        "\n{}  {}",
        st.head("LEDS"),
        st.dim("each shows what the footswitch below it does")
    );
    let led1 = vec![
        format!("on: {}", controls::led1(false, false).describe()),
        format!("off: {}", controls::led1(true, false).describe()),
        format!(
            "after a rig change: {}",
            controls::led1(false, true).describe()
        ),
    ];
    let mut groups: Vec<(LedShows, Vec<&str>)> = Vec::new();
    for p in POSITIONS {
        let shows = controls::led2(false, Mode::from_toggle(p));
        let place = PLACES[usize::from(p)];
        match groups.iter_mut().find(|(s, _)| *s == shows) {
            Some((_, places)) => places.push(place),
            None => groups.push((shows, vec![place])),
        }
    }
    let mut led2: Vec<String> = groups
        .into_iter()
        .map(|(shows, places)| format!("T2 {}: {}", places.join("/"), shows.describe()))
        .collect();
    led2.push(format!("off: {}", controls::led2(true, Mode::A).describe()));
    for (name, lines) in [("LED 1 (FS1)", led1), ("LED 2 (FS2)", led2)] {
        for (i, text) in lines.iter().enumerate() {
            let label = if i == 0 { name } else { "" };
            let _ = writeln!(o, "   {}{text}", st.key(&format!("{label:<14}")));
        }
    }
}

fn knobs(o: &mut String, st: &Style) {
    let _ = writeln!(
        o,
        "\n{}  {}",
        st.head("KNOBS"),
        st.dim("top row: levels (K3 always a volume) · bottom row: the sound")
    );
    let rows: Vec<(String, [String; 3])> = [(true, "in the jam"), (false, "in a rig")]
        .into_iter()
        .map(|(jam, place)| {
            let cells =
                POSITIONS.map(|p| short_page(controls::knob_page(jam, Mode::from_toggle(p))));
            (place.to_owned(), cells)
        })
        .collect();
    t2_table(o, st, &rows);
    let jam_only: Vec<&str> = [(Toggle::T1, "T1"), (Toggle::T3, "T3")]
        .into_iter()
        .filter(|(t, _)| !controls::toggle_active(*t, false))
        .map(|(_, n)| n)
        .collect();
    if !jam_only.is_empty() {
        let _ = writeln!(
            o,
            "   {}",
            st.dim(&format!("{} act in the jam only.", jam_only.join(" and ")))
        );
    }
}

fn short_page(page: KnobPage) -> String {
    match page {
        KnobPage::Sound => "the slot's sound",
        KnobPage::Setup => "the setup page",
        KnobPage::Macros => "the rig's knobs",
    }
    .into()
}

fn setup_page(o: &mut String, st: &Style) {
    let _ = writeln!(
        o,
        "\n{}  {}",
        st.head("THE JAM'S SETUP PAGE"),
        st.dim("T2 middle, in the jam")
    );
    for (k, (desc, unit)) in SETUP_LEVELS.iter().enumerate() {
        let v = |x: f32| if *unit == "dB" { signed(x) } else { num(x) };
        let _ = writeln!(
            o,
            "   {}{:<12}{:<18}noon {} {unit}",
            st.key(&format!("K{:<3}", k + 1)),
            desc.name,
            format!("{} … {} {unit}", v(desc.min), v(desc.max)),
            v(desc.default)
        );
    }
    let slots: Vec<String> = (1..=JAM_SLOTS).map(|s| s.to_string()).collect();
    let _ = writeln!(
        o,
        "   {}pick slot {}'s effect, by where the knob points:",
        st.key("K4 K5 K6  "),
        slots.join(" · ")
    );
    let zones: Vec<&str> = std::iter::once("empty")
        .chain(JAM_EFFECT_IDS.iter().copied())
        .collect();
    for line in wrap(&zones, " · ", WIDTH - 13) {
        let _ = writeln!(o, "             {line}");
    }
    let _ = writeln!(
        o,
        "             {}",
        st.dim(&format!(
            "fully left to fully right; a pick loads once the knob rests {:.1} s",
            f64::from(controls::PICK_SETTLE_MS) / 1000.0
        ))
    );
}

/// A table of K1–K6 labels per row: the top row's three, then the bottom's.
fn knob_table(o: &mut String, st: &Style, rows: &[(String, [String; 6])]) {
    let name_w = rows
        .iter()
        .map(|(n, _)| n.chars().count())
        .max()
        .unwrap_or(0)
        + 2;
    let col_w: [usize; 6] = core::array::from_fn(|k| {
        rows.iter()
            .map(|(_, cells)| cells[k].chars().count())
            .max()
            .unwrap_or(0)
            .max(2)
            + 2
    });
    let mut head = format!("   {:<name_w$}", "");
    for (k, w) in col_w.iter().enumerate() {
        if k == 3 {
            head.push_str("│ ");
        }
        head.push_str(&format!("{:<w$}", format!("K{}", k + 1)));
    }
    let _ = writeln!(o, "{}", st.dim(head.trim_end()));
    for (name, cells) in rows {
        let mut line = String::new();
        for (k, cell) in cells.iter().enumerate() {
            if k == 3 {
                line.push_str("│ ");
            }
            line.push_str(&format!("{cell:<w$}", w = col_w[k]));
        }
        let _ = writeln!(
            o,
            "   {}{}",
            st.key(&format!("{name:<name_w$}")),
            line.trim_end()
        );
    }
}

fn jam_effects(o: &mut String, st: &Style) {
    let _ = writeln!(
        o,
        "\n{}  {}",
        st.head("JAM EFFECTS"),
        st.dim("T2 up edits A, down edits B; T1 picks the slot")
    );
    let registry = EffectRegistry::new();
    let rows: Vec<(String, [String; 6])> = JAM_EFFECT_IDS
        .iter()
        .filter_map(|&id| Some((id.to_owned(), knob_labels(&registry, id)?)))
        .collect();
    knob_table(o, st, &rows);
}

/// Each knob's parameter name for `id` ("—" for an unmapped knob).
fn knob_labels(registry: &EffectRegistry, id: &str) -> Option<[String; 6]> {
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

fn factory_rigs(o: &mut String, st: &Style) {
    let _ = writeln!(
        o,
        "\n{}  {}",
        st.head("FACTORY RIGS"),
        st.dim("in a rig, the knobs are the rig's own")
    );
    let rows: Vec<(String, [String; 6])> = sonido_rigs::factory_rigs()
        .into_iter()
        .map(|rig| {
            let cells = core::array::from_fn(|k| {
                let name = &rig.macros[k].name;
                if name.is_empty() {
                    "—".to_owned()
                } else {
                    name.clone()
                }
            });
            (rig.name, cells)
        })
        .collect();
    knob_table(o, st, &rows);
}

fn rig_list_and_power_on(o: &mut String, st: &Style) {
    let _ = writeln!(
        o,
        "\n{}  jam → bank rigs 1–{} (the filled ones) → factory rigs",
        st.head("RIG LIST"),
        controls::BANK_SLOTS
    );
    let hold = |fs| controls::footswitch(fs, Press::Hold, Mode::A).describe();
    let _ = writeln!(
        o,
        "   {}",
        st.dim(&format!(
            "FS1 hold: {} · FS2 hold: {} · the list wraps around",
            hold(Footswitch::Fs1),
            hold(Footswitch::Fs2)
        ))
    );
    let mut gestures: Vec<(PowerOn, Vec<&str>)> = Vec::new();
    for (fs1, fs2, held) in [
        (true, false, "FS1"),
        (true, true, "FS1 + FS2"),
        (false, false, "nothing"),
        (false, true, "FS2"),
    ] {
        let what = controls::power_on(fs1, fs2);
        match gestures.iter_mut().find(|(w, _)| *w == what) {
            Some((_, h)) => h.push(held),
            None => gestures.push((what, vec![held])),
        }
    }
    let _ = writeln!(
        o,
        "\n{}  {}",
        st.head("POWER-ON"),
        st.dim("held as the pedal starts")
    );
    for (what, held) in gestures {
        let _ = writeln!(
            o,
            "   {}{}",
            st.key(&format!("{:<20}", held.join(" or "))),
            what.describe()
        );
    }
}

/// One effect's page: each knob's parameter, its travel and its noon, as the
/// pedal maps it.
pub fn effect_card(id: &str) -> anyhow::Result<String> {
    let st = Style::detect();
    let registry = EffectRegistry::new();
    let map = knob_map(id).ok_or_else(|| {
        let mapped: Vec<&str> = sonido_registry::EFFECT_UIDS
            .iter()
            .map(|(id, _)| *id)
            .filter(|id| knob_map(id).is_some())
            .collect();
        anyhow::anyhow!(
            "'{id}' has no pedal knob map; these do: {}",
            mapped.join(", ")
        )
    })?;
    let effect = registry
        .create(id, SR)
        .ok_or_else(|| anyhow::anyhow!("no effect '{id}'"))?;
    let mut out = String::new();
    let o = &mut out;
    let jam = JAM_EFFECT_IDS.contains(&id);
    let _ = writeln!(
        o,
        "{}  {}",
        st.head(&format!("{} on the pedal's knobs", id.to_uppercase())),
        st.dim(if jam {
            "in the jam: T2 up edits A, down edits B"
        } else {
            "not in the jam's palette; its knob map"
        })
    );
    let _ = writeln!(
        o,
        "{}",
        st.dim(&format!(
            "   {:<7}{:<15}{:<30}{}",
            "knob", "parameter", "fully left … fully right", "noon"
        ))
    );
    for (k, &idx) in map.iter().enumerate() {
        if k == 3 {
            let _ = writeln!(o, "   {}", st.dim("─────"));
        }
        let knob = st.key(&format!("{:<7}", format!("K{}", k + 1)));
        if idx == NULL_KNOB {
            let _ = writeln!(o, "   {knob}—");
            continue;
        }
        let idx = usize::from(idx);
        let Some(desc) = effect.effect_param_info(idx) else {
            continue;
        };
        let at = |pos: f32| {
            let v = knob_to_param(id, idx, &desc, pos);
            effect.effect_format_value(idx, v).unwrap_or_else(|| num(v))
        };
        let _ = writeln!(
            o,
            "   {knob}{:<15}{:<30}{}",
            desc.name,
            format!("{} … {}", at(0.0), at(1.0)),
            at(0.5)
        );
    }
    Ok(out)
}

/// `items` joined by `sep` into lines no wider than `width`.
fn wrap(items: &[&str], sep: &str, width: usize) -> Vec<String> {
    let mut lines = vec![String::new()];
    for item in items {
        let last = lines.last_mut().expect("one line");
        if last.is_empty() {
            last.push_str(item);
        } else if last.chars().count() + sep.chars().count() + item.chars().count() <= width {
            last.push_str(sep);
            last.push_str(item);
        } else {
            last.push_str(sep.trim_end());
            lines.push((*item).to_owned());
        }
    }
    lines
}

/// A level in dB with its sign.
fn signed(v: f32) -> String {
    if v.abs() < 1e-4 {
        "0".into()
    } else {
        format!("{:+}", v.round() as i64)
    }
}

/// A number without a pointless fraction.
fn num(v: f32) -> String {
    if (v - v.round()).abs() < 1e-4 {
        format!("{}", v.round() as i64)
    } else {
        format!("{v:.1}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_shows_every_control_and_the_jam_and_factory_rigs() {
        let card = card();
        for word in [
            "FS1 tap",
            "FS2 hold",
            "LED 1",
            "LED 2",
            "tap tempo",
            "glide",
            "Morph",
        ] {
            assert!(card.contains(word), "card is missing {word:?}");
        }
        for id in JAM_EFFECT_IDS {
            assert!(card.contains(id), "card is missing jam effect {id}");
        }
        for rig in sonido_rigs::factory_rigs() {
            assert!(card.contains(&rig.name), "card is missing {}", rig.name);
        }
    }

    #[test]
    fn card_fits_an_80_column_terminal() {
        let pages = std::iter::once(card()).chain(
            sonido_registry::EFFECT_UIDS
                .iter()
                .filter(|(id, _)| knob_map(id).is_some())
                .map(|(id, _)| effect_card(id).unwrap()),
        );
        for page in pages {
            for line in page.lines() {
                assert!(line.chars().count() <= WIDTH, "too wide: {line}");
            }
        }
    }

    #[test]
    fn every_mapped_effect_has_a_page() {
        for (id, _) in sonido_registry::EFFECT_UIDS {
            if knob_map(id).is_some() {
                let page = effect_card(id).unwrap();
                assert!(page.contains("K6"), "{id}");
            }
        }
        assert!(effect_card("not-an-effect").is_err());
    }
}
