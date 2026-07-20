//! Accessibility for the native GUI: text scaling and screen-reader labelling.
//!
//! This window draws most of its surface with the painter rather than with
//! egui widgets, which costs two things a11y normally gets for free. First,
//! there is no built-in way to enlarge text — painter-drawn runs read their
//! size from the [`egui::TextStyle`] table, so the only lever that moves
//! everything at once is that table. Second, a painted row emits no widget
//! semantics at all: the accesskit tree sees an anonymous rect, and a screen
//! reader announces silence where a button should be.
//!
//! The scale half rebuilds the text-style table from a fixed base table (the
//! same sizes [`super::theme::install`] installs) multiplied by the requested
//! scale, so the operation is idempotent — it never reads the current, already
//! scaled sizes, and repeated application cannot compound. Call it *after*
//! `theme::install`, which owns fonts, spacing and the unscaled baseline.
//!
//! The labelling half wraps [`egui::Response::widget_info`], the always-present
//! path into egui's accesskit output (`accesskit` is a non-optional dependency
//! of egui 0.35, so none of this sits behind a cargo feature this crate turns
//! off). Allocate an interactive rect for the row, paint it, then hand the
//! response a role and a label.
//!
//! Scale arithmetic is pure — no [`egui::Context`], no I/O — so clamping,
//! stepping and formatting are exhaustively unit-testable on any host.

use eframe::egui;
use egui::{FontFamily, FontId, TextStyle, WidgetInfo, WidgetType};

// ─── scale range ─────────────────────────────────────────────────────────────

/// Smallest and largest text scale the window offers, and the step between.
pub const SCALE_MIN: f32 = 0.85;
pub const SCALE_MAX: f32 = 1.50;
pub const SCALE_STEP: f32 = 0.05;
pub const SCALE_DEFAULT: f32 = 1.0;

// ─── base type scale (mirrors `theme::install`) ──────────────────────────────

const BASE_HEADING: f32 = 19.0;
const BASE_BODY: f32 = 13.5;
const BASE_BUTTON: f32 = 13.5;
const BASE_SMALL: f32 = 11.5;
const BASE_MONO: f32 = 12.5;

// ─── scale arithmetic (pure) ─────────────────────────────────────────────────

/// Clamps any scale into the supported range; non-finite input returns
/// [`SCALE_DEFAULT`].
pub fn clamp_scale(scale: f32) -> f32 {
    if scale.is_finite() {
        scale.clamp(SCALE_MIN, SCALE_MAX)
    } else {
        SCALE_DEFAULT
    }
}

/// The next multiple of [`SCALE_STEP`] in the given direction, clamped. An
/// off-grid `current` (only reachable from a hand-edited `gui.json`) moves to
/// the adjacent notch rather than being rounded first: rounding 1.04 to 1.05
/// and *then* stepping would land on 1.10, skipping a notch the user can see.
pub fn step_scale(current: f32, up: bool) -> f32 {
    let base = if current.is_finite() {
        current
    } else {
        SCALE_DEFAULT
    };
    // `base / SCALE_STEP` lands a hair under the integer for several on-grid
    // values, so a bare floor/ceil would leave the button stuck. The epsilon is
    // a thousandth of a step — far below any offset worth honouring.
    const EPS: f32 = 1e-3;
    let n = base / SCALE_STEP;
    let next = if up {
        (n + EPS).floor() + 1.0
    } else {
        (n - EPS).ceil() - 1.0
    };
    clamp_scale(next * SCALE_STEP)
}

/// "100%" style label for the current scale.
pub fn scale_label(scale: f32) -> String {
    format!("{}%", (clamp_scale(scale) * 100.0).round() as i32)
}

/// One scaled text size, rounded to a tidy tenth. Always derived from `base`,
/// never from an already scaled size — the basis of `apply_text_scale`'s
/// idempotence.
fn scaled_size(base: f32, scale: f32) -> f32 {
    (base * clamp_scale(scale) * 10.0).round() / 10.0
}

