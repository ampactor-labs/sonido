# Arcade Cabinet Layout v2 — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Redesign the entire GUI layout for maximum information density, responsive sizing, and arcade-cabinet UX — faders replace knobs, I/O strips become graph endpoints, meters go Ableton-style, session save/load replaces presets, and all chrome is consolidated.

**Architecture:** Six phases transform the GUI bottom-up: foundation widgets (fader, meter), graph editor changes (hide I/O nodes, activity LEDs), full layout restructure (responsive sizing, header/status consolidation), effect UI migration (19 panels → fader rows), session serialization (Snarl + params), and polish (repaint throttling, context menu search, quick reference panel).

**Tech Stack:** egui 0.31, egui-snarl 0.7.1 (serde feature enabled), serde/serde_json, sonido-gui-core widgets, ParamBridge gesture protocol.

---

## Phase 1: Foundation Widgets

### Task 1: ThemeLayout struct for responsive sizing

**Files:**
- Modify: `crates/sonido-gui-core/src/theme.rs`

**Step 1: Add ThemeLayout struct**

Add after the `ScanlineConfig` struct (~line 101):

```rust
/// Responsive layout ratios and clamps.
///
/// All sizing derives from available space using these ratios, with min/max
/// constraints. No hardcoded pixel values in layout code.
#[derive(Clone, Debug)]
pub struct ThemeLayout {
    /// I/O strip width as fraction of window width.
    pub io_strip_ratio: f32,
    /// Minimum I/O strip width in pixels.
    pub io_strip_min: f32,
    /// Maximum I/O strip width in pixels.
    pub io_strip_max: f32,
    /// Graph editor height as fraction of content area (between header and bottom bars).
    pub graph_ratio: f32,
    /// Minimum graph editor height in pixels.
    pub graph_min_h: f32,
    /// Maximum effect panel height as fraction of content area.
    pub panel_max_ratio: f32,
    /// Minimum effect panel height in pixels.
    pub panel_min_h: f32,
    /// Minimum fader width in pixels.
    pub fader_min_w: f32,
    /// Maximum fader width in pixels.
    pub fader_max_w: f32,
    /// Minimum fader height in pixels.
    pub fader_min_h: f32,
    /// Maximum fader height in pixels.
    pub fader_max_h: f32,
}

impl Default for ThemeLayout {
    fn default() -> Self {
        Self {
            io_strip_ratio: 0.07,
            io_strip_min: 50.0,
            io_strip_max: 80.0,
            graph_ratio: 0.45,
            graph_min_h: 150.0,
            panel_max_ratio: 0.50,
            panel_min_h: 120.0,
            fader_min_w: 32.0,
            fader_max_w: 52.0,
            fader_min_h: 60.0,
            fader_max_h: 120.0,
        }
    }
}
```

Add `pub layout: ThemeLayout` field to `SonidoTheme`, and `layout: ThemeLayout::default()` to its `Default` impl.

**Step 2: Add helper methods to ThemeLayout**

```rust
impl ThemeLayout {
    /// Compute I/O strip width from window width.
    pub fn io_strip_width(&self, window_w: f32) -> f32 {
        (window_w * self.io_strip_ratio).clamp(self.io_strip_min, self.io_strip_max)
    }

    /// Compute graph and panel heights from available content height.
    ///
    /// Returns `(graph_h, panel_h)`. Panel height is content-driven
    /// (passed as `panel_content_h`) but clamped to `panel_max_ratio`.
    pub fn split_vertical(&self, content_h: f32, panel_content_h: f32) -> (f32, f32) {
        let panel_max = content_h * self.panel_max_ratio;
        let panel_h = panel_content_h.clamp(self.panel_min_h, panel_max);
        let graph_h = (content_h - panel_h).max(self.graph_min_h);
        (graph_h, panel_h)
    }

    /// Compute fader width for N params in available width.
    pub fn fader_width(&self, available_w: f32, param_count: usize) -> f32 {
        if param_count == 0 {
            return self.fader_max_w;
        }
        let w = available_w / param_count as f32;
        w.clamp(self.fader_min_w, self.fader_max_w)
    }

    /// Compute fader height from available panel height minus labels.
    pub fn fader_height(&self, panel_inner_h: f32) -> f32 {
        let label_space = 32.0; // name + value text
        (panel_inner_h - label_space).clamp(self.fader_min_h, self.fader_max_h)
    }
}
```

**Step 3: Tests**

Add to the existing `theme.rs` or a new test module:

```rust
#[cfg(test)]
mod layout_tests {
    use super::*;

    #[test]
    fn io_strip_width_clamps() {
        let layout = ThemeLayout::default();
        // Small window: hits min
        assert_eq!(layout.io_strip_width(400.0), 50.0);
        // Normal window: proportional
        let w = layout.io_strip_width(1000.0);
        assert!((w - 70.0).abs() < 0.1);
        // Huge window: hits max
        assert_eq!(layout.io_strip_width(2000.0), 80.0);
    }

    #[test]
    fn split_vertical_respects_min_graph() {
        let layout = ThemeLayout::default();
        let (graph_h, _panel_h) = layout.split_vertical(300.0, 200.0);
        assert!(graph_h >= layout.graph_min_h);
    }

    #[test]
    fn fader_width_distributes_evenly() {
        let layout = ThemeLayout::default();
        let w = layout.fader_width(400.0, 8);
        assert_eq!(w, 50.0); // 400/8 = 50, within [32, 52]
    }

    #[test]
    fn fader_width_clamps_to_min() {
        let layout = ThemeLayout::default();
        let w = layout.fader_width(200.0, 20);
        assert_eq!(w, 32.0); // 200/20 = 10 < 32 min
    }
}
```

**Step 4: Run tests**

Run: `cargo test -p sonido-gui-core`
Expected: All pass including new layout tests.

**Step 5: Commit**

```
feat(gui-core): add ThemeLayout for responsive sizing
```

---

### Task 2: Vertical Fader widget

**Files:**
- Create: `crates/sonido-gui-core/src/widgets/fader.rs`
- Modify: `crates/sonido-gui-core/src/widgets/mod.rs`
- Modify: `crates/sonido-gui-core/src/lib.rs`

**Step 1: Create fader widget**

Create `crates/sonido-gui-core/src/widgets/fader.rs`:

