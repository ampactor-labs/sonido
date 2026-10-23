//! The pedal's controls: what every footswitch press, toggle, knob and LED
//! does, as functions the firmware runs and `sonido pedal controls` draws.
//! Both call these, so the card can't drift from the pedal.
//!
//! The rules (ADR-032): the left footswitch is on/off, the right one is time,
//! and holding either changes rig; the knobs' top row is levels (K3 always a
//! volume) and the bottom row is the sound; each LED shows what the
//! footswitch below it does.

use sonido_core::{ParamDescriptor, ParamScale};

use crate::NUM_TOGGLES;
use crate::pedal::{JAM_SLOTS, Topology};

/// Rig bank slots the pedal keeps in flash.
pub const BANK_SLOTS: usize = 15;

/// A footswitch held this long is a hold, acted on while still down; a
/// shorter press is a tap, acted on at release.
pub const HOLD_MS: u32 = 400;

/// A pick knob's effect loads once its zone has held this long, so sweeping
/// past effects doesn't build each one.
pub const PICK_SETTLE_MS: u32 = 300;

/// T2: which sound plays, and which one the knobs edit in the jam.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Up: sound A.
    A,
    /// Middle: between A and B, where FS2 glides.
    Morph,
    /// Down: sound B.
    B,
}

impl Mode {
    /// From the T2 toggle position (0 = up, 1 = middle, 2 = down).
    pub fn from_toggle(position: u8) -> Self {
        match position {
            1 => Self::Morph,
            2 => Self::B,
            _ => Self::A,
        }
    }

    /// For people.
    pub fn label(self) -> &'static str {
        match self {
            Self::A => "A",
            Self::Morph => "Morph",
            Self::B => "B",
        }
    }
}

/// A toggle switch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Toggle {
    /// Left: the jam slot the knobs edit.
    T1,
    /// Middle: [`Mode`].
    T2,
    /// Right: the jam's routing ([`Topology`]).
    T3,
}

impl Toggle {
    /// Its place on the pedal, left to right (0, 1, 2), as the hardware reads
    /// it.
    pub fn index(self) -> usize {
        match self {
            Self::T1 => 0,
            Self::T2 => 1,
            Self::T3 => 2,
        }
    }

    /// For people: "T1", "T2", "T3".
    pub fn label(self) -> &'static str {
        match self {
            Self::T1 => "T1",
            Self::T2 => "T2",
            Self::T3 => "T3",
        }
    }
}

/// The toggles a rig assigns, in `Patch::toggles` order (T2 is always
/// [`Mode`]).
pub const RIG_TOGGLES: [Toggle; NUM_TOGGLES] = [Toggle::T1, Toggle::T3];

/// What a toggle does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToggleRole {
    /// The jam's T1: the slot the knobs edit ([`slot`]).
    Slot,
    /// T2, always: [`Mode`].
    Mode,
    /// The jam's T3: how its slots are wired ([`Topology`]).
    Routing,
    /// T1 or T3 in a rig: that rig's own toggle, `Patch::toggles[i]` (which
    /// does nothing if the rig leaves it unused).
    Rig(usize),
}

/// What `toggle` does in the jam or in a rig.
pub fn toggle_role(toggle: Toggle, jam: bool) -> ToggleRole {
    match (toggle, jam) {
        (Toggle::T2, _) => ToggleRole::Mode,
        (Toggle::T1, true) => ToggleRole::Slot,
        (Toggle::T3, true) => ToggleRole::Routing,
        (t, false) => ToggleRole::Rig(RIG_TOGGLES.iter().position(|&r| r == t).unwrap_or(0)),
    }
}

/// The jam slot (0-based) T1 at `position` selects.
pub fn slot(position: u8) -> usize {
    usize::from(position).min(JAM_SLOTS - 1)
}

/// A footswitch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Footswitch {
    /// Left.
    Fs1,
    /// Right.
    Fs2,
}

/// How a footswitch was pressed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Press {
    /// Shorter than [`HOLD_MS`], acted on at release.
    Tap,
    /// [`HOLD_MS`] or longer, acted on as it gets there.
    Hold,
}

