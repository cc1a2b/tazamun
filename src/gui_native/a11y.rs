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
//! The scale half is arithmetic and nothing else. [`super::theme`] owns the
//! type table and every size in it; this module owns only *which* scale the
//! reader is on — the range, the notches inside it, and the label that names
//! one — and hands that number to the theme. It used to keep a second copy of
//! the base sizes and rebuild the table itself, and a copy of a table is a
//! table that drifts: editing the type scale moved the window right up until
//! the reader touched the text size control, which then reinstated the sizes
//! the copy still remembered.
//!
//! The labelling half wraps [`egui::Response::widget_info`], egui's path into
//! its accesskit output. Allocate an interactive rect for the row, paint it,
//! then hand the response a role and a label.
//!
//! One thing that path needs is not in this crate's hands. `accesskit` the
//! types crate is a non-optional dependency of egui 0.35, but the *platform
//! adapter* that carries the tree to the screen reader is `egui-winit`'s, and
//! eframe only pulls it in — and only then calls `Context::enable_accesskit` —
//! under its own `accesskit` feature. `Cargo.toml` takes eframe with
//! `default-features = false` and does not list it, so
//! `Context::accesskit_node_builder` currently returns `None` and every label
//! below, however correct, reaches nobody. Adding `"accesskit"` to eframe's
//! feature list is what switches this module on.
//!
//! The same lever names the painted *figures* — the scales, the peer sky, the
//! status strip — with one rule: a figure announces one sentence, never one
//! fragment per mark. [`sentence`] and [`clause`] build that sentence,
//! [`describe`] attaches it, and [`spoken_ms`] / [`spoken_size`] spell out the
//! units the drawn captions abbreviate, because "12 ms" read aloud is "twelve
//! em ess".
//!
//! Scale arithmetic and sentence building are pure — no [`egui::Context`], no
//! I/O — so clamping, stepping, joining and formatting are exhaustively
//! unit-testable on any host.

use eframe::egui;
use egui::{WidgetInfo, WidgetType};

use super::theme;

// ─── scale range ─────────────────────────────────────────────────────────────

/// Smallest and largest text scale the window offers, and the step between.
///
/// The bounds are the ones [`theme::set_scale`] enforces. They are not a
/// separate policy: a wider range here would only let the control report a
/// percentage the theme quietly refuses to adopt. The step is a twentieth,
/// which is exactly the resolution the theme stores, so every notch survives
/// the round trip through it.
pub const SCALE_MIN: f32 = 0.7;
pub const SCALE_MAX: f32 = 2.2;
const SCALE_STEP: f32 = 0.05;
pub const SCALE_DEFAULT: f32 = 1.0;

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

// ─── application ─────────────────────────────────────────────────────────────

/// Applies `scale` to every text style in the window. Idempotent: the theme
/// derives each size from its own fixed type scale rather than from the sizes
/// currently installed, so repeated calls cannot compound.
pub fn apply_text_scale(ctx: &egui::Context, scale: f32) {
    // Clamped here rather than left to `theme::set_scale`: the bare `f32::clamp`
    // there passes a NaN straight through, and the integer cast behind it turns
    // NaN into zero — a scale of 0.0 is a window with no text in it.
    theme::set_scale(clamp_scale(scale));
    theme::restyle(ctx);
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

/// Names a painter-drawn figure — the scales, the sky, the status strip — to
/// assistive technology. The label role, because a figure reports rather than
/// acts: it reads like a line of text placed where the ink is, instead of the
/// silence an anonymous rect announces.
///
/// Hand it one whole sentence, built by [`sentence`]. A figure that is also a
/// stop on the Tab route allocates its rect with
/// [`egui::Sense::focusable_noninteractive`] — focusable without gaining a
/// pointer behaviour the drawing does not have — and rings itself with
/// [`super::focusnav::ring`].
pub fn describe(resp: &egui::Response, sentence: &str) {
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Label, resp.enabled(), sentence));
}

// ─── sentence building (pure) ────────────────────────────────────────────────

