//! Marginalia for the sidebar's multi-selection: the manuscript brace
//! (accolade) a scribe would set against a passage — one per contiguous run of
//! selected rows, drawn in the margin as two mirrored halves, each a
//! quarter-ellipse cusp swell, a straight spine, and a quarter-ellipse
//! terminal curl hooking into the margin — plus the bulk-action bar at the
//! sidebar foot. The brace reveals from its cusp outward by arc length, so it
//! reads as being drawn on, pen-down at the midpoint. Pure presentation over
//! `theme` and `ornament`; zero I/O, total for degenerate inputs, and the
//! brace's only clock is the caller-supplied `t`.

use eframe::egui;
use egui::{Pos2, RichText, Sense, Stroke};
use egui::{pos2, vec2};

use super::{components, controls, ornament, theme};

/// Runs shorter than this get no brace — a mark this small reads as an
/// artifact, not an accolade.
const MIN_RUN: f32 = 8.0;
/// Cusp reach toward the rows (+x), before short-run scaling.
const CUSP_REACH: f32 = 6.0;
/// Terminal-curl reach into the margin (-x), before short-run scaling.
const TERM_REACH: f32 = 5.0;
const CUSP_STEPS: usize = 12;
const TERM_STEPS: usize = 10;
/// Structural bound on one half's polyline: both arcs are fixed-step and the
/// spine is a single segment, so run height cannot grow the allocation.
const MAX_HALF_PTS: usize = CUSP_STEPS + TERM_STEPS + 2;

/// The vertical extent of one contiguous run of selected rows, in screen space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Run {
    pub top: f32,
    pub bottom: f32,
}

/// Draws one manuscript brace per run, in the margin at horizontal position `x`
/// (the brace's spine sits on `x`, cusp pointing toward +x, i.e. toward the
/// rows). `t` is 0..=1 for the draw-on animation; nothing is drawn at t <= 0.
pub fn brace(painter: &egui::Painter, x: f32, runs: &[Run], t: f32) {
    if !x.is_finite() || !t.is_finite() || t <= 0.0 {
        return;
    }
    let stroke = Stroke::new(1.0, theme::GOLD.linear_multiply(0.75));
    for run in runs {
        let Some(paths) = brace_paths(x, run, t) else {
            continue;
        };
        for half in paths.halves {
            if half.len() >= 2 {
                painter.add(egui::Shape::line(half, stroke));
            }
        }
        // Pen-down mark: the house diamond grows at the cusp tip.
        ornament::diamond(
            painter,
            paths.cusp,
            2.0 * paths.ease,
            theme::GOLD.linear_multiply(0.7 * paths.ease),
        );
    }
}

/// What the bulk-action bar reported this frame.
#[derive(Clone, Copy, Debug)]
pub struct BulkOut {
    pub start: bool,
    pub stop: bool,
    pub clear: bool,
}

/// The bulk-action bar that appears at the sidebar foot while a selection
/// exists: a count, then the actions that make sense for what is selected.
/// `any_running` / `any_stopped` gate which verbs are offered.
pub fn bulk_bar(ui: &mut egui::Ui, count: usize, any_running: bool, any_stopped: bool) -> BulkOut {
    let mut out = BulkOut {
        start: false,
        stop: false,
        clear: false,
    };
    components::notched_card(ui, Some(theme::GOLD), |ui| {
        ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            controls::count_chip(ui, count);
            let word = if count == 0 {
                "none selected"
            } else {
                "selected"
            };
            ui.label(
                RichText::new(word)
                    .size(11.5)
                    .family(theme::fam_medium())
                    .color(theme::DIM),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if controls::ghost_small(ui, "clear").clicked() {
                    out.clear = true;
                }
            });
        });
        // Two fixed-width verb slots, always allocated, so the row never
        // reflows as verbs come and go; an absent verb leaves the scribe's
        // null mark instead of a hole.
        let gap = 8.0;
        let slot_w = ((ui.available_width() - gap) / 2.0).max(0.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            out.start = verb_slot(ui, slot_w, any_stopped, |ui| {
                components::bevel_primary(ui, "start")
            });
            out.stop = verb_slot(ui, slot_w, any_running, |ui| {
                controls::bevel_danger(ui, "stop")
            });
        });
    });
    out
}

