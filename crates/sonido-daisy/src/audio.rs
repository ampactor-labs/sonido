//! Audio interface for the Daisy Seed — SAI + codec + DMA.
//!
//! Absorbed from `daisy-embassy` to give full control over the platform layer.
//! Currently supports the PCM3060 codec (`seed_1_2` feature, Daisy Seed rev 1.2
//! and later — the Hothouse DIY pedal platform).
//!
//! # Architecture
//!
//! The audio path is: codec ADC → SAI1 RX → DMA → CPU → DMA → SAI1 TX → codec DAC.
//!
//! For the PCM3060 (hardware-configured, no I2C):
//! - SAI1 sub-block A (TX) is the **master** — drives MCLK, SCK, FS
//! - SAI1 sub-block B (RX) is the **slave** — synchronized to TX
//! - DMA1_CH0 services TX, DMA1_CH1 services RX
//!
//! DMA buffers are placed in SRAM1 (`0x30000000`, `.sram1_bss` section)
//! because DMA1 can only access D2 SRAM on the STM32H750.
//!
//! # Usage
//!
//! ```ignore
//! let audio = sonido_daisy::audio::AudioPeripherals {
//!     codec_pins: sonido_daisy::codec_pins!(p),
//!     sai1: p.SAI1,
//!     dma1_ch0: p.DMA1_CH0,
//!     dma1_ch1: p.DMA1_CH1,
//! };
//! let idle = audio.prepare_interface(Default::default()).await;
//! let mut running = idle.start_interface().await.unwrap();
//! running.start_callback(|input, output| {
//!     output.copy_from_slice(input); // passthrough
//! }).await.unwrap();
//! ```

use core::convert::Infallible;
use core::marker::PhantomData;

use defmt::info;
use embassy_stm32::{self as hal, Peri};
use grounded::uninit::GroundedArrayCell;
use hal::peripherals::*;
use hal::sai::{self, MasterClockDivider};

// ── Constants ────────────────────────────────────────────────────────────

/// Samples per block (stereo pairs).
pub const BLOCK_LENGTH: usize = 32;

/// Half-DMA buffer length: one block × 2 channels (interleaved L/R).
pub const HALF_DMA_BUFFER_LENGTH: usize = BLOCK_LENGTH * 2;

/// Full DMA buffer: two half-buffers (double-buffered).
pub const DMA_BUFFER_LENGTH: usize = HALF_DMA_BUFFER_LENGTH * 2;

/// MCLK-to-Fs ratio (256× oversampling, standard for PCM3060/WM8731).
const CLOCK_RATIO: u32 = 256;

// ── DMA buffers (must be in SRAM1 for DMA1 access) ──────────────────────

#[unsafe(link_section = ".sram1_bss")]
static TX_BUFFER: GroundedArrayCell<u32, DMA_BUFFER_LENGTH> = GroundedArrayCell::uninit();

#[unsafe(link_section = ".sram1_bss")]
static RX_BUFFER: GroundedArrayCell<u32, DMA_BUFFER_LENGTH> = GroundedArrayCell::uninit();

// ── Public types ─────────────────────────────────────────────────────────

/// One interleaved stereo block: `[L0, R0, L1, R1, ..., L31, R31]`.
pub type InterleavedBlock = [u32; HALF_DMA_BUFFER_LENGTH];

/// Sample rate selection.
#[derive(Clone, Copy)]
pub enum Fs {
    /// 8 kHz
    Fs8000,
    /// 32 kHz
    Fs32000,
    /// 44.1 kHz
    Fs44100,
    /// 48 kHz (default)
    Fs48000,
    /// 88.2 kHz
    Fs88200,
    /// 96 kHz
    Fs96000,
}

impl Fs {
    /// Computes the SAI master clock divider for this sample rate.
    pub fn into_clock_divider(self) -> MasterClockDivider {
        let fs = match self {
            Fs::Fs8000 => 8000,
            Fs::Fs32000 => 32000,
            Fs::Fs44100 => 44100,
            Fs::Fs48000 => 48000,
            Fs::Fs88200 => 88200,
            Fs::Fs96000 => 96000,
        };
        let kernel_clock = hal::rcc::frequency::<SAI1>().0;
        let mclk_div = (kernel_clock / (fs * CLOCK_RATIO)) as u8;
        mclk_div_from_u8(mclk_div)
    }
}

