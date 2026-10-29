//! grid_pedal: a guitar pedal that measures the frequency of the power grid
//! and reports it to a computer over USB.
//!
//! The pedal is a Cleveland Music Co. Hothouse with a Daisy Seed inside. The
//! mains wiring in a room gives off a hum at the grid's frequency, close to
//! 60 Hz in North America. A guitar pickup, or a guitar cable with nothing
//! plugged into its other end, picks up that hum. The pedal measures the
//! hum's frequency with `sonido_core::grid::GridEar`, the same code that HUM
//! uses when it listens through a computer's audio input. The audio passes
//! through unchanged, so the pedal can stay in a guitar's signal chain.
//!
//! Over its one USB cable, the pedal shows up on the computer as both a serial
//! port and a keyboard.
//!
//! While a program has the serial port open, the pedal writes one line of JSON
//! to it every 50 ms:
//!
//!   {"hz_fast":59.9810,"hz":59.9830,"rocof":-0.004,"level_db":-18.5,"harmonic":1,"source":"pedal"}
//!
//! `hz_fast` is the frequency over the last 100 ms, and `hz` is the frequency
//! over the last second, both in Hz. `rocof` is how fast the frequency is
//! changing, in Hz per second. `level_db` is the hum's level in dB relative to
//! full scale. `harmonic` says which harmonic of the hum the pedal follows: 1,
//! 2 or 3. Until the pedal locks onto the hum, and whenever it loses the hum,
//! `hz_fast`, `hz` and `rocof` are `null`. The HUM director reads these lines,
//! so their format must not change. Section C5 of `CONTRACTS.md` in the HUM
//! repository (https://github.com/ampactor-labs/hum) defines the format.
//!
//! The keyboard types one letter when a control moves. The HUM page acts on
//! each letter:
//!
//! - Footswitch 1 types `e`. On `e`, the page replays a recorded grid event.
//! - Footswitch 2 types `i`. On `i`, the page cuts the hum to silence, and a
//!   second `i` brings it back.
//! - Toggle 1 types `h` when it moves into the down position, and types `h`
//!   again when it moves out of it. Up and middle count as the same position,
//!   so moving between them types nothing. On `h`, the page switches its clock
//!   between now and tomorrow's rush hour.
//!
//! A footswitch types its letter once per press, after it has been down for
//! about 30 ms. Holding it down types nothing more. The toggle's new position
//! counts after the toggle has rested there for about 30 ms. Plugging the
//! pedal in types nothing, whatever positions the controls are in.
//!
//! The two LEDs show the reading. LED 1 blinks once every two seconds while
//! the frequency is within 0.036 Hz of 60 Hz. It blinks faster as the
//! frequency moves further away, up to four times a second at 0.236 Hz away.
//! LED 2 lights while the frequency changes faster than 0.1 Hz per second.
//! Both LEDs stay lit while the pedal hears no hum.
//!
//! To build the firmware and flash it over USB, run these commands in
//! `crates/sonido-daisy`. `cargo objcopy` comes from cargo-binutils.
//!
//!   cargo objcopy --example grid_pedal --release -- -O binary -R .sram1_bss grid_pedal.bin
//!   dfu-util -a 0 -s 0x90040000:leave -D grid_pedal.bin
//!
//! `dfu-util` writes the firmware through the Daisy bootloader, which listens
//! for only 2.5 seconds after a reset. Press RESET on the Seed, and start
//! `dfu-util` while the Seed's LED pulses. To make the bootloader wait longer,
//! press BOOT while the LED pulses. Do not hold BOOT while you press RESET:
//! that starts the chip's built-in bootloader instead, and it cannot write to
//! the flash memory that holds this firmware. `docs/EMBEDDED.md` explains the
//! bootloader in full.
//!
//! To read the lines, run `./run.sh pedal /dev/ttyACM0` in the HUM repository,
//! or run `cat /dev/ttyACM0`.

#![no_std]
#![no_main]

use core::convert::Infallible;
use core::fmt::Write;
use core::sync::atomic::{AtomicI32, AtomicU32, Ordering};

use defmt_rtt as _;
use embassy_executor::Spawner;
use embassy_stm32 as hal;
use embassy_stm32::usb::Driver;
use embassy_stm32::{bind_interrupts, peripherals, usb};
use embassy_time::{Duration, Instant, Timer};
use embassy_usb::class::cdc_acm::{CdcAcmClass, State};
use embassy_usb::class::hid::{self, HidWriter, ReportId, RequestHandler};
use embassy_usb::control::OutResponse;
use embassy_usb::driver::EndpointError;
use embedded_alloc::LlffHeap as Heap;
use panic_probe as _;
use static_cell::StaticCell;

