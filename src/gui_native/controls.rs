//! Crafted secondary controls for the native GUI: the ghost-button family,
//! the red bevel for destructive actions, the animated disclosure chevron,
//! dotted-leader key/value rows, count chips and diamond bullets — the pieces
//! that retire the last stock-egui widgets from the app's quieter corners.
//! Pure presentation over `theme` and `ornament`; zero I/O, total for
//! degenerate inputs, and the only clock is `animate_bool_with_time`.

use eframe::egui;
use egui::{Color32, CornerRadius, FontFamily, FontId, RichText, Sense, Stroke, Vec2};
use egui::{pos2, vec2};

use super::{ornament, theme};

const DANGER_FILL: Color32 = Color32::from_rgb(0x66, 0x24, 0x24);
const DANGER_HOVER: Color32 = Color32::from_rgb(0x7d, 0x2c, 0x2c);
const DANGER_INK: Color32 = Color32::from_rgb(0xff, 0xd9, 0xd9);

/// Secondary action: transparent fill, hairline border; on hover the fill
/// warms to BG3 and a 2px gold underline sweeps in from the left (animated);
/// while pressed the border turns gold. Standard control height (~28).
pub fn ghost_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    ghost_impl(ui, label, 13.0, 28.0, vec2(14.0, 4.0))
}

/// The compact row variant of [`ghost_button`] (~20px tall, 11.5 text) for
/// dense lists (file rows, version rows).
pub fn ghost_small(ui: &mut egui::Ui, label: &str) -> egui::Response {
    ghost_impl(ui, label, 11.5, 20.0, vec2(9.0, 2.0))
}

fn ghost_impl(
    ui: &mut egui::Ui,
    label: &str,
    text_size: f32,
    height: f32,
    padding: Vec2,
) -> egui::Response {
    let resp = ui
        .scope(|ui| {
            ui.spacing_mut().interact_size.y = height;
            ui.spacing_mut().button_padding = padding;
            let v = &mut ui.style_mut().visuals;
            for w in [
                &mut v.widgets.inactive,
                &mut v.widgets.hovered,
                &mut v.widgets.active,
            ] {
                w.fg_stroke = Stroke::new(1.0, theme::INK);
                w.corner_radius = CornerRadius::same(theme::R_BUTTON);
                w.expansion = 0.0;
            }
            v.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
            v.widgets.inactive.bg_stroke = theme::stroke_faint();
            v.widgets.hovered.weak_bg_fill = theme::BG3;
            v.widgets.hovered.bg_stroke = theme::stroke_faint();
            v.widgets.active.weak_bg_fill = theme::BG3;
            v.widgets.active.bg_stroke = Stroke::new(1.0, theme::GOLD);
            ui.add(egui::Button::new(
                RichText::new(label)
                    .size(text_size)
                    .family(theme::fam_medium())
                    .color(theme::INK),
            ))
        })
        .inner;
    let t = ui
        .ctx()
        .animate_bool_with_time(resp.id, resp.hovered(), 0.16);
    if t > 0.0 && resp.rect.width() > 8.0 {
        let left = resp.rect.left() + 4.0;
        ui.painter().hline(
            egui::Rangef::new(left, left + (resp.rect.width() - 8.0) * t),
            resp.rect.bottom() - 2.0,
            Stroke::new(2.0, theme::GOLD.linear_multiply(0.85 * t)),
        );
    }
    resp
}

/// Destructive primary: the bevel treatment in the red family — fill 0x66/24/24,
/// hover 0x7d/2c/2c, label 0xff/d9/d9 (fam_medium), 1px white-12% inner top
/// highlight suppressed while pressed. Min height 30.
pub fn bevel_danger(ui: &mut egui::Ui, label: &str) -> egui::Response {
    let resp = ui
        .scope(|ui| {
            ui.spacing_mut().interact_size.y = 30.0;
            let v = &mut ui.style_mut().visuals;
            for w in [
                &mut v.widgets.inactive,
                &mut v.widgets.hovered,
                &mut v.widgets.active,
            ] {
                w.fg_stroke = Stroke::new(1.0, DANGER_INK);
                w.bg_stroke = Stroke::NONE;
                w.corner_radius = CornerRadius::same(theme::R_BUTTON);
                w.expansion = 0.0;
            }
            v.widgets.inactive.weak_bg_fill = DANGER_FILL;
            v.widgets.hovered.weak_bg_fill = DANGER_HOVER;
            v.widgets.active.weak_bg_fill = DANGER_FILL;
            ui.add(egui::Button::new(
                RichText::new(label)
                    .size(13.5)
                    .family(theme::fam_medium())
                    .color(DANGER_INK),
            ))
        })
        .inner;
    if !resp.is_pointer_button_down_on() && resp.rect.width() > 8.0 {
        ui.painter().hline(
            egui::Rangef::new(resp.rect.left() + 3.0, resp.rect.right() - 3.0),
            resp.rect.top() + 1.5,
            Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 30)),
        );
    }
    resp
}