/// Audio configuration (currently just sample rate).
pub struct AudioConfig {
    /// Sample rate.
    pub fs: Fs,
}

impl Default for AudioConfig {
    fn default() -> Self {
        AudioConfig { fs: Fs::Fs48000 }
    }
}

// ── Codec pins (PCM3060 / seed_1_2) ─────────────────────────────────────

/// SAI1 pin set for the PCM3060 codec (Daisy Seed 1.2+).
///
/// No I2C pins — PCM3060 is hardware-configured.
#[allow(non_snake_case)]
pub struct CodecPins<'a> {
    /// SAI1 master clock output (PE2).
    pub MCLK_A: Peri<'a, PE2>,
    /// SAI1 serial clock (PE5).
    pub SCK_A: Peri<'a, PE5>,
    /// SAI1 frame sync / word select (PE4).
    pub FS_A: Peri<'a, PE4>,
    /// SAI1 serial data A — TX to codec DAC (PE6).
    pub SD_A: Peri<'a, PE6>,
    /// SAI1 serial data B — RX from codec ADC (PE3).
    pub SD_B: Peri<'a, PE3>,
}

// ── AudioPeripherals ─────────────────────────────────────────────────────

/// Peripherals required for audio I/O.
///
/// Construct this directly (don't use `new_daisy_board!`) when you need
/// other GPIO pins for knobs, toggles, footswitches, or LEDs.
pub struct AudioPeripherals<'a> {
    /// Codec pin connections (SAI1 signals).
    pub codec_pins: CodecPins<'a>,
    /// SAI1 peripheral.
    pub sai1: Peri<'a, SAI1>,
    /// DMA channel for SAI TX.
    pub dma1_ch0: Peri<'a, DMA1_CH0>,
    /// DMA channel for SAI RX.
    pub dma1_ch1: Peri<'a, DMA1_CH1>,
}

impl<'a> AudioPeripherals<'a> {
    /// Prepares the audio interface: configures SAI, allocates DMA buffers,
    /// sets up the codec.
    ///
    /// Returns an [`Interface`] in the [`Idle`] state. Call
    /// [`Interface::start_interface`] to begin audio clocks, then
    /// [`Interface::start_callback`] to enter the processing loop.
    pub async fn prepare_interface(self, audio_config: AudioConfig) -> Interface<'a, Idle> {
        let tx_buffer: &mut [u32] = unsafe {
            TX_BUFFER.initialize_all_copied(0);
            let (ptr, len) = TX_BUFFER.get_ptr_len();
            core::slice::from_raw_parts_mut(ptr, len)
        };
        let rx_buffer: &mut [u32] = unsafe {
            RX_BUFFER.initialize_all_copied(0);
            let (ptr, len) = RX_BUFFER.get_ptr_len();
            core::slice::from_raw_parts_mut(ptr, len)
        };

        let codec = Codec::new(self, audio_config, tx_buffer, rx_buffer);

        Interface {
            codec,
            _state: PhantomData,
        }
    }
}

// ── Interface state machine ──────────────────────────────────────────────

/// Marker: interface configured but SAI not started.
pub struct Idle;

/// Marker: SAI running, ready for audio callbacks.
pub struct Running;

/// Sealed trait for interface states.
pub trait InterfaceState {}
impl InterfaceState for Idle {}
impl InterfaceState for Running {}

/// SAI audio interface with typestate (Idle → Running).
///
/// # Lifecycle
///
/// 1. [`AudioPeripherals::prepare_interface`] → `Interface<Idle>`
/// 2. [`Interface::start_interface`] → `Interface<Running>`
/// 3. [`Interface::start_callback`] → infinite audio processing loop
pub struct Interface<'a, S: InterfaceState> {
    codec: Codec<'a>,
    _state: PhantomData<S>,
}

