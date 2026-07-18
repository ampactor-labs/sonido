//! Binary projection: one [`Patch`] ↔ one 4 KB QSPI sector.
//!
//! The same codec compiles into the host tools (encode + decode) and the
//! firmware (decode only), so there is no second hand-mirrored struct layout to
//! drift — the bug class that bit the legacy `DaisyPresetSlot`.
//!
//! # Layout
//!
//! ```text
//! Header (16 bytes)
//!   0   u32  magic = "SNDP"
//!   4   u16  format_version
//!   6   u16  flags (reserved, 0)
//!   8   u32  crc32 of the payload
//!   12  u16  payload_len
//!   14  u16  reserved (0)
//! Payload (payload_len bytes, starts at offset 16)
//!   name[24] · input_gain f32 · master_volume f32
//!   morph: speed f32 · mode u8 · slot_locks u8
//!   6 × macro: name[16] · count u8 · count × (target[3] · min f32 · max f32 · curve u8)
//!   node_count u8 · nodes: uid u16 · bypassed u8 · pcount u8 · A[pc]f32 · B[pc]f32
//!   edge_count u8 · edges: from[2] · to[2]
//! ```
//!
//! `decode` is fully bounds-checked and never panics: a truncated or corrupt
//! sector returns [`PatchError`], so the firmware can fall back to a known-good
//! patch instead of locking up.

#[cfg(not(feature = "std"))]
use alloc::{string::String, vec::Vec};

use sonido_core::{GlobalParam, MacroTarget, MorphCurve, MorphMode};

use crate::{
    GlobalControls, MACRO_NAME_LEN, MAX_NODES, MAX_PARAMS, MacroDef, MacroMappingSpec, MorphConfig,
    NUM_MACROS, PATCH_FORMAT_VERSION, PATCH_MAGIC, PATCH_NAME_LEN, Patch, PatchEdge, PatchEndpoint,
    PatchNode, SECTOR_SIZE,
};

const HEADER_LEN: usize = 16;

/// Errors from [`encode`] / [`decode`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PatchError {
    /// Output buffer is smaller than the encoded patch.
    BufferTooSmall,
    /// Input ended mid-field.
    Truncated,
    /// Magic word did not match [`PATCH_MAGIC`].
    BadMagic(u32),
    /// Format version is newer than this build understands.
    UnsupportedVersion(u16),
    /// CRC mismatch — the sector is corrupt.
    BadCrc {
        /// CRC stored in the header.
        expected: u32,
        /// CRC computed over the payload.
        actual: u32,
    },
    /// Encoded payload exceeds a 4 KB sector.
    TooLarge(usize),
    /// More than [`MAX_NODES`] nodes.
    TooManyNodes(usize),
    /// A node declares more than [`MAX_PARAMS`] parameters.
    TooManyParams(usize),
    /// A node's A/B snapshots differ in length.
    SnapshotLenMismatch,
    /// An enum tag byte was out of range. `what` names the field.
    BadTag {
        /// Field whose tag was invalid.
        what: &'static str,
        /// The offending byte.
        tag: u8,
    },
}