use sonido_core::grid::GridEar;
use sonido_daisy::controls::HothouseBuffer;
use sonido_daisy::hothouse::{POLL_INTERVAL_MS, hothouse_control_task};
use sonido_daisy::{
    BLOCK_SIZE, BufWriter, ClockProfile, SAMPLE_RATE, heartbeat, led::UserLed, u24_to_f32, usb_task,
};

#[global_allocator]
static HEAP: Heap = Heap::empty();

bind_interrupts!(struct Irqs {
    OTG_FS => usb::InterruptHandler<peripherals::USB_OTG_FS>;
});

static CONTROLS: HothouseBuffer = HothouseBuffer::new();

// ── The reading, published by the audio callback and read by the serial task ──
// Fixed point so the serial line formats with integers only: no float
// formatting is linked into the image.

/// Frequency over the last 100 ms, ×10000. Valid when VALID is 1.
static HZ_FAST_X1E4: AtomicI32 = AtomicI32::new(0);
/// Frequency over the last second, ×10000.
static HZ_X1E4: AtomicI32 = AtomicI32::new(0);
/// Rate of change over 300 ms, ×1000 (mHz per second).
static ROCOF_X1E3: AtomicI32 = AtomicI32::new(0);
/// The tracked band's level in dBFS, ×10.
static LEVEL_X10: AtomicI32 = AtomicI32::new(-1200);
/// The harmonic tracked: 1, 2 or 3.
static HARMONIC: AtomicU32 = AtomicU32::new(1);
/// 1 when the ear has a reading, 0 while it listens or the hum is gone.
static VALID: AtomicU32 = AtomicU32::new(0);
/// Counts ticks, so the serial task writes one line per tick and never repeats one.
static SEQ: AtomicU32 = AtomicU32::new(0);

/// Blocks per tick: 50 ms at 48 kHz in 32-sample blocks is 75 blocks exactly.
const BLOCKS_PER_TICK: u32 = (SAMPLE_RATE as u32 / 20) / BLOCK_SIZE as u32;
const NOMINAL_HZ: f32 = 60.0;
const DEADBAND: f32 = 0.036;
const FULL_AT: f32 = 0.200;
const ROCOF_ENTER: f32 = 0.10;

static EP_OUT_BUF: StaticCell<[u8; 256]> = StaticCell::new();
static CONFIG_DESC: StaticCell<[u8; 256]> = StaticCell::new();
static BOS_DESC: StaticCell<[u8; 256]> = StaticCell::new();
static MSOS_DESC: StaticCell<[u8; 256]> = StaticCell::new();
static CONTROL_BUF: StaticCell<[u8; 64]> = StaticCell::new();
static CDC_STATE: StaticCell<State<'static>> = StaticCell::new();
static HID_STATE: StaticCell<hid::State<'static>> = StaticCell::new();
static HID_HANDLER: StaticCell<KeyboardLights> = StaticCell::new();

/// Writes a fixed-point integer as a decimal with `decimals` places.
fn write_fixed(w: &mut BufWriter<'_>, value: i32, decimals: u32) {
    let scale = 10i32.pow(decimals);
    let sign = if value < 0 { "-" } else { "" };
    let abs = value.unsigned_abs();
    let whole = abs / scale as u32;
    let frac = abs % scale as u32;
    match decimals {
        4 => {
            let _ = write!(w, "{sign}{whole}.{frac:04}");
        }
        3 => {
            let _ = write!(w, "{sign}{whole}.{frac:03}");
        }
        1 => {
            let _ = write!(w, "{sign}{whole}.{frac:01}");
        }
        _ => {
            let _ = write!(w, "{sign}{whole}");
        }
    }
}