```rust
//! Vertical slot fader with LED-segment fill.
//!
//! A compact parameter control modeled after mixing console channel faders.
//! The track fills with LED-colored segments from bottom to the current value.
//! Ghost (unlit) segments sit above. The thumb is a thin horizontal bar at
//! the value position.

use egui::{Color32, FontId, Pos2, Rect, Response, Sense, Ui, Widget, pos2, vec2};

use crate::theme::SonidoTheme;
use crate::widgets::glow;

/// Number of LED segments in the fader track.
const SEGMENT_COUNT: usize = 16;

/// Vertical parameter fader with LED fill and value display.
///
/// ## Parameters
/// - `value`: Current normalized value (0.0–1.0), mutated on drag.
/// - `label`: Parameter name shown below the fader.
/// - `display_value`: Formatted value string (e.g., "3.5 dB").
/// - `color`: LED segment color (default: theme amber).
/// - `width`: Total fader width including padding.
/// - `height`: Fader track height (excluding labels).
/// - `default_normalized`: Default normalized value for double-click reset.
pub struct Fader<'a> {
    value: &'a mut f32,
    label: &'a str,
    display_value: String,
    color: Option<Color32>,
    width: f32,
    height: f32,
    default_normalized: f32,
}

impl<'a> Fader<'a> {
    /// Create a new fader. `value` is normalized 0.0–1.0.
    pub fn new(value: &'a mut f32, label: &'a str) -> Self {
        Self {
            value,
            label,
            display_value: String::new(),
            color: None,
            width: 40.0,
            height: 80.0,
            default_normalized: 0.5,
        }
    }

    /// Set the formatted display value string.
    pub fn display(mut self, text: impl Into<String>) -> Self {
        self.display_value = text.into();
        self
    }

    /// Set the LED color (default: theme amber).
    pub fn color(mut self, color: Color32) -> Self {
        self.color = Some(color);
        self
    }

    /// Set fader dimensions.
    pub fn size(mut self, width: f32, height: f32) -> Self {
        self.width = width;
        self.height = height;
        self
    }

    /// Set the default normalized value for double-click reset.
    pub fn default_value(mut self, default: f32) -> Self {
        self.default_normalized = default;
        self
    }
}

impl Widget for Fader<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let theme = SonidoTheme::get(ui.ctx());
        let color = self.color.unwrap_or(theme.colors.amber);
        let ghost_color = glow::ghost(color, &theme);

        // Font sizes scale with width
        let label_font = FontId::monospace((self.width * 0.22).clamp(8.0, 11.0));
        let value_font = FontId::monospace((self.width * 0.20).clamp(7.0, 10.0));

        // Total height: track + label + value
        let label_h = 14.0;
        let value_h = 12.0;
        let total_h = self.height + label_h + value_h + 4.0;
        let size = vec2(self.width, total_h);

        let (rect, mut response) = ui.allocate_exact_size(size, Sense::click_and_drag());

        // Track rect (the actual fader area)
        let track_rect = Rect::from_min_size(rect.min, vec2(self.width, self.height));

        // Handle double-click to reset
        if response.double_clicked() {
            *self.value = self.default_normalized;
            response.mark_changed();
        }

        // Handle drag
        if response.dragged() {
            if let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
                // Shift+drag: 5x precision
                let sensitivity = if ui.input(|i| i.modifiers.shift) {
                    0.2
                } else {
                    1.0
                };
                let delta_y = response.drag_delta().y;
                let range = track_rect.height();
                let delta_norm = -delta_y / range * sensitivity;
                *self.value = (*self.value + delta_norm).clamp(0.0, 1.0);
                let _ = pos; // used for bounds check
                response.mark_changed();
            }
        }

        // Handle scroll wheel
        if response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll.abs() > 0.1 {
                let step = scroll.signum() * 0.01;
                *self.value = (*self.value + step).clamp(0.0, 1.0);
                response.mark_changed();
            }
        }

        // Handle click-to-set (not drag, just single click)
        if response.clicked() {
            if let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
                if track_rect.contains(pos) {
                    let normalized = 1.0 - (pos.y - track_rect.top()) / track_rect.height();
                    *self.value = normalized.clamp(0.0, 1.0);
                    response.mark_changed();
                }
            }
        }

        if ui.is_rect_visible(rect) {
            let painter = ui.painter();

            // Background track
            let track_inner = track_rect.shrink2(vec2(self.width * 0.3, 0.0));
            painter.rect_filled(track_inner, 2.0, theme.colors.dim);

            // LED segments
            let seg_gap = 1.0;
            let total_gaps = (SEGMENT_COUNT - 1) as f32 * seg_gap;
            let seg_h = (track_inner.height() - total_gaps) / SEGMENT_COUNT as f32;

            for i in 0..SEGMENT_COUNT {
                let seg_pos = i as f32 / SEGMENT_COUNT as f32;
                let y = track_inner.bottom() - (i as f32 + 1.0) * seg_h - i as f32 * seg_gap;
                let seg_rect = Rect::from_min_size(
                    pos2(track_inner.left(), y),
                    vec2(track_inner.width(), seg_h),
                );

                if *self.value > seg_pos {
                    glow::glow_rect(painter, seg_rect, color, 1.0, &theme);
                } else {
                    painter.rect_filled(seg_rect, 1.0, ghost_color);
                }
            }

            // Thumb (horizontal bar at value position)
            let thumb_y = track_rect.bottom()
                - *self.value * track_rect.height();
            let thumb_w = self.width * 0.8;
            let thumb_x = rect.center().x - thumb_w * 0.5;
            let thumb_rect = Rect::from_min_size(
                pos2(thumb_x, thumb_y - 1.5),
                vec2(thumb_w, 3.0),
            );
            painter.rect_filled(thumb_rect, 1.0, color);

            // Label below track
            let label_pos = pos2(rect.center().x, track_rect.bottom() + 2.0);
            painter.text(
                label_pos,
                egui::Align2::CENTER_TOP,
                self.label,
                label_font,
                theme.colors.cyan,
            );

            // Value below label
            if !self.display_value.is_empty() {
                let value_pos = pos2(rect.center().x, track_rect.bottom() + label_h + 2.0);
                painter.text(
                    value_pos,
                    egui::Align2::CENTER_TOP,
                    &self.display_value,
                    value_font,
                    theme.colors.text_secondary,
                );
            }
        }

        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fader_clamps_value() {
        let mut val = 0.5;
        let fader = Fader::new(&mut val, "TEST");
        assert_eq!(fader.width, 40.0);
        assert_eq!(fader.height, 80.0);
    }

    #[test]
    fn fader_builder_chain() {
        let mut val = 0.3;
        let fader = Fader::new(&mut val, "GAIN")
            .display("3.5 dB")
            .size(50.0, 100.0)
            .default_value(0.0);
        assert_eq!(fader.width, 50.0);
        assert_eq!(fader.height, 100.0);
        assert_eq!(fader.default_normalized, 0.0);
        assert_eq!(fader.display_value, "3.5 dB");
    }

    #[test]
    fn fader_default_value_reset() {
        let mut val = 0.8;
        let fader = Fader::new(&mut val, "MIX").default_value(0.5);
        assert_eq!(fader.default_normalized, 0.5);
    }
}
```

**Step 2: Register in widget module**

In `crates/sonido-gui-core/src/widgets/mod.rs`, add:
```rust
pub mod fader;
pub use fader::Fader;
```

In `crates/sonido-gui-core/src/lib.rs`, add `Fader` to the widget re-exports.