// ─── application ─────────────────────────────────────────────────────────────

/// Applies `scale` to every text style in the window. Idempotent: it always
/// derives from the fixed base sizes, never from the current (already scaled)
/// ones, so repeated calls cannot compound.
pub fn apply_text_scale(ctx: &egui::Context, scale: f32) {
    let heading = scaled_size(BASE_HEADING, scale);
    let body = scaled_size(BASE_BODY, scale);
    let button = scaled_size(BASE_BUTTON, scale);
    let small = scaled_size(BASE_SMALL, scale);
    let mono = scaled_size(BASE_MONO, scale);

    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (
                TextStyle::Heading,
                FontId::new(heading, FontFamily::Name("inter-semibold".into())),
            ),
            (TextStyle::Body, FontId::new(body, FontFamily::Proportional)),
            (
                TextStyle::Button,
                FontId::new(button, FontFamily::Name("inter-medium".into())),
            ),
            (
                TextStyle::Small,
                FontId::new(small, FontFamily::Proportional),
            ),
            (
                TextStyle::Monospace,
                FontId::new(mono, FontFamily::Monospace),
            ),
        ]
        .into();
    });
}

// ─── screen-reader labelling ─────────────────────────────────────────────────

/// Describes a painter-drawn row to assistive technology: a labelled button
/// role, so a screen reader announces it instead of silence.
pub fn label_button(resp: &egui::Response, label: &str) {
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, resp.enabled(), label));
}