/// Painter-drawn disclosure chevron in a 16x16 click cell: a right-pointing
/// triangle that rotates smoothly to down when `open` (animate_bool), DIM at
/// rest, INK on hover.
pub fn chevron(ui: &mut egui::Ui, open: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    let t = ui.ctx().animate_bool_with_time(resp.id, open, 0.14);
    let angle = t * std::f32::consts::FRAC_PI_2;
    let c = rect.center();
    let color = if resp.hovered() {
        theme::INK
    } else {
        theme::DIM
    };
    // Isoceles triangle pointing right at rest (vertex angles 0/140/220 deg),
    // rotated toward down as the section opens.
    let points: Vec<_> = [0.0_f32, 140.0, 220.0]
        .into_iter()
        .map(|deg| {
            let a = deg.to_radians() + angle;
            pos2(c.x + 5.0 * a.cos(), c.y + 5.0 * a.sin())
        })
        .collect();
    ui.painter()
        .add(egui::Shape::convex_polygon(points, color, Stroke::NONE));
    resp
}

/// A dotted-leader key/value line (book-index style): label (12, DIM) on the
/// left, value (12, monospace, INK) on the right, and a row of 1px leader dots
/// (FAINT at 40%, one every 4px) filling the gap on the shared baseline.
pub fn leader_row(ui: &mut egui::Ui, label: &str, value: &str) {
    let (rect, _) =
        ui.allocate_exact_size(vec2(ui.available_width().max(0.0), 18.0), Sense::hover());
    if rect.width() <= 0.0 {
        return;
    }
    let p = ui.painter().with_clip_rect(rect);
    let label_galley = p.layout_no_wrap(
        label.to_owned(),
        FontId::new(12.0, FontFamily::Proportional),
        theme::DIM,
    );
    let value_galley = p.layout_no_wrap(
        value.to_owned(),
        FontId::new(12.0, FontFamily::Monospace),
        theme::INK,
    );
    let label_size = label_galley.size();
    let value_size = value_galley.size();
    let dots_from = rect.left() + label_size.x + 8.0;
    let dots_to = rect.right() - value_size.x - 8.0;
    // Leaders only when a real gap remains; a long value just clips instead.
    if dots_to - dots_from >= 12.0 {
        let baseline_y = rect.bottom() - 5.0;
        let count = (((dots_to - dots_from) / 4.0).floor() as usize + 1).min(2048);
        for k in 0..count {
            p.circle_filled(
                pos2(dots_from + k as f32 * 4.0, baseline_y),
                0.7,
                theme::FAINT.linear_multiply(0.4),
            );
        }
    }
    p.galley(
        pos2(rect.left(), rect.center().y - label_size.y / 2.0),
        label_galley,
        theme::DIM,
    );
    p.galley(
        pos2(
            rect.right() - value_size.x,
            rect.center().y - value_size.y / 2.0,
        ),
        value_galley,
        theme::INK,
    );
}

/// A tiny gold count chip (mono 9.5 on GOLD-tinted pill, ~16x14 min); draws
/// nothing when `n == 0`.
pub fn count_chip(ui: &mut egui::Ui, n: usize) {
    if n == 0 {
        return;
    }
    let text = if n > 99 {
        "99+".to_owned()
    } else {
        n.to_string()
    };
    let galley =
        ui.painter()
            .layout_no_wrap(text, FontId::new(9.5, FontFamily::Monospace), theme::GOLD);
    let size = galley.size();
    let (rect, _) = ui.allocate_exact_size(vec2((size.x + 8.0).max(16.0), 14.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 7.0, theme::GOLD.linear_multiply(0.15));
    p.galley(
        pos2(
            rect.center().x - size.x / 2.0,
            rect.center().y - size.y / 2.0,
        ),
        galley,
        theme::GOLD,
    );
}

/// A small gold diamond bullet for list lines (10x14 cell, r=2.2 at 50% gold).
pub fn diamond_bullet(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(10.0, 14.0), Sense::hover());
    ornament::diamond(
        ui.painter(),
        rect.center(),
        2.2,
        theme::GOLD.linear_multiply(0.5),
    );
}
