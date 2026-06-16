//! Right-click parameter → macro-mapping action plumbing.
//!
//! A parameter knob's context menu lets the user bind that parameter to one of
//! the six performance macros (K1–K6) or clear its binding. The menu is drawn
//! deep inside an effect panel ([`GenericPanel`](crate::effects_ui::GenericPanel)),
//! but the macro map lives in the host application. The chosen action is
//! therefore surfaced through egui temp-data: [`param_macro_menu`] stashes it and
//! the host drains it once per frame with [`take_macro_action`]. This keeps
//! gui-core decoupled from the host's macro map while letting the standalone
//! editor and the plugin reuse the same menu.

use egui::{Id, Response};

use crate::{ParamIndex, SlotIndex};

/// Number of performance macros (K1–K6) the context menu offers.
pub const NUM_MACROS: usize = 6;

/// egui temp-data key under which the pending [`MacroAction`] is stashed.
const PENDING_ACTION: &str = "sonido.pending_macro_action";

/// A macro-binding action requested from a parameter's right-click menu.
///
/// Slot/param are stored as raw indices so the type is `Copy` and lifetime-free
/// — it round-trips cleanly through egui temp-data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MacroAction {
    /// Bind parameter `param` of `slot` to macro `macro_index` (0-based).
    Map {
        /// Chain slot index of the parameter's effect.
        slot: usize,
        /// Parameter index within that slot.
        param: usize,
        /// Destination macro, 0-based (`0` ⇒ K1).
        macro_index: usize,
    },
    /// Remove any macro binding for parameter `param` of `slot`.
    Clear {
        /// Chain slot index of the parameter's effect.
        slot: usize,
        /// Parameter index within that slot.
        param: usize,
    },
}

/// Attach a "Map to Macro 1–6 / Clear" context menu to a parameter widget.
///
/// Right-clicking `response` opens the menu; the chosen action is stashed in
/// egui temp-data for the host to drain with [`take_macro_action`]. Call this on
/// the [`Response`] of any bridged parameter widget (knob or combo).
pub fn param_macro_menu(response: &Response, slot: SlotIndex, param: ParamIndex) {
    response.context_menu(|ui| {
        ui.label(
            egui::RichText::new("Macro mapping")
                .font(egui::FontId::monospace(10.0))
                .weak(),
        );
        for m in 0..NUM_MACROS {
            if ui.button(format!("Map to Macro {}", m + 1)).clicked() {
                stash(
                    ui.ctx(),
                    MacroAction::Map {
                        slot: slot.0,
                        param: param.0,
                        macro_index: m,
                    },
                );
                ui.close_menu();
            }
        }
        ui.separator();
        if ui.button("Clear macro mapping").clicked() {
            stash(
                ui.ctx(),
                MacroAction::Clear {
                    slot: slot.0,
                    param: param.0,
                },
            );
            ui.close_menu();
        }
    });
}

/// Stash a pending action in egui temp-data.
///
/// Overwrites any prior pending action — at most one menu item is clicked per
/// frame, and the host drains it the same frame.
fn stash(ctx: &egui::Context, action: MacroAction) {
    ctx.data_mut(|d| d.insert_temp(Id::new(PENDING_ACTION), action));
}

/// Drain the pending [`MacroAction`], if any.
///
/// Call once per frame from the host after rendering parameter panels; returns
/// the action and clears it so it fires exactly once.
pub fn take_macro_action(ctx: &egui::Context) -> Option<MacroAction> {
    ctx.data_mut(|d| {
        let id = Id::new(PENDING_ACTION);
        let action = d.get_temp::<MacroAction>(id);
        if action.is_some() {
            d.remove::<MacroAction>(id);
        }
        action
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn take_returns_none_when_empty() {
        let ctx = egui::Context::default();
        assert!(take_macro_action(&ctx).is_none());
    }

    #[test]
    fn stash_then_take_roundtrips_and_fires_once() {
        let ctx = egui::Context::default();
        let action = MacroAction::Map {
            slot: 1,
            param: 2,
            macro_index: 3,
        };
        stash(&ctx, action);
        assert_eq!(take_macro_action(&ctx), Some(action));
        // Second drain is empty — the action fires exactly once.
        assert!(take_macro_action(&ctx).is_none());
    }

    #[test]
    fn stash_overwrites_prior_pending() {
        let ctx = egui::Context::default();
        stash(&ctx, MacroAction::Clear { slot: 0, param: 0 });
        let latest = MacroAction::Map {
            slot: 5,
            param: 1,
            macro_index: 0,
        };
        stash(&ctx, latest);
        assert_eq!(take_macro_action(&ctx), Some(latest));
    }
}