/// One C5 line per tick over USB serial.
#[embassy_executor::task]
async fn line_task(mut class: CdcAcmClass<'static, Driver<'static, peripherals::USB_OTG_FS>>) {
    let mut buf = [0u8; 192];
    let mut last_seq = 0u32;
    loop {
        class.wait_connection().await;
        defmt::info!("grid_pedal: serial connected");
        loop {
            embassy_time::Timer::after_millis(10).await;
            let seq = SEQ.load(Ordering::Relaxed);
            if seq == last_seq {
                continue;
            }
            last_seq = seq;
            let valid = VALID.load(Ordering::Relaxed) == 1;
            let mut w = BufWriter::new(&mut buf);
            let _ = write!(w, "{{\"hz_fast\":");
            if valid {
                write_fixed(&mut w, HZ_FAST_X1E4.load(Ordering::Relaxed), 4);
            } else {
                let _ = write!(w, "null");
            }
            let _ = write!(w, ",\"hz\":");
            if valid {
                write_fixed(&mut w, HZ_X1E4.load(Ordering::Relaxed), 4);
            } else {
                let _ = write!(w, "null");
            }
            let _ = write!(w, ",\"rocof\":");
            if valid {
                write_fixed(&mut w, ROCOF_X1E3.load(Ordering::Relaxed), 3);
            } else {
                let _ = write!(w, "null");
            }
            let _ = write!(w, ",\"level_db\":");
            write_fixed(&mut w, LEVEL_X10.load(Ordering::Relaxed), 1);
            let _ = write!(
                w,
                ",\"harmonic\":{},\"source\":\"pedal\"}}\r\n",
                HARMONIC.load(Ordering::Relaxed)
            );
            let len = w.pos;
            let mut ok = true;
            for chunk in buf[..len].chunks(64) {
                if class.write_packet(chunk).await.is_err() {
                    ok = false;
                    break;
                }
            }
            if !ok {
                defmt::warn!("grid_pedal: serial disconnected");
                break;
            }
        }
    }
}

// ── The keyboard: footswitch 1, footswitch 2 and toggle 1 type keys ──

/// A boot keyboard report is 8 bytes: one byte for modifier keys such as
/// Shift, one reserved byte, and six bytes for up to six keys held down.
const REPORT_LEN: usize = 8;

/// The standard boot keyboard report descriptor, from appendix E.6 of the USB
/// HID specification, version 1.11. It tells the computer the layout of the
/// 8-byte report above. It also declares a 1-byte report that the computer
/// sends to set the keyboard's Num Lock and Caps Lock lights.
const KEYBOARD_REPORT_DESCRIPTOR: &[u8] = &[
    0x05, 0x01, // Usage Page (Generic Desktop)
    0x09, 0x06, // Usage (Keyboard)
    0xA1, 0x01, // Collection (Application)
    0x05, 0x07, //   Usage Page (Keyboard)
    0x19, 0xE0, //   Usage Minimum (Left Control)
    0x29, 0xE7, //   Usage Maximum (Right GUI)
    0x15, 0x00, //   Logical Minimum (0)
    0x25, 0x01, //   Logical Maximum (1)
    0x75, 0x01, //   Report Size (1)
    0x95, 0x08, //   Report Count (8)
    0x81, 0x02, //   Input (Data, Variable, Absolute): the modifier byte
    0x95, 0x01, //   Report Count (1)
    0x75, 0x08, //   Report Size (8)
    0x81, 0x01, //   Input (Constant): the reserved byte
    0x95, 0x05, //   Report Count (5)
    0x75, 0x01, //   Report Size (1)
    0x05, 0x08, //   Usage Page (LEDs)
    0x19, 0x01, //   Usage Minimum (Num Lock)
    0x29, 0x05, //   Usage Maximum (Kana)
    0x91, 0x02, //   Output (Data, Variable, Absolute): five light bits
    0x95, 0x01, //   Report Count (1)
    0x75, 0x03, //   Report Size (3)
    0x91, 0x01, //   Output (Constant): three padding bits
    0x95, 0x06, //   Report Count (6)
    0x75, 0x08, //   Report Size (8)
    0x15, 0x00, //   Logical Minimum (0)
    0x25, 0x65, //   Logical Maximum (101)
    0x05, 0x07, //   Usage Page (Keyboard)
    0x19, 0x00, //   Usage Minimum (0)
    0x29, 0x65, //   Usage Maximum (101)
    0x81, 0x00, //   Input (Data, Array): the six key bytes
    0xC0, // End Collection
];

/// Key codes from the keyboard page of the USB HID Usage Tables.
const KEY_E: u8 = 0x08;
const KEY_H: u8 = 0x0B;
const KEY_I: u8 = 0x0C;

/// `read_toggle` returns 2 while a toggle is down (see `hothouse::decode_toggle`).
const TOGGLE_DOWN: u8 = 2;

/// How long a control must read the same before its new position counts.
/// When a switch moves, its contacts bounce for a few milliseconds, which is
/// much less than this.
const SETTLE: Duration = Duration::from_millis(30);

/// The keyboard function of the USB device.
type Keyboard = HidWriter<'static, Driver<'static, peripherals::USB_OTG_FS>, REPORT_LEN>;

