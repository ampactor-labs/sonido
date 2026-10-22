//! The graph player's editor: a RIG-family cabinet with a rig browser.
//!
//! The cabinet's controls are the player's own parameters (six macros, morph,
//! morph speed, input and master gain; the power button is the Bypass
//! param), plus one control that only exists here: **RIG**, a selector over
//! the rig library. Choosing a rig replaces the plugin's patch and asks the
//! host to restart the plugin, so `activate()` rebuilds the player off the
//! audio thread. The patch is the plugin's saved state, so the project
//! remembers the choice.
//!
//! The library is the factory rigs (every factory preset, as a patch) plus
//! the rig folder the Sonido editor exports to (Export ▸ CLAP;
//! [`sonido_config::paths::rig_library_dir`]), rescanned every couple of
//! seconds while the editor is open.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use sonido_core::{ParamDescriptor, ParamFlags};
use sonido_gui_core::cabinet::{Cabinet, CabinetIo, RigView};
use sonido_gui_core::param_bridge::{ParamBridge, ParamIndex, SlotIndex};
use sonido_gui_core::theme::Family;
use sonido_patch::Patch;
use sonido_registry::{EffectRegistry, effect_by_uid};

use crate::graph_player::GraphShared;
use crate::gui::EditorModel;

/// How often the open editor rescans the rig folder.
const RESCAN: Duration = Duration::from_secs(2);

/// The rig library folder (shared with the editor's Export ▸ CLAP).
pub fn rig_dir() -> Option<PathBuf> {
    sonido_config::paths::rig_library_dir()
}

/// One rig the browser can load.
#[derive(Clone, Debug, PartialEq)]
struct RigEntry {
    /// Shown in the selector.
    label: String,
    patch: Patch,
}

/// The rig library: every loadable rig, and the selector labels for it.
struct RigLibrary {
    entries: Vec<RigEntry>,
    /// Leaked once per change of the list (the descriptor wants `'static`);
    /// rescans that find the same rigs reuse them.
    labels: &'static [&'static str],
    scanned: Option<Instant>,
}

impl RigLibrary {
    fn new() -> Self {
        let mut lib = Self {
            entries: Vec::new(),
            labels: &[],
            scanned: None,
        };
        lib.rescan();
        lib
    }

    /// The built-in rigs: every factory preset, converted once.
    fn factory() -> Vec<RigEntry> {
        sonido_config::factory_patches(&EffectRegistry::new())
            .into_iter()
            .map(|c| RigEntry {
                label: format!("{} (factory)", c.patch.name),
                patch: c.patch,
            })
            .collect()
    }

    /// Factory rigs, then every `*.json` patch in the rig folder (unreadable
    /// files skipped), each group sorted by name.
    fn scan() -> Vec<RigEntry> {
        let mut entries = Self::factory();
        entries.extend(Self::scan_dir());
        entries
    }

    /// Every `*.json` patch in the rig folder.
    fn scan_dir() -> Vec<RigEntry> {
        let Some(dir) = rig_dir() else {
            return Vec::new();
        };
        let Ok(read) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut entries: Vec<RigEntry> = read
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .filter_map(|p| {
                let patch: Patch = serde_json::from_slice(&std::fs::read(&p).ok()?).ok()?;
                let label = if patch.name.trim().is_empty() {
                    p.file_stem()?.to_string_lossy().into_owned()
                } else {
                    patch.name.clone()
                };
                Some(RigEntry { label, patch })
            })
            .collect();
        entries.sort_by_key(|e| e.label.to_lowercase());
        entries
    }

    /// Rescan; returns whether the rig list changed.
    fn rescan(&mut self) -> bool {
        let entries = Self::scan();
        let changed = entries != self.entries;
        if entries != self.entries || self.labels.is_empty() {
            let labels: Vec<&'static str> = entries
                .iter()
                .map(|e| &*Box::leak(e.label.clone().into_boxed_str()))
                .collect();
            self.labels = Box::leak(labels.into_boxed_slice());
            self.entries = entries;
        }
        self.scanned = Some(Instant::now());
        changed
    }

    /// Rescan if [`RESCAN`] has passed; returns whether the rig list changed.
    fn rescan_if_due(&mut self) -> bool {
        self.scanned.is_none_or(|t| t.elapsed() >= RESCAN) && self.rescan()
    }

    /// Index of the entry matching `patch`, if any.
    fn position_of(&self, patch: &Patch) -> Option<usize> {
        self.entries.iter().position(|e| e.patch == *patch)
    }
}

/// What the RIG selector shows: its labels, where the library starts in them,
/// and which one is current. Rebuilt only when the library or the patch
/// changes, so its labels are leaked once per change, not per frame.
struct RigChoice {
    labels: &'static [&'static str],
    /// 1 when the project's own rig (not in the library) is listed first.
    offset: usize,
    current: usize,
}

