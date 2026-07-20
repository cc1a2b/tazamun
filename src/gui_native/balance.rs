//! The conflict scales (الميزان): a painter-drawn balance for the app's most
//! delicate moment — tazamun preserved a copy rather than overwrite it, and
//! the user must weigh which bytes live. A beam on a khatam fulcrum tilts
//! toward the heavier side (log scale, so a 10x gap leans rather than slams),
//! a pan hangs from each end naming its weight, and a card beneath each pan
//! carries what that side is. Pure presentation over `theme` and `ornament` —
//! zero I/O, deterministic, total for degenerate inputs.

use eframe::egui;
use egui::{Color32, FontFamily, FontId, Rect, Stroke, StrokeKind};
use egui::{pos2, vec2};

use super::{ornament, theme};

/// One pan of the scales: what it holds and how it should read.
pub struct Side<'a> {
    /// Short label above the pan, e.g. "preserved copy".
    pub title: &'a str,
    /// Bytes this side holds; drives the tilt and the weight caption.
    pub size: u64,
    /// Human size string already formatted by the caller, e.g. "17 B".
    pub size_text: &'a str,
    /// When this side came to be, already formatted, e.g. "kept 2026-07-17 20:08".
    pub when: &'a str,
    /// One short clause of context, e.g. the quarantine reason.
    pub note: &'a str,
    /// The side's accent (WARN for the preserved copy, LAPIS for the synced one).
    pub accent: egui::Color32,
}

/// Draws the scales: a beam on a khatam fulcrum tilting toward the heavier
/// side, a hanging pan under each end, and a card beneath each pan carrying
/// its title, size, time, and note. Fills the available width; ~190px tall.
pub fn scales(ui: &mut egui::Ui, left: Side<'_>, right: Side<'_>) {
    let w = ui.available_width().max(280.0);
    let (rect, _) = ui.allocate_exact_size(vec2(w, 190.0), egui::Sense::hover());
    if !rect.is_finite() || !rect.is_positive() {
        return;
    }
    let p = ui.painter_at(rect);

    let fulcrum = pos2(rect.center().x, rect.top() + 46.0);
    let half = (w * 0.5 - 70.0).clamp(60.0, 200.0);
    let lean = beam_tilt(left.size, right.size);
    // Screen y grows downward, so the heavier side takes +dy and sinks. The
    // dip is capped so a steep lean on a long beam never drives the pans and
    // their captions into the cards below.
    let dy = (half * lean.sin()).clamp(-9.0, 9.0);
    let dx = half * lean.cos();
    let l_end = pos2(fulcrum.x - dx, fulcrum.y + dy);
    let r_end = pos2(fulcrum.x + dx, fulcrum.y - dy);

    let stand = Stroke::new(1.5, theme::FAINT.linear_multiply(0.5));
    let foot_y = rect.top() + 78.0;
    p.line_segment([fulcrum, pos2(fulcrum.x, foot_y)], stand);
    p.line_segment(
        [
            pos2(fulcrum.x - 12.0, foot_y),
            pos2(fulcrum.x + 12.0, foot_y),
        ],
        stand,
    );

    p.line_segment(
        [l_end, r_end],
        Stroke::new(2.0, theme::GOLD.linear_multiply(0.75)),
    );
    ornament::khatam(&p, fulcrum, 9.0, theme::GOLD, true);

    pan(&p, l_end, left.accent, left.size_text);
    pan(&p, r_end, right.accent, right.size_text);

    // True-vertical plumb so the tilt reads; starts just below the khatam.
    p.line_segment(
        [
            pos2(fulcrum.x, fulcrum.y + 10.0),
            pos2(fulcrum.x, rect.top() + 92.0),
        ],
        Stroke::new(1.0, theme::FAINT.linear_multiply(0.25)),
    );

    let cw = (w - 30.0) * 0.5;
    let top = rect.top() + 96.0;
    side_card(
        &p,
        Rect::from_min_max(
            pos2(rect.left() + 10.0, top),
            pos2(rect.left() + 10.0 + cw, rect.bottom()),
        ),
        &left,
    );
    side_card(
        &p,
        Rect::from_min_max(
            pos2(rect.right() - 10.0 - cw, top),
            pos2(rect.right() - 10.0, rect.bottom()),
        ),
        &right,
    );
}

