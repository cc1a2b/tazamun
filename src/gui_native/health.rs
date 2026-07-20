//! Peer-health visual primitives for the Peers view: a painter-drawn
//! signal-strength mark, an RTT sparkline, and up/down transfer-rate arrows.
//! Same discipline as `ornament` — palette colors only, hairline-weight
//! strokes, low-alpha fills; every mark is geometry, never a glyph. Pure
//! presentation over `theme`: zero I/O, deterministic, total for degenerate
//! inputs.

use eframe::egui;

use super::theme;

/// Signal-strength mark: three concentric quarter-arcs opening up-right from a
/// base dot, like a radio mark. `lit` 0..=3 arcs in `color`, the rest in
/// FAINT at 35%. Allocates 18x16.
pub fn signal_arcs(ui: &mut egui::Ui, lit: u8, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(18.0, 16.0), egui::Sense::hover());
    if !rect.is_finite() {
        return;
    }
    let lit = usize::from(lit.min(3));
    let base = egui::pos2(rect.left() + 2.0, rect.bottom() - 2.0);
    let unlit = theme::FAINT.linear_multiply(0.35);
    let p = ui.painter();
    p.circle_filled(base, 1.8, color);
    for (i, radius) in [4.5_f32, 8.0, 11.5].into_iter().enumerate() {
        let arc_color = if i < lit { color } else { unlit };
        // Quarter sweep from straight up to straight right (screen y-down).
        let points: Vec<egui::Pos2> = (0..=10)
            .map(|k| {
                let a = (270.0 + 9.0 * k as f32).to_radians();
                egui::pos2(base.x + radius * a.cos(), base.y + radius * a.sin())
            })
            .collect();
        p.add(egui::Shape::line(points, egui::Stroke::new(1.5, arc_color)));
    }
}

/// RTT sparkline: `samples` are pre-normalized 0..=1 (oldest first). Draws a
/// 1.25px polyline in `color`, a soft fill to the baseline at 10% alpha, and a
/// 2.5px dot on the newest point in GOLD_HI. Fewer than 2 samples draws a
/// centered faint dash instead. Allocates exactly `size`.
pub fn sparkline(ui: &mut egui::Ui, samples: &[f32], size: egui::Vec2, color: egui::Color32) {
    let size = if size.is_finite() {
        size.max(egui::Vec2::ZERO)
    } else {
        egui::Vec2::ZERO
    };
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    if !rect.is_finite() || !rect.is_positive() {
        return;
    }
    let p = ui.painter();
    if samples.len() < 2 {
        let half = (rect.width() * 0.5).min(5.0);
        p.hline(
            egui::Rangef::new(rect.center().x - half, rect.center().x + half),
            rect.center().y,
            egui::Stroke::new(1.0, theme::FAINT),
        );
        return;
    }
    let span_w = rect.width() - 4.0;
    let span_h = rect.height() - 4.0;
    if span_w <= 0.0 || span_h <= 0.0 {
        return;
    }
    let baseline = rect.bottom() - 2.0;
    let step = span_w / (samples.len() - 1) as f32;
    let points: Vec<egui::Pos2> = samples
        .iter()
        .enumerate()
        .map(|(i, &s)| {
            let v = if s.is_nan() { 0.0 } else { s.clamp(0.0, 1.0) };
            egui::pos2(rect.left() + 2.0 + step * i as f32, baseline - v * span_h)
        })
        .collect();
    // Cheap area fill for a non-convex region: one hairline per sample.
    let fill = egui::Stroke::new(1.0, color.linear_multiply(0.10));
    for pt in &points {
        if pt.y < baseline {
            p.line_segment([*pt, egui::pos2(pt.x, baseline)], fill);
        }
    }
    let newest = points.last().copied();
    p.add(egui::Shape::line(points, egui::Stroke::new(1.25, color)));
    if let Some(dot) = newest {
        p.circle_filled(dot, 2.5, theme::GOLD_HI);
    }
}

/// Up/down transfer rates: two small painter arrows (up in GOOD, down in
/// LAPIS) each followed by its label in mono 10.5 (INK when the rate string
/// is not the em-dash placeholder, FAINT when it is).
pub fn rate_arrows(ui: &mut egui::Ui, up: &str, down: &str) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        rate_arrow(ui, theme::GOOD, true);
        rate_label(ui, up);
        rate_arrow(ui, theme::LAPIS, false);
        rate_label(ui, down);
    });
}

/// One 10x14 arrow cell: a 7px vertical shaft, chevron head (half-width 3.5)
/// at the tip — top of the shaft when `up`, bottom otherwise.
fn rate_arrow(ui: &mut egui::Ui, color: egui::Color32, up: bool) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 14.0), egui::Sense::hover());
    if !rect.is_finite() {
        return;
    }
    let c = rect.center();
    let dir = if up { -1.0 } else { 1.0 };
    let tip = egui::pos2(c.x, c.y + 3.5 * dir);
    let stroke = egui::Stroke::new(1.4, color);
    let p = ui.painter();
    p.line_segment([tip, egui::pos2(c.x, c.y - 3.5 * dir)], stroke);
    p.line_segment([tip, egui::pos2(c.x - 3.5, tip.y - 3.5 * dir)], stroke);
    p.line_segment([tip, egui::pos2(c.x + 3.5, tip.y - 3.5 * dir)], stroke);
}

fn rate_label(ui: &mut egui::Ui, s: &str) {
    let color = if s == "—" { theme::FAINT } else { theme::INK };
    ui.label(
        egui::RichText::new(s)
            .size(10.5)
            .family(egui::FontFamily::Monospace)
            .color(color),
    );
}