impl RigChoice {
    fn new(lib: &RigLibrary, patch: &Patch) -> Self {
        if lib.entries.is_empty() && patch.nodes.is_empty() {
            return Self {
                labels: &["NONE SAVED"],
                offset: 0,
                current: 0,
            };
        }
        if let Some(i) = lib.position_of(patch) {
            return Self {
                labels: lib.labels,
                offset: 0,
                current: i,
            };
        }
        // The project's own rig isn't in the library (or there is none yet):
        // list it first.
        let first = if patch.nodes.is_empty() {
            "NO RIG".to_owned()
        } else {
            format!("{} (project)", patch.name)
        };
        let mut labels: Vec<&'static str> = vec![Box::leak(first.into_boxed_str())];
        labels.extend_from_slice(lib.labels);
        Self {
            labels: Box::leak(labels.into_boxed_slice()),
            offset: 1,
            current: 0,
        }
    }
}

/// The graph player's editor model (see the [module docs](self)).
pub struct GraphModel {
    shared: GraphShared,
    library: Mutex<RigLibrary>,
    choice: Mutex<RigChoice>,
    /// Patch generation the rig view and choice were last built from.
    shown: Mutex<Option<u64>>,
}

impl GraphModel {
    /// The editor model for one graph-player instance.
    pub fn new(shared: GraphShared) -> Self {
        let library = RigLibrary::new();
        let choice = RigChoice::new(&library, &shared.patch());
        Self {
            shared,
            library: Mutex::new(library),
            choice: Mutex::new(choice),
            shown: Mutex::new(None),
        }
    }

    /// The parameters the cabinet's deck shows (for sizing the window before
    /// any editor exists): the player's params minus Bypass, plus RIG.
    pub fn deck_descriptors(shared: &GraphShared) -> Vec<ParamDescriptor> {
        let mut v: Vec<ParamDescriptor> = shared
            .descriptors()
            .iter()
            .filter(|d| {
                Some(d.id)
                    != shared
                        .bypass_index()
                        .and_then(|i| shared.descriptor(i))
                        .map(|d| d.id)
            })
            .cloned()
            .collect();
        v.push(rig_descriptor(&["A RIG NAME OF TYPICAL LENGTH"]));
        v
    }

    fn rig_view(&self) -> RigView {
        let patch = self.shared.patch();
        let registry = EffectRegistry::new();
        let chain = patch
            .nodes
            .iter()
            .filter_map(|n| effect_by_uid(n.effect_uid))
            .map(|id| {
                let short = registry.descriptor(id).map_or(id, |d| d.short_name);
                (short.to_owned(), Family::of_effect(id))
            })
            .collect();
        let empty_hint = match rig_dir() {
            Some(dir) => format!(
                "Pick a rig in RIG below, or export your own from the Sonido editor (Export > CLAP) to {}",
                dir.display()
            ),
            None => "Pick a rig in RIG below, or export your own from the Sonido editor".to_owned(),
        };
        RigView {
            name: patch.name.clone(),
            chain,
            empty_hint,
        }
    }

    /// Index of the virtual RIG param (after the player's own).
    fn rig_index(&self) -> usize {
        self.shared.param_count()
    }
}

/// The RIG selector's descriptor over `labels`.
fn rig_descriptor(labels: &'static [&'static str]) -> ParamDescriptor {
    let max = labels.len().saturating_sub(1) as f32;
    ParamDescriptor::custom("Rig", "Rig", 0.0, max.max(1.0), 0.0)
        .with_step(1.0)
        .with_flags(ParamFlags::STEPPED)
        .with_step_labels(labels)
}

impl ParamBridge for GraphModel {
    fn slot_count(&self) -> usize {
        1
    }

    #[allow(clippy::unnecessary_literal_bound)] // the trait's signature
    fn effect_id(&self, _slot: SlotIndex) -> &str {
        "graph-player"
    }

    fn param_count(&self, _slot: SlotIndex) -> usize {
        self.shared.param_count() + 1
    }

    fn param_descriptor(&self, _slot: SlotIndex, param: ParamIndex) -> Option<ParamDescriptor> {
        if param.0 == self.rig_index() {
            return Some(rig_descriptor(self.choice.lock().unwrap().labels));
        }
        let mut desc = *self.shared.descriptor(param.0)?;
        // The power button is Bypass; don't also show it as a deck control.
        if Some(param.0) == self.shared.bypass_index() {
            desc.flags = desc.flags.union(ParamFlags::HIDDEN);
        }
        Some(desc)
    }

