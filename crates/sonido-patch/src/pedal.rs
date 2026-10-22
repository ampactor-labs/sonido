//! The pedal's own rig shapes, as ordinary [`Patch`]es.
//!
//! The pedal firmware plays every rig through [`PatchPlayer`](crate::PatchPlayer):
//! rigs flashed from the editor, built-in factory rigs, and the **jam**, the
//! three-slot rig you build on the pedal by scrolling effects into slots, routed
//! by the T3 toggle. [`jam_patch`] turns a jam into a patch; [`PedalState`] is
//! what the pedal keeps in its state sector across power cycles (the active
//! rig, the jam and its routing).

#[cfg(not(feature = "std"))]
use alloc::vec::Vec;

use crate::binary::{self, PatchError, crc32};
use crate::{Patch, PatchEdge, PatchEndpoint, PatchNode, SECTOR_SIZE};

/// Effect slots in a jam (one per T1 position).
pub const JAM_SLOTS: usize = 3;

/// How a jam's populated slots are routed (the T3 toggle).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Topology {
    /// 1 → 2 → 3.
    #[default]
    Linear,
    /// Input splits into every slot; the slots merge into the output.
    Parallel,
    /// Slot 1, then a split into the rest, merged into the output.
    Fan,
}

impl Topology {
    /// From the T3 toggle position (0 = up, 1 = middle, 2 = down).
    pub fn from_toggle(position: u8) -> Self {
        match position {
            1 => Self::Parallel,
            2 => Self::Fan,
            _ => Self::Linear,
        }
    }

    fn to_byte(self) -> u8 {
        match self {
            Self::Linear => 0,
            Self::Parallel => 1,
            Self::Fan => 2,
        }
    }
}

/// One populated jam slot: its effect and its A and B states.
#[derive(Clone, Debug, PartialEq)]
pub struct JamSlot {
    /// Stable effect UID (`sonido_registry::EFFECT_UIDS`).
    pub effect_uid: u16,
    /// A-state parameter values, in descriptor order.
    pub params_a: Vec<f32>,
    /// B-state parameter values, same length as `params_a`.
    pub params_b: Vec<f32>,
}

/// For each jam slot, the index of its node in [`jam_patch`]'s output.
pub fn jam_node_indices(slots: &[Option<JamSlot>; JAM_SLOTS]) -> [Option<usize>; JAM_SLOTS] {
    let mut next = 0;
    core::array::from_fn(|i| {
        slots[i].as_ref().map(|_| {
            next += 1;
            next - 1
        })
    })
}

/// The patch for a jam: populated slots become nodes in slot order, wired by
/// `topology`. Empty slots are skipped, so with one effect every topology is
/// a plain chain, and with none the input goes straight to the output. Jams
/// have no macros; on the pedal the knobs edit the selected slot directly.
pub fn jam_patch(slots: &[Option<JamSlot>; JAM_SLOTS], topology: Topology) -> Patch {
    use PatchEndpoint::{Input, Merge, Node, Output, Split};

    let mut patch = Patch::new("Jam");
    for slot in slots.iter().flatten() {
        let mut node = PatchNode::new(slot.effect_uid, slot.params_a.clone());
        node.params_b.clone_from(&slot.params_b);
        patch.nodes.push(node);
    }
    let n = patch.nodes.len() as u8;
    let edge = |from, to| PatchEdge::new(from, to);
    patch.edges = match (n, topology) {
        (0, _) => vec_of([edge(Input, Output)]),
        (1, _) | (_, Topology::Linear) => {
            let mut edges = Vec::new();
            edges.push(edge(Input, Node(0)));
            for i in 1..n {
                edges.push(edge(Node(i - 1), Node(i)));
            }
            edges.push(edge(Node(n - 1), Output));
            edges
        }
        (_, Topology::Parallel) => {
            let mut edges = Vec::new();
            edges.push(edge(Input, Split(0)));
            for i in 0..n {
                edges.push(edge(Split(0), Node(i)));
                edges.push(edge(Node(i), Merge(0)));
            }
            edges.push(edge(Merge(0), Output));
            edges
        }
        (_, Topology::Fan) => {
            let mut edges = Vec::new();
            edges.push(edge(Input, Node(0)));
            edges.push(edge(Node(0), Split(0)));
            for i in 1..n {
                edges.push(edge(Split(0), Node(i)));
                edges.push(edge(Node(i), Merge(0)));
            }
            edges.push(edge(Merge(0), Output));
            edges
        }
    };
    patch
}