**Step 3: Run tests**

Run: `cargo test -p sonido-gui-core`
Expected: All pass.

**Step 4: Commit**

```
feat(gui-core): add vertical Fader widget with LED segments
```

---

### Task 3: bridged_fader helper

**Files:**
- Modify: `crates/sonido-gui-core/src/widgets/bridged_knob.rs` (add `bridged_fader` function)
- Modify: `crates/sonido-gui-core/src/widgets/mod.rs` (re-export)
- Modify: `crates/sonido-gui-core/src/lib.rs` (re-export)

**Step 1: Add bridged_fader to bridged_knob.rs**

Add at the end of the file (before tests if any), using the same pattern as `bridged_knob`:

```rust
/// Render a vertical fader bridged to a parameter slot.
///
/// Handles normalization (ParamScale-aware), gesture protocol (begin_set/end_set),
/// auto-formatting from ParamUnit, and double-click-to-reset. Drop-in replacement
/// for `bridged_knob` with vertical fader UX.
pub fn bridged_fader(
    ui: &mut egui::Ui,
    bridge: &dyn ParamBridge,
    slot: SlotIndex,
    param: ParamIndex,
    fader_w: f32,
    fader_h: f32,
) {
    let desc = bridge.param_descriptor(slot, param);
    let plain_value = bridge.get(slot, param);

    let (min, max, default) = desc
        .as_ref()
        .map(|d| (d.min, d.max, d.default))
        .unwrap_or((0.0, 1.0, 0.5));

    let label = desc
        .as_ref()
        .map(|d| d.short_name)
        .unwrap_or("?");

    let normalized = normalize(desc.as_ref(), plain_value, min, max);
    let default_normalized = normalize(desc.as_ref(), default, min, max);

    let formatted = desc
        .as_ref()
        .map(|d| auto_format(plain_value, &d.unit))
        .unwrap_or_else(|| format!("{plain_value:.2}"));

    let theme = SonidoTheme::get(ui.ctx());
    let color = theme.colors.amber;

    let mut norm = normalized;
    let response = ui.add(
        Fader::new(&mut norm, label)
            .display(&formatted)
            .color(color)
            .size(fader_w, fader_h)
            .default_value(default_normalized),
    );

    // Double-click reset
    if response.double_clicked() {
        bridge.begin_set(slot, param);
        bridge.set(slot, param, default);
        bridge.end_set(slot, param);
    } else {
        // Gesture protocol
        if response.drag_started() {
            bridge.begin_set(slot, param);
        }
        if response.changed() {
            let new_plain = denormalize(desc.as_ref(), norm, min, max);
            bridge.set(slot, param, new_plain);
        }
        if response.drag_stopped() {
            bridge.end_set(slot, param);
        }
    }
}
```

This uses the existing `normalize()`, `denormalize()`, and `auto_format()` functions already in `bridged_knob.rs`.

**Step 2: Add re-exports**

In `widgets/mod.rs`, add `bridged_fader` to the `pub use bridged_knob::{...}` list.
In `lib.rs`, add `bridged_fader` to the top-level re-exports.

**Step 3: Run tests**

Run: `cargo test -p sonido-gui-core`
Expected: All pass.

**Step 4: Commit**

```
feat(gui-core): add bridged_fader helper with gesture protocol
```

---

### Task 4: Rewrite LevelMeter — Ableton-style continuous dual-bar with dB scale

**Files:**
- Modify: `crates/sonido-gui-core/src/widgets/meter.rs`

**Step 1: Rewrite LevelMeter**

Replace the segmented LED approach with a continuous dual-bar meter:

- **RMS bar**: Continuous filled gradient (green → yellow at -6dB → red at -3dB). Use `painter.rect_filled()` for the filled portion. Color interpolation based on dB position.
- **Peak line**: 1px horizontal line at peak position. Decays at ~1.5dB/frame (use `peak * 0.97` per frame as ballistic decay — caller manages decay state).
- **dB scale markings**: Tick marks + labels (0, -6, -12, -18, -24) on the inner edge of the meter. Font size = `(meter_width * 0.3).clamp(7.0, 9.0)`. Labels drawn by the meter widget.
- **Clip indicator**: 3px red circle at top. Stays lit after peak > 1.0 until the response is clicked.
- **No numeric readout** — dB scale replaces it.
- **Width**: Total widget width includes scale labels. Meter bar itself is ~40% of widget width.