impl<'a> Interface<'a, Idle> {
    /// Starts audio clocks and transitions to the Running state.
    ///
    /// Must be called before [`Interface::start_callback`]. Call
    /// `start_callback` immediately afterwards to avoid SAI overruns.
    pub async fn start_interface(mut self) -> Result<Interface<'a, Running>, sai::Error> {
        self.codec.start().await?;
        Ok(Interface {
            codec: self.codec,
            _state: PhantomData,
        })
    }
}

impl Interface<'_, Running> {
    /// Enters the audio processing loop.
    ///
    /// Calls `callback` once per block (32 stereo pairs = 64 interleaved `u32`s).
    /// Both slices are `[L0, R0, L1, R1, ..., L31, R31]`.
    ///
    /// Runs forever unless an SAI error occurs. The closure is `FnMut`, not
    /// `async` — it runs in the Embassy executor thread, not a hardware ISR.
    pub async fn start_callback(
        &mut self,
        mut callback: impl FnMut(&[u32], &mut [u32]),
    ) -> Result<Infallible, sai::Error> {
        info!("enter audio callback loop");
        let mut write_buf = [0u32; HALF_DMA_BUFFER_LENGTH];
        let mut read_buf = [0u32; HALF_DMA_BUFFER_LENGTH];
        loop {
            self.codec.read(&mut read_buf).await?;
            callback(&read_buf, &mut write_buf);
            self.codec.write(&write_buf).await?;
        }
    }
}

// ── PCM3060 Codec (seed_1_2) ─────────────────────────────────────────────

/// Low-level SAI driver for the PCM3060 codec.
///
/// PCM3060 is hardware-configured (no I2C). SAI sub-block A is the master
/// transmitter, sub-block B is the slave receiver.
struct Codec<'a> {
    sai_tx: sai::Sai<'a, SAI1, u32>,
    sai_rx: sai::Sai<'a, SAI1, u32>,
}

impl<'a> Codec<'a> {
    fn new(
        p: AudioPeripherals<'a>,
        audio_config: AudioConfig,
        tx_buffer: &'a mut [u32],
        rx_buffer: &'a mut [u32],
    ) -> Self {
        info!("set up PCM3060 SAI");

        let (sub_block_tx, sub_block_rx) = hal::sai::split_subblocks(p.sai1);

        // TX = master, asynchronous, drives MCLK/SCK/FS
        let mut sai_tx_config = sai::Config::default();
        sai_tx_config.mode = sai::Mode::Master;
        sai_tx_config.tx_rx = sai::TxRx::Transmitter;
        sai_tx_config.sync_output = true;
        sai_tx_config.clock_strobe = sai::ClockStrobe::Falling;
        sai_tx_config.master_clock_divider = audio_config.fs.into_clock_divider();
        sai_tx_config.stereo_mono = sai::StereoMono::Stereo;
        sai_tx_config.data_size = sai::DataSize::Data24;
        sai_tx_config.bit_order = sai::BitOrder::MsbFirst;
        sai_tx_config.frame_sync_polarity = sai::FrameSyncPolarity::ActiveHigh;
        sai_tx_config.frame_sync_offset = sai::FrameSyncOffset::OnFirstBit;
        sai_tx_config.frame_length = 64;
        sai_tx_config.frame_sync_active_level_length = sai::word::U7(32);
        sai_tx_config.fifo_threshold = sai::FifoThreshold::Quarter;

        // RX = slave, synchronized to TX
        let mut sai_rx_config = sai_tx_config;
        sai_rx_config.mode = sai::Mode::Slave;
        sai_rx_config.tx_rx = sai::TxRx::Receiver;
        sai_rx_config.sync_input = sai::SyncInput::Internal;
        sai_rx_config.clock_strobe = sai::ClockStrobe::Rising;
        sai_rx_config.sync_output = false;

        let sai_tx = sai::Sai::new_asynchronous_with_mclk(
            sub_block_tx,
            p.codec_pins.SCK_A,
            p.codec_pins.SD_A,
            p.codec_pins.FS_A,
            p.codec_pins.MCLK_A,
            p.dma1_ch0,
            tx_buffer,
            sai_tx_config,
        );
        let sai_rx = sai::Sai::new_synchronous(
            sub_block_rx,
            p.codec_pins.SD_B,
            p.dma1_ch1,
            rx_buffer,
            sai_rx_config,
        );

        Self { sai_tx, sai_rx }
    }

