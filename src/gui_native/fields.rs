//! Crafted text inputs and the focus system for the native GUI: the house
//! field (a recessed well whose hairline rule grows into a centre-out gold
//! underline on focus), its monospace variant for tickets and paths, the
//! painter-drawn search field with magnifier and clear mark, and the
//! corner-ticked focus ring. Pure presentation over `theme`; zero I/O, total
//! for degenerate inputs, and the only clock is `animate_bool_with_time`.

use eframe::egui;
use egui::{CornerRadius, FontFamily, Margin, Rangef, Rect, Sense, Stroke, StrokeKind};
use egui::{pos2, vec2};

use super::{register, theme};

/// The magnifier and the clear mark, as fractions of the field's own type step:
/// a mark beside 27px text cannot be the 4px circle that suited 12px text.
const GLASS_R: f32 = 0.34;
const CLEAR_SIDE: f32 = 1.3;
/// The mark's cross arm, as a fraction of its cell.
const CLEAR_ARM: f32 = 0.21;

/// The well's height: the line box the type inside it lays out into, with the
/// air the focus underline needs beneath the descenders.
///
/// It used to be 30, which held a 12.5pt label and nothing else — at 2.2 the
/// type is 27.5pt and the well it is typed into had not moved. Ruled at the
/// label step whichever face the well carries, so a ticket well and a text well
/// standing side by side are still one height.
fn field_h() -> f32 {
    well_h(register::line_h(theme::step::LABEL))
}

/// Pure: the well around one line box.
fn well_h(line_h: f32) -> f32 {
    line_h + theme::space::M * 2.0
}

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
/// `state` tints the rule (settled / blocked) when not Neutral. As tall as the
/// type it holds.
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
    let glass_r = theme::sized(theme::step::LABEL) * GLASS_R;
    let clear_side = theme::sized(theme::step::LABEL) * CLEAR_SIDE;
    let spec = FieldSpec {
        state: FieldState::Neutral,
        mono: false,
        // Each mark reserves its own width rather than a number that used to
        // clear it: the marks grow with the type they sit beside.
        lead: glass_r * 2.0 + theme::space::M,
        trail: if text.is_empty() {
            0.0
        } else {
            clear_side + theme::space::XS
        },
    };
    let (response, rect) = field_impl(ui, text, hint, width, spec);

    // Magnifier: glass sits up-left of the mark centre so glass plus handle
    // reads optically centred in the room the lead reserved for it.
    let s = Stroke::new(1.3, theme::ink_muted());
    let glass = pos2(
        rect.left() + inset_x() + glass_r,
        rect.center().y - glass_r * 0.33,
    );
    let d = std::f32::consts::FRAC_1_SQRT_2;
    let p = ui.painter();
    p.circle_stroke(glass, glass_r, s);
    p.line_segment(
        [
            pos2(glass.x + glass_r * d, glass.y + glass_r * d),
            pos2(glass.x + glass_r * 1.86 * d, glass.y + glass_r * 1.86 * d),
        ],
        s,
    );

    // Re-check emptiness post-edit so the mark tracks this frame's content;
    // the caller owns the buffer, so a click only reports `cleared`.
    let mut cleared = false;
    if !text.is_empty() {
        let clear_rect = Rect::from_min_size(
            pos2(
                rect.right() - theme::space::S - clear_side,
                rect.center().y - clear_side * 0.5,
            ),
            vec2(clear_side, clear_side),
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
        let r = clear_side * CLEAR_ARM;
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

/// The well's horizontal inset: where the type starts, and where a painted mark
/// is set from the edge.
fn inset_x() -> f32 {
    theme::space::M + theme::space::XS
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
    let step = if spec.mono {
        theme::step::DATA
    } else {
        theme::step::LABEL
    };
    // Room for a few characters of the type actually being typed, not for a few
    // characters of the type this was measured against once.
    let min_w = theme::sized(step) * 3.0 + spec.lead + spec.trail;
    let width = if width.is_finite() {
        width.max(min_w)
    } else {
        min_w
    };
    let (rect, _bg) = ui.allocate_exact_size(vec2(width, field_h()), Sense::hover());

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
        pos2(
            rect.left() + inset_x() + spec.lead,
            rect.top() + theme::space::S,
        ),
        pos2(
            rect.right() - inset_x() - spec.trail,
            rect.bottom() - theme::space::S,
        ),
    );

    let te = egui::TextEdit::singleline(text)
        .frame(egui::Frame::new())
        .desired_width(inner.width())
        .hint_text(hint.to_owned())
        .text_color(theme::ink())
        .margin(Margin::ZERO)
        .font(if spec.mono {
            theme::font(step, theme::fam_mono())
        } else {
            theme::font(step, FontFamily::Proportional)
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
    let y = rect.bottom() - theme::space::S + theme::RULE_W;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Every text scale the control can actually reach.
    fn scales() -> impl Iterator<Item = f32> {
        (14..=44).map(|k| k as f32 / 20.0)
    }

    /// The line box `register::line_h` returns: Plex Sans Arabic is 1.5em, and
    /// a path or a folder name typed into one of these wells is ordinary.
    fn line_box(step: f32, scale: f32) -> f32 {
        step * scale * 1.5
    }

    /// The defect, as arithmetic: at 30px the well stopped clearing its own
    /// type somewhere around 115%, which is where the user first saw it.
    #[test]
    fn a_well_clears_the_type_typed_into_it() {
        for scale in scales() {
            let line = line_box(theme::step::LABEL, scale);
            let h = well_h(line);
            assert!(
                h >= line,
                "a {line} line box does not fit a {h} well at {scale}"
            );
            // The inner rect the edit is put into, which is the well less the
            // symmetric inset, still has to hold the line.
            let inner = h - theme::space::S * 2.0;
            assert!(
                inner >= line,
                "the edit is given {inner} for a {line} line at {scale}"
            );
        }
    }

    #[test]
    fn a_well_grows_with_the_text_scale() {
        let mut prev: Option<(f32, f32)> = None;
        for scale in scales() {
            let h = well_h(line_box(theme::step::LABEL, scale));
            if let Some((was, before)) = prev {
                assert!(h > before, "well flat from {was} to {scale}");
            }
            prev = Some((scale, h));
        }
    }

    /// The well is the same height whichever face it carries, or a ticket field
    /// and the field beside it sit on two different baselines.
    #[test]
    fn both_faces_rule_the_same_well() {
        for scale in scales() {
            let label = well_h(line_box(theme::step::LABEL, scale));
            let data = well_h(line_box(theme::step::DATA, scale));
            assert!(
                label >= data,
                "the mono well is the taller of the two at {scale}"
            );
        }
    }
}