/// Beam angle in radians, positive when the left side is heavier. Log scale
/// so magnitude differences lean the beam instead of slamming it; equal sizes
/// give a level beam.
fn beam_tilt(left: u64, right: u64) -> f32 {
    let ratio = ((left.max(1) as f32).ln() - (right.max(1) as f32).ln()) / 6.0;
    let tilt = ratio.clamp(-1.0, 1.0) * 0.20;
    if tilt.is_finite() { tilt } else { 0.0 }
}

/// A hanger dropping from the beam end, a shallow bowed pan, and the weight
/// caption centred beneath it.
fn pan(p: &egui::Painter, end: egui::Pos2, accent: Color32, size_text: &str) {
    let hang = pos2(end.x, end.y + 16.0);
    p.line_segment([end, hang], Stroke::new(1.0, accent.linear_multiply(0.55)));
    let points: Vec<egui::Pos2> = (0..=14)
        .map(|k| {
            let x = -22.0 + k as f32 * (44.0 / 14.0);
            let t = x / 22.0;
            pos2(hang.x + x, hang.y + 9.0 * (1.0 - t * t))
        })
        .collect();
    p.add(egui::Shape::line(points, Stroke::new(1.5, accent)));
    let galley = p.layout_no_wrap(
        size_text.to_owned(),
        FontId::new(10.5, FontFamily::Monospace),
        accent,
    );
    let gw = galley.size().x;
    p.galley(pos2(hang.x - gw * 0.5, hang.y + 12.0), galley, accent);
}

/// The card under one pan: BG2 fill, hairline stroke, an accent filament down
/// the left spine, then title / size-and-when / wrapped note, all painted and
/// clipped to the card so nothing steals layout or bleeds.
fn side_card(p: &egui::Painter, rect: Rect, side: &Side<'_>) {
    if !rect.is_finite() || !rect.is_positive() {
        return;
    }
    p.rect_filled(rect, theme::R_CARD, theme::BG2);
    p.rect_stroke(
        rect,
        theme::R_CARD,
        theme::stroke_faint(),
        StrokeKind::Inside,
    );
    let bar_h = (rect.height() - 16.0).max(0.0);
    let bar = Rect::from_center_size(pos2(rect.left() + 1.0, rect.center().y), vec2(2.0, bar_h));
    p.rect_filled(bar, 1.0, side.accent);

    let pc = p.with_clip_rect(rect);
    let x = rect.left() + 12.0;
    let mut y = rect.top() + 10.0;

    let title = pc.layout_no_wrap(
        side.title.to_owned(),
        FontId::new(12.0, theme::fam_medium()),
        side.accent,
    );
    let title_h = title.size().y;
    pc.galley(pos2(x, y), title, side.accent);
    y += title_h.max(14.0) + 3.0;

    let size_color = side.accent.linear_multiply(0.85);
    let size_g = pc.layout_no_wrap(
        side.size_text.to_owned(),
        FontId::new(10.5, FontFamily::Monospace),
        size_color,
    );
    let when_g = pc.layout_no_wrap(
        side.when.to_owned(),
        FontId::new(10.5, FontFamily::Proportional),
        theme::FAINT,
    );
    let line_h = size_g.size().y.max(when_g.size().y).max(12.0);
    let mut mx = x;
    if !side.size_text.is_empty() {
        mx += size_g.size().x + 7.0;
        pc.galley(pos2(x, y), size_g, size_color);
        if !side.when.is_empty() {
            ornament::diamond(
                &pc,
                pos2(mx, y + line_h * 0.5),
                1.8,
                theme::GOLD.linear_multiply(0.5),
            );
            mx += 9.0;
        }
    }
    pc.galley(pos2(mx, y), when_g, theme::FAINT);
    y += line_h + 6.0;

    let wrap = (rect.width() - 24.0).max(10.0);
    let note = pc.layout(
        side.note.to_owned(),
        FontId::new(11.0, FontFamily::Proportional),
        theme::DIM,
        wrap,
    );
    pc.galley(pos2(x, y), note, theme::DIM);
}