/// A fixed-size slot for one bulk verb: the button justified to fill it when
/// offered, a faint centered diamond when not. Returns whether it was clicked.
fn verb_slot(
    ui: &mut egui::Ui,
    width: f32,
    offered: bool,
    add: impl FnOnce(&mut egui::Ui) -> egui::Response,
) -> bool {
    if !width.is_finite() || width < 2.0 {
        return false;
    }
    let size = vec2(width, 30.0);
    if offered {
        ui.allocate_ui_with_layout(
            size,
            egui::Layout::centered_and_justified(egui::Direction::LeftToRight),
            |ui| add(ui).clicked(),
        )
        .inner
    } else {
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        ornament::diamond(
            ui.painter(),
            rect.center(),
            2.2,
            theme::FAINT.linear_multiply(0.35),
        );
        false
    }
}

// ─── brace geometry (pure — no painter, exhaustively testable) ───────────────

/// Both halves of one brace, already revealed to the animation's arc-length
/// fraction, plus the cusp tip and eased t for the pen-down mark.
struct BracePaths {
    halves: [Vec<Pos2>; 2],
    cusp: Pos2,
    ease: f32,
}

/// Validates one run and builds its brace. `None` for non-finite or inverted
/// bounds, runs under [`MIN_RUN`], non-finite `x`, or `t <= 0`.
fn brace_paths(x: f32, run: &Run, t: f32) -> Option<BracePaths> {
    if !x.is_finite() || !t.is_finite() || t <= 0.0 {
        return None;
    }
    if !run.top.is_finite() || !run.bottom.is_finite() || run.bottom - run.top < MIN_RUN {
        return None;
    }
    let t = t.min(1.0);
    // Ease-out: the pen moves decisively off the cusp, settles at the curls.
    let ease = 1.0 - (1.0 - t) * (1.0 - t);
    let mid = (run.top + run.bottom) * 0.5;
    let h = (run.bottom - run.top) * 0.5;
    let cusp = pos2(x + CUSP_REACH.min(h * 0.5), mid);
    Some(BracePaths {
        halves: [
            half_path(x, mid, run.top, h, ease),
            half_path(x, mid, run.bottom, h, ease),
        ],
        cusp,
        ease,
    })
}

/// One half of the accolade, cusp tip -> spine -> terminal curl, revealed to
/// `ease` of its arc length. The cusp arc keeps a horizontal tangent at the
/// tip (so the mirrored halves meet in a point) and a vertical one at the
/// spine; the terminal arc is the reverse, hooking flat into the margin.
fn half_path(x: f32, mid: f32, edge: f32, h: f32, ease: f32) -> Vec<Pos2> {
    let dir = if edge >= mid { 1.0 } else { -1.0 };
    let k = (h * 0.40).min(7.0);
    let c = (h * 0.35).min(5.5);
    let d_cusp = CUSP_REACH.min(h * 0.5);
    let d_term = TERM_REACH.min(h * 0.4);
    let jy = mid + dir * k;
    let mut full = Vec::with_capacity(MAX_HALF_PTS);
    for i in 0..=CUSP_STEPS {
        let phi = std::f32::consts::FRAC_PI_2 * (1.0 - i as f32 / CUSP_STEPS as f32);
        full.push(pos2(
            x + d_cusp * (1.0 - phi.cos()),
            jy - dir * k * phi.sin(),
        ));
    }
    full.push(pos2(x, edge - dir * c));
    for i in 1..=TERM_STEPS {
        let psi = std::f32::consts::FRAC_PI_2 * i as f32 / TERM_STEPS as f32;
        full.push(pos2(
            x - d_term * (1.0 - psi.cos()),
            edge - dir * c * (1.0 - psi.sin()),
        ));
    }
    reveal(&full, ease)
}

