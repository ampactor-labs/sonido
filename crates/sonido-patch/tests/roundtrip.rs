//! Cross-projection roundtrip tests: a `Patch` survives JSON and binary intact,
//! and the two projections agree.

use proptest::prelude::*;
use sonido_patch::{
    GlobalControls, GlobalParam, MAX_NODES, MAX_PARAMS, MacroMappingSpec, MacroTarget, MorphConfig,
    MorphCurve, MorphMode, NUM_MACROS, Patch, PatchEdge, PatchEndpoint, PatchNode, SECTOR_SIZE,
    binary,
};

#[test]
fn json_roundtrip_explicit() {
    let mut p = Patch::linear_chain(
        "JSON Rig",
        vec![
            PatchNode::new(1, vec![8.0, 0.0]),
            PatchNode::new(6, vec![300.0, 0.4, 0.5]),
        ],
    );
    p.nodes[1].params_b = vec![600.0, 0.6, 0.5];
    p.morph.mode = MorphMode::Latch;
    p.macros[2].name = "Wet".into();
    p.macros[2].mappings.push(MacroMappingSpec {
        target: MacroTarget::Global(GlobalParam::MasterVolume),
        min: -12.0,
        max: 0.0,
        curve: MorphCurve::Linear,
    });

    let json = serde_json::to_string_pretty(&p).unwrap();
    let back: Patch = serde_json::from_str(&json).unwrap();
    assert_eq!(p, back);
}

#[test]
fn json_and_binary_agree() {
    let mut p = Patch::linear_chain("Agree", vec![PatchNode::new(11, vec![0.5, 0.7, 0.3])]);
    p.nodes[0].params_b = vec![0.9, 0.2, 0.8];

    let json_back: Patch = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
    let mut buf = [0u8; SECTOR_SIZE];
    let len = binary::encode(&p, &mut buf).unwrap();
    let bin_back = binary::decode(&buf[..len]).unwrap();

    assert_eq!(json_back, bin_back);
}

// --- Property: any well-formed patch survives a binary roundtrip ------------

fn arb_curve() -> impl Strategy<Value = MorphCurve> {
    prop_oneof![
        Just(MorphCurve::Linear),
        Just(MorphCurve::Logarithmic),
        Just(MorphCurve::Snap),
    ]
}

fn arb_mode() -> impl Strategy<Value = MorphMode> {
    prop_oneof![
        Just(MorphMode::Ramp),
        Just(MorphMode::Momentary),
        Just(MorphMode::Latch),
    ]
}

prop_compose! {
    fn arb_node()(
        uid in 1u16..=35,
        bypassed in any::<bool>(),
        pc in 0usize..=MAX_PARAMS,
    )(
        params_a in prop::collection::vec(-1e3f32..1e3, pc),
        params_b in prop::collection::vec(-1e3f32..1e3, pc),
        uid in Just(uid),
        bypassed in Just(bypassed),
    ) -> PatchNode {
        PatchNode { effect_uid: uid, bypassed, params_a, params_b }
    }
}

prop_compose! {
    // Name capped at PATCH_NAME_LEN (24): longer names are truncated in the
    // binary projection by design, so they are not part of the lossless contract.
    fn arb_patch()(
        name in "[ -~]{0,24}",
        nodes in prop::collection::vec(arb_node(), 0..=MAX_NODES),
        speed in 0.1f32..10.0,
        mode in arb_mode(),
        locks in any::<u8>(),
        in_gain in -24.0f32..24.0,
        master in -40.0f32..6.0,
        curve in arb_curve(),
        global in prop::bool::ANY,
    ) -> Patch {
        let n = nodes.len();
        let mut p = Patch::linear_chain(name, nodes);
        p.morph = MorphConfig { speed, mode, slot_locks: locks };
        p.globals = GlobalControls { input_gain_db: in_gain, master_volume_db: master };
        // One macro mapping that always validates: a global target, or a real
        // slot when the chain is non-empty.
        let target = if global || n == 0 {
            MacroTarget::Global(GlobalParam::MorphPosition)
        } else {
            MacroTarget::Slot { slot: (n as u8) - 1, param: 0 }
        };
        p.macros[0].name = "M1".into();
        p.macros[0].mappings.push(MacroMappingSpec { target, min: 0.0, max: 1.0, curve });
        p
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(400))]

    #[test]
    fn binary_roundtrip_is_lossless(p in arb_patch()) {
        let mut buf = [0u8; SECTOR_SIZE];
        let len = binary::encode(&p, &mut buf).unwrap();
        prop_assert!(len <= SECTOR_SIZE);
        let back = binary::decode(&buf[..len]).unwrap();
        prop_assert_eq!(p, back);
    }

    #[test]
    fn json_roundtrip_is_lossless(p in arb_patch()) {
        let json = serde_json::to_string(&p).unwrap();
        let back: Patch = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(p, back);
    }

    // Any byte soup either decodes to something re-encodable or errors cleanly —
    // it must never panic (the firmware reads possibly-corrupt flash).
    #[test]
    fn decode_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..600)) {
        let _ = binary::decode(&bytes);
    }
}

#[test]
fn worst_case_patch_fits_one_sector() {
    // 8 nodes × 16 params (A+B) + 6 macros × 8 mappings + 24 edges: the design's
    // worst case must fit a single 4 KB sector with headroom.
    let nodes: Vec<PatchNode> = (0..MAX_NODES)
        .map(|i| {
            let mut n = PatchNode::new((i as u16) + 1, vec![0.5; MAX_PARAMS]);
            n.params_b = vec![0.7; MAX_PARAMS];
            n
        })
        .collect();
    let mut p = Patch::linear_chain("Worst Case Patch Name 24c", nodes);
    for mi in 0..NUM_MACROS {
        p.macros[mi].name = "MacroNameSixteenC".into();
        for _ in 0..8 {
            p.macros[mi].mappings.push(MacroMappingSpec {
                target: MacroTarget::Slot { slot: 0, param: 0 },
                min: 0.0,
                max: 1.0,
                curve: MorphCurve::Logarithmic,
            });
        }
    }
    // Pad edges toward the worst case.
    for i in 0..(MAX_NODES - 1) {
        p.edges.push(PatchEdge::new(
            PatchEndpoint::Node(i as u8),
            PatchEndpoint::Split(0),
        ));
    }

    let mut buf = [0u8; SECTOR_SIZE];
    let len = binary::encode(&p, &mut buf).expect("worst case must fit a sector");
    assert!(len <= SECTOR_SIZE, "len {len} exceeds sector");
    // Confirm comfortable headroom (design claim: < 2 KB).
    assert!(
        len < 2048,
        "worst-case patch is {len} bytes, expected < 2048"
    );
}
