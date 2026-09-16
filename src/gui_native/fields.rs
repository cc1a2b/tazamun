//! Crafted text inputs and the focus system for the native GUI: the house
//! field (a recessed well whose hairline rule grows into a centre-out gold
//! underline on focus), its monospace variant for tickets and paths, the
//! painter-drawn search field with magnifier and clear mark, and the
//! corner-ticked focus ring. Pure presentation over `theme`; zero I/O, total
//! for degenerate inputs, and the only clock is `animate_bool_with_time`.

use eframe::egui;
use egui::{CornerRadius, FontFamily, Margin, Rangef, Rect, Sense, Stroke, StrokeKind};
use egui::{pos2, vec2};

use super::theme;

const FIELD_H: f32 = 30.0;

/// Validation tint for a field's rule and border.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FieldState {
    Neutral,
    Valid,
    Invalid,
}

/// Per-variant knobs for [`field_impl`]; `lead`/`trail` reserve horizontal
/// room inside the well for painter-drawn marks.
struct FieldSpec {
    state: FieldState,
    mono: bool,
    lead: f32,
    trail: f32,
}

/// The house text field: a recessed well with a hairline rule that grows into
/// a gold underline from the centre outward as the field takes focus.
/// `state` tints the rule (settled / blocked) when not Neutral. Height 30.
pub fn text_field(
    ui: &mut egui::Ui,
    text: &mut String,
    hint: &str,
    width: f32,
    state: FieldState,
) -> egui::Response {
    let spec = FieldSpec {
        state,
        mono: false,
        lead: 0.0,
        trail: 0.0,
    };
    field_impl(ui, text, hint, width, spec).0
}

/// As [`text_field`], but the content renders monospace — for tickets, ids,
/// and paths.
pub fn text_field_mono(
    ui: &mut egui::Ui,
    text: &mut String,
    hint: &str,
    width: f32,
) -> egui::Response {
    text_field_mono_state(ui, text, hint, width, FieldState::Neutral)
}

/// Monospace with a validation tint — for input that is self-describing enough
/// to judge as it is typed, such as a `tzm1` ticket.
pub fn text_field_mono_state(
    ui: &mut egui::Ui,
    text: &mut String,
    hint: &str,
    width: f32,
    state: FieldState,
) -> egui::Response {
    let spec = FieldSpec {
        state,
        mono: true,
        lead: 0.0,
        trail: 0.0,
    };
    field_impl(ui, text, hint, width, spec).0
}

/// Outcome of [`search_field`]: the edit response plus whether the drawn
/// clear mark was clicked this frame.
pub struct SearchOut {
    pub response: egui::Response,
    pub cleared: bool,
}

