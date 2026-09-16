//! The control set: the quiet siblings of the primary plate — the ghost-button
//! family, the destructive plate, the disclosure chevron, the dotted-leader
//! key/value line, the count chip and the diamond bullet.
//!
//! Pure presentation over `theme`, `ornament`, `register` and the plate form in
//! `components`; zero I/O, total for degenerate inputs, and the only clock is
//! `animate_bool_with_time`.
//!
//! Every control here is drawn rather than themed, because egui resolves a
//! widget's visuals from the *previous* frame's response — which is one frame
//! late for a focus ring, and the ring is the only thing telling a keyboard
//! user where they are.

use eframe::egui;
use egui::{FontFamily, Sense, Stroke, StrokeKind, Vec2};
use egui::{pos2, vec2};

use super::{components, ornament, register, theme};

/// The secondary action: a hairline-outlined control, transparent at rest, with
/// a gold rule that sweeps in along its foot under the pointer.
pub fn ghost_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    ghost(
        ui,
        label,
        theme::step::LABEL,
        vec2(theme::space::L, theme::space::M),
    )
}

/// The row-sized [`ghost_button`], for the dense registers where a full-height
/// control would break the line pitch.
pub fn ghost_small(ui: &mut egui::Ui, label: &str) -> egui::Response {
    ghost(
        ui,
        label,
        theme::step::META,
        vec2(theme::space::M, theme::space::S),
    )
}

/// The ghost body.
///
/// * **rest** — no fill, a `rule_divider` hairline, the label in `ink`.
/// * **hover** — the ground lifts to `bg_raise`, the hairline warms to
///   `gold_deep`, and a 2px gold rule sweeps along the foot from the left.
/// * **focus** — the focus ring outside the control and the foot rule drawn to
///   its full length, with the resting ground kept so focus never has to be
///   told apart from hover by colour alone.
/// * **pressed** — the hairline goes to full `gold` and the label drops one
///   pixel.
/// * **disabled** — hairline and label in `ink_disabled`, no fill, no sweep, no
///   ring, no pointer cursor.
fn ghost(ui: &mut egui::Ui, label: &str, step: f32, pad: Vec2) -> egui::Response {
    let enabled = ui.is_enabled();
    let face = if enabled {
        theme::ink()
    } else {
        theme::ink_disabled()
    };
    let galley = ui.painter().layout_no_wrap(
        label.to_owned(),
        theme::font(step, theme::fam_medium()),
        face,
    );
    let size = vec2(
        galley.size().x + pad.x * 2.0,
        (theme::sized(step) + pad.y * 2.0).max(galley.size().y),
    );
    let (rect, resp) = ui.allocate_at_least(size, Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, label));

    let pressed = enabled && resp.is_pointer_button_down_on();
    let hovered = enabled && resp.hovered();
    let focused = enabled && resp.has_focus();
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let reached = ui.ctx().animate_bool_with_time(
        resp.id,
        hovered || focused,
        theme::dur(theme::motion::STATE),
    );
    let sweep = if !enabled {
        0.0
    } else if pressed {
        1.0
    } else {
        reached
    };

    let body = components::snapped(rect);
    let edge = if !enabled {
        // Held back deliberately: at full strength `ink_disabled` is a *heavier*
        // line than the resting rule in both palettes, so an unusable control
        // would be the loudest thing in the row.
        theme::alpha(theme::ink_disabled(), 140)
    } else if pressed {
        theme::gold()
    } else if hovered {
        theme::gold_deep()
    } else {
        theme::rule_divider()
    };
    let p = ui.painter();
    if hovered || pressed {
        p.rect_filled(body, theme::R_CONTROL, theme::bg_raise());
    }
    p.rect_stroke(
        body,
        theme::R_CONTROL,
        Stroke::new(theme::RULE_W, edge),
        StrokeKind::Inside,
    );

    let mut at = components::centre(rect, galley.size());
    if pressed {
        at.y += 1.0;
    }
    p.galley(at, galley, face);

    if sweep > 0.0 && body.width() > theme::space::M {
        let from = body.left() + theme::space::S;
        let span = (body.width() - theme::space::M) * sweep;
        p.hline(
            egui::Rangef::new(from, from + span),
            theme::snap(body.bottom() - theme::RULE_ACCENT_W),
            Stroke::new(theme::RULE_ACCENT_W, theme::alpha(theme::gold(), 215)),
        );
    }
    if focused {
        register::focus_ring(p, rect);
    }
    resp
}

/// The destructive action: the plate form in the danger family, so it is the
/// same object as the primary it sits beside in a confirm dialog and differs
/// only in colour. States are [`components::plate`]'s.
pub fn bevel_danger(ui: &mut egui::Ui, label: &str) -> egui::Response {
    components::plate(
        ui,
        label,
        theme::danger_fill(),
        theme::danger_hover(),
        theme::on_danger(),
    )
}

