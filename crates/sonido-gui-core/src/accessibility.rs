//! Accessibility integration for the Sonido GUI.
//!
//! All interactive widgets announce their name, current value, range, and unit
//! when focused, enabling screen reader support.  Parameter changes are
//! described as human-readable strings so assistive technology can report them
//! without coupling to numeric values.
//!
//! A high-contrast theme option is available via [`AccessibilityContext`] — when
//! `high_contrast` is `true`, [`SonidoTheme`](crate::theme::SonidoTheme) replaces
//! the normal amber/green palette with WCAG-AA-compliant colours.
//!
//! # Integration Pattern
//!
//! ```rust,ignore
//! let ctx = AccessibilityContext { screen_reader_active: true, high_contrast: false };
//! my_widget.show(ui, &ctx);
//! if ctx.screen_reader_active {
//!     ui.ctx().output_mut(|o| o.speak(my_widget.accessible_label()));
//! }
//! ```
//!
//! # Status
//!
//! Types and trait defined.  egui `AccessKit` integration is TODO.

/// Accessibility context passed to widgets during rendering.
///
/// Constructed once per frame and threaded through every widget call that
/// participates in accessibility.  Cheap to clone — two booleans.
#[derive(Debug, Clone, Copy, Default)]
pub struct AccessibilityContext {
    /// Whether a screen reader is active.
    ///
    /// When `true`, widgets should produce accessible labels via
    /// [`Accessible::accessible_label`] and route them through
    /// `egui::Context::output_mut` → `speak`.
    pub screen_reader_active: bool,

    /// Whether the high-contrast theme variant should be applied.
    ///
    /// Overrides the normal SonidoTheme palette with colours meeting WCAG AA
    /// contrast ratios (≥ 4.5 : 1 for normal text, ≥ 3 : 1 for large text).
    pub high_contrast: bool,
}

/// Trait for widgets that expose accessible names and roles.
///
/// Implement this on any widget that carries a parameter value so screen
/// readers can describe the control and its state.
///
/// # Contract
///
/// - `accessible_label` must include the parameter name and formatted value
///   with units, e.g. `"Drive: 18 dB"`.
/// - `accessible_role` must return one of the ARIA role strings listed below.
///
/// # ARIA Roles Used in Sonido
///
/// | Role       | Widget                        |
/// |------------|-------------------------------|
/// | `"slider"` | [`Knob`](crate::widgets::Knob), [`Fader`](crate::widgets::Fader) |
/// | `"button"` | Footswitch, bypass toggle     |
/// | `"combobox"` | Effect selector, filter type |
/// | `"meter"`  | Level meter, GR meter         |
pub trait Accessible {
    /// Return an accessible name + value string for screen readers.
    ///
    /// # Example Output
    ///
    /// `"Drive: 18 dB"`, `"Filter type: Low-pass"`, `"Bypass: active"`
    fn accessible_label(&self) -> String;

    /// Return the ARIA role string for this widget.
    ///
    /// Valid values: `"slider"`, `"button"`, `"toggle"`, `"combobox"`, `"meter"`.
    fn accessible_role(&self) -> &str;
}
