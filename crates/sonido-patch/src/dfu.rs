//! Firmware updates for the pedal over USB DFU (desktop hosts; runs `dfu-util`).
//!
//! The pedal restarts into its bootloader on the rig link's
//! [`Request::Bootloader`](crate::link::Request::Bootloader) (`sonido pedal
//! update`, the editor), or when both footswitches are held at power-on. The
//! Daisy bootloader then lists its QSPI flash at `0x90000000`, and firmware
//! goes to [`FIRMWARE_ADDR`]. Holding BOOT through RESET starts the chip's ROM
//! bootloader instead, which has the same USB id but cannot reach QSPI flash,
//! so it is not accepted. Rigs never travel this way: they go over the link
//! ([`crate::client`]), which checks them first.
//!
//! The parsing and argument-construction logic is pure and unit-tested; only
//! [`list_devices`] and [`flash_firmware`] start a process.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

/// USB id of the STM32 DFU bootloaders, as `dfu-util -l` prints it.
pub const DFU_ID: &str = "0483:df11";
/// Where firmware is flashed (the Daisy bootloader's application entry).
pub const FIRMWARE_ADDR: u32 = 0x9004_0000;
/// The QSPI flash region the Daisy bootloader lists.
const QSPI_REGION: &str = "0x90000000";

/// A device line parsed from `dfu-util -l`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DfuDevice {
    /// `vid:pid`, e.g. `"0483:df11"`.
    pub id: String,
    /// DFU alt-setting index, if present.
    pub alt: Option<u8>,
    /// Alt-setting name, e.g. `"@Flash /0x90000000/..."`.
    pub name: String,
}

/// DFU operation failures, mapped to actionable messages.
#[derive(Debug)]
#[non_exhaustive]
pub enum DfuError {
    /// `dfu-util` is not installed / not on PATH.
    NotInstalled,
    /// No pedal in its bootloader was found.
    NoDevice,
    /// `dfu-util` ran but exited non-zero (stderr captured).
    Failed(String),
    /// Spawning or I/O failed.
    Io(String),
}

impl std::fmt::Display for DfuError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotInstalled => write!(
                f,
                "dfu-util not found: install it (e.g. `sudo dnf install dfu-util`)"
            ),
            Self::NoDevice => write!(
                f,
                "no pedal in its bootloader: run `sonido pedal update`, or hold both \
                 footswitches while powering the pedal on"
            ),
            Self::Failed(e) => write!(f, "dfu-util failed: {e}"),
            Self::Io(e) => write!(f, "could not run dfu-util: {e}"),
        }
    }
}

impl std::error::Error for DfuError {}

/// Parse the output of `dfu-util -l` into the devices it lists.
///
/// `dfu-util -l` prints lines like:
/// ```text
/// Found DFU: [0483:df11] ver=0200, devnum=12, cfg=1, intf=0, path="...", alt=0, name="@Flash /0x90000000/64*4Kg/...", serial="..."
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

/// The alt setting the Daisy bootloader exposes its QSPI flash on, if it is
/// listed.
pub fn firmware_alt(devices: &[DfuDevice]) -> Option<u8> {
    devices
        .iter()
        .find(|d| d.id == DFU_ID && d.name.contains(QSPI_REGION))
        .and_then(|d| d.alt)
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
    let out = Command::new("dfu-util")
        .arg("-l")
        .output()
        .map_err(spawn_error)?;
    Ok(parse_dfu_list(&String::from_utf8_lossy(&out.stdout)))
}

/// Wait up to `timeout` for a pedal to appear in its bootloader; returns the
/// alt setting to flash firmware through.
pub fn wait_for_bootloader(timeout: Duration) -> Result<u8, DfuError> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(alt) = firmware_alt(&list_devices()?) {
            return Ok(alt);
        }
        if Instant::now() > deadline {
            return Err(DfuError::NoDevice);
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

/// Flash the firmware image at `bin` to a pedal waiting in its bootloader;
/// the pedal restarts into it.
pub fn flash_firmware(bin: &Path) -> Result<(), DfuError> {
    let alt = firmware_alt(&list_devices()?).ok_or(DfuError::NoDevice)?;
    let out = Command::new("dfu-util")
        .args(download_args(alt, FIRMWARE_ADDR, &bin.to_string_lossy()))
        .output()
        .map_err(spawn_error)?;
    if out.status.success() {
        Ok(())
    } else {
        Err(DfuError::Failed(
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        ))
    }
}

fn spawn_error(e: std::io::Error) -> DfuError {
    if e.kind() == std::io::ErrorKind::NotFound {
        DfuError::NotInstalled
    } else {
        DfuError::Io(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ROM bootloader (BOOT held through RESET): no QSPI region.
    const ROM: &str = r#"
Found DFU: [0483:df11] ver=0200, devnum=12, cfg=1, intf=0, path="1-2", alt=1, name="@Option Bytes   /0x5200201C/01*128 e", serial="200364500000"
Found DFU: [0483:df11] ver=0200, devnum=12, cfg=1, intf=0, path="1-2", alt=0, name="@Internal Flash   /0x08000000/16*128Kg", serial="200364500000"
"#;

    /// The Daisy bootloader: QSPI flash on alt 0.
    const DAISY: &str = r#"
dfu-util 0.11
Found DFU: [0483:df11] ver=0200, devnum=14, cfg=1, intf=0, path="1-2", alt=0, name="@Flash /0x90000000/64*4Kg/0x90040000/60*64Kg/0x90400000/64*64Kg", serial="200364500000"
"#;

    #[test]
    fn parses_devices() {
        let devices = parse_dfu_list(ROM);
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].id, DFU_ID);
        assert_eq!(devices[0].alt, Some(1));
        assert!(devices[1].name.contains("Internal Flash"));
    }

    #[test]
    fn only_the_daisy_bootloader_can_take_firmware() {
        assert_eq!(firmware_alt(&parse_dfu_list(DAISY)), Some(0));
        assert_eq!(firmware_alt(&parse_dfu_list(ROM)), None);
        assert_eq!(firmware_alt(&parse_dfu_list("no devices found")), None);
    }

    #[test]
    fn download_args_are_dfuse_leave() {
        let args = download_args(0, FIRMWARE_ADDR, "/tmp/fw.bin");
        assert_eq!(
            args,
            vec!["-a", "0", "-s", "0x90040000:leave", "-D", "/tmp/fw.bin"]
        );
    }
}