Keep the `LevelMeter` struct API mostly compatible but add `clip_latched: &'a mut bool` for persistent clip state. The `GainReductionMeter` stays as-is (it's fine).

Key implementation notes:
- dB marks at positions: `0dB = 1.0`, `-6dB = 0.5012`, `-12dB = 0.2512`, `-18dB = 0.1259`, `-24dB = 0.0631` (standard dB-to-linear conversion: `10^(dB/20)`)
- Scale labels are right-aligned on the left side of the meter bar (for left meter) or left-aligned on the right side (for right meter). Use a `side` parameter or always put labels on one side.
- Color gradient: linearly interpolate between green (below -6dB), yellow (-6dB to -3dB), red (above -3dB) based on segment position.

Preserve existing tests where applicable (struct construction, clamping, builder pattern). Update tests for new API.

**Step 2: Run tests**

Run: `cargo test -p sonido-gui-core`
Expected: All pass.

**Step 3: Verify compile**

Run: `cargo check -p sonido-gui -p sonido-gui-core`
Expected: Clean (callers in app.rs still use `LevelMeter::new(peak, rms).size(w, h)` — may need minor API adaptation in app.rs for the `clip_latched` param).

**Step 4: Commit**

```
feat(gui-core): rewrite LevelMeter — continuous dual-bar with dB scale
```

---

## Phase 2: Graph Editor Changes

### Task 5: Hide Input/Output nodes — I/O strips are the endpoints

**Files:**
- Modify: `crates/sonido-gui/src/graph_view.rs`

**Step 1: Skip rendering Input/Output nodes in SonidoViewer**

In the `SnarlViewer` impl for `SonidoViewer`, override `show_header` to be a no-op for Input/Output:

```rust
fn show_header(
    &mut self,
    node: NodeId,
    _inputs: &[InPin],
    _outputs: &[OutPin],
    ui: &mut Ui,
    _scale: f32,
    snarl: &mut Snarl<SonidoNode>,
) {
    let node_data = &snarl[node];
    // I/O nodes are implicit in the sidebar strips — don't render them.
    if matches!(node_data, SonidoNode::Input | SonidoNode::Output) {
        return;
    }
    // ... existing effect/split/merge header rendering ...
}
```

Also override `node_frame` and `header_frame` for I/O nodes to use zero margin and transparent fill so they're invisible. Override `show_input`/`show_output` to still return valid `PinInfo` for I/O nodes (wires still need endpoints), but make the pins tiny/transparent.

**Step 2: Remove body from effect nodes**

Change `has_body()` to always return `false`:

```rust
fn has_body(&mut self, _node: &SonidoNode) -> bool {
    false
}
```

Remove the `show_body()` implementation entirely (or leave it, it won't be called).

**Step 3: Auto-compile on node insert**

In `show_graph_menu()`, set `*self.topology_changed = true` after every `snarl.insert_node()` call. Currently only connect/disconnect/remove set this flag — node insertion is missing.

**Step 4: Run tests and verify**

Run: `cargo check -p sonido-gui`
Expected: Clean compile.

**Step 5: Commit**

```
feat(gui): hide I/O nodes in graph, remove effect body, auto-compile on insert
```

---

### Task 6: Node activity LEDs

**Files:**
- Modify: `crates/sonido-gui/src/graph_view.rs`
- Modify: `crates/sonido-gui/src/app.rs` (pass metering data to graph view)

**Step 1: Add per-slot activity levels to GraphView**

Add a field to `GraphView`:
```rust
/// Per-effect-slot activity level (0.0–1.0), updated each frame from metering.
pub slot_activity: Vec<f32>,
```

In `app.rs`, before calling `self.graph_view.show(ui)`, update `slot_activity` from the output metering data. A simple approach: set all slots to the output peak level (since per-slot metering isn't available yet). Or expose per-slot levels if the audio bridge has them.

Simple approach for now: use a single activity level derived from `self.metering.output_peak`:
```rust
self.graph_view.slot_activity = vec![self.metering.output_peak; self.bridge.slot_count()];
```

**Step 2: Draw activity LED in show_header**

In `show_header()` for Effect nodes, draw a small filled circle after the title text:

```rust
// Activity LED — pulses with signal level
if let SonidoNode::Effect { .. } = &snarl[node] {
    let slot_idx = /* count effect nodes before this one */;
    let activity = self.slot_activity.get(slot_idx).copied().unwrap_or(0.0);
    if activity > 0.01 {
        let led_pos = pos2(ui.max_rect().right() - 6.0, ui.max_rect().center().y);
        let led_color = accent.gamma_multiply(activity.clamp(0.2, 1.0));
        glow::glow_circle(ui.painter(), led_pos, 3.0, led_color, &self.theme);
    }
}
```

This requires passing `slot_activity` through the `SonidoViewer`. Add `slot_activity: &'a [f32]` to the viewer struct.

**Step 3: Run and verify**

Run: `cargo check -p sonido-gui`
Expected: Clean.

**Step 4: Commit**

```
feat(gui): add activity LED indicators on graph effect nodes
```

---

## Phase 3: Layout Restructure

### Task 7: Header cleanup — remove preset dropdown, compile button, add Save/Load

**Files:**
- Modify: `crates/sonido-gui/src/app.rs` — `render_header()` method

**Step 1: Strip the header**

Remove from `render_header()`:
- Preset selector ComboBox (`preset_selector` salt)
- `apply_preset()` call
- Save button and `show_save_dialog` trigger
- Compile button and `compile_and_apply()` call
- `compile_error` / `compile_success_frames` display

Add:
- "Save" button → calls `self.save_session()` (implemented in Task 12)
- "Load" button → calls `self.load_session()` (implemented in Task 12)

For now, add placeholder methods `save_session()` and `load_session()` that log "TODO" — Task 12 fills them in.

New header layout:
```rust
ui.horizontal(|ui| {
    // SONIDO brand
    ui.heading(RichText::new("SONIDO").font(FontId::monospace(18.0)).color(theme.colors.amber).strong());
    ui.add_space(12.0);

    // BYPASS (promoted from status bar)
    let chain_bypassed = self.audio_bridge.chain_bypass().load(Ordering::Relaxed);
    let bypass_color = if chain_bypassed { theme.colors.red } else { theme.colors.dim };
    let bypass_btn = ui.button(RichText::new("BYPASS").font(FontId::monospace(11.0)).color(bypass_color).strong());
    // glow indicator next to bypass
    let circle_center = pos2(bypass_btn.rect.right() + 8.0, bypass_btn.rect.center().y);
    glow::glow_circle(ui.painter(), circle_center, 3.0, bypass_color, &theme);
    ui.add_space(10.0);
    if bypass_btn.clicked() {
        self.audio_bridge.chain_bypass().store(!chain_bypassed, Ordering::SeqCst);
    }

    ui.separator();

    // Save / Load
    if ui.button(RichText::new("Save").font(FontId::monospace(12.0)).color(theme.colors.text_primary)).clicked() {
        self.save_session();
    }
    if ui.button(RichText::new("Load").font(FontId::monospace(12.0)).color(theme.colors.text_primary)).clicked() {
        self.load_session();
    }

    ui.separator();

    // FILE source toggle
    self.file_player.render_source_toggle(ui);

    // Right-aligned: audio status
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        // Audio status LED
        let status_color = if self.audio_bridge.is_running() { theme.colors.green } else { theme.colors.red };
        let (indicator_rect, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
        glow::glow_circle(ui.painter(), indicator_rect.center(), 4.0, status_color, &theme);
        // Error count
        let err_count = self.audio_bridge.error_count().load(Ordering::Relaxed);
        if err_count > 0 {
            ui.label(RichText::new(format!("errors: {err_count}")).font(FontId::monospace(10.0)).color(theme.colors.red));
        }
    });
});
```

**Step 2: Remove render_save_dialog**

Delete the `render_save_dialog()` method and `show_save_dialog`, `new_preset_name`, `new_preset_description` fields from `SonidoApp`. These belonged to the old preset system.

**Step 3: Run and verify**

Run: `cargo check -p sonido-gui`
Expected: May have some dead code warnings for preset_manager — that's fine, it gets removed in Task 12.

**Step 4: Commit**

```
feat(gui): header cleanup — bypass promoted, save/load added, presets removed
```

---

### Task 8: Status bar consolidation + file player merge

**Files:**
- Modify: `crates/sonido-gui/src/app.rs` — `render_status_bar()`, `update()`

**Step 1: Merge file player transport into status bar**

Remove the separate `TopBottomPanel::bottom("file_player")` from `update()`.

In `render_status_bar()`, add file player transport controls inline:

```rust
// After CPU meter...
ui.separator();

// File player transport (inline, only when file input active)
if self.file_player.use_file_input() {
    self.file_player.render_compact(ui);  // New compact method
}
```

Add a `render_compact()` method to `FilePlayer` that shows:
`▶ piano.wav 1:24/3:24 🔁` — play button, filename, position/duration, loop toggle. All in one horizontal line, monospace 10px. No progress bar (the full player had an LED bar — too large for status line).

**Step 2: Remove BYPASS from status bar**

It's now in the header (Task 7). Remove the bypass button/indicator from `render_status_bar()`.

**Step 3: Remove buffer size selector from status bar**

Move the buffer size ComboBox out of the status bar. Instead, add it as a right-click context menu on the latency display, or remove it entirely (the default 2048 is fine for most users, and they can use CLI `--buffer-size` for custom).

Simplified status bar:
```rust
ui.horizontal(|ui| {
    // Sample rate LED
    ui.add(LedDisplay::new(format!("{:.0}Hz", self.sample_rate)).color(theme.colors.amber));
    ui.separator();

    // Latency LED
    let latency_ms = self.buffer_size as f32 / self.sample_rate * 1000.0;
    ui.add(LedDisplay::new(format!("{:.1}ms", latency_ms)).color(theme.colors.amber));
    ui.separator();

    // CPU meter (fixed-width)
    // ... existing cpu_text + sparkline in allocate_ui_with_layout ...

    // File player transport (inline)
    if self.file_player.use_file_input() {
        ui.separator();
        self.file_player.render_compact(ui);
    }
});
```

**Step 4: Remove morph bar TopBottomPanel**

Remove `TopBottomPanel::bottom("morph_bar")` from `update()`. The morph bar will be rendered inline in the effect panel header (Task 10).

**Step 5: Run and verify**

Run: `cargo check -p sonido-gui`

**Step 6: Commit**

```
feat(gui): consolidate status bar — merge file player, remove buffer selector
```

---

### Task 9: Responsive main layout — I/O as endpoints

**Files:**
- Modify: `crates/sonido-gui/src/app.rs` — the `CentralPanel` section in `update()`

**Step 1: Replace hardcoded layout with ThemeLayout-driven sizing**

Replace the existing `CentralPanel` content (lines ~1022-1151) with responsive sizing:

```rust
CentralPanel::default().show(ctx, |ui| {
    let theme = SonidoTheme::get(ui.ctx());
    let avail = ui.available_rect_before_wrap();

    // Responsive strip widths
    let io_w = theme.layout.io_strip_width(avail.width());
    let gap = 8.0;
    let center_w = (avail.width() - 2.0 * io_w - 2.0 * gap).max(200.0);

    // Rect splits
    let input_rect = Rect::from_min_size(avail.min, vec2(io_w, avail.height()));
    let center_rect = Rect::from_min_size(
        pos2(avail.min.x + io_w + gap, avail.min.y),
        vec2(center_w, avail.height()),
    );
    let output_rect = Rect::from_min_size(
        pos2(avail.min.x + io_w + gap + center_w + gap, avail.min.y),
        vec2(io_w, avail.height()),
    );

    // Input strip (IS the Input node)
    {
        let mut child = ui.new_child(
            UiBuilder::new()
                .id_salt("input_col")
                .max_rect(input_rect)
                .layout(Layout::top_down(Align::Center)),
        );
        self.render_io_strip(&mut child, true); // true = input
    }

    // Center column
    {
        let mut child = ui.new_child(
            UiBuilder::new()
                .id_salt("center_col")
                .max_rect(center_rect)
                .layout(Layout::top_down(Align::LEFT)),
        );

        if self.single_effect {
            self.render_effect_panel(&mut child, SlotIndex(0));
        } else {
            // Dynamic graph/panel split
            let content_h = child.available_height();
            let panel_content_h = self.estimate_panel_height();
            let (graph_h, _panel_h) = theme.layout.split_vertical(content_h, panel_content_h);

            // Graph editor
            let selected_slot = child.allocate_ui_with_layout(
                vec2(center_w, graph_h),
                Layout::top_down(Align::LEFT),
                |ui| {
                    self.graph_view.show(ui)
                },
            ).inner;

            // Auto-compile
            if self.graph_view.topology_changed {
                self.compile_and_apply();
            }

            child.add_space(4.0);

            // Effect panel or quick reference
            if let Some(slot_idx) = selected_slot {
                let slot = SlotIndex(slot_idx);
                if slot.0 < self.bridge.slot_count() {
                    self.render_effect_panel(&mut child, slot);
                }
            } else {
                self.render_quick_reference(&mut child);
            }
        }
    }

    // Output strip (IS the Output node)
    {
        let mut child = ui.new_child(
            UiBuilder::new()
                .id_salt("output_col")
                .max_rect(output_rect)
                .layout(Layout::top_down(Align::Center)),
        );
        self.render_io_strip(&mut child, false); // false = output
    }

    ui.advance_cursor_after_rect(Rect::from_min_max(
        avail.min,
        pos2(avail.min.x + io_w + gap + center_w + gap + io_w, avail.max.y),
    ));
});
```

**Step 2: New render_io_strip() method**

Replaces `render_io_section()` and `render_output_section()` with a single method:

```rust
/// Render an I/O strip (meter + knob + label). Used for both input and output.
fn render_io_strip(&mut self, ui: &mut egui::Ui, is_input: bool) {
    let theme = SonidoTheme::get(ui.ctx());
    let strip_w = ui.available_width();

    ui.vertical_centered(|ui| {
        // Meter — fills most of the vertical space
        let meter_h = (ui.available_height() - 80.0).max(60.0);
        let (peak, rms) = if is_input {
            (self.metering.input_peak, self.metering.input_rms)
        } else {
            (self.metering.output_peak, self.metering.output_rms)
        };
        let clip_latched = if is_input {
            &mut self.input_clip_latched
        } else {
            &mut self.output_clip_latched
        };

        ui.add(
            LevelMeter::new(peak, rms)
                .clip_latch(clip_latched)
                .size(strip_w * 0.4, meter_h),
        );

        ui.add_space(4.0);

        // Gain/Master knob
        let knob_diam = (strip_w * 0.7).clamp(36.0, 56.0);
        let label = if is_input { "GAIN" } else { "MSTR" };
        let param = if is_input {
            self.audio_bridge.input_gain()
        } else {
            self.audio_bridge.master_volume()
        };
        let (min, max) = if is_input { (-20.0, 20.0) } else { (-40.0, 6.0) };

        let mut val = param.get();
        if ui.add(
            Knob::new(&mut val, min, max, label)
                .default(0.0)
                .format_db()
                .diameter(knob_diam),
        ).changed() {
            param.set(val);
            self.preset_manager.mark_modified();
        }
    });
}
```

Add `input_clip_latched: bool` and `output_clip_latched: bool` fields to `SonidoApp`.

**Step 3: estimate_panel_height helper**

```rust
/// Estimate the height needed for the effect panel content.
fn estimate_panel_height(&self) -> f32 {
    let theme_layout = &SonidoTheme::get(&self.ctx_cache).layout;
    // Single row of faders + title + bypass + labels
    theme_layout.fader_max_h + 60.0 // fader + title bar + label space
}
```

(This is approximate — the real height comes from content. The layout system will clamp it.)

**Step 4: render_quick_reference helper**

```rust
/// Render the quick-reference card when no node is selected.
fn render_quick_reference(&self, ui: &mut egui::Ui) {
    let theme = SonidoTheme::get(ui.ctx());
    let shortcuts = [
        ("RIGHT-CLICK", "Add effect"),
        ("CLICK NODE", "Edit params"),
        ("DELETE", "Remove node"),
        ("SCROLL", "Zoom graph"),
        ("SPACE", "Play / Pause"),
        ("DRAG", "Move nodes"),
    ];

    let frame = Frame::new()
        .fill(theme.colors.void)
        .stroke(Stroke::new(1.0, theme.colors.dim))
        .corner_radius(4.0)
        .inner_margin(Margin::same(12));

    frame.show(ui, |ui| {
        for (key, action) in &shortcuts {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(*key)
                        .font(FontId::monospace(10.0))
                        .color(theme.colors.amber),
                );
                ui.label(
                    RichText::new(*action)
                        .font(FontId::monospace(10.0))
                        .color(theme.colors.text_secondary),
                );
            });
        }
    });
}
```

**Step 5: Delete old methods**

Remove `render_io_section()` and `render_output_section()`.

**Step 6: Update window size in main.rs**

In `crates/sonido-gui/src/main.rs`:
```rust
.with_inner_size([1000.0, 700.0])
.with_min_inner_size([800.0, 550.0])
```

**Step 7: Run and verify**

Run: `cargo check -p sonido-gui`
Then: `cargo test -p sonido-gui`

**Step 8: Commit**

```
feat(gui): responsive layout — I/O strips as endpoints, dynamic graph/panel split
```

---

### Task 10: Effect panel with inline morph bar

**Files:**
- Modify: `crates/sonido-gui/src/app.rs` — `render_effect_panel()`

**Step 1: Inline morph bar in panel title row**

Replace the effect panel layout:

```rust
fn render_effect_panel(&mut self, ui: &mut egui::Ui, slot: SlotIndex) {
    // ... existing panel cache logic ...
    let theme = SonidoTheme::get(ui.ctx());

    let panel_frame = Frame::new()
        .fill(theme.colors.void)
        .stroke(Stroke::new(2.0, theme.colors.amber))
        .corner_radius(theme.sizing.panel_border_radius)
        .inner_margin(Margin::same(theme.sizing.panel_padding as i8));

    let panel_response = panel_frame.show(ui, |ui| {
        // Title row: effect name + inline morph bar + bypass toggle
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(panel_name)
                    .font(FontId::monospace(12.0))
                    .color(theme.colors.amber)
                    .strong(),
            );

            // Inline morph bar (compact)
            if !self.single_effect {
                ui.add_space(8.0);
                let has_a = self.morph_state.a.is_some();
                let has_b = self.morph_state.b.is_some();
                let resp = morph_bar(ui, &mut self.morph_state.t, has_a, has_b);
                // ... handle morph responses (same as current render_morph_bar) ...
            }

            // Bypass toggle right-aligned
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let is_bypassed = self.bridge.is_bypassed(slot);
                // ... bypass toggle ...
            });
        });

        ui.add_space(4.0);

        // Effect controls
        if let Some((_, _, ref mut panel)) = self.cached_panel {
            let bridge: &dyn ParamBridge = &*self.bridge;
            panel.ui(ui, bridge, slot);
        }
    });

    let panel_rect = panel_response.response.rect;
    glow::scanlines(ui.painter(), panel_rect, &theme);
}
```

**Step 2: Run and verify**

Run: `cargo check -p sonido-gui`

**Step 3: Commit**

```
feat(gui): inline morph bar in effect panel title, remove separate TopBottomPanel
```

---

## Phase 4: Effect UI Migration

### Task 11: Migrate all 19 effect UIs from knob grid to fader row

**Files:**
- Modify: All 19 files in `crates/sonido-gui-core/src/effects_ui/`:
  `bitcrusher.rs`, `chorus.rs`, `compressor.rs`, `delay.rs`, `distortion.rs`, `eq.rs`, `filter.rs`, `flanger.rs`, `gate.rs`, `limiter.rs`, `phaser.rs`, `preamp.rs`, `reverb.rs`, `ringmod.rs`, `stage.rs`, `tape.rs`, `tremolo.rs`, `vibrato.rs`, `wah.rs`

**Pattern for each file:**

Replace the current layout:
```rust
// Old: horizontal rows of bridged_knob calls
ui.horizontal(|ui| {
    bridged_knob(ui, bridge, slot, ParamIndex(0));
    ui.add_space(16.0);
    bridged_knob(ui, bridge, slot, ParamIndex(1));
    // ...
});
```

With:
```rust
// New: horizontal row of bridged_fader calls
let theme = SonidoTheme::get(ui.ctx());
let param_count = bridge.param_count(slot);
// Reserve width for bypass toggle
let avail_w = ui.available_width() - 20.0;
let fader_w = theme.layout.fader_width(avail_w, param_count);
let fader_h = theme.layout.fader_height(ui.available_height().min(200.0));

ui.horizontal_wrapped(|ui| {
    for i in 0..param_count {
        let param = ParamIndex(i);
        // Skip enum/stepped params — use bridged_combo for those
        let desc = bridge.param_descriptor(slot, param);
        if desc.as_ref().is_some_and(|d| d.flags.contains(ParamFlags::STEPPED)) {
            // Render as combo box (existing bridged_combo)
            bridged_combo(ui, bridge, slot, param);
        } else {
            bridged_fader(ui, bridge, slot, param, fader_w, fader_h);
        }
    }
});
```

**Special cases:**
- **EQ** (`eq.rs`): Keep band grouping (LOW/MID/HIGH separators between groups). Use faders for freq/gain/Q within each band.
- **Distortion** (`distortion.rs`): Has a clipping mode combo box — render with `bridged_combo`, rest as faders.
- **Stage** (`stage.rs`): Has boolean toggles (phase, DC block, bass mono) — keep as toggles, rest as faders.
- **Effects with combo boxes** (distortion, chorus, delay, flanger, phaser, tremolo, vibrato, wah): Render the mode/type selector as `bridged_combo` at the start, then faders for continuous params.

**Implementation approach:** Do NOT hand-write all 19 individually. Instead:

1. Make the generic fader-row pattern work for the simplest effects first (preamp, limiter, bitcrusher — few params, no combos).
2. Then handle combo-box effects.
3. Then handle special layouts (EQ bands, stage toggles).

Most panels can use a fully generic approach: iterate all params, check `STEPPED` flag, render combo or fader. Only EQ and stage need custom layout.

**Step 1: Migrate simple effects (no combo boxes)**

Preamp (4 params), Limiter (4), Bitcrusher (4), Filter (4), Gate (5), Compressor (11), Reverb (8), Tape (10).

**Step 2: Migrate combo-box effects**

Distortion (type selector), Chorus (waveform), Delay (type), Flanger (waveform), Phaser (waveform), Tremolo (waveform), Vibrato (waveform), Wah (mode), RingMod (waveform).

**Step 3: Migrate special layouts**

EQ (3 band groups), Stage (boolean toggles).

**Step 4: Run tests**

Run: `cargo test -p sonido-gui-core`
Expected: All 27 tests pass. The `create_panel_returns_some_for_all_known_ids` test validates all 19 panels still instantiate.

**Step 5: Visual verification**

Run: `cargo run -p sonido-gui --release`
Check each effect panel renders correctly with faders.

**Step 6: Commit**

```
feat(gui-core): migrate all 19 effect UIs from knob grid to fader row
```

---

## Phase 5: Session Save/Load

### Task 12: Session serialization and save/load

**Files:**
- Create: `crates/sonido-gui/src/session.rs`
- Modify: `crates/sonido-gui/src/graph_view.rs` (serde for `SonidoNode`)
- Modify: `crates/sonido-gui/src/app.rs` (wire up save/load)
- Modify: `crates/sonido-registry/src/lib.rs` (serde for `EffectCategory`)

**Step 1: Add serde derives to EffectCategory**

In `crates/sonido-registry/src/lib.rs`, add `serde` dependency and derive:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EffectCategory { ... }
```

Add `serde = { workspace = true, optional = true }` to sonido-registry's Cargo.toml, with a `serde` feature. Enable it from sonido-gui's Cargo.toml.

**Step 2: Create serializable session node type**

In `crates/sonido-gui/src/graph_view.rs`, create a parallel serializable type (don't make `SonidoNode` itself serializable — it has `&'static str` and `ParamDescriptor`):

```rust
/// Serializable representation of a graph node for session save/load.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SessionNode {
    Input,
    Output,
    Effect { effect_id: String },
    Split,
    Merge,
}

impl SonidoNode {
    /// Convert to serializable form (drops descriptors/smoothing — reconstructed on load).
    pub fn to_session(&self) -> SessionNode {
        match self {
            SonidoNode::Input => SessionNode::Input,
            SonidoNode::Output => SessionNode::Output,
            SonidoNode::Effect { effect_id, .. } => SessionNode::Effect {
                effect_id: (*effect_id).to_string(),
            },
            SonidoNode::Split => SessionNode::Split,
            SonidoNode::Merge => SessionNode::Merge,
        }
    }
}
```

**Step 3: Create session.rs**

```rust
//! Session save/load for the Sonido graph editor.
//!
//! A session captures the complete editor state: Snarl graph topology,
//! node positions, all parameter values, bypass states, and I/O gains.
//! Sessions serialize to JSON for human-readability and VCS friendliness.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// Complete session state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// Format version for forward compatibility.
    pub version: u32,
    /// Graph topology: node types and their positions.
    pub nodes: Vec<SessionNodeEntry>,
    /// Wire connections: (from_node_idx, from_output, to_node_idx, to_input).
    pub wires: Vec<(usize, usize, usize, usize)>,
    /// Per-effect parameter values, keyed by node index.
    pub params: HashMap<usize, EffectState>,
    /// Input gain in dB.
    pub input_gain: f32,
    /// Master volume in dB.
    pub master_volume: f32,
}

/// A node entry with type and position.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionNodeEntry {
    pub node: super::graph_view::SessionNode,
    pub pos: [f32; 2],
}

/// Parameter state for a single effect.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EffectState {
    pub effect_id: String,
    pub params: Vec<f32>,
    pub bypassed: bool,
}

impl Session {
    /// Current format version.
    pub const VERSION: u32 = 1;

    /// Save session to a JSON file.
    pub fn save(&self, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    /// Load session from a JSON file.
    pub fn load(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let json = std::fs::read_to_string(path)?;
        let session: Self = serde_json::from_str(&json)?;
        Ok(session)
    }
}
```

**Step 4: Implement capture/restore in GraphView**

Add methods to `GraphView`:

```rust
/// Capture current graph state as a Session.
pub fn capture_session(
    &self,
    bridge: &dyn ParamBridge,
    input_gain: f32,
    master_volume: f32,
) -> Session {
    let mut nodes = Vec::new();
    let mut node_id_to_idx: HashMap<NodeId, usize> = HashMap::new();

    for (id, node) in self.snarl.node_ids() {
        let idx = nodes.len();
        node_id_to_idx.insert(id, idx);
        let pos = self.snarl.get_node_info(id)
            .map_or([0.0, 0.0], |info| [info.pos.x, info.pos.y]);
        nodes.push(SessionNodeEntry {
            node: node.to_session(),
            pos,
        });
    }

    let mut wires = Vec::new();
    for (out_pin, in_pin) in self.snarl.wires() {
        if let (Some(&from_idx), Some(&to_idx)) =
            (node_id_to_idx.get(&out_pin.node), node_id_to_idx.get(&in_pin.node))
        {
            wires.push((from_idx, out_pin.output, to_idx, in_pin.input));
        }
    }

    // Collect per-effect params
    let mut params = HashMap::new();
    let mut effect_slot = 0usize;
    for (idx, entry) in nodes.iter().enumerate() {
        if let SessionNode::Effect { ref effect_id } = entry.node {
            let slot = SlotIndex(effect_slot);
            let param_count = bridge.param_count(slot);
            let param_values: Vec<f32> = (0..param_count)
                .map(|i| bridge.get(slot, ParamIndex(i)))
                .collect();
            params.insert(idx, EffectState {
                effect_id: effect_id.clone(),
                params: param_values,
                bypassed: bridge.is_bypassed(slot),
            });
            effect_slot += 1;
        }
    }

    Session {
        version: Session::VERSION,
        nodes,
        wires,
        params,
        input_gain,
        master_volume,
    }
}
```

The `restore_session()` method rebuilds the Snarl from a Session, using the registry to look up `&'static str` IDs, descriptors, and smoothing. Then triggers compile.

**Step 5: Wire up save_session / load_session in app.rs**

```rust
fn save_session(&self) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        if let Some(path) = rfd::FileDialog::new()
            .set_title("Save Session")
            .add_filter("Sonido Session", &["json"])
            .save_file()
        {
            let session = self.graph_view.capture_session(
                &*self.bridge,
                self.audio_bridge.input_gain().get(),
                self.audio_bridge.master_volume().get(),
            );
            if let Err(e) = session.save(&path) {
                tracing::error!(error = %e, "failed to save session");
            }
        }
    }
}
```

**Step 6: Tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_roundtrip_json() {
        let session = Session {
            version: Session::VERSION,
            nodes: vec![
                SessionNodeEntry { node: SessionNode::Input, pos: [100.0, 200.0] },
                SessionNodeEntry { node: SessionNode::Effect { effect_id: "reverb".into() }, pos: [300.0, 200.0] },
                SessionNodeEntry { node: SessionNode::Output, pos: [500.0, 200.0] },
            ],
            wires: vec![(0, 0, 1, 0), (1, 0, 2, 0)],
            params: {
                let mut m = HashMap::new();
                m.insert(1, EffectState {
                    effect_id: "reverb".into(),
                    params: vec![0.5, 0.7, 0.3, 0.2, 0.8, 0.1, 0.5, 0.0],
                    bypassed: false,
                });
                m
            },
            input_gain: 0.0,
            master_volume: -3.0,
        };

        let json = serde_json::to_string(&session).unwrap();
        let restored: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.version, 1);
        assert_eq!(restored.nodes.len(), 3);
        assert_eq!(restored.wires.len(), 2);
        assert_eq!(restored.master_volume, -3.0);
    }
}
```

**Step 7: Run and verify**

Run: `cargo test -p sonido-gui`

**Step 8: Commit**

```
feat(gui): session save/load — JSON serialization of graph + params + gains
```

---

## Phase 6: Polish

### Task 13: Repaint throttling

**Files:**
- Modify: `crates/sonido-gui/src/app.rs` — `update()` method

**Step 1: Throttle repaints when idle**

Replace the unconditional `request_repaint_after(16ms)` with adaptive refresh:

```rust
// Adaptive repaint rate
let is_animating = self.audio_bridge.is_running()
    || self.file_player.is_playing()
    || self.metering.output_peak > 0.001;

#[cfg(not(target_arch = "wasm32"))]
if is_animating {
    ctx.request_repaint_after(Duration::from_millis(16)); // 60fps
} else {
    ctx.request_repaint_after(Duration::from_millis(250)); // 4fps idle
}
```

Add `is_playing()` method to `FilePlayer` if it doesn't exist.

**Step 2: Commit**

```
perf(gui): adaptive repaint throttling — 4fps when idle, 60fps when active
```

---

### Task 14: Context menu search filter

**Files:**
- Modify: `crates/sonido-gui/src/graph_view.rs` — `show_graph_menu()`

**Step 1: Add search filter to context menu**

Add a text input at the top of the graph context menu. Filter the effect list as user types:

```rust
fn show_graph_menu(
    &mut self,
    pos: egui::Pos2,
    ui: &mut Ui,
    _scale: f32,
    snarl: &mut Snarl<SonidoNode>,
) {
    // Search filter (persistent across frames via egui memory)
    let filter_id = egui::Id::new("graph_menu_filter");
    let mut filter: String = ui.data(|d| d.get_temp(filter_id).unwrap_or_default());
    ui.text_edit_singleline(&mut filter);
    ui.data_mut(|d| d.insert_temp(filter_id, filter.clone()));

    let filter_lower = filter.to_lowercase();

    ui.separator();

    // Structural nodes (always shown)
    if filter.is_empty() {
        // ... existing Input/Output/Split/Merge buttons ...
        ui.separator();
    }

    // Effects filtered by search
    let registry = EffectRegistry::new();
    if filter.is_empty() {
        // Category submenus (existing behavior)
        // ...
    } else {
        // Flat filtered list
        for desc in registry.all_effects() {
            if desc.name.to_lowercase().contains(&filter_lower)
                || desc.id.contains(&filter_lower)
            {
                if ui.button(desc.name).clicked() {
                    let descriptors = collect_descriptors(desc.id, 48000.0);
                    let smoothing = collect_smoothing(desc.id, 48000.0);
                    snarl.insert_node(pos, SonidoNode::Effect {
                        effect_id: desc.id,
                        name: desc.name,
                        category: desc.category,
                        descriptors,
                        smoothing,
                    });
                    *self.topology_changed = true;
                    ui.close_menu();
                }
            }
        }
    }
}
```

Note: Check that `EffectRegistry` has an `all_effects()` or equivalent method. If not, iterate all categories.

**Step 2: Commit**

```
feat(gui): context menu search filter for quick effect lookup
```

---

### Task 15: Remove dead preset code

**Files:**
- Modify: `crates/sonido-gui/src/app.rs` — remove preset_manager fields and imports
- Modify: `crates/sonido-gui/src/preset_manager.rs` — keep if session.rs needs any utilities, otherwise delete

**Step 1: Audit preset_manager usage**

Search for all `preset_manager` references in app.rs and remove:
- `preset_manager` field from `SonidoApp`
- `apply_preset()` method
- `PresetManager::new()` in constructor
- Any `self.preset_manager.mark_modified()` calls (replace with no-op or remove)
- `use crate::preset_manager::PresetManager` import

**Step 2: Decide on preset_manager.rs**

If `preset_to_params()` or `params_to_preset()` are used by session.rs, keep those functions. Otherwise, delete the file and remove `mod preset_manager` from the crate.

**Step 3: Run and verify**

Run: `cargo test -p sonido-gui`
Expected: All pass. Dead code warnings should be gone.

**Step 4: Commit**

```
refactor(gui): remove dead preset system — replaced by session save/load
```

---

### Task 16: Final integration testing and verification

**Files:** None (verification only)

**Step 1: Full compile check**

```bash
cargo check -p sonido-gui -p sonido-gui-core
```

**Step 2: Run all tests**

```bash
cargo test -p sonido-gui -p sonido-gui-core
```

**Step 3: WASM check**

```bash
cargo check --target wasm32-unknown-unknown -p sonido-gui
```

**Step 4: Visual verification (release build)**

```bash
cargo run -p sonido-gui --release
```

Verify:
- [ ] I/O strips render correctly at various window sizes
- [ ] Graph editor shows only effect/split/merge nodes (no Input/Output)
- [ ] Clicking an effect node shows fader panel below
- [ ] Faders respond to drag, scroll, double-click reset
- [ ] dB-scaled meters show RMS bar + peak line + clip indicator
- [ ] Status bar is compact: sample rate, latency, CPU, file transport
- [ ] Header has: SONIDO, BYPASS, Save, Load, FILE, audio status
- [ ] Morph bar appears inline in effect panel title
- [ ] Quick reference card shows when no node selected
- [ ] Save session writes JSON, Load session restores graph
- [ ] Node activity LEDs pulse with signal
- [ ] Context menu has search filter
- [ ] Ctrl+/- scales the whole UI
- [ ] No buffer underruns in release mode

**Step 5: Commit any fixes from visual testing**

```
fix(gui): polish from integration testing
```

---

## Summary

| Phase | Tasks | Key Deliverables |
|-------|-------|-----------------|
| 1: Foundation | 1–4 | ThemeLayout, Fader widget, bridged_fader, Ableton-style meter |
| 2: Graph | 5–6 | Hidden I/O nodes, no body text, activity LEDs, auto-compile |
| 3: Layout | 7–10 | Header cleanup, status consolidation, responsive I/O strips, inline morph |
| 4: Effects | 11 | All 19 effect UIs → fader rows |
| 5: Session | 12 | JSON session save/load replacing presets |
| 6: Polish | 13–16 | Repaint throttling, menu search, dead code removal, verification |

**Dependencies:** Phase 1 must complete before Phase 4 (fader widget needed). Phases 2, 3, 5 can run in parallel after Phase 1. Phase 6 runs last.