fn vec_of<const N: usize>(items: [PatchEdge; N]) -> Vec<PatchEdge> {
    items.into_iter().collect()
}

/// Which rig the pedal is playing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum RigRef {
    /// The jam.
    #[default]
    Jam,
    /// Rig bank slot `n` (0-based).
    Bank(u8),
    /// Built-in factory rig `n` (0-based).
    Factory(u8),
}

impl RigRef {
    /// One-byte wire/flash form: 0 = jam, 1..=0x7F = bank slot + 1,
    /// 0x80.. = factory rig + 0x80.
    pub fn to_byte(self) -> u8 {
        match self {
            Self::Jam => 0,
            Self::Bank(n) => n.saturating_add(1).min(0x7F),
            Self::Factory(n) => 0x80 | (n & 0x7F),
        }
    }

    /// Inverse of [`to_byte`](Self::to_byte).
    pub fn from_byte(byte: u8) -> Self {
        match byte {
            0 => Self::Jam,
            1..=0x7F => Self::Bank(byte - 1),
            _ => Self::Factory(byte & 0x7F),
        }
    }
}

/// What the pedal keeps in its state sector: the active rig, the jam and the
/// tapped tempo.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct PedalState {
    /// The rig to start with at power-on.
    pub active: RigRef,
    /// The jam's routing.
    pub topology: Topology,
    /// The jam's slots.
    pub jam: [Option<JamSlot>; JAM_SLOTS],
    /// Tapped tempo in BPM (to 0.1), or `None` before the first tap.
    pub bpm: Option<f32>,
}

/// State sector header: magic, active rig, topology, slot → node map, tempo
/// (BPM × 10, 0 = none), CRC32 of the header.
const STATE_MAGIC: [u8; 4] = *b"SPS1";
const STATE_HEADER: usize = 16;

/// Why a state sector could not be read.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum StateError {
    /// Not a state sector (erased, or another format).
    NoState,
    /// The header's checksum does not match.
    BadHeader,
    /// The embedded jam patch did not decode.
    Jam(PatchError),
}

impl PedalState {
    /// Encode into a 4 KB state sector (header, then the jam as a patch).
    ///
    /// # Errors
    ///
    /// Propagates [`PatchError`] if the jam does not fit.
    pub fn encode(&self, out: &mut [u8; SECTOR_SIZE]) -> Result<(), PatchError> {
        out.fill(0xFF);
        let patch = jam_patch(&self.jam, self.topology);
        binary::encode(&patch, &mut out[STATE_HEADER..])?;
        let map = jam_node_indices(&self.jam);
        out[..4].copy_from_slice(&STATE_MAGIC);
        out[4] = self.active.to_byte();
        out[5] = self.topology.to_byte();
        for (i, node) in map.iter().enumerate() {
            out[6 + i] = node.map_or(0xFF, |n| n as u8);
        }
        let bpm: u16 = self
            .bpm
            .map_or(0, |b| ((b * 10.0 + 0.5) as u32).clamp(1, 65_535) as u16);
        out[9..11].copy_from_slice(&bpm.to_le_bytes());
        out[11] = 0;
        let crc = crc32(&out[..12]);
        out[12..16].copy_from_slice(&crc.to_le_bytes());
        Ok(())
    }

