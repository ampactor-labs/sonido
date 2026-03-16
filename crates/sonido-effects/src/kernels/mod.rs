//! Kernel-architecture effect implementations.
//!
//! All 20 effects in `sonido-effects` are implemented using the
//! [`DspKernel`](sonido_core::DspKernel) pattern: pure DSP separated from parameter
//! ownership. Each effect defines:
//!
//! - A `Params` struct (parameter values + metadata via [`KernelParams`](sonido_core::KernelParams))
//! - A `Kernel` struct (DSP state only — filters, delay lines, ADAA processors)
//!
//! Kernels are deployed via [`Adapter`](sonido_core::kernel::Adapter) for desktop/plugin
//! use, or called directly on embedded targets.

pub mod amp;
pub mod bitcrusher;
pub mod cabinet;
pub mod chorus;
pub mod compressor;
pub mod deesser;
pub mod delay;
pub mod distortion;
pub mod drone;
pub mod eq;
pub mod filter;
pub mod flanger;
pub mod gate;
pub mod glitch;
pub mod limiter;
pub mod looper;
pub mod multiband_comp;
pub mod phaser;
pub mod pitch_shift;
pub mod plate_reverb;
pub mod preamp;
pub mod reverb;
pub mod ringmod;
pub mod shelving_eq;
pub mod spring_reverb;
pub mod stage;
pub mod stereo_widener;
pub mod tape;
pub mod texture;
pub mod time_stretch;
pub mod transient_shaper;
pub mod tremolo;
pub mod tuner;
pub mod vibrato;
pub mod wah;

pub use amp::{AmpKernel, AmpParams};
pub use bitcrusher::{BitcrusherKernel, BitcrusherParams};
pub use cabinet::{CabinetKernel, CabinetParams};
pub use chorus::{ChorusKernel, ChorusParams};
pub use compressor::{CompressorKernel, CompressorParams};
pub use deesser::{DeesserKernel, DeesserParams};
pub use delay::{DelayKernel, DelayParams};
pub use distortion::{DistortionKernel, DistortionParams};
pub use drone::{DroneKernel, DroneParams};
pub use eq::{EqKernel, EqParams};
pub use filter::{FilterKernel, FilterParams};
pub use flanger::{FlangerKernel, FlangerParams};
pub use gate::{GateKernel, GateParams};
pub use glitch::{GlitchKernel, GlitchParams};
pub use limiter::{LimiterKernel, LimiterParams};
pub use looper::{LooperKernel, LooperParams};
pub use multiband_comp::{MultibandCompKernel, MultibandCompParams};
pub use phaser::{PhaserKernel, PhaserParams};
pub use pitch_shift::{PitchShiftKernel, PitchShiftParams};
pub use plate_reverb::{PlateReverbKernel, PlateReverbParams};
pub use preamp::{PreampKernel, PreampParams};
pub use reverb::{ReverbKernel, ReverbParams};
pub use ringmod::{RingModKernel, RingModParams};
pub use shelving_eq::{ShelvingEqKernel, ShelvingEqParams};
pub use spring_reverb::{SpringReverbKernel, SpringReverbParams};
pub use stage::{StageKernel, StageParams};
pub use stereo_widener::{StereoWidenerKernel, StereoWidenerParams};
pub use tape::{TapeKernel, TapeParams};
pub use texture::{TextureKernel, TextureParams};
pub use time_stretch::{TimeStretchKernel, TimeStretchParams};
pub use transient_shaper::{TransientShaperKernel, TransientShaperParams};
pub use tremolo::{TremoloKernel, TremoloParams};
pub use tuner::{TunerKernel, TunerParams};
pub use vibrato::{VibratoKernel, VibratoParams};
pub use wah::{WahKernel, WahParams};