/// The disclosure mark: a painter-drawn triangle in a square click cell,
/// pointing right when shut and turning through a quarter circle as the section
/// opens. `ink_muted` at rest, `ink` under the pointer or while focused, `gold`
/// while pressed, `ink_disabled` when the cell cannot be used; the focus ring
/// goes round the whole cell.
pub fn chevron(ui: &mut egui::Ui, open: bool) -> egui::Response {
    let enabled = ui.is_enabled();
    let side = theme::sized(theme::step::TITLE) + theme::space::XS;
    let (rect, resp) = ui.allocate_exact_size(vec2(side, side), Sense::click());
    let name = if open { "Collapse" } else { "Expand" };
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, name));
    if enabled && resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let turn = ui
        .ctx()
        .animate_bool_with_time(resp.id, open, theme::dur(theme::motion::STATE));
    let color = if !enabled {
        theme::ink_disabled()
    } else if resp.is_pointer_button_down_on() {
        theme::gold()
    } else if resp.hovered() || resp.has_focus() {
        theme::ink()
    } else {
        theme::ink_muted()
    };
    // Isoceles triangle pointing right at rest (vertex angles 0/140/220
    // degrees), rotated toward down as the section opens.
    let angle = turn * std::f32::consts::FRAC_PI_2;
    let c = rect.center();
    let r = side * 0.3;
    let points: Vec<_> = [0.0_f32, 140.0, 220.0]
        .into_iter()
        .map(|deg| {
            let a = deg.to_radians() + angle;
            pos2(c.x + r * a.cos(), c.y + r * a.sin())
        })
        .collect();
    let p = ui.painter();
    p.add(egui::Shape::convex_polygon(points, color, Stroke::NONE));
    if enabled && resp.has_focus() {
        register::focus_ring(p, rect);
    }
    resp
}

/// A key and its value on one line, joined by a book-index leader: the key in
/// `ink_muted` on the left, the value in the tabular face on the right, and a
/// run of hairline dots across the gap on the shared baseline. The value is the
/// record, so when the line is too narrow the key elides and the value stays
/// whole.
pub fn leader_row(ui: &mut egui::Ui, label: &str, value: &str) {
    let h = theme::sized(theme::step::BODY) + theme::space::S;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width().max(0.0), h), Sense::hover());
    if rect.width() <= 0.0 {
        return;
    }
    let p = ui.painter().with_clip_rect(rect);
    let value_galley = p.layout_no_wrap(
        value.to_owned(),
        theme::font(theme::step::DATA, theme::fam_mono()),
        theme::ink(),
    );
    let value_w = value_galley.size().x;

    let mut job = egui::text::LayoutJob::single_section(
        label.to_owned(),
        egui::TextFormat {
            font_id: theme::font(theme::step::LABEL, FontFamily::Proportional),
            color: theme::ink_muted(),
            ..Default::default()
        },
    );
    job.wrap = egui::text::TextWrapping::truncate_at_width(
        (rect.width() - value_w - theme::space::XL).max(0.0),
    );
    let label_galley = p.layout_job(job);
    let label_size = label_galley.size();

    let from = rect.left() + label_size.x + theme::space::M;
    let to = rect.right() - value_w - theme::space::M;
    // Leaders only when a real gap remains; below that the line is already full.
    if to - from >= theme::space::L {
        // The value's own metrics, so the dots keep sitting on its baseline at
        // every text scale.
        let baseline = rect.center().y + value_galley.size().y * 0.3;
        let count = (((to - from) / theme::space::S).floor() as usize + 1).min(2048);
        let dot = theme::alpha(theme::ink_faint(), 120);
        for k in 0..count {
            p.circle_filled(pos2(from + k as f32 * theme::space::S, baseline), 0.7, dot);
        }
    }
    p.galley(
        pos2(rect.left(), rect.center().y - label_size.y * 0.5),
        label_galley,
        theme::ink_muted(),
    );
    p.galley(
        pos2(
            rect.right() - value_w,
            rect.center().y - value_galley.size().y * 0.5,
        ),
        value_galley,
        theme::ink(),
    );
}

/// A tally: the count in the tabular face inside a square-cut gold chip, at
/// least as wide as it is tall so a single digit reads as a mark rather than a
/// sliver. Nothing is drawn for zero — an absent count is not a count of none.
pub fn count_chip(ui: &mut egui::Ui, n: usize) {
    if n == 0 {
        return;
    }
    let text = if n > 99 {
        "99+".to_owned()
    } else {
        n.to_string()
    };
    let c = theme::gold();
    let galley = ui.painter().layout_no_wrap(
        text,
        theme::font(theme::step::CAPTION, theme::fam_mono_medium()),
        c,
    );
    let h = theme::sized(theme::step::META) + theme::space::S;
    let (rect, _) = ui.allocate_exact_size(
        vec2((galley.size().x + theme::space::M).max(h), h),
        Sense::hover(),
    );
    let p = ui.painter();
    p.rect_filled(rect, theme::R_CHIP, theme::wash::of(c, theme::wash::TINT));
    p.rect_stroke(
        rect,
        theme::R_CHIP,
        Stroke::new(theme::RULE_W, theme::alpha(c, 110)),
        StrokeKind::Inside,
    );
    p.galley(components::centre(rect, galley.size()), galley, c);
}

/// The house diamond, sized and spaced to open a line of running text.
pub fn diamond_bullet(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(
        vec2(theme::space::L, theme::sized(theme::step::BODY)),
        Sense::hover(),
    );
    ornament::diamond(ui.painter(), rect.center(), 2.2, theme::gold_deep());
}