/// What a footswitch press does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Turn the pedal on or off (bypass crossfades to dry).
    OnOff,
    /// Set the tempo from the taps.
    TapTempo,
    /// Glide to the other side of the morph, or turn back mid-glide.
    Glide,
    /// Play the previous rig in the list.
    PreviousRig,
    /// Play the next rig in the list.
    NextRig,
}

impl Action {
    /// For people.
    pub fn describe(self) -> &'static str {
        match self {
            Self::OnOff => "on/off",
            Self::TapTempo => "tap tempo",
            Self::Glide => "glide A ⇄ B",
            Self::PreviousRig => "previous rig",
            Self::NextRig => "next rig",
        }
    }
}

/// What pressing `footswitch` this way does with T2 at `mode`.
pub fn footswitch(footswitch: Footswitch, press: Press, mode: Mode) -> Action {
    match (footswitch, press) {
        (Footswitch::Fs1, Press::Tap) => Action::OnOff,
        (Footswitch::Fs1, Press::Hold) => Action::PreviousRig,
        (Footswitch::Fs2, Press::Tap) if mode == Mode::Morph => Action::Glide,
        (Footswitch::Fs2, Press::Tap) => Action::TapTempo,
        (Footswitch::Fs2, Press::Hold) => Action::NextRig,
    }
}

/// What the six knobs control.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KnobPage {
    /// The focused jam slot's effect, its A or B state (the effect's
    /// `knob_map` in `sonido_platform::knob_mapping`).
    Sound,
    /// The jam's setup page: [`SETUP_LEVELS`] on K1–K3, the three slots'
    /// effects on K4–K6 ([`pick_zone`]).
    Setup,
    /// The rig's six macros.
    Macros,
}

impl KnobPage {
    /// For people.
    pub fn describe(self) -> &'static str {
        match self {
            Self::Sound => "the T1 slot's effect, its A or B sound",
            Self::Setup => "the jam's setup page",
            Self::Macros => "the rig's six knobs",
        }
    }
}

/// Which page the knobs are on.
pub fn knob_page(jam: bool, mode: Mode) -> KnobPage {
    match (jam, mode) {
        (false, _) => KnobPage::Macros,
        (true, Mode::Morph) => KnobPage::Setup,
        (true, _) => KnobPage::Sound,
    }
}

/// The setup page's level knobs, K1–K3, with their unit for people:
/// input, morph time (seconds for a glide A ⇄ B), master. Noon is each
/// default.
pub const SETUP_LEVELS: [(ParamDescriptor, &str); 3] = [
    (
        ParamDescriptor::gain_db("Input", "Input", -12.0, 12.0, 0.0),
        "dB",
    ),
    (
        ParamDescriptor::custom("Morph Time", "Morph Time", 0.2, 5.0, 1.0)
            .with_scale(ParamScale::Logarithmic),
        "s",
    ),
    (
        ParamDescriptor::gain_db("Master", "Master", -60.0, 12.0, 0.0),
        "dB",
    ),
];

/// The zone a pick knob at `knob` (0–1) selects among `zones` equal zones
/// (0 is empty, then one per jam effect), holding `current` until the knob is
/// clearly past its edge so a knob resting on a border doesn't flicker
/// between two effects.
pub fn pick_zone(current: usize, knob: f32, zones: usize) -> usize {
    let x = knob * zones as f32;
    if current < zones && (current as f32 - 0.15..current as f32 + 1.15).contains(&x) {
        return current;
    }
    (x as usize).min(zones - 1)
}

/// What an LED shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LedShows {
    /// Steadily lit.
    Lit,
    /// Dark.
    Dark,
    /// Blinking a count: the rig's position after a change, or an error code.
    Count,
    /// A flash on every beat of the tempo.
    Beat,
    /// Where the morph is: dark at A, lit at B, flickering while it glides
    /// (a glide always runs to an end, so it rests only at A or B).
    MorphPosition,
}

impl LedShows {
    /// For people.
    pub fn describe(self) -> &'static str {
        match self {
            Self::Lit => "lit",
            Self::Dark => "dark",
            Self::Count => "blinks the rig's number (or an error code)",
            Self::Beat => "flashes the tempo",
            Self::MorphPosition => "dark at A, lit at B, flickers while gliding",
        }
    }
}