    async fn start(&mut self) -> Result<(), sai::Error> {
        info!("start PCM3060 SAI");
        // PCM3060: TX is master. Must write once to start clocks, then
        // the slave RX can synchronize and begin receiving.
        let write_buf = [0u32; HALF_DMA_BUFFER_LENGTH];
        self.sai_tx.write(&write_buf).await?;
        self.sai_rx.start()
    }

    async fn read(&mut self, buf: &mut [u32]) -> Result<(), sai::Error> {
        self.sai_rx.read(buf).await
    }

    async fn write(&mut self, buf: &[u32]) -> Result<(), sai::Error> {
        self.sai_tx.write(buf).await
    }
}

// ── MCLK divider lookup ─────────────────────────────────────────────────

const fn mclk_div_from_u8(v: u8) -> MasterClockDivider {
    match v {
        1 => MasterClockDivider::DIV1,
        2 => MasterClockDivider::DIV2,
        3 => MasterClockDivider::DIV3,
        4 => MasterClockDivider::DIV4,
        5 => MasterClockDivider::DIV5,
        6 => MasterClockDivider::DIV6,
        7 => MasterClockDivider::DIV7,
        8 => MasterClockDivider::DIV8,
        9 => MasterClockDivider::DIV9,
        10 => MasterClockDivider::DIV10,
        11 => MasterClockDivider::DIV11,
        12 => MasterClockDivider::DIV12,
        13 => MasterClockDivider::DIV13,
        14 => MasterClockDivider::DIV14,
        15 => MasterClockDivider::DIV15,
        16 => MasterClockDivider::DIV16,
        17 => MasterClockDivider::DIV17,
        18 => MasterClockDivider::DIV18,
        19 => MasterClockDivider::DIV19,
        20 => MasterClockDivider::DIV20,
        21 => MasterClockDivider::DIV21,
        22 => MasterClockDivider::DIV22,
        23 => MasterClockDivider::DIV23,
        24 => MasterClockDivider::DIV24,
        25 => MasterClockDivider::DIV25,
        26 => MasterClockDivider::DIV26,
        27 => MasterClockDivider::DIV27,
        28 => MasterClockDivider::DIV28,
        29 => MasterClockDivider::DIV29,
        30 => MasterClockDivider::DIV30,
        31 => MasterClockDivider::DIV31,
        32 => MasterClockDivider::DIV32,
        33 => MasterClockDivider::DIV33,
        34 => MasterClockDivider::DIV34,
        35 => MasterClockDivider::DIV35,
        36 => MasterClockDivider::DIV36,
        37 => MasterClockDivider::DIV37,
        38 => MasterClockDivider::DIV38,
        39 => MasterClockDivider::DIV39,
        40 => MasterClockDivider::DIV40,
        41 => MasterClockDivider::DIV41,
        42 => MasterClockDivider::DIV42,
        43 => MasterClockDivider::DIV43,
        44 => MasterClockDivider::DIV44,
        45 => MasterClockDivider::DIV45,
        46 => MasterClockDivider::DIV46,
        47 => MasterClockDivider::DIV47,
        48 => MasterClockDivider::DIV48,
        49 => MasterClockDivider::DIV49,
        50 => MasterClockDivider::DIV50,
        51 => MasterClockDivider::DIV51,
        52 => MasterClockDivider::DIV52,
        53 => MasterClockDivider::DIV53,
        54 => MasterClockDivider::DIV54,
        55 => MasterClockDivider::DIV55,
        56 => MasterClockDivider::DIV56,
        57 => MasterClockDivider::DIV57,
        58 => MasterClockDivider::DIV58,
        59 => MasterClockDivider::DIV59,
        60 => MasterClockDivider::DIV60,
        61 => MasterClockDivider::DIV61,
        62 => MasterClockDivider::DIV62,
        63 => MasterClockDivider::DIV63,
        _ => panic!(),
    }
}
