//! First-light onboarding shell for the zero-session install: the opening
//! page of the manuscript. A watermarked colophon panel (`first_light_frame`)
//! hosts a column of numbered step medallions joined by a strapwork thread;
//! the integrator pours the real forms into each step. Pure presentation over
//! `theme` and `ornament` — zero I/O, deterministic, total for degenerate
//! inputs.

use eframe::egui;
use egui::{Color32, Margin, Painter, Pos2, Rangef, Sense, Stroke};
use egui::{pos2, vec2};

use super::{a11y, ornament, theme};

/// The medallion in words. The numeral is painted, and the ring's strength is
/// the only thing separating the step in hand from the ones still to come —
/// neither survives being looked at without eyes.
const STEP_LEAD: &str = "step";
const STEP_ACTIVE: &str = "you are here";
const STEP_FUTURE: &str = "not started yet";

pub enum StepState {
    /// The step the user is on now — gold ring, full-strength number.
    Active,
    /// Not yet reachable — faint ring and number.
    Future,
}

/// A numbered step medallion: a khatam-ringed disc with the step number at its
/// center. Allocates 34x34.
pub fn medallion(ui: &mut egui::Ui, n: u8, state: StepState) {
    let (rect, resp) = ui.allocate_exact_size(vec2(34.0, 34.0), Sense::hover());
    // Named, not focusable: the step's own title and hint are real labels
    // beside it, so this rides with them rather than claiming a stop of its own.
    a11y::describe(&resp, &step_sentence(n, &state));
    if !rect.is_finite() {
        return;
    }
    let c = rect.center();
    let p = ui.painter();
    match state {
        StepState::Active => {
            p.circle_filled(c, 16.5, theme::wash::of(theme::gold(), theme::wash::TINT));
            ornament::khatam(p, c, 15.0, theme::gold(), false);
            numeral(p, c, n, theme::ink());
        }
        StepState::Future => {
            ornament::khatam(p, c, 15.0, theme::ink_faint(), false);
            numeral(p, c, n, theme::ink_faint());
        }
    }
}

/// The sentence one medallion announces: its number, and whether it is the
/// step in hand. It cannot say "of three" — the column's length belongs to the
/// caller laying the steps out, not to one medallion.
fn step_sentence(n: u8, state: &StepState) -> String {
    let standing = match state {
        StepState::Active => STEP_ACTIVE,
        StepState::Future => STEP_FUTURE,
    };
    format!("{STEP_LEAD} {n}, {standing}")
}

/// The vertical strapwork thread joining medallions: a short girih-flavored
/// strand (a hairline thread carrying a small diamond at its midpoint) of the
/// given height, centered in a 34px column so it aligns under the medallions.
/// Pure ornament — it joins two medallions that each say what they are, so it
/// stays silent rather than announcing a line.
pub fn connector(ui: &mut egui::Ui, height: f32) {
    if !height.is_finite() || height <= 0.0 {
        return;
    }
    let (rect, _) = ui.allocate_exact_size(vec2(34.0, height), Sense::hover());
    if !rect.is_finite() {
        return;
    }
    let p = ui.painter();
    // Pixel-centered so the hairline stays crisp; the diamond rides the line.
    let x = theme::snap(rect.center().x);
    p.vline(
        x,
        Rangef::new(rect.top(), rect.bottom()),
        Stroke::new(theme::RULE_W, theme::alpha(theme::gold(), 56)),
    );
    ornament::diamond(
        p,
        pos2(x, rect.center().y),
        2.2,
        theme::alpha(theme::gold(), 102),
    );
}

/// The first-light panel: a full-width frame with a large ghost-khatam
/// watermark and generous padding, hosting whatever the integrator lays out
/// inside (the step column). Notched-colophon silhouette: one 14px 45-degree
/// cut on the top-right, hairline stroke, BG1 fill.
pub fn first_light_frame(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    const NOTCH: f32 = 14.0;
    // Reserved slots keep the body and the watermark beneath whatever `add`
    // paints — the notched_card placeholder-then-set idiom, twice over.
    let bg = ui.painter().add(egui::Shape::Noop);
    let wm = ui.painter().add(egui::Shape::Noop);
    let rect = egui::Frame::new()
        .inner_margin(Margin::same(22))
        .show(ui, |ui| {
            let w = ui.available_width().max(0.0);
            ui.set_min_width(w);
            add(ui);
        })
        .response
        .rect;
    if !rect.is_finite() || !rect.is_positive() {
        return;
    }
    let notch = NOTCH.min(rect.width()).min(rect.height());
    let points = vec![
        rect.left_top(),
        pos2(rect.right() - notch, rect.top()),
        pos2(rect.right(), rect.top() + notch),
        rect.right_bottom(),
        rect.left_bottom(),
    ];
    ui.painter().set(
        bg,
        egui::Shape::convex_polygon(
            points,
            theme::bg_chrome(),
            Stroke::new(theme::RULE_W, theme::rule_hair()),
        ),
    );
    let center = pos2(rect.right() - rect.height() * 0.42, rect.center().y);
    let radius = rect.height() * 0.38;
    // Set through a painter clipped to the panel so a narrow rect cannot let
    // the ghost spill past the left edge.
    ui.painter_at(rect)
        .set(wm, ghost_khatam(center, radius, theme::gold()));
}

/// One step numeral centered on `c`.
fn numeral(p: &Painter, c: Pos2, n: u8, color: Color32) {
    let galley = p.layout_no_wrap(
        n.to_string(),
        theme::font(theme::step::LABEL, theme::fam_semibold()),
        color,
    );
    let pos = c - galley.size() / 2.0;
    p.galley(pos, galley, color);
}

/// The khatam outline as one retained shape, so it can land in a reserved
/// paint slot beneath content — `ornament::khatam` paints immediately and
/// cannot. Same construction as the ornament original; `color` arrives at full
/// strength and is washed to watermark weight here.
fn ghost_khatam(center: Pos2, radius: f32, color: Color32) -> egui::Shape {
    if !center.is_finite() || !radius.is_finite() || radius <= 0.0 {
        return egui::Shape::Noop;
    }
    let ghost = theme::wash::of(color, theme::wash::GHOST);
    let outline = Stroke::new(theme::RULE_W, ghost);
    egui::Shape::Vec(vec![
        egui::Shape::closed_line(square(center, radius, std::f32::consts::FRAC_PI_4), outline),
        egui::Shape::closed_line(square(center, radius, 0.0), outline),
        egui::Shape::circle_filled(center, radius * 0.08, ghost),
    ])
}

/// Four vertices of a square with circumradius `radius`, rotated by `phase`.
fn square(center: Pos2, radius: f32, phase: f32) -> Vec<Pos2> {
    (0..4)
        .map(|k| {
            let a = phase + k as f32 * std::f32::consts::FRAC_PI_2;
            pos2(center.x + radius * a.cos(), center.y + radius * a.sin())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_step_in_hand_is_told_apart_from_the_ones_to_come() {
        assert_eq!(step_sentence(1, &StepState::Active), "step 1, you are here");
        assert_eq!(
            step_sentence(2, &StepState::Future),
            "step 2, not started yet"
        );
        assert_eq!(
            step_sentence(3, &StepState::Future),
            "step 3, not started yet"
        );
    }
}
