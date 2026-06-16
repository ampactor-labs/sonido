//! DFU flashing helper for the Daisy Seed (native only).
//!
//! Patches and firmware reach the pedal as data over `dfu-util`. The pedal must
//! be in its STM32 bootloader (VID:PID `0483:df11`) first — hold both
//! footswitches ~1.5 s, or press BOOT+RESET on the Seed.
//!
//! The parsing and argument-construction logic is split out as pure functions so
//! it is unit-tested without spawning anything; only [`flash_patch`]/[`flash_firmware`]
//! touch a process.

#![cfg(not(target_arch = "wasm32"))]

use std::process::Command;

use sonido_patch::SECTOR_SIZE;

/// STM32 system-bootloader USB id, as `dfu-util -l` prints it.
pub const STM_BOOTLOADER_ID: &str = "0483:df11";

/// Flash base address mapped by the Daisy bootloader (QSPI XIP base).
pub const FLASH_BASE: u32 = 0x9000_0000;
/// Where firmware is flashed (Electrosmith bootloader entry point).
pub const FIRMWARE_ADDR: u32 = 0x9004_0000;
/// First patch slot's DFU address (mirrors `qspi_flash::PATCH_BANK_ADDR`).
pub const PATCH_BANK_ADDR: u32 = 0x907F_0000;

/// A device line parsed from `dfu-util -l`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DfuDevice {
    /// `vid:pid`, e.g. `"0483:df11"`.
    pub id: String,
    /// DFU alt-setting index, if present.
    pub alt: Option<u8>,
    /// Alt-setting name, e.g. `"@Internal Flash  /0x08000000/..."`.
    pub name: String,
}

impl DfuDevice {
    /// Whether this is the STM32 system bootloader.
    pub fn is_bootloader(&self) -> bool {
        self.id == STM_BOOTLOADER_ID
    }
}

/// DFU operation failures, mapped to actionable messages.
#[derive(Debug)]
#[non_exhaustive]
pub enum DfuError {
    /// `dfu-util` is not installed / not on PATH.
    NotInstalled,
    /// No device in bootloader mode was found.
    NoDevice,
    /// `dfu-util` ran but exited non-zero (stderr captured).
    Failed(String),
    /// Spawning or I/O failed.
    Io(String),
    /// The patch payload was not a single sector.
    BadPayload,
}

impl std::fmt::Display for DfuError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotInstalled => write!(
                f,
                "dfu-util not found — install it (e.g. `sudo dnf install dfu-util`)"
            ),
            Self::NoDevice => write!(
                f,
                "no pedal in bootloader mode — hold both footswitches ~1.5 s \
                 (or press BOOT+RESET), then retry"
            ),
            Self::Failed(e) => write!(f, "dfu-util failed: {e}"),
            Self::Io(e) => write!(f, "could not run dfu-util: {e}"),
            Self::BadPayload => write!(
                f,
                "patch payload must be exactly one {SECTOR_SIZE}-byte sector"
            ),
        }
    }
}

impl std::error::Error for DfuError {}

/// DFU address for patch slot `n`.
pub const fn patch_slot_addr(slot: u8) -> u32 {
    PATCH_BANK_ADDR + (slot as u32) * SECTOR_SIZE as u32
}

/// Parse the output of `dfu-util -l` into the devices it lists.
///
/// `dfu-util -l` prints lines like:
/// ```text
/// Found DFU: [0483:df11] ver=2200, devnum=12, cfg=1, intf=0, path="...", alt=0, name="@Internal Flash  /0x08000000/...", serial="..."
/// ```
pub fn parse_dfu_list(output: &str) -> Vec<DfuDevice> {
    let mut devices = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if !line.starts_with("Found DFU:") && !line.starts_with("Found Runtime:") {
            continue;
        }
        let Some(id) = between(line, "[", "]") else {
            continue;
        };
        let alt = field(line, "alt=").and_then(|s| s.parse::<u8>().ok());
        let name = between(line, "name=\"", "\"").unwrap_or_default();
        devices.push(DfuDevice {
            id: id.to_owned(),
            alt,
            name: name.to_owned(),
        });
    }
    devices
}

/// Whether any listed device is the STM bootloader.
pub fn bootloader_present(devices: &[DfuDevice]) -> bool {
    devices.iter().any(DfuDevice::is_bootloader)
}

/// Build the `dfu-util` argument vector for a DfuSe download to `address`.
///
/// `:leave` makes the device reset and run the new image after flashing.
pub fn download_args(alt: u8, address: u32, path: &str) -> Vec<String> {
    vec![
        "-a".into(),
        alt.to_string(),
        "-s".into(),
        format!("0x{address:08X}:leave"),
        "-D".into(),
        path.into(),
    ]
}

