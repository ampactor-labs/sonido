//! Load a [`Patch`] from QSPI flash and build it into a runnable graph.
//!
//! This ties the firmware-specific pieces to the shared, host-tested
//! [`sonido_patch::build_graph_from_patch`]: read a 4 KB sector
//! ([`QspiFlash`]) → decode ([`sonido_patch::decode`]) → build a
//! [`GraphEngine`] with [`pedal_make_effect`], which links **only** the curated
//! pedal effect set so the firmware stays inside its 480 KB budget.

extern crate alloc;
use alloc::boxed::Box;

use sonido_core::EffectWithParams;
use sonido_core::GraphEngine;
use sonido_core::kernel::Adapter;
use sonido_effects::kernels::{
    BitcrusherKernel, ChorusKernel, DelayKernel, DistortionKernel, FilterKernel, PhaserKernel,
    ReverbKernel, RingModKernel,
};
use sonido_patch::{Patch, PatchBuildError, PatchError, SECTOR_SIZE, build_graph_from_patch};
use sonido_registry::effect_by_uid;

use crate::qspi_flash::{QspiFlash, patch_slot_addr};

/// Construct a pedal effect by stable UID, or `None` if it is not on the pedal.
///
/// Matches by the curated string id (so it tracks
/// [`PEDAL_EFFECT_IDS`](sonido_registry::PEDAL_EFFECT_IDS)) and constructs the
/// kernel directly — `EffectRegistry::new()` is never called, so only these
/// eight kernels are linked into the firmware. Uses the smoothing adapter so
/// macro/morph parameter writes don't zipper.
pub fn pedal_make_effect(
    uid: u16,
    sr: f32,
) -> Option<(Box<dyn EffectWithParams + Send>, &'static str)> {
    let name = effect_by_uid(uid)?;
    let effect: Box<dyn EffectWithParams + Send> = match name {
        "chorus" => Box::new(Adapter::new(ChorusKernel::new(sr), sr)),
        "phaser" => Box::new(Adapter::new(PhaserKernel::new(sr), sr)),
        "distortion" => Box::new(Adapter::new(DistortionKernel::new(sr), sr)),
        "bitcrusher" => Box::new(Adapter::new(BitcrusherKernel::new(sr), sr)),
        "delay" => Box::new(Adapter::new(DelayKernel::new(sr), sr)),
        "reverb" => Box::new(Adapter::new(ReverbKernel::new(sr), sr)),
        "ringmod" => Box::new(Adapter::new(RingModKernel::new(sr), sr)),
        "filter" => Box::new(Adapter::new(FilterKernel::new(sr), sr)),
        _ => return None,
    };
    Some((effect, name))
}

/// Why a patch could not be loaded from flash.
#[derive(Debug)]
#[non_exhaustive]
pub enum LoadError {
    /// The sector did not decode (bad magic/crc/version/truncation).
    Decode(PatchError),
    /// The decoded patch could not be built into a graph (unknown effect, bad edge…).
    Build(PatchBuildError),
}

/// LED blink-count for a load failure, so a headless pedal can signal the cause.
///
/// 2 = corrupt/empty sector, 3 = an effect not on the pedal, 4 = graph error.
pub fn error_blink_code(err: &LoadError) -> u8 {
    match err {
        LoadError::Decode(_) => 2,
        LoadError::Build(PatchBuildError::UnknownEffect(_)) => 3,
        LoadError::Build(_) => 4,
    }
}

/// Decode the patch in flash slot `slot` into `buf` (reused, [`SECTOR_SIZE`]).
pub fn load_patch(
    flash: &mut QspiFlash,
    slot: usize,
    buf: &mut [u8; SECTOR_SIZE],
) -> Result<Patch, LoadError> {
    flash.read(patch_slot_addr(slot), buf);
    sonido_patch::decode(buf).map_err(LoadError::Decode)
}

/// Read flash slot `slot`, decode it, and build a runnable [`GraphEngine`].
///
/// All allocation and graph compilation happens here, off the audio path — the
/// caller invokes this from the control/rebuild task, never the SAI callback.
pub fn load_and_build(
    flash: &mut QspiFlash,
    slot: usize,
    buf: &mut [u8; SECTOR_SIZE],
    sample_rate: f32,
    block_size: usize,
) -> Result<(Patch, GraphEngine), LoadError> {
    let patch = load_patch(flash, slot, buf)?;
    let engine = build_graph_from_patch(&patch, sample_rate, block_size, pedal_make_effect)
        .map_err(LoadError::Build)?;
    Ok((patch, engine))
}
