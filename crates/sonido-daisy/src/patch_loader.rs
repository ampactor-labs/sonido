//! Rigs on the pedal: read them from the QSPI rig bank, check them, and build
//! their effects.
//!
//! Effects come from [`EffectRegistry::create`], the same constructor the
//! plugins and the editor use, so a rig's parameters are smoothed and sound the
//! same on the pedal as in a DAW. [`check_rig`] applies the rule the editor's
//! export check applies (`sonido_patch::validate_for_pedal` with
//! `sonido_platform::cycle_table`): every effect available, routing valid, CPU
//! within [`sonido_platform::cycle_table::CHAIN_FRACTION`] of the block, memory
//! within SDRAM.

extern crate alloc;

use alloc::boxed::Box;

use sonido_core::EffectWithParams;
use sonido_patch::link::ErrorCode;
use sonido_patch::validate::{Severity, daisy_load, has_errors, validate_structure};
use sonido_patch::{Patch, PatchBuildError, PatchError, SECTOR_SIZE};
use sonido_registry::{EffectRegistry, effect_by_uid};

use crate::qspi_flash::{QspiFlash, patch_slot_addr};

/// Construct an effect by stable UID through the registry.
pub fn make_effect(
    registry: &EffectRegistry,
    uid: u16,
    sample_rate: f32,
) -> Option<(Box<dyn EffectWithParams + Send>, &'static str)> {
    let id = effect_by_uid(uid)?;
    Some((registry.create(id, sample_rate)?, id))
}

/// Why a rig could not be loaded.
#[derive(Debug)]
#[non_exhaustive]
pub enum LoadError {
    /// The sector did not decode (empty, corrupt, or a newer format).
    Decode(PatchError),
    /// The rig decoded but the pedal refuses it.
    Refused(ErrorCode),
    /// The rig passed its checks but its graph did not build.
    Build(PatchBuildError),
}

/// LED blink-count for a load failure, so a headless pedal can signal the cause.
///
/// 2 = empty or corrupt slot, 3 = an effect this firmware lacks, 4 = too heavy
/// for the CPU or memory, 5 = routing that does not build.
pub fn error_blink_code(err: &LoadError) -> u8 {
    match err {
        LoadError::Decode(_) | LoadError::Refused(ErrorCode::NotARig) => 2,
        LoadError::Refused(ErrorCode::UnknownEffect) => 3,
        LoadError::Refused(ErrorCode::OverBudget) => 4,
        LoadError::Refused(_) | LoadError::Build(_) => 5,
    }
}

/// Check a rig against what the pedal can run.
///
/// # Errors
///
/// [`ErrorCode::UnknownEffect`], [`ErrorCode::BadRig`] (structure) or
/// [`ErrorCode::OverBudget`] (CPU or SDRAM).
pub fn check_rig(patch: &Patch) -> Result<(), ErrorCode> {
    if patch
        .nodes
        .iter()
        .any(|n| effect_by_uid(n.effect_uid).is_none())
    {
        return Err(ErrorCode::UnknownEffect);
    }
    if has_errors(&validate_structure(patch)) {
        return Err(ErrorCode::BadRig);
    }
    // The same CPU and memory rule export applies.
    if daisy_load(patch.nodes.iter().map(|n| n.effect_uid)).severity() == Some(Severity::Error) {
        return Err(ErrorCode::OverBudget);
    }
    Ok(())
}

/// Read and decode the rig in bank slot `slot`, using `buf` as the sector.
///
/// # Errors
///
/// [`LoadError::Decode`] for an empty or corrupt slot.
pub async fn load_patch(
    flash: &mut QspiFlash<'_>,
    slot: usize,
    buf: &mut [u8; SECTOR_SIZE],
) -> Result<Patch, LoadError> {
    flash.read(patch_slot_addr(slot), buf).await;
    sonido_patch::decode(buf).map_err(LoadError::Decode)
}