/// Answers the computer's report requests to the keyboard. The computer sends
/// each keyboard a report that sets its Num Lock and Caps Lock lights. The
/// pedal has no such lights, so it accepts the report and ignores it. If the
/// pedal refused it, the computer would see a failed request.
struct KeyboardLights;

impl RequestHandler for KeyboardLights {
    fn set_report(&mut self, id: ReportId, _data: &[u8]) -> OutResponse {
        match id {
            ReportId::Out(_) => OutResponse::Accepted,
            ReportId::In(_) | ReportId::Feature(_) => OutResponse::Rejected,
        }
    }
}

/// One control's position. A change counts only after every reading for
/// `SETTLE` has agreed on it. Contact bounce flips the reading back and forth
/// for a few milliseconds, so it does not count as a move.
struct Debounced<T> {
    /// The position the pedal acts on.
    position: T,
    /// The latest reading.
    reading: T,
    /// When the readings started to agree on `reading`.
    since: Instant,
}

impl<T: Copy + PartialEq> Debounced<T> {
    /// Starts at `position`, as if the control had rested there since `now`.
    fn new(position: T, now: Instant) -> Self {
        Self {
            position,
            reading: position,
            since: now,
        }
    }

    /// Takes the reading made at `now`. Returns the new position when a change
    /// counts, and `None` otherwise.
    fn update(&mut self, reading: T, now: Instant) -> Option<T> {
        if reading != self.reading {
            self.reading = reading;
            self.since = now;
        }
        if reading == self.position || now - self.since < SETTLE {
            return None;
        }
        self.position = reading;
        Some(reading)
    }
}

/// Types one key: a report that holds the key down, then a report that
/// releases every key. Each write waits until the computer has read the
/// previous report, so the computer sees both a press and a release.
async fn type_key(keyboard: &mut Keyboard, key: u8) -> Result<(), EndpointError> {
    keyboard.write(&[0, 0, key, 0, 0, 0, 0, 0]).await?;
    keyboard.write(&[0; REPORT_LEN]).await
}

/// Reads the controls as often as the control task updates them, and types a
/// key for each change that counts. Runs until a report fails to send, which
/// means the computer has reset or dropped the USB connection.
async fn type_control_changes(keyboard: &mut Keyboard) -> Result<Infallible, EndpointError> {
    // The positions at connection are the starting point, so plugging the
    // pedal in types nothing.
    let now = Instant::now();
    let mut foot1 = Debounced::new(CONTROLS.read_footswitch(0), now);
    let mut foot2 = Debounced::new(CONTROLS.read_footswitch(1), now);
    let mut toggle1_down = Debounced::new(CONTROLS.read_toggle(0) == TOGGLE_DOWN, now);
    loop {
        Timer::after_millis(POLL_INTERVAL_MS).await;
        let now = Instant::now();
        // A footswitch types when its press counts. Its release types nothing.
        if foot1.update(CONTROLS.read_footswitch(0), now) == Some(true) {
            type_key(keyboard, KEY_E).await?;
        }
        if foot2.update(CONTROLS.read_footswitch(1), now) == Some(true) {
            type_key(keyboard, KEY_I).await?;
        }
        // The toggle types when it moves into down and when it moves out.
        let down = CONTROLS.read_toggle(0) == TOGGLE_DOWN;
        if toggle1_down.update(down, now).is_some() {
            type_key(keyboard, KEY_H).await?;
        }
    }
}