/// LED 1, over FS1 (on/off): lit while on, dark when off, and blinking the
/// rig's position after a rig change (or an error code).
pub fn led1(bypassed: bool, counting: bool) -> LedShows {
    match (counting, bypassed) {
        (true, _) => LedShows::Count,
        (false, true) => LedShows::Dark,
        (false, false) => LedShows::Lit,
    }
}

/// LED 2, over FS2 (time): the tempo, or the morph position where FS2 glides.
pub fn led2(bypassed: bool, mode: Mode) -> LedShows {
    match (bypassed, mode) {
        (true, _) => LedShows::Dark,
        (false, Mode::Morph) => LedShows::MorphPosition,
        (false, _) => LedShows::Beat,
    }
}

/// What holding footswitches while powering on does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerOn {
    /// Start the USB bootloader for a firmware update.
    Bootloader,
    /// Start in the jam.
    Jam,
    /// Play the rig played last.
    LastRig,
}

impl PowerOn {
    /// For people.
    pub fn describe(self) -> &'static str {
        match self {
            Self::Bootloader => "the bootloader, for a firmware update",
            Self::Jam => "start in the jam",
            Self::LastRig => "the rig you played last",
        }
    }
}

/// The power-on gesture for footswitches held as the pedal starts.
pub fn power_on(fs1_held: bool, fs2_held: bool) -> PowerOn {
    match (fs1_held, fs2_held) {
        (true, true) => PowerOn::Bootloader,
        (true, false) => PowerOn::Jam,
        _ => PowerOn::LastRig,
    }
}

impl Topology {
    /// For people.
    pub fn label(self) -> &'static str {
        match self {
            Self::Linear => "Linear",
            Self::Parallel => "Parallel",
            Self::Fan => "Fan",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn left_is_on_off_right_is_time_holds_change_rig() {
        for mode in [Mode::A, Mode::Morph, Mode::B] {
            assert_eq!(footswitch(Footswitch::Fs1, Press::Tap, mode), Action::OnOff);
            assert_eq!(
                footswitch(Footswitch::Fs1, Press::Hold, mode),
                Action::PreviousRig
            );
            assert_eq!(
                footswitch(Footswitch::Fs2, Press::Hold, mode),
                Action::NextRig
            );
        }
        assert_eq!(
            footswitch(Footswitch::Fs2, Press::Tap, Mode::A),
            Action::TapTempo
        );
        assert_eq!(
            footswitch(Footswitch::Fs2, Press::Tap, Mode::Morph),
            Action::Glide
        );
    }

    #[test]
    fn the_jam_owns_t1_and_t3_a_rig_assigns_them() {
        assert_eq!(toggle_role(Toggle::T1, true), ToggleRole::Slot);
        assert_eq!(toggle_role(Toggle::T3, true), ToggleRole::Routing);
        for jam in [true, false] {
            assert_eq!(toggle_role(Toggle::T2, jam), ToggleRole::Mode);
        }
        for (i, &t) in RIG_TOGGLES.iter().enumerate() {
            assert_eq!(toggle_role(t, false), ToggleRole::Rig(i));
        }
    }

    #[test]
    fn each_led_shows_its_footswitch() {
        // LED 1 is FS1's: on/off.
        assert_eq!(led1(false, false), LedShows::Lit);
        assert_eq!(led1(true, false), LedShows::Dark);
        // LED 2 is FS2's: the tempo, or the glide's morph position.
        assert_eq!(led2(false, Mode::A), LedShows::Beat);
        assert_eq!(led2(false, Mode::Morph), LedShows::MorphPosition);
    }

    #[test]
    fn pick_zones_hold_at_their_borders() {
        let zones = 9;
        assert_eq!(pick_zone(usize::MAX, 0.0, zones), 0);
        assert_eq!(pick_zone(usize::MAX, 1.0, zones), 8);
        // Resting just past the 3/4 border keeps zone 3; clearly past moves.
        let border = 4.0 / zones as f32;
        assert_eq!(pick_zone(3, border + 0.005, zones), 3);
        assert_eq!(pick_zone(3, border + 0.03, zones), 4);
    }
}