/// The one sentence a composite figure announces: `lead` names the figure and
/// what it holds, `parts` are its marks in reading order.
///
/// A figure is one thing, not a heap of marks. A reader who reaches the peer
/// mesh must hear "peer mesh, 3 peers: …" once — not fourteen fragments as the
/// khatams, threads and captions go by — so every figure joins here rather than
/// labelling each mark it paints. Empty parts drop out, which is what makes an
/// optional clause free at the call site.
pub fn sentence<S: AsRef<str>>(lead: &str, parts: &[S]) -> String {
    let lead = lead.trim();
    let body = clause(parts);
    match (lead.is_empty(), body.is_empty()) {
        (true, _) => body,
        (false, true) => lead.to_owned(),
        (false, false) => format!("{lead}: {body}"),
    }
}

/// The parts of a figure joined into one clause, dropping the empty ones.
pub fn clause<S: AsRef<str>>(parts: &[S]) -> String {
    parts
        .iter()
        .map(|p| p.as_ref().trim())
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Round-trip time with its unit spelled out. "12 ms" is read aloud as "twelve
/// em ess", which is not a duration, so the announcement says the word while
/// the drawn caption keeps the short form.
pub fn spoken_ms(ms: u64) -> String {
    if ms == 1 {
        "1 millisecond".to_owned()
    } else {
        format!("{ms} milliseconds")
    }
}

/// Spells the unit of a size or rate this window has already formatted:
/// "1.5 KB/s" reads as "1.5 kilobytes per second", "1 B" as "1 byte". Text that
/// is not shaped like a value and one of the units
/// [`super::telemetry::fmt_rate`] prints comes back unchanged — an abbreviated
/// caption still beats silence.
pub fn spoken_size(text: &str) -> String {
    const UNITS: [(&str, &str, &str); 5] = [
        ("B", "byte", "bytes"),
        ("KB", "kilobyte", "kilobytes"),
        ("MB", "megabyte", "megabytes"),
        ("GB", "gigabyte", "gigabytes"),
        ("TB", "terabyte", "terabytes"),
    ];
    let text = text.trim();
    let (head, per_second) = match text.strip_suffix("/s") {
        Some(head) => (head.trim_end(), true),
        None => (text, false),
    };
    let Some((value, unit)) = head.rsplit_once(' ') else {
        return text.to_owned();
    };
    let Some(&(_, one, many)) = UNITS.iter().find(|(symbol, _, _)| *symbol == unit) else {
        return text.to_owned();
    };
    let word = if value.trim() == "1" { one } else { many };
    if per_second {
        format!("{value} {word} per second")
    } else {
        format!("{value} {word}")
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, MutexGuard, PoisonError};

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
        assert!(approx(clamp_scale(3.0), SCALE_MAX));
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
        assert_eq!(scale_label(0.1), "70%");
        assert_eq!(scale_label(4.0), "220%");
        assert_eq!(scale_label(f32::NAN), "100%");
    }

    // ─── sentence building ───────────────────────────────────────────────────

    #[test]
    fn sentence_leads_then_lists() {
        let parts = ["laptop direct 12 milliseconds", "desk offline"];
        assert_eq!(
            sentence("peer mesh, 2 peers", &parts),
            "peer mesh, 2 peers: laptop direct 12 milliseconds, desk offline"
        );
    }

    #[test]
    fn sentence_survives_missing_halves() {
        let none: [&str; 0] = [];
        assert_eq!(sentence("peer mesh", &none), "peer mesh");
        assert_eq!(sentence("", &["one", "two"]), "one, two");
        assert_eq!(sentence("", &none), "");
        // A figure whose every clause is optional must not announce a colon
        // with nothing behind it.
        assert_eq!(sentence("status", &["", "  "]), "status");
    }

    #[test]
    fn clause_drops_empty_parts_and_trims() {
        assert_eq!(clause(&["a", "", "b"]), "a, b");
        assert_eq!(clause(&["  a  ", " "]), "a");
        let owned = vec![String::from("x"), String::new()];
        assert_eq!(clause(&owned), "x");
    }

    // ─── spoken units ────────────────────────────────────────────────────────

    #[test]
    fn spoken_ms_says_the_unit() {
        assert_eq!(spoken_ms(12), "12 milliseconds");
        assert_eq!(spoken_ms(0), "0 milliseconds");
        assert_eq!(spoken_ms(1), "1 millisecond");
        assert_eq!(spoken_ms(240), "240 milliseconds");
    }

    #[test]
    fn spoken_size_expands_every_unit_the_window_prints() {
        assert_eq!(spoken_size("17 B"), "17 bytes");
        assert_eq!(spoken_size("1 B"), "1 byte");
        assert_eq!(spoken_size("1.5 KB"), "1.5 kilobytes");
        assert_eq!(spoken_size("3.0 MB"), "3.0 megabytes");
        assert_eq!(spoken_size("2.0 GB"), "2.0 gigabytes");
        assert_eq!(spoken_size("5.0 TB"), "5.0 terabytes");
    }

    #[test]
    fn spoken_size_reads_a_rate_per_second() {
        assert_eq!(spoken_size("1.2 MB/s"), "1.2 megabytes per second");
        assert_eq!(spoken_size("512 B/s"), "512 bytes per second");
        assert_eq!(spoken_size("1 B/s"), "1 byte per second");
    }

    #[test]
    fn spoken_size_passes_unknown_shapes_through() {
        // The em-dash the rate formatter uses for an idle link is an absence,
        // not a measurement — the caller decides what to call it.
        assert_eq!(spoken_size("—"), "—");
        assert_eq!(spoken_size(""), "");
        assert_eq!(spoken_size("17"), "17");
        assert_eq!(spoken_size("17 parsecs"), "17 parsecs");
        assert_eq!(spoken_size("kept just now"), "kept just now");
    }

    // ─── application ─────────────────────────────────────────────────────────

    /// The scale is one process-global and cargo runs these tests on several
    /// threads, so two of them writing it would each read the other's value.
    /// Poison-tolerant: a test that panics holding this must fail for its own
    /// reason, not turn every other one into a lock error.
    static SCALE_LOCK: Mutex<()> = Mutex::new(());

    fn hold_scale() -> MutexGuard<'static, ()> {
        SCALE_LOCK.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The size egui hands a painter for [`TextStyle::Body`]. Read from the
    /// dark table specifically — `theme::restyle` writes both, and pinning one
    /// keeps the assertion independent of the host's system theme.
    fn installed_body(ctx: &egui::Context) -> f32 {
        ctx.style_of(egui::Theme::Dark)
            .text_styles
            .get(&egui::TextStyle::Body)
            .expect("body text style")
            .size
    }

    /// The property this module exists for, and the one that was broken while
    /// it kept a private copy of the type table: moving the scale must move the
    /// size the rest of the window is painted at, and must move egui's own
    /// table with it.
    #[test]
    fn apply_text_scale_moves_the_type_table() {
        let _scale = hold_scale();
        let ctx = egui::Context::default();

        apply_text_scale(&ctx, SCALE_DEFAULT);
        let base = theme::sized(theme::step::BODY);
        assert!(approx(installed_body(&ctx), base));

        apply_text_scale(&ctx, SCALE_MAX);
        let big = theme::sized(theme::step::BODY);
        assert!(big > base, "{big} is not larger than {base}");
        assert!(approx(installed_body(&ctx), big));

        apply_text_scale(&ctx, SCALE_MIN);
        let small = theme::sized(theme::step::BODY);
        assert!(small < base, "{small} is not smaller than {base}");
        assert!(approx(installed_body(&ctx), small));

        apply_text_scale(&ctx, SCALE_DEFAULT);
    }

    /// A NaN reaching the theme's own clamp survives it and then casts to zero
    /// — a window with no text in it. It has to be screened here.
    #[test]
    fn apply_text_scale_screens_a_non_finite_scale() {
        let _scale = hold_scale();
        let ctx = egui::Context::default();

        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            apply_text_scale(&ctx, bad);
            assert!(approx(theme::scale(), SCALE_DEFAULT), "{bad} got through");
            assert!(approx(installed_body(&ctx), theme::step::BODY));
        }
    }

    /// Every notch the buttons can reach must survive the theme's storage,
    /// which keeps the scale in twentieths. A step finer than that would let
    /// two presses of the control produce one change on screen.
    #[test]
    fn every_notch_survives_the_themes_storage() {
        let _scale = hold_scale();

        let mut s = SCALE_MIN;
        while s < SCALE_MAX {
            theme::set_scale(s);
            assert!(approx(theme::scale(), s), "{s} did not survive storage");
            s = step_scale(s, true);
        }
        theme::set_scale(SCALE_MAX);
        assert!(approx(theme::scale(), SCALE_MAX));
        theme::set_scale(SCALE_DEFAULT);
    }
}