/// As [`label_button`], for a row that represents a selectable item in a list
/// (the sidebar sessions, palette entries), including its selected state.
pub fn label_selectable(resp: &egui::Response, label: &str, selected: bool) {
    resp.widget_info(|| {
        WidgetInfo::selected(WidgetType::SelectableLabel, resp.enabled(), selected, label)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-6
    }

    // ─── clamp_scale ─────────────────────────────────────────────────────────

    #[test]
    fn clamp_passes_in_range_values_through() {
        assert!(approx(clamp_scale(1.0), 1.0));
        assert!(approx(clamp_scale(1.25), 1.25));
        assert!(approx(clamp_scale(SCALE_MIN), SCALE_MIN));
        assert!(approx(clamp_scale(SCALE_MAX), SCALE_MAX));
    }

    #[test]
    fn clamp_pulls_below_min_up() {
        assert!(approx(clamp_scale(0.5), SCALE_MIN));
        assert!(approx(clamp_scale(0.0), SCALE_MIN));
        assert!(approx(clamp_scale(-3.0), SCALE_MIN));
    }

    #[test]
    fn clamp_pulls_above_max_down() {
        assert!(approx(clamp_scale(2.0), SCALE_MAX));
        assert!(approx(clamp_scale(1000.0), SCALE_MAX));
    }

    #[test]
    fn clamp_maps_non_finite_to_default() {
        // NaN would survive a bare `f32::clamp`, so it is screened first.
        assert!(approx(clamp_scale(f32::NAN), SCALE_DEFAULT));
        assert!(approx(clamp_scale(f32::INFINITY), SCALE_DEFAULT));
        assert!(approx(clamp_scale(f32::NEG_INFINITY), SCALE_DEFAULT));
    }

    // ─── step_scale ──────────────────────────────────────────────────────────

    #[test]
    fn step_moves_one_notch_from_default() {
        assert!(approx(step_scale(1.0, true), 1.05));
        assert!(approx(step_scale(1.0, false), 0.95));
    }

    #[test]
    fn step_snaps_an_off_grid_value_to_the_grid() {
        // An off-grid value moves to the adjacent notch in the direction
        // pressed — never rounded first, which would skip a notch going up
        // from 1.04, and never carrying the offset forward.
        assert!(approx(step_scale(1.02, true), 1.05));
        assert!(approx(step_scale(1.02, false), 1.00));
        assert!(approx(step_scale(1.04, true), 1.05));
        assert!(approx(step_scale(1.04, false), 1.00));
    }

    #[test]
    fn step_round_trips_back_to_the_start() {
        let up = step_scale(1.0, true);
        assert!(approx(step_scale(up, false), 1.0));
    }

    #[test]
    fn step_clamps_at_both_ends() {
        assert!(approx(step_scale(SCALE_MAX, true), SCALE_MAX));
        assert!(approx(step_scale(SCALE_MIN, false), SCALE_MIN));
        // Already out of range: stepping cannot escape the range either.
        assert!(approx(step_scale(9.0, true), SCALE_MAX));
        assert!(approx(step_scale(-9.0, false), SCALE_MIN));
    }

    #[test]
    fn step_treats_non_finite_as_default_stepped_once() {
        assert!(approx(step_scale(f32::NAN, true), 1.05));
        assert!(approx(step_scale(f32::NAN, false), 0.95));
        assert!(approx(step_scale(f32::INFINITY, false), 0.95));
        assert!(approx(step_scale(f32::NEG_INFINITY, true), 1.05));
    }

    #[test]
    fn stepping_up_from_min_reaches_max_and_stops() {
        let mut s = SCALE_MIN;
        for _ in 0..100 {
            s = step_scale(s, true);
        }
        assert!(approx(s, SCALE_MAX));

        for _ in 0..100 {
            s = step_scale(s, false);
        }
        assert!(approx(s, SCALE_MIN));
    }

    // ─── scale_label ─────────────────────────────────────────────────────────

    #[test]
    fn label_formats_whole_percents() {
        assert_eq!(scale_label(0.85), "85%");
        assert_eq!(scale_label(1.0), "100%");
        assert_eq!(scale_label(1.5), "150%");
    }

    #[test]
    fn label_rounds_to_the_nearest_percent() {
        assert_eq!(scale_label(1.234), "123%");
        assert_eq!(scale_label(1.236), "124%");
        // 1.005 is the knife edge: it multiplies to exactly 100.5 in f32, and
        // `f32::round` breaks a tie away from zero. Unreachable in practice —
        // the scale only ever holds multiples of SCALE_STEP.
        assert_eq!(scale_label(1.005), "101%");
    }

    #[test]
    fn label_clamps_before_formatting() {
        assert_eq!(scale_label(0.1), "85%");
        assert_eq!(scale_label(4.0), "150%");
        assert_eq!(scale_label(f32::NAN), "100%");
    }

    // ─── scaled_size ─────────────────────────────────────────────────────────

    #[test]
    fn scaled_size_is_identity_at_default_scale() {
        assert!(approx(scaled_size(BASE_HEADING, 1.0), 19.0));
        assert!(approx(scaled_size(BASE_BODY, 1.0), 13.5));
        assert!(approx(scaled_size(BASE_SMALL, 1.0), 11.5));
        assert!(approx(scaled_size(BASE_MONO, 1.0), 12.5));
    }

    #[test]
    fn scaled_size_rounds_to_one_decimal() {
        // 11.5 * 0.85 = 9.775 → 9.8
        assert!(approx(scaled_size(BASE_SMALL, 0.85), 9.8));
        // 13.5 * 1.5 = 20.25 → 20.3
        assert!(approx(scaled_size(BASE_BODY, 1.5), 20.3));
    }

    #[test]
    fn scaled_size_never_compounds() {
        // Feeding the result back in is not what `apply_text_scale` does, but
        // the derivation being a pure function of the *base* is why: the same
        // (base, scale) pair always yields the same size.
        let once = scaled_size(BASE_BODY, 1.25);
        assert!(approx(scaled_size(BASE_BODY, 1.25), once));
        assert!(scaled_size(once, 1.25) > once);
    }

    #[test]
    fn scaled_size_ignores_non_finite_scale() {
        assert!(approx(scaled_size(BASE_BODY, f32::NAN), BASE_BODY));
        assert!(approx(
            scaled_size(BASE_HEADING, f32::INFINITY),
            BASE_HEADING
        ));
    }
}