/// Types keys for the footswitches and toggle 1 while a computer has the
/// keyboard set up.
#[embassy_executor::task]
async fn keys_task(mut keyboard: Keyboard) {
    loop {
        // Wait until the computer has set up the keyboard.
        keyboard.ready().await;
        defmt::info!("grid_pedal: keyboard connected");
        let _ = type_control_changes(&mut keyboard).await;
        defmt::warn!("grid_pedal: keyboard disconnected");
    }
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    sonido_daisy::enable_d2_sram();
    unsafe {
        HEAP.init(0x3000_8000, 256 * 1024);
    }
    let config = sonido_daisy::rcc_config(ClockProfile::Performance);
    let p = hal::init(config);

    let led = UserLed::new(p.PC7);
    spawner.spawn(heartbeat(led)).unwrap();

    let ctrl = sonido_daisy::hothouse_pins!(p);
    spawner
        .spawn(hothouse_control_task(ctrl, &CONTROLS))
        .unwrap();

    let driver = Driver::new_fs(
        p.USB_OTG_FS,
        Irqs,
        p.PA12,
        p.PA11,
        EP_OUT_BUF.init([0u8; 256]),
        hal::usb::Config::default(),
    );
    let mut usb_config = embassy_usb::Config::new(0x1209, 0x0001);
    usb_config.manufacturer = Some("Sonido");
    usb_config.product = Some("HUM grid pedal");
    usb_config.serial_number = Some("060");
    let cdc_state = CDC_STATE.init(State::new());
    let mut builder = embassy_usb::Builder::new(
        driver,
        usb_config,
        CONFIG_DESC.init([0; 256]),
        BOS_DESC.init([0; 256]),
        MSOS_DESC.init([0; 256]),
        CONTROL_BUF.init([0; 64]),
    );
    let class = CdcAcmClass::new(&mut builder, cdc_state, 64);
    // The keyboard shares the USB device with the serial port.
    let keyboard = Keyboard::new(
        &mut builder,
        HID_STATE.init(hid::State::new()),
        hid::Config {
            report_descriptor: KEYBOARD_REPORT_DESCRIPTOR,
            request_handler: Some(HID_HANDLER.init(KeyboardLights)),
            // The computer asks the keyboard for a report at least every 10 ms.
            poll_ms: 10,
            max_packet_size: REPORT_LEN as u16,
        },
    );
    let usb = builder.build();
    spawner.spawn(usb_task(usb)).unwrap();
    spawner.spawn(line_task(class)).unwrap();
    spawner.spawn(keys_task(keyboard)).unwrap();

    let audio_peripherals = sonido_daisy::audio::AudioPeripherals {
        codec_pins: sonido_daisy::codec_pins!(p),
        sai1: p.SAI1,
        dma1_ch0: p.DMA1_CH0,
        dma1_ch1: p.DMA1_CH1,
    };
    let interface = audio_peripherals
        .prepare_interface(Default::default())
        .await;
    let mut interface = defmt::unwrap!(interface.start_interface().await);
    defmt::info!("grid_pedal: listening");

    let mut ear = GridEar::new(NOMINAL_HZ, SAMPLE_RATE);
    let mut blocks: u32 = 0;
    let mut phase: f32 = 0.0;
    let mut rate_hz: f32 = 0.5;
    let mut event = false;
    let mut lost = true;
    let block_seconds = BLOCK_SIZE as f32 / SAMPLE_RATE;

    defmt::unwrap!(
        interface
            .start_callback(move |input, output| {
                output.copy_from_slice(input);
                // The left channel is the ear's input: the pickup, or the cable.
                for i in (0..input.len()).step_by(2) {
                    ear.process(u24_to_f32(input[i]));
                }
                blocks += 1;
                if blocks >= BLOCKS_PER_TICK {
                    blocks = 0;
                    match ear.tick() {
                        Some(r) => {
                            HZ_FAST_X1E4.store((r.hz_fast * 10000.0) as i32, Ordering::Relaxed);
                            HZ_X1E4.store((r.hz * 10000.0) as i32, Ordering::Relaxed);
                            ROCOF_X1E3.store((r.rocof * 1000.0) as i32, Ordering::Relaxed);
                            LEVEL_X10.store((r.level_db * 10.0) as i32, Ordering::Relaxed);
                            HARMONIC.store(r.harmonic as u32, Ordering::Relaxed);
                            VALID.store(1, Ordering::Relaxed);
                            let dev = libm::fabsf(r.hz_fast - NOMINAL_HZ);
                            let past = if dev > DEADBAND {
                                (dev - DEADBAND) / FULL_AT
                            } else {
                                0.0
                            };
                            rate_hz = 0.5 + 3.5 * if past > 1.0 { 1.0 } else { past };
                            event = libm::fabsf(r.rocof) > ROCOF_ENTER;
                            lost = false;
                        }
                        None => {
                            VALID.store(0, Ordering::Relaxed);
                            LEVEL_X10.store(-1200, Ordering::Relaxed);
                            lost = true;
                        }
                    }
                    SEQ.fetch_add(1, Ordering::Relaxed);
                }
                // The LEDs: the heartbeat in light.
                phase += rate_hz * block_seconds;
                if phase >= 1.0 {
                    phase -= 1.0;
                }
                let breathe = if lost {
                    1.0
                } else if phase < 0.5 {
                    1.0
                } else {
                    0.0
                };
                CONTROLS.write_led(0, breathe);
                CONTROLS.write_led(1, if lost || event { 1.0 } else { 0.0 });
            })
            .await
    );
}