impl core::fmt::Display for PatchError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::BufferTooSmall => write!(f, "output buffer too small for patch"),
            Self::Truncated => write!(f, "patch data truncated"),
            Self::BadMagic(m) => write!(f, "bad magic 0x{m:08X} (expected SNDP)"),
            Self::UnsupportedVersion(v) => write!(f, "unsupported patch format version {v}"),
            Self::BadCrc { expected, actual } => {
                write!(
                    f,
                    "crc mismatch: header {expected:#010x} ≠ computed {actual:#010x}"
                )
            }
            Self::TooLarge(n) => write!(f, "payload {n} bytes exceeds 4 KB sector"),
            Self::TooManyNodes(n) => write!(f, "{n} nodes exceeds max {MAX_NODES}"),
            Self::TooManyParams(n) => write!(f, "{n} params exceeds max {MAX_PARAMS}"),
            Self::SnapshotLenMismatch => write!(f, "A/B snapshot length mismatch"),
            Self::BadTag { what, tag } => write!(f, "invalid {what} tag {tag}"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for PatchError {}

// ---------------------------------------------------------------------------
// CRC32 (IEEE 802.3, reflected). Table-less to keep the firmware rodata small.
// ---------------------------------------------------------------------------

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in bytes {
        crc ^= b as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

// ---------------------------------------------------------------------------
// Cursor helpers
// ---------------------------------------------------------------------------

struct Writer<'a> {
    buf: &'a mut [u8],
    pos: usize,
}

impl<'a> Writer<'a> {
    fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, pos: 0 }
    }
    fn u8(&mut self, v: u8) -> Result<(), PatchError> {
        let p = self.pos;
        *self.buf.get_mut(p).ok_or(PatchError::BufferTooSmall)? = v;
        self.pos += 1;
        Ok(())
    }
    fn bytes(&mut self, v: &[u8]) -> Result<(), PatchError> {
        let end = self.pos + v.len();
        self.buf
            .get_mut(self.pos..end)
            .ok_or(PatchError::BufferTooSmall)?
            .copy_from_slice(v);
        self.pos = end;
        Ok(())
    }
    fn u16(&mut self, v: u16) -> Result<(), PatchError> {
        self.bytes(&v.to_le_bytes())
    }
    fn u32(&mut self, v: u32) -> Result<(), PatchError> {
        self.bytes(&v.to_le_bytes())
    }
    fn f32(&mut self, v: f32) -> Result<(), PatchError> {
        self.bytes(&v.to_le_bytes())
    }
    /// Write `s` into a fixed `len`-byte NUL-padded field (UTF-8, truncated).
    fn fixed_str(&mut self, s: &str, len: usize) -> Result<(), PatchError> {
        let src = s.as_bytes();
        let n = src.len().min(len);
        // Truncate on a char boundary so we never split a multibyte sequence.
        let mut n = n;
        while n > 0 && (src[n - 1] & 0xC0) == 0x80 && n == src.len().min(len) && n < src.len() {
            n -= 1;
        }
        for i in 0..len {
            self.u8(if i < n { src[i] } else { 0 })?;
        }
        Ok(())
    }
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }
    fn u8(&mut self) -> Result<u8, PatchError> {
        let v = *self.buf.get(self.pos).ok_or(PatchError::Truncated)?;
        self.pos += 1;
        Ok(v)
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], PatchError> {
        let end = self.pos + n;
        let s = self.buf.get(self.pos..end).ok_or(PatchError::Truncated)?;
        self.pos = end;
        Ok(s)
    }
    fn u16(&mut self) -> Result<u16, PatchError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, PatchError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn f32(&mut self) -> Result<f32, PatchError> {
        Ok(f32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn fixed_str(&mut self, len: usize) -> Result<String, PatchError> {
        let raw = self.take(len)?;
        let end = raw.iter().position(|&b| b == 0).unwrap_or(len);
        Ok(String::from_utf8_lossy(&raw[..end]).into_owned())
    }
}

// ---------------------------------------------------------------------------
// Enum <-> byte
// ---------------------------------------------------------------------------

fn curve_to_u8(c: MorphCurve) -> u8 {
    match c {
        MorphCurve::Linear => 0,
        MorphCurve::Logarithmic => 1,
        MorphCurve::Snap => 2,
        _ => 0,
    }
}
fn curve_from_u8(b: u8) -> Result<MorphCurve, PatchError> {
    match b {
        0 => Ok(MorphCurve::Linear),
        1 => Ok(MorphCurve::Logarithmic),
        2 => Ok(MorphCurve::Snap),
        _ => Err(PatchError::BadTag {
            what: "curve",
            tag: b,
        }),
    }
}

fn mode_to_u8(m: MorphMode) -> u8 {
    match m {
        MorphMode::Ramp => 0,
        MorphMode::Momentary => 1,
        MorphMode::Latch => 2,
        _ => 0,
    }
}
fn mode_from_u8(b: u8) -> Result<MorphMode, PatchError> {
    match b {
        0 => Ok(MorphMode::Ramp),
        1 => Ok(MorphMode::Momentary),
        2 => Ok(MorphMode::Latch),
        _ => Err(PatchError::BadTag {
            what: "morph-mode",
            tag: b,
        }),
    }
}

fn global_to_u8(g: GlobalParam) -> u8 {
    match g {
        GlobalParam::InputGain => 0,
        GlobalParam::MasterVolume => 1,
        GlobalParam::MorphPosition => 2,
        GlobalParam::MorphSpeed => 3,
        _ => 0,
    }
}
fn global_from_u8(b: u8) -> Result<GlobalParam, PatchError> {
    match b {
        0 => Ok(GlobalParam::InputGain),
        1 => Ok(GlobalParam::MasterVolume),
        2 => Ok(GlobalParam::MorphPosition),
        3 => Ok(GlobalParam::MorphSpeed),
        _ => Err(PatchError::BadTag {
            what: "global-param",
            tag: b,
        }),
    }
}

fn write_target(w: &mut Writer, t: MacroTarget) -> Result<(), PatchError> {
    match t {
        MacroTarget::Slot { slot, param } => {
            w.u8(0)?;
            w.u8(slot)?;
            w.u8(param)?;
        }
        MacroTarget::Global(g) => {
            w.u8(1)?;
            w.u8(global_to_u8(g))?;
            w.u8(0)?;
        }
    }
    Ok(())
}
fn read_target(r: &mut Reader) -> Result<MacroTarget, PatchError> {
    let tag = r.u8()?;
    let a = r.u8()?;
    let b = r.u8()?;
    match tag {
        0 => Ok(MacroTarget::Slot { slot: a, param: b }),
        1 => Ok(MacroTarget::Global(global_from_u8(a)?)),
        _ => Err(PatchError::BadTag {
            what: "macro-target",
            tag,
        }),
    }
}

fn write_endpoint(w: &mut Writer, e: PatchEndpoint) -> Result<(), PatchError> {
    let (tag, idx) = match e {
        PatchEndpoint::Input => (0u8, 0u8),
        PatchEndpoint::Output => (1, 0),
        PatchEndpoint::Node(i) => (2, i),
        PatchEndpoint::Split(i) => (3, i),
        PatchEndpoint::Merge(i) => (4, i),
    };
    w.u8(tag)?;
    w.u8(idx)
}
fn read_endpoint(r: &mut Reader) -> Result<PatchEndpoint, PatchError> {
    let tag = r.u8()?;
    let idx = r.u8()?;
    match tag {
        0 => Ok(PatchEndpoint::Input),
        1 => Ok(PatchEndpoint::Output),
        2 => Ok(PatchEndpoint::Node(idx)),
        3 => Ok(PatchEndpoint::Split(idx)),
        4 => Ok(PatchEndpoint::Merge(idx)),
        _ => Err(PatchError::BadTag {
            what: "endpoint",
            tag,
        }),
    }
}

// ---------------------------------------------------------------------------
// encode / decode
// ---------------------------------------------------------------------------

/// Encode `patch` into `out` and return the total byte length written
/// (header + payload). `out` is typically a `[u8; 4096]` sector buffer.
///
/// # Errors
///
/// Caps are enforced here: [`TooManyNodes`](PatchError::TooManyNodes),
/// [`TooManyParams`](PatchError::TooManyParams),
/// [`SnapshotLenMismatch`](PatchError::SnapshotLenMismatch),
/// [`BufferTooSmall`](PatchError::BufferTooSmall), and
/// [`TooLarge`](PatchError::TooLarge) (payload > 4 KB sector).
pub fn encode(patch: &Patch, out: &mut [u8]) -> Result<usize, PatchError> {
    if patch.nodes.len() > MAX_NODES {
        return Err(PatchError::TooManyNodes(patch.nodes.len()));
    }
    for n in &patch.nodes {
        if n.params_a.len() != n.params_b.len() {
            return Err(PatchError::SnapshotLenMismatch);
        }
        if n.params_a.len() > MAX_PARAMS {
            return Err(PatchError::TooManyParams(n.params_a.len()));
        }
    }
    if out.len() < HEADER_LEN {
        return Err(PatchError::BufferTooSmall);
    }

    // Write the payload after the header, then backfill the header.
    let payload_len = {
        let mut w = Writer::new(&mut out[HEADER_LEN..]);
        w.fixed_str(&patch.name, PATCH_NAME_LEN)?;
        w.f32(patch.globals.input_gain_db)?;
        w.f32(patch.globals.master_volume_db)?;
        w.f32(patch.morph.speed)?;
        w.u8(mode_to_u8(patch.morph.mode))?;
        w.u8(patch.morph.slot_locks)?;

        for m in &patch.macros {
            w.fixed_str(&m.name, MACRO_NAME_LEN)?;
            // Mapping count fits in u8 (knob fan-out is small); clamp defensively.
            let count = m.mappings.len().min(u8::MAX as usize);
            w.u8(count as u8)?;
            for spec in m.mappings.iter().take(count) {
                write_target(&mut w, spec.target)?;
                w.f32(spec.min)?;
                w.f32(spec.max)?;
                w.u8(curve_to_u8(spec.curve))?;
            }
        }

        w.u8(patch.nodes.len() as u8)?;
        for n in &patch.nodes {
            w.u16(n.effect_uid)?;
            w.u8(n.bypassed as u8)?;
            w.u8(n.params_a.len() as u8)?;
            for &v in &n.params_a {
                w.f32(v)?;
            }
            for &v in &n.params_b {
                w.f32(v)?;
            }
        }

        w.u8(patch.edges.len().min(u8::MAX as usize) as u8)?;
        for e in patch.edges.iter().take(u8::MAX as usize) {
            write_endpoint(&mut w, e.from)?;
            write_endpoint(&mut w, e.to)?;
        }
        w.pos
    };

    let total = HEADER_LEN + payload_len;
    if total > SECTOR_SIZE {
        return Err(PatchError::TooLarge(payload_len));
    }
    if payload_len > u16::MAX as usize {
        return Err(PatchError::TooLarge(payload_len));
    }

    let crc = crc32(&out[HEADER_LEN..total]);
    let mut h = Writer::new(&mut out[..HEADER_LEN]);
    h.u32(PATCH_MAGIC)?;
    h.u16(PATCH_FORMAT_VERSION)?;
    h.u16(0)?; // flags
    h.u32(crc)?;
    h.u16(payload_len as u16)?;
    h.u16(0)?; // reserved

    Ok(total)
}

/// Decode a patch from `data` (header + payload). Fully bounds-checked.
///
/// # Errors
///
/// [`BadMagic`](PatchError::BadMagic),
/// [`UnsupportedVersion`](PatchError::UnsupportedVersion),
/// [`Truncated`](PatchError::Truncated), [`BadCrc`](PatchError::BadCrc), or a
/// [`BadTag`](PatchError::BadTag) for an out-of-range enum byte.
pub fn decode(data: &[u8]) -> Result<Patch, PatchError> {
    if data.len() < HEADER_LEN {
        return Err(PatchError::Truncated);
    }
    let mut h = Reader::new(&data[..HEADER_LEN]);
    let magic = h.u32()?;
    if magic != PATCH_MAGIC {
        return Err(PatchError::BadMagic(magic));
    }
    let version = h.u16()?;
    if version > PATCH_FORMAT_VERSION {
        return Err(PatchError::UnsupportedVersion(version));
    }
    let _flags = h.u16()?;
    let stored_crc = h.u32()?;
    let payload_len = h.u16()? as usize;

    let payload = data
        .get(HEADER_LEN..HEADER_LEN + payload_len)
        .ok_or(PatchError::Truncated)?;
    let actual_crc = crc32(payload);
    if actual_crc != stored_crc {
        return Err(PatchError::BadCrc {
            expected: stored_crc,
            actual: actual_crc,
        });
    }

    let mut r = Reader::new(payload);
    let name = r.fixed_str(PATCH_NAME_LEN)?;
    let globals = GlobalControls {
        input_gain_db: r.f32()?,
        master_volume_db: r.f32()?,
    };
    let morph = MorphConfig {
        speed: r.f32()?,
        mode: mode_from_u8(r.u8()?)?,
        slot_locks: r.u8()?,
    };

    let mut macros: [MacroDef; NUM_MACROS] = core::array::from_fn(|_| MacroDef::default());
    for m in macros.iter_mut() {
        m.name = r.fixed_str(MACRO_NAME_LEN)?;
        let count = r.u8()? as usize;
        let mut mappings = Vec::with_capacity(count);
        for _ in 0..count {
            let target = read_target(&mut r)?;
            let min = r.f32()?;
            let max = r.f32()?;
            let curve = curve_from_u8(r.u8()?)?;
            mappings.push(MacroMappingSpec {
                target,
                min,
                max,
                curve,
            });
        }
        m.mappings = mappings;
    }

    let node_count = r.u8()? as usize;
    if node_count > MAX_NODES {
        return Err(PatchError::TooManyNodes(node_count));
    }
    let mut nodes = Vec::with_capacity(node_count);
    for _ in 0..node_count {
        let effect_uid = r.u16()?;
        let bypassed = r.u8()? != 0;
        let param_count = r.u8()? as usize;
        if param_count > MAX_PARAMS {
            return Err(PatchError::TooManyParams(param_count));
        }
        let mut params_a = Vec::with_capacity(param_count);
        for _ in 0..param_count {
            params_a.push(r.f32()?);
        }
        let mut params_b = Vec::with_capacity(param_count);
        for _ in 0..param_count {
            params_b.push(r.f32()?);
        }
        nodes.push(PatchNode {
            effect_uid,
            bypassed,
            params_a,
            params_b,
        });
    }

    let edge_count = r.u8()? as usize;
    let mut edges = Vec::with_capacity(edge_count);
    for _ in 0..edge_count {
        let from = read_endpoint(&mut r)?;
        let to = read_endpoint(&mut r)?;
        edges.push(PatchEdge { from, to });
    }

    Ok(Patch {
        name,
        format_version: version,
        nodes,
        edges,
        macros,
        morph,
        globals,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PatchEndpoint;
    use alloc::vec;

    fn sample_patch() -> Patch {
        let mut p = Patch::linear_chain(
            "Test Rig",
            vec![
                PatchNode::new(1, vec![8.0, 0.0, 0.0]),
                PatchNode::new(11, vec![0.5, 0.7, 0.3, 0.0]),
            ],
        );
        p.nodes[0].params_b = vec![20.0, 3.0, -2.0];
        p.nodes[1].bypassed = true;
        p.morph.mode = MorphMode::Momentary;
        p.morph.speed = 3.5;
        p.morph.set_locked(1, true);
        p.globals.input_gain_db = -1.5;
        p.globals.master_volume_db = -3.0;
        p.macros[0].name = "Drive".into();
        p.macros[0].mappings.push(MacroMappingSpec {
            target: MacroTarget::Slot { slot: 0, param: 0 },
            min: 0.0,
            max: 40.0,
            curve: MorphCurve::Linear,
        });
        p.macros[1].name = "Space".into();
        p.macros[1].mappings.push(MacroMappingSpec {
            target: MacroTarget::Global(GlobalParam::MorphPosition),
            min: 0.0,
            max: 1.0,
            curve: MorphCurve::Logarithmic,
        });
        p
    }

    #[test]
    fn roundtrip_binary() {
        let p = sample_patch();
        let mut buf = [0u8; SECTOR_SIZE];
        let len = encode(&p, &mut buf).unwrap();
        assert!(len < SECTOR_SIZE);
        let back = decode(&buf[..len]).unwrap();
        assert_eq!(p, back);
        // Decoding the full padded sector also works (trailing zeros ignored
        // because payload_len is explicit).
        let back_full = decode(&buf).unwrap();
        assert_eq!(p, back_full);
    }

    #[test]
    fn bad_magic_rejected() {
        let p = sample_patch();
        let mut buf = [0u8; SECTOR_SIZE];
        let len = encode(&p, &mut buf).unwrap();
        buf[0] ^= 0xFF;
        assert!(matches!(decode(&buf[..len]), Err(PatchError::BadMagic(_))));
    }

    #[test]
    fn corrupt_payload_caught_by_crc() {
        let p = sample_patch();
        let mut buf = [0u8; SECTOR_SIZE];
        let len = encode(&p, &mut buf).unwrap();
        buf[HEADER_LEN + 30] ^= 0x01; // flip a payload bit
        assert!(matches!(
            decode(&buf[..len]),
            Err(PatchError::BadCrc { .. })
        ));
    }

    #[test]
    fn truncated_rejected() {
        let p = sample_patch();
        let mut buf = [0u8; SECTOR_SIZE];
        let len = encode(&p, &mut buf).unwrap();
        assert_eq!(decode(&buf[..len - 1]), Err(PatchError::Truncated));
        assert_eq!(decode(&buf[..8]), Err(PatchError::Truncated));
    }

    #[test]
    fn too_many_nodes_rejected_on_encode() {
        let mut p = Patch::new("big");
        p.nodes = (0..MAX_NODES + 1)
            .map(|_| PatchNode::new(1, vec![0.0]))
            .collect();
        let mut buf = [0u8; SECTOR_SIZE];
        assert_eq!(
            encode(&p, &mut buf),
            Err(PatchError::TooManyNodes(MAX_NODES + 1))
        );
    }

    #[test]
    fn empty_patch_roundtrips() {
        let p = Patch::new("empty");
        let mut buf = [0u8; SECTOR_SIZE];
        let len = encode(&p, &mut buf).unwrap();
        let back = decode(&buf[..len]).unwrap();
        assert_eq!(p, back);
        assert_eq!(back.edges.len(), 0);
        assert_eq!(back.nodes.len(), 0);
    }

    #[test]
    fn split_merge_endpoints_roundtrip() {
        let mut p = Patch::new("parallel");
        p.nodes = vec![PatchNode::new(6, vec![0.3]), PatchNode::new(11, vec![0.5])];
        p.edges = vec![
            PatchEdge::new(PatchEndpoint::Input, PatchEndpoint::Split(0)),
            PatchEdge::new(PatchEndpoint::Split(0), PatchEndpoint::Node(0)),
            PatchEdge::new(PatchEndpoint::Split(0), PatchEndpoint::Node(1)),
            PatchEdge::new(PatchEndpoint::Node(0), PatchEndpoint::Merge(0)),
            PatchEdge::new(PatchEndpoint::Node(1), PatchEndpoint::Merge(0)),
            PatchEdge::new(PatchEndpoint::Merge(0), PatchEndpoint::Output),
        ];
        let mut buf = [0u8; SECTOR_SIZE];
        let len = encode(&p, &mut buf).unwrap();
        assert_eq!(decode(&buf[..len]).unwrap(), p);
    }
}