/// A search field: a painter-drawn magnifier at the leading edge and, once
/// there is text, a clear mark at the trailing edge.
pub fn search_field(ui: &mut egui::Ui, text: &mut String, hint: &str, width: f32) -> SearchOut {
    let spec = FieldSpec {
        state: FieldState::Neutral,
        mono: false,
        lead: 16.0,
        trail: if text.is_empty() { 0.0 } else { 18.0 },
    };
    let (response, rect) = field_impl(ui, text, hint, width, spec);

    // Magnifier: glass sits up-left of the mark centre so glass plus handle
    // reads optically centred at (left + 17, centre y).
    let s = Stroke::new(1.3, theme::ink_muted());
    let glass = pos2(rect.left() + 15.6, rect.center().y - 1.4);
    let d = std::f32::consts::FRAC_1_SQRT_2;
    let p = ui.painter();
    p.circle_stroke(glass, 4.2, s);
    p.line_segment(
        [
            pos2(glass.x + 4.2 * d, glass.y + 4.2 * d),
            pos2(glass.x + 7.8 * d, glass.y + 7.8 * d),
        ],
        s,
    );

    // Re-check emptiness post-edit so the mark tracks this frame's content;
    // the caller owns the buffer, so a click only reports `cleared`.
    let mut cleared = false;
    if !text.is_empty() {
        let clear_rect = Rect::from_min_size(
            pos2(rect.right() - 20.0, rect.center().y - 8.0),
            vec2(16.0, 16.0),
        );
        let mark = ui
            .interact(clear_rect, response.id.with("clear"), Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        let color = if mark.hovered() {
            theme::ink()
        } else {
            theme::ink_muted()
        };
        let c = clear_rect.center();
        let r = 3.4;
        let s = Stroke::new(1.3, color);
        let p = ui.painter();
        p.line_segment([pos2(c.x - r, c.y - r), pos2(c.x + r, c.y + r)], s);
        p.line_segment([pos2(c.x - r, c.y + r), pos2(c.x + r, c.y - r)], s);
        cleared = mark.clicked();
    }

    SearchOut { response, cleared }
}

/// A crafted focus ring for any widget: a hairline rounded outline inset 2px
/// in the palette's focus colour, with four tiny corner ticks. `t` is 0..=1
/// (animate it with `ctx.animate_bool_with_time`); nothing is drawn at t <= 0.
pub fn focus_ring(ui: &egui::Ui, rect: Rect, t: f32) {
    if !t.is_finite() || t <= 0.0 || !rect.is_finite() || !rect.is_positive() {
        return;
    }
    let t = t.min(1.0);
    let r = rect.shrink(2.0);
    if !r.is_positive() {
        return;
    }
    let p = ui.painter();
    p.rect_stroke(
        r,
        CornerRadius::same(theme::R_CONTROL),
        Stroke::new(
            theme::RULE_W,
            theme::alpha(theme::focus(), (140.0 * t) as u8),
        ),
        StrokeKind::Inside,
    );
    let s = Stroke::new(1.25, theme::alpha(theme::focus(), (230.0 * t) as u8));
    // An L of two 4px arms hugging each corner.
    let arms = [
        (r.left_top(), 1.0, 1.0),
        (r.right_top(), -1.0, 1.0),
        (r.left_bottom(), 1.0, -1.0),
        (r.right_bottom(), -1.0, -1.0),
    ];
    for (c, sx, sy) in arms {
        p.line_segment([c, pos2(c.x + sx * 4.0, c.y)], s);
        p.line_segment([c, pos2(c.x, c.y + sy * 4.0)], s);
    }
}

/// Shared body: paints the well, hosts the frameless edit, then draws the
/// resting hairline and the focus-grown rule.
fn field_impl(
    ui: &mut egui::Ui,
    text: &mut String,
    hint: &str,
    width: f32,
    spec: FieldSpec,
) -> (egui::Response, Rect) {
    let min_w = 40.0 + spec.lead + spec.trail;
    let width = if width.is_finite() {
        width.max(min_w)
    } else {
        min_w
    };
    let (rect, _bg) = ui.allocate_exact_size(vec2(width, FIELD_H), Sense::hover());

    let p = ui.painter();
    p.rect_filled(
        rect,
        CornerRadius::same(theme::R_CONTROL),
        theme::bg_sunken(),
    );
    p.rect_stroke(
        rect,
        CornerRadius::same(theme::R_CONTROL),
        Stroke::new(theme::RULE_W, theme::rule_hair()),
        StrokeKind::Inside,
    );

    let inner = Rect::from_min_max(
        pos2(rect.left() + 10.0 + spec.lead, rect.top() + 4.0),
        pos2(rect.right() - 10.0 - spec.trail, rect.bottom() - 4.0),
    );

    let te = egui::TextEdit::singleline(text)
        .frame(egui::Frame::new())
        .desired_width(inner.width())
        .hint_text(hint.to_owned())
        .text_color(theme::ink())
        .margin(Margin::ZERO)
        .font(if spec.mono {
            theme::font(theme::step::DATA, theme::fam_mono())
        } else {
            theme::font(theme::step::LABEL, FontFamily::Proportional)
        });
    // TextEdit recolors its hint with `weak_text_color`, so scope that to the
    // faint ink rather than tinting the hint text directly.
    let resp = ui
        .scope(|ui| {
            ui.style_mut().visuals.weak_text_color = Some(theme::ink_faint());
            ui.put(inner, te)
        })
        .inner;

    let t = ui
        .ctx()
        .animate_bool_with_time(
            resp.id.with("rule"),
            resp.has_focus(),
            theme::dur(theme::motion::STATE),
        )
        .clamp(0.0, 1.0);
    let (accent, resting) = match spec.state {
        FieldState::Neutral => (theme::gold(), theme::alpha(theme::ink_faint(), 89)),
        FieldState::Valid => (
            theme::custody_good(),
            theme::alpha(theme::custody_good(), 128),
        ),
        FieldState::Invalid => (
            theme::custody_blocked(),
            theme::alpha(theme::custody_blocked(), 128),
        ),
    };
    let y = rect.bottom() - 3.0;
    let p = ui.painter();
    p.hline(
        Rangef::new(inner.left(), inner.right()),
        y,
        Stroke::new(theme::RULE_W, resting),
    );
    if t > 0.0 && inner.width() > 0.0 {
        let half = inner.width() * 0.5 * t;
        let cx = inner.center().x;
        p.hline(
            Rangef::new(cx - half, cx + half),
            y,
            Stroke::new(theme::RULE_ACCENT_W, accent),
        );
    }

    (resp, rect)
}
