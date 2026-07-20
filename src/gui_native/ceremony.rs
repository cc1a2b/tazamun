//! Ceremonial set-pieces for the native GUI: the invite rendered as a real
//! ticket (perforation, punched notches, QR stub), the rotating-khatam
//! loading mark, keyboard key caps, and dialog adornments. Pure presentation
//! over `theme` and `ornament` — zero I/O, deterministic except the
//! time-driven rotation, total for degenerate inputs.

use eframe::egui;
use egui::{CornerRadius, FontFamily, FontId, Rect, Sense, Stroke, StrokeKind};
use egui::{pos2, vec2};

use super::{ornament, theme};

/// The invite rendered as an actual ticket: a wide rounded body holding the
/// tzm1 string (wrapped monospace) with a small khatam mark, a dashed
/// perforation line, and a right-hand stub carrying the QR (when given) —
/// punched notches top and bottom of the perforation like a real ticket.
/// Fills the available width; height adapts to the wrapped text and stub.
pub fn ticket_card(ui: &mut egui::Ui, ticket: &str, qr: Option<&egui::TextureHandle>) {
    const STUB_W: f32 = 108.0;
    const PAD: f32 = 12.0;
    const NOTCH_R: f32 = 7.0;

    let width = ui.available_width().max(200.0);
    let body_wrap = width - STUB_W - PAD * 3.0 - 18.0;
    let galley = ui.painter().layout(
        ticket.to_owned(),
        FontId::new(10.0, FontFamily::Monospace),
        theme::INK,
        body_wrap.max(60.0),
    );
    let height = (galley.size().y + PAD * 2.0).max(if qr.is_some() { 118.0 } else { 66.0 });
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    if !rect.is_finite() || !rect.is_positive() {
        return;
    }

    let perf_x = rect.right() - STUB_W;
    {
        let p = ui.painter();
        let cr = CornerRadius::same(theme::R_CARD + 2);
        p.rect_filled(rect, cr, theme::BG1);
        p.rect_stroke(rect, cr, theme::stroke_faint(), StrokeKind::Inside);
        p.extend(egui::Shape::dashed_line(
            &[
                pos2(perf_x, rect.top() + 6.0),
                pos2(perf_x, rect.bottom() - 6.0),
            ],
            Stroke::new(1.0, theme::FAINT.linear_multiply(0.6)),
            5.0,
            4.0,
        ));
        // The window base "punches through" at both ends of the perforation.
        p.circle_filled(pos2(perf_x, rect.top()), NOTCH_R, theme::BG0);
        p.circle_filled(pos2(perf_x, rect.bottom()), NOTCH_R, theme::BG0);
        ornament::khatam(
            p,
            pos2(rect.left() + PAD + 7.0, rect.top() + PAD + 7.0),
            7.0,
            theme::GOLD,
            false,
        );
        p.galley(
            pos2(rect.left() + PAD + 18.0, rect.top() + PAD),
            galley,
            theme::INK,
        );
    }

    let stub_center = pos2(perf_x + STUB_W * 0.5, rect.center().y);
    if let Some(qr) = qr {
        let side = (STUB_W - 20.0).min(height - 20.0);
        let qr_rect = Rect::from_center_size(stub_center, vec2(side, side));
        ui.put(
            qr_rect,
            egui::Image::new(qr)
                .fit_to_exact_size(qr_rect.size())
                .texture_options(egui::TextureOptions::NEAREST),
        );
    } else {
        ornament::khatam(ui.painter(), stub_center, 16.0, theme::FAINT, false);
    }
}

/// The signature loading mark: a slowly rotating eight-point star outline in
/// gold, self-repainting (~50ms) only while visible. Allocates size x size.
pub fn loading_mark(ui: &mut egui::Ui, size: f32) {
    if !size.is_finite() || size <= 0.0 {
        return;
    }
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    let angle = (ui.input(|i| i.time) * 0.9) as f32;
    let c = rect.center();
    let r = size * 0.42;
    let p = ui.painter();
    for (phase, color) in [
        (angle, theme::GOLD),
        (
            angle + std::f32::consts::FRAC_PI_4,
            theme::GOLD.linear_multiply(0.7),
        ),
    ] {
        let points: Vec<egui::Pos2> = (0..4)
            .map(|k| {
                let a = phase + k as f32 * std::f32::consts::FRAC_PI_2;
                pos2(c.x + r * a.cos(), c.y + r * a.sin())
            })
            .collect();
        p.add(egui::Shape::closed_line(points, Stroke::new(1.25, color)));
    }
    p.circle_filled(c, size * 0.07, theme::GOLD_HI);
    // Only runs while the mark is drawn — repaint stops with visibility.
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_millis(50));
}

/// A tiny keyboard key cap: rounded 4px chip, BG_INPUT fill, hairline stroke,
/// mono 10 label, min 20px wide, 16px tall.
pub fn keycap(ui: &mut egui::Ui, label: &str) {
    let galley = ui.painter().layout_no_wrap(
        label.to_owned(),
        FontId::new(10.0, FontFamily::Monospace),
        theme::DIM,
    );
    let w = (galley.size().x + 10.0).max(20.0);
    let (rect, _) = ui.allocate_exact_size(vec2(w, 16.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 4.0, theme::BG_INPUT);
    p.rect_stroke(rect, 4.0, theme::stroke_faint(), StrokeKind::Inside);
    p.galley(rect.center() - galley.size() * 0.5, galley, theme::DIM);
}

/// Adorns an already-painted dialog card: two corner flourishes (top-left
/// inward, bottom-right inward) in gold at low strength; when `danger`, a
/// large very-faint BAD khatam watermark behind the card center instead of
/// the gold flourishes' calm.
pub fn adorn_dialog(p: &egui::Painter, rect: Rect, danger: bool) {
    if !rect.is_finite() || !rect.is_positive() {
        return;
    }
    if danger {
        ornament::khatam(
            p,
            rect.center(),
            rect.width().min(rect.height()) * 0.36,
            theme::BAD.linear_multiply(0.06),
            false,
        );
    } else {
        let gold = theme::GOLD.linear_multiply(0.5);
        ornament::corner_flourish(p, rect.left_top(), vec2(1.0, 1.0), 46.0, gold);
        ornament::corner_flourish(p, rect.right_bottom(), vec2(-1.0, -1.0), 46.0, gold);
    }
}