    fn get(&self, _slot: SlotIndex, param: ParamIndex) -> f32 {
        if param.0 == self.rig_index() {
            return self.choice.lock().unwrap().current as f32;
        }
        self.shared.get_value(param.0).unwrap_or(0.0)
    }

    fn set(&self, _slot: SlotIndex, param: ParamIndex, value: f32) {
        if param.0 == self.rig_index() {
            let offset = self.choice.lock().unwrap().offset;
            let Some(i) = (value.round() as usize).checked_sub(offset) else {
                return; // the first ("(project)" / "NO RIG") entry: already loaded
            };
            let lib = self.library.lock().unwrap();
            if let Some(entry) = lib.entries.get(i) {
                self.shared.set_patch(entry.patch.clone());
                self.shared.request_restart();
                self.shared.notify_host();
            }
            return;
        }
        self.shared.set_value(param.0, value);
        self.shared.notify_host();
    }

    fn is_bypassed(&self, _slot: SlotIndex) -> bool {
        self.shared
            .bypass_index()
            .and_then(|i| self.shared.get_value(i))
            .is_some_and(|v| v > 0.5)
    }

    fn set_bypassed(&self, _slot: SlotIndex, bypassed: bool) {
        if let Some(i) = self.shared.bypass_index() {
            self.shared.gesture_begin(i);
            self.shared.set_value(i, if bypassed { 1.0 } else { 0.0 });
            self.shared.gesture_end(i);
            self.shared.notify_host();
        }
    }

    fn begin_set(&self, _slot: SlotIndex, param: ParamIndex) {
        if param.0 < self.rig_index() {
            self.shared.gesture_begin(param.0);
            self.shared.notify_host();
        }
    }

    fn end_set(&self, _slot: SlotIndex, param: ParamIndex) {
        if param.0 < self.rig_index() {
            self.shared.gesture_end(param.0);
            self.shared.notify_host();
        }
    }
}

impl EditorModel for GraphModel {
    fn bridge(&self) -> &dyn ParamBridge {
        self
    }

    fn io(&self) -> CabinetIo<'_> {
        CabinetIo {
            tap: Some(self.shared.tap()),
            sample_rate: self.shared.sample_rate(),
            latency: self.shared.latency_samples(),
        }
    }

    fn new_cabinet(&self) -> Cabinet {
        Cabinet::custom(
            "graph-player",
            "Graph Player",
            "Plays a whole Sonido rig: macros, A/B morph and gains",
            Family::Rig,
        )
    }

    fn prepare(&self, cabinet: &mut Cabinet) {
        let mut lib = self.library.lock().unwrap();
        let rescanned = lib.rescan_if_due();
        let generation = self.shared.generation();
        let mut shown = self.shown.lock().unwrap();
        if rescanned || *shown != Some(generation) {
            *shown = Some(generation);
            let patch = self.shared.patch();
            *self.choice.lock().unwrap() = RigChoice::new(&lib, &patch);
            cabinet.set_rig(Some(self.rig_view()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_starts_with_every_factory_rig() {
        let factory = RigLibrary::factory();
        assert_eq!(factory.len(), sonido_config::factory_preset_names().len());
        assert!(factory.iter().all(|e| !e.patch.nodes.is_empty()));
        let lib = RigLibrary::new();
        assert_eq!(&lib.entries[..factory.len()], &factory[..]);
        assert_eq!(lib.labels.len(), lib.entries.len());
    }

    #[test]
    fn rig_choice_lists_unknown_rigs_first_and_finds_known_ones() {
        let lib = RigLibrary::new();
        let fresh = RigChoice::new(&lib, &Patch::new("graph"));
        assert_eq!(
            (fresh.offset, fresh.current, fresh.labels[0]),
            (1, 0, "NO RIG")
        );
        let known = RigChoice::new(&lib, &lib.entries[2].patch);
        assert_eq!((known.offset, known.current), (0, 2));
    }

    #[test]
    fn choosing_a_rig_loads_it_and_asks_for_a_restart() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let restarts = Arc::new(AtomicUsize::new(0));
        let r = Arc::clone(&restarts);
        let shared = GraphShared::new(
            None,
            Some(Box::new(move || {
                r.fetch_add(1, Ordering::SeqCst);
            })),
        );
        let model = GraphModel::new(shared.clone());
        let rig = model.rig_index();
        // Fresh plugin: "NO RIG" sits at 0, so library entry 1 is index 2.
        model.set(SlotIndex(0), ParamIndex(rig), 2.0);
        let lib = model.library.lock().unwrap();
        assert_eq!(shared.patch(), lib.entries[1].patch);
        assert_eq!(restarts.load(Ordering::SeqCst), 1);
    }
}