    /// Decode a state sector written by [`encode`](Self::encode).
    ///
    /// # Errors
    ///
    /// [`StateError::NoState`] for an erased or foreign sector,
    /// [`StateError::BadHeader`] for a corrupt header, [`StateError::Jam`] if
    /// the jam patch is corrupt.
    pub fn decode(data: &[u8; SECTOR_SIZE]) -> Result<Self, StateError> {
        if data[..4] != STATE_MAGIC {
            return Err(StateError::NoState);
        }
        let crc = u32::from_le_bytes([data[12], data[13], data[14], data[15]]);
        if crc != crc32(&data[..12]) {
            return Err(StateError::BadHeader);
        }
        let patch = binary::decode(&data[STATE_HEADER..]).map_err(StateError::Jam)?;
        let mut jam: [Option<JamSlot>; JAM_SLOTS] = Default::default();
        for (i, slot) in jam.iter_mut().enumerate() {
            let node = data[6 + i];
            if let Some(n) = patch.nodes.get(node as usize) {
                *slot = Some(JamSlot {
                    effect_uid: n.effect_uid,
                    params_a: n.params_a.clone(),
                    params_b: n.params_b.clone(),
                });
            }
        }
        let bpm = u16::from_le_bytes([data[9], data[10]]);
        Ok(Self {
            active: RigRef::from_byte(data[4]),
            topology: Topology::from_toggle(data[5]),
            jam,
            bpm: (bpm > 0).then(|| f32::from(bpm) / 10.0),
        })
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;
    use crate::build_graph_from_patch;
    use sonido_registry::{EffectRegistry, effect_by_uid, effect_uid};

    fn slot(id: &str, registry: &EffectRegistry) -> JamSlot {
        let uid = effect_uid(id).unwrap();
        let effect = registry.create(id, 48_000.0).unwrap();
        let params: Vec<f32> = (0..effect.effect_param_count())
            .map(|i| effect.effect_get_param(i))
            .collect();
        JamSlot {
            effect_uid: uid,
            params_a: params.clone(),
            params_b: params,
        }
    }

    fn builds(patch: &Patch, registry: &EffectRegistry) -> usize {
        let engine = build_graph_from_patch(patch, 48_000.0, 32, |uid, sr| {
            let id = effect_by_uid(uid)?;
            Some((registry.create(id, sr)?, id))
        })
        .unwrap();
        engine.slot_count()
    }

    #[test]
    fn every_topology_and_fill_builds() {
        let registry = EffectRegistry::new();
        let all = ["distortion", "delay", "reverb"];
        for topology in [Topology::Linear, Topology::Parallel, Topology::Fan] {
            for mask in 0u8..8 {
                let slots: [Option<JamSlot>; JAM_SLOTS] = core::array::from_fn(|i| {
                    (mask & (1 << i) != 0).then(|| slot(all[i], &registry))
                });
                let patch = jam_patch(&slots, topology);
                assert_eq!(
                    builds(&patch, &registry),
                    mask.count_ones() as usize,
                    "{topology:?} mask {mask:03b}"
                );
            }
        }
    }

    #[test]
    fn node_indices_skip_empty_slots() {
        let registry = EffectRegistry::new();
        let slots = [
            None,
            Some(slot("chorus", &registry)),
            Some(slot("delay", &registry)),
        ];
        assert_eq!(jam_node_indices(&slots), [None, Some(0), Some(1)]);
    }

    #[test]
    fn state_round_trips() {
        let registry = EffectRegistry::new();
        let mut b = slot("reverb", &registry);
        b.params_b[1] = 90.0;
        let state = PedalState {
            active: RigRef::Bank(4),
            topology: Topology::Fan,
            jam: [Some(slot("distortion", &registry)), None, Some(b)],
            bpm: Some(97.5),
        };
        let mut sector = [0u8; SECTOR_SIZE];
        state.encode(&mut sector).unwrap();
        assert_eq!(PedalState::decode(&sector).unwrap(), state);
    }

    #[test]
    fn erased_or_corrupt_sectors_are_rejected() {
        let erased = [0xFFu8; SECTOR_SIZE];
        assert_eq!(PedalState::decode(&erased), Err(StateError::NoState));
        let mut sector = [0u8; SECTOR_SIZE];
        PedalState::default().encode(&mut sector).unwrap();
        sector[4] ^= 1;
        assert_eq!(PedalState::decode(&sector), Err(StateError::BadHeader));
    }

    #[test]
    fn rig_refs_round_trip() {
        for r in [
            RigRef::Jam,
            RigRef::Bank(0),
            RigRef::Bank(14),
            RigRef::Factory(2),
        ] {
            assert_eq!(RigRef::from_byte(r.to_byte()), r);
        }
    }
}