/// Truncates `path` to the leading `frac` of its cumulative arc length,
/// interpolating the final point. Under 2 points there is nothing to draw.
fn reveal(path: &[Pos2], frac: f32) -> Vec<Pos2> {
    if path.len() < 2 || !frac.is_finite() || frac <= 0.0 {
        return Vec::new();
    }
    if frac >= 1.0 {
        return path.to_vec();
    }
    let total: f32 = path.windows(2).map(|w| w[0].distance(w[1])).sum();
    if !total.is_finite() || total <= 0.0 {
        return Vec::new();
    }
    let target = total * frac;
    let mut out = vec![path[0]];
    let mut walked = 0.0;
    for w in path.windows(2) {
        let seg = w[0].distance(w[1]);
        if walked + seg >= target {
            let s = if seg > 0.0 {
                (target - walked) / seg
            } else {
                0.0
            };
            out.push(w[0].lerp(w[1], s));
            return out;
        }
        walked += seg;
        out.push(w[1]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(top: f32, bottom: f32) -> Run {
        Run { top, bottom }
    }

    fn poly_len(path: &[Pos2]) -> f32 {
        path.windows(2).map(|w| w[0].distance(w[1])).sum()
    }

    #[test]
    fn full_brace_reaches_cusp_and_terminals() {
        let p = brace_paths(100.0, &run(40.0, 120.0), 1.0).unwrap();
        assert_eq!(p.cusp, pos2(106.0, 80.0));
        for (half, edge) in [(&p.halves[0], 40.0_f32), (&p.halves[1], 120.0)] {
            let first = half[0];
            let last = half[half.len() - 1];
            assert!(
                (first.x - 106.0).abs() < 1e-3,
                "half starts at the cusp tip"
            );
            assert!((first.y - 80.0).abs() < 1e-3);
            assert!(
                (last.x - 95.0).abs() < 1e-3,
                "terminal hooks into the margin"
            );
            assert!(
                (last.y - edge).abs() < 1e-3,
                "terminal lands on the run edge"
            );
        }
    }

    #[test]
    fn halves_mirror_about_the_midline() {
        let p = brace_paths(50.0, &run(10.0, 90.0), 1.0).unwrap();
        assert_eq!(p.halves[0].len(), p.halves[1].len());
        for (a, b) in p.halves[0].iter().zip(&p.halves[1]) {
            assert!((a.x - b.x).abs() < 1e-3);
            assert!((a.y + b.y - 100.0).abs() < 1e-3);
        }
    }

    #[test]
    fn rejects_non_finite_inputs() {
        let ok = run(0.0, 40.0);
        assert!(brace_paths(f32::NAN, &ok, 1.0).is_none());
        assert!(brace_paths(f32::INFINITY, &ok, 1.0).is_none());
        assert!(brace_paths(0.0, &ok, f32::NAN).is_none());
        assert!(brace_paths(0.0, &run(f32::NAN, 40.0), 1.0).is_none());
        assert!(brace_paths(0.0, &run(0.0, f32::INFINITY), 1.0).is_none());
        assert!(brace_paths(0.0, &run(f32::NEG_INFINITY, 0.0), 1.0).is_none());
    }

    #[test]
    fn rejects_degenerate_and_inverted_runs() {
        assert!(brace_paths(0.0, &run(40.0, 40.0), 1.0).is_none());
        assert!(brace_paths(0.0, &run(40.0, 10.0), 1.0).is_none());
        assert!(brace_paths(0.0, &run(0.0, MIN_RUN - 0.5), 1.0).is_none());
        assert!(brace_paths(0.0, &run(0.0, MIN_RUN), 1.0).is_some());
    }

    #[test]
    fn rejects_t_at_or_below_zero_and_clamps_above_one() {
        let r = run(0.0, 60.0);
        assert!(brace_paths(0.0, &r, 0.0).is_none());
        assert!(brace_paths(0.0, &r, -1.0).is_none());
        let clamped = brace_paths(0.0, &r, 5.0).unwrap();
        let full = brace_paths(0.0, &r, 1.0).unwrap();
        assert_eq!(clamped.halves[0], full.halves[0]);
        assert_eq!(clamped.halves[1], full.halves[1]);
    }

    #[test]
    fn pathological_height_stays_capped_and_finite() {
        let p = brace_paths(0.0, &run(0.0, 1.0e7), 1.0).unwrap();
        for half in &p.halves {
            assert!(half.len() <= MAX_HALF_PTS);
            assert!(half.iter().all(|q| q.x.is_finite() && q.y.is_finite()));
        }
    }

    #[test]
    fn partial_t_reveals_from_the_cusp_by_arc_length() {
        let r = run(0.0, 100.0);
        let part = brace_paths(0.0, &r, 0.25).unwrap();
        let full = brace_paths(0.0, &r, 1.0).unwrap();
        for (p, f) in part.halves.iter().zip(&full.halves) {
            assert_eq!(p[0], f[0], "reveal starts at the cusp tip");
            let frac = poly_len(p) / poly_len(f);
            let ease = 1.0 - 0.75_f32 * 0.75;
            assert!(
                (frac - ease).abs() < 0.02,
                "revealed {frac}, eased t {ease}"
            );
        }
    }

    #[test]
    fn reveal_handles_edge_cases() {
        let a = pos2(0.0, 0.0);
        let b = pos2(10.0, 0.0);
        assert!(reveal(&[], 0.5).is_empty());
        assert!(reveal(&[a], 0.5).is_empty());
        assert!(reveal(&[a, b], 0.0).is_empty());
        assert!(reveal(&[a, b], f32::NAN).is_empty());
        assert!(
            reveal(&[a, a], 0.5).is_empty(),
            "zero-length path draws nothing"
        );
        assert_eq!(reveal(&[a, b], 1.0), vec![a, b]);
        assert_eq!(reveal(&[a, b], 0.5), vec![a, pos2(5.0, 0.0)]);
    }
}