fn between<'a>(s: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let i = s.find(start)? + start.len();
    let rest = &s[i..];
    let j = rest.find(end)?;
    Some(&rest[..j])
}

fn field<'a>(s: &'a str, key: &str) -> Option<&'a str> {
    let i = s.find(key)? + key.len();
    let rest = &s[i..];
    let end = rest.find([',', ' ']).unwrap_or(rest.len());
    Some(&rest[..end])
}

// ── Process-touching surface (thin; not unit-tested) ─────────────────────────

/// List DFU devices by running `dfu-util -l`.
pub fn list_devices() -> Result<Vec<DfuDevice>, DfuError> {
    let out = Command::new("dfu-util").arg("-l").output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            DfuError::NotInstalled
        } else {
            DfuError::Io(e.to_string())
        }
    })?;
    Ok(parse_dfu_list(&String::from_utf8_lossy(&out.stdout)))
}

/// Whether a pedal is currently in bootloader mode.
pub fn pedal_in_bootloader() -> bool {
    list_devices()
        .map(|d| bootloader_present(&d))
        .unwrap_or(false)
}

/// Flash one patch sector to slot `slot`, returning on success.
///
/// `payload` must be exactly one [`SECTOR_SIZE`] sector. Writes it to a temp
/// file and invokes `dfu-util`.
pub fn flash_patch(slot: u8, payload: &[u8]) -> Result<(), DfuError> {
    if payload.len() != SECTOR_SIZE {
        return Err(DfuError::BadPayload);
    }
    if !pedal_in_bootloader() {
        return Err(DfuError::NoDevice);
    }
    let mut path = std::env::temp_dir();
    path.push(format!("sonido_patch_slot{slot}.bin"));
    std::fs::write(&path, payload).map_err(|e| DfuError::Io(e.to_string()))?;
    let args = download_args(0, patch_slot_addr(slot), &path.to_string_lossy());
    run(&args)
}

/// Flash a firmware image file at [`FIRMWARE_ADDR`].
pub fn flash_firmware(bin_path: &str) -> Result<(), DfuError> {
    if !pedal_in_bootloader() {
        return Err(DfuError::NoDevice);
    }
    let args = download_args(0, FIRMWARE_ADDR, bin_path);
    run(&args)
}

fn run(args: &[String]) -> Result<(), DfuError> {
    let out = Command::new("dfu-util").args(args).output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            DfuError::NotInstalled
        } else {
            DfuError::Io(e.to_string())
        }
    })?;
    if out.status.success() {
        Ok(())
    } else {
        Err(DfuError::Failed(
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
dfu-util 0.11

Copyright 2005-2009 Weston Schmidt, Harald Welte and OpenMoko Inc.
Found DFU: [0483:df11] ver=2200, devnum=12, cfg=1, intf=0, path="1-2", alt=0, name="@Internal Flash  /0x08000000/16*128Kg", serial="200364500000"
Found DFU: [0483:df11] ver=2200, devnum=12, cfg=1, intf=0, path="1-2", alt=1, name="@QSPI Flash  /0x90000000/512*4Kg", serial="200364500000"
"#;

    #[test]
    fn parses_bootloader_devices() {
        let devices = parse_dfu_list(SAMPLE);
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].id, "0483:df11");
        assert_eq!(devices[0].alt, Some(0));
        assert!(devices[0].name.contains("Internal Flash"));
        assert_eq!(devices[1].alt, Some(1));
        assert!(bootloader_present(&devices));
    }

    #[test]
    fn no_bootloader_when_absent() {
        let devices = parse_dfu_list("Found DFU: [1234:5678] alt=0, name=\"x\"");
        assert!(!bootloader_present(&devices));
    }

    #[test]
    fn empty_output_yields_nothing() {
        assert!(parse_dfu_list("no devices found").is_empty());
    }

    #[test]
    fn patch_slot_addresses() {
        assert_eq!(patch_slot_addr(0), 0x907F_0000);
        assert_eq!(patch_slot_addr(1), 0x907F_1000);
        assert_eq!(patch_slot_addr(7), 0x907F_7000);
    }

    #[test]
    fn download_args_are_dfuse_leave() {
        let args = download_args(0, 0x907F_1000, "/tmp/p.bin");
        assert_eq!(
            args,
            vec!["-a", "0", "-s", "0x907F1000:leave", "-D", "/tmp/p.bin"]
        );
    }
}
