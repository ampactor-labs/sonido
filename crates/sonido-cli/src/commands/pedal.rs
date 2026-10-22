//! `sonido pedal`: talk to a Sonido pedal over USB.
//!
//! The pedal is a USB serial device while it plays (`sonido_patch::link`).
//! These commands show and step its rig list, send rigs to its bank (checked
//! here with the CPU rule the pedal enforces, then again by the pedal), read
//! rigs and the jam back as editor JSON, and update its firmware.

use std::path::PathBuf;
use std::time::Duration;

use clap::{Args, Subcommand};
use sonido_patch::client::open_pedal;
use sonido_patch::dfu;
use sonido_patch::pedal::RigRef;
use sonido_patch::{Patch, link::Info};
use sonido_registry::effect_by_uid;

use super::patch::{RigSource, check_for_pedal, print_rig};

/// The pedal over USB.
#[derive(Args)]
pub struct PedalArgs {
    /// Serial port (default: the first Sonido pedal found).
    #[arg(long, global = true)]
    port: Option<String>,
    #[command(subcommand)]
    command: PedalCommand,
}

/// Subcommands for `sonido pedal`.
#[derive(Subcommand)]
enum PedalCommand {
    /// How to play the pedal: every control, drawn from this build's source
    /// (no pedal needed). Give an effect for its knobs with their ranges.
    Controls {
        /// An effect, e.g. `distortion`.
        effect: Option<String>,
    },
    /// Firmware, CPU load, and what the pedal is playing.
    Info,
    /// The rig list, in the order the footswitches step through it.
    List,
    /// Send a rig to the bank: it replaces the rig with the same name, or
    /// takes the first empty slot (or --slot). The pedal plays it at once.
    Push {
        #[command(flatten)]
        rig: RigSource,
        /// Bank slot 1–15 instead of the name rule.
        #[arg(short, long)]
        slot: Option<u8>,
    },
    /// Save a bank rig (`1`–`15`) or the jam (`jam`) as editor JSON.
    Pull {
        /// `jam` or a bank slot, 1–15.
        what: String,
        /// Output `.json` file (prints the rig without one).
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Empty bank slot 1–15.
    Erase {
        /// Bank slot, 1–15.
        slot: u8,
    },
    /// Play a rig: `jam`, a bank slot `1`–`15`, or a factory rig by name.
    Play {
        /// What to play.
        rig: String,
    },
    /// Restart the pedal into its bootloader; with a firmware `.bin`, flash
    /// it with dfu-util (the pedal then restarts into it).
    Update {
        /// Firmware image (`sonido_pedal.bin`).
        firmware: Option<PathBuf>,
    },
}

/// Run `sonido pedal`.
pub fn run(args: PedalArgs) -> anyhow::Result<()> {
    let open = || -> anyhow::Result<_> { Ok(open_pedal(args.port.as_deref())?) };
    match args.command {
        PedalCommand::Controls { effect } => match effect {
            Some(id) => print!("{}", super::pedal_card::effect_card(&id)?),
            None => print!("{}", super::pedal_card::card()),
        },
        PedalCommand::Info => {
            let mut link = open()?;
            let info = link.info()?;
            let status = link.status()?;
            println!(
                "Firmware:  {} (link protocol {})",
                info.firmware, info.protocol
            );
            println!(
                "Playing:   {}{}",
                rig_label(status.active, &info),
                if status.bypassed { " (off)" } else { "" }
            );
            println!(
                "Morph:     {}% of the way to B",
                u32::from(status.morph) * 100 / 255
            );
            println!(
                "CPU:       {:.1}% mean, {:.1}% peak",
                f32::from(status.load_mean) / 10.0,
                f32::from(status.load_peak) / 10.0
            );
            println!(
                "Capacity:  {} bank slots, {} factory rigs, {} effects",
                info.slot_count,
                info.factory_count,
                info.effects.len()
            );
        }
        PedalCommand::List => {
            let mut link = open()?;
            let info = link.info()?;
            let jam = link.read_jam()?;
            let mut position = 1;
            let playing = |r: RigRef| {
                if r == info.active {
                    "  ◀ playing"
                } else {
                    ""
                }
            };
            println!(
                "{position:>3}  Jam            {}{}",
                effects(&jam),
                playing(RigRef::Jam)
            );
            for slot in 0..info.slot_count {
                match link.read_slot(slot)? {
                    Some(rig) => {
                        position += 1;
                        println!(
                            "{position:>3}  Bank {:<2}        {}{}",
                            slot + 1,
                            rig.name,
                            playing(RigRef::Bank(slot))
                        );
                    }
                    None => println!("     Bank {:<2}        (empty)", slot + 1),
                }
            }
            let names = factory_names();
            for i in 0..info.factory_count {
                position += 1;
                let name = names.get(usize::from(i)).map_or("?", String::as_str);
                println!(
                    "{position:>3}  Factory {:<2}     {name}{}",
                    i + 1,
                    playing(RigRef::Factory(i))
                );
            }
        }
        PedalCommand::Push { rig, slot } => {
            let patch = rig.load()?;
            check_for_pedal(&patch)?;
            let mut link = open()?;
            let slot = match slot {
                Some(n) => {
                    let slot = bank_slot(n)?;
                    link.write_slot(slot, &patch)?;
                    slot
                }
                None => link.send_rig(&patch)?,
            };
            println!(
                "Sent '{}' to bank slot {}; the pedal is playing it.",
                patch.name,
                slot + 1
            );
        }
        PedalCommand::Pull { what, output } => {
            let mut link = open()?;
            let patch = if what.eq_ignore_ascii_case("jam") {
                link.read_jam()?
            } else {
                let n: u8 = what.parse().map_err(|_| {
                    anyhow::anyhow!("expected `jam` or a bank slot 1–15, got '{what}'")
                })?;
                link.read_slot(bank_slot(n)?)?
                    .ok_or_else(|| anyhow::anyhow!("bank slot {n} is empty"))?
            };
            match output {
                Some(path) => {
                    std::fs::write(&path, serde_json::to_vec_pretty(&patch)?)?;
                    println!("Saved '{}' to {}", patch.name, path.display());
                }
                None => print_rig(&patch),
            }
        }
        PedalCommand::Erase { slot: n } => {
            open()?.erase_slot(bank_slot(n)?)?;
            println!("Bank slot {n} is empty.");
        }
        PedalCommand::Play { rig } => {
            let target = parse_rig(&rig)?;
            let mut link = open()?;
            link.activate(target)?;
            let info = link.info()?;
            println!("Playing {}.", rig_label(target, &info));
        }
        PedalCommand::Update { firmware } => {
            // Already there (both footswitches held at power-on)? Then there
            // is no serial port to ask.
            let waiting =
                dfu::list_devices().is_ok_and(|devices| dfu::firmware_alt(&devices).is_some());
            if !waiting {
                open()?.enter_bootloader()?;
                println!("The pedal is restarting into its bootloader.");
            }
            let Some(bin) = firmware else {
                println!(
                    "Flash it with: dfu-util -a 0 -s 0x{:08X}:leave -D sonido_pedal.bin",
                    dfu::FIRMWARE_ADDR
                );
                return Ok(());
            };
            dfu::wait_for_bootloader(Duration::from_secs(10))?;
            println!("Flashing {}…", bin.display());
            dfu::flash_firmware(&bin)?;
            println!("Done: the pedal is restarting into the new firmware.");
        }
    }
    Ok(())
}

/// 0-based bank slot from a 1-based one.
fn bank_slot(n: u8) -> anyhow::Result<u8> {
    if (1..=15).contains(&n) {
        Ok(n - 1)
    } else {
        anyhow::bail!("bank slots are 1–15, got {n}")
    }
}

/// `jam`, a bank slot `1`–`15`, or a factory rig by (case-insensitive) name.
fn parse_rig(s: &str) -> anyhow::Result<RigRef> {
    if s.eq_ignore_ascii_case("jam") {
        return Ok(RigRef::Jam);
    }
    if let Ok(n) = s.parse::<u8>() {
        return Ok(RigRef::Bank(bank_slot(n)?));
    }
    let names = factory_names();
    names
        .iter()
        .position(|n| n.eq_ignore_ascii_case(s))
        .map(|i| RigRef::Factory(i as u8))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no rig '{s}': use `jam`, a bank slot 1–15, or one of: {}",
                names.join(", ")
            )
        })
}

/// The factory rigs' names, as this build of Sonido (and the firmware built
/// with it) has them.
fn factory_names() -> Vec<String> {
    sonido_rigs::factory_rigs()
        .into_iter()
        .map(|r| r.name)
        .collect()
}

fn rig_label(rig: RigRef, info: &Info) -> String {
    match rig {
        RigRef::Jam => "the jam".into(),
        RigRef::Bank(n) => format!("bank slot {}", n + 1),
        RigRef::Factory(n) => factory_names()
            .get(usize::from(n))
            .filter(|_| n < info.factory_count)
            .map_or_else(
                || format!("factory rig {}", n + 1),
                |name| format!("'{name}'"),
            ),
    }
}

/// "distortion · delay · reverb", or "(empty)".
fn effects(patch: &Patch) -> String {
    if patch.nodes.is_empty() {
        return "(empty)".into();
    }
    patch
        .nodes
        .iter()
        .map(|n| effect_by_uid(n.effect_uid).unwrap_or("?"))
        .collect::<Vec<_>>()
        .join(" · ")
}
