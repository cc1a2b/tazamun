//! Ceremonial set-pieces for the native GUI: the invite rendered as a real
//! ticket (perforation, punched notches, QR stub), the rotating-khatam
//! loading mark, keyboard key caps, and dialog adornments. Pure presentation
//! over `theme` and `ornament` — zero I/O, deterministic except the
//! time-driven rotation, total for degenerate inputs.

use eframe::egui;
use egui::{CornerRadius, Rect, Sense, Stroke, StrokeKind};
use egui::{pos2, vec2};

use super::{a11y, focusnav, ornament, theme};

/// What the ticket is called when it is read out rather than seen.
const TICKET_LEAD: &str = "invite ticket";
/// The stub, in words. The QR is the same ticket in another hand, so it is
/// named but never read out a second time.
const TICKET_QR: &str = "QR code on the stub";
/// A card drawn before the daemon has minted anything.
const TICKET_NONE: &str = "invite ticket, none yet";

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
        theme::font(theme::step::CAPTION, theme::fam_mono()),
        theme::ink(),
        body_wrap.max(60.0),
    );
    let height = (galley.size().y + PAD * 2.0).max(if qr.is_some() { 118.0 } else { 66.0 });
    // The ticket string is a painted galley, so the value beside the working
    // "Copy ticket" button is otherwise unreadable: copyable but unhearable.
    // A stop on the Tab route of its own is what makes it say itself.
    let (rect, resp) =
        ui.allocate_exact_size(vec2(width, height), Sense::focusable_noninteractive());
    a11y::describe(&resp, &ticket_sentence(ticket, qr.is_some()));
    if !rect.is_finite() || !rect.is_positive() {
        return;
    }

    let perf_x = rect.right() - STUB_W;
    {
        let p = ui.painter();
        let cr = CornerRadius::same(theme::R_OVERLAY);
        p.rect_filled(rect, cr, theme::bg_chrome());
        p.rect_stroke(
            rect,
            cr,
            Stroke::new(theme::RULE_W, theme::rule_hair()),
            StrokeKind::Inside,
        );
        p.extend(egui::Shape::dashed_line(
            &[
                pos2(perf_x, rect.top() + 6.0),
                pos2(perf_x, rect.bottom() - 6.0),
            ],
            Stroke::new(theme::RULE_W, theme::alpha(theme::ink_faint(), 153)),
            5.0,
            4.0,
        ));
        // The window base "punches through" at both ends of the perforation.
        p.circle_filled(pos2(perf_x, rect.top()), NOTCH_R, theme::bg_deep());
        p.circle_filled(pos2(perf_x, rect.bottom()), NOTCH_R, theme::bg_deep());
        ornament::khatam(
            p,
            pos2(rect.left() + PAD + 7.0, rect.top() + PAD + 7.0),
            7.0,
            theme::gold(),
            false,
        );
        p.galley(
            pos2(rect.left() + PAD + 18.0, rect.top() + PAD),
            galley,
            theme::ink(),
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
        ornament::khatam(ui.painter(), stub_center, 16.0, theme::ink_faint(), false);
    }

    focusnav::ring(ui, &resp);
}

/// The sentence the ticket announces: the ticket itself, then the stub. The
/// value is the whole point of the card — a reader who can copy an invite but
/// never hear it cannot check that it is the invite they meant to send.
fn ticket_sentence(ticket: &str, has_qr: bool) -> String {
    let ticket = ticket.trim();
    if ticket.is_empty() {
        return TICKET_NONE.to_owned();
    }
    let mut parts = vec![ticket];
    if has_qr {
        parts.push(TICKET_QR);
    }
    a11y::sentence(TICKET_LEAD, &parts)
}

/// The signature loading mark: a slowly rotating eight-point star outline in
/// gold, self-repainting only while visible. Under reduced motion the star
/// stands still and nothing is scheduled. Allocates size x size.
pub fn loading_mark(ui: &mut egui::Ui, size: f32) {
    if !size.is_finite() || size <= 0.0 {
        return;
    }
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    let spinning = !theme::reduced_motion();
    let angle = if spinning {
        (ui.input(|i| i.time) * 0.9) as f32
    } else {
        0.0
    };
    let c = rect.center();
    let r = size * 0.42;
    let p = ui.painter();
    for (phase, color) in [
        (angle, theme::gold()),
        (
            angle + std::f32::consts::FRAC_PI_4,
            theme::alpha(theme::gold(), 179),
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
    p.circle_filled(c, size * 0.07, theme::gold_bright());
    // Only runs while the mark is drawn — repaint stops with visibility.
    if spinning {
        ui.ctx().request_repaint_after(theme::motion::CADENCE);
    }
}

/// A tiny keyboard key cap: a sunken chip with a hairline stroke and a mono
/// caption label, min 20px wide, 16px tall.
pub fn keycap(ui: &mut egui::Ui, label: &str) {
    let galley = ui.painter().layout_no_wrap(
        label.to_owned(),
        theme::font(theme::step::CAPTION, theme::fam_mono()),
        theme::ink_muted(),
    );
    let w = (galley.size().x + 10.0).max(20.0);
    let (rect, _) = ui.allocate_exact_size(vec2(w, 16.0), Sense::hover());
    let cr = CornerRadius::same(theme::R_CHIP);
    let p = ui.painter();
    p.rect_filled(rect, cr, theme::bg_sunken());
    p.rect_stroke(
        rect,
        cr,
        Stroke::new(theme::RULE_W, theme::rule_hair()),
        StrokeKind::Inside,
    );
    p.galley(
        rect.center() - galley.size() * 0.5,
        galley,
        theme::ink_muted(),
    );
}

/// Adorns an already-painted dialog card: two corner flourishes (top-left
/// inward, bottom-right inward) in gold at low strength; when `danger`, a
/// large very-faint blocked-custody khatam watermark behind the card center
/// instead of the gold flourishes' calm.
pub fn adorn_dialog(p: &egui::Painter, rect: Rect, danger: bool) {
    if !rect.is_finite() || !rect.is_positive() {
        return;
    }
    if danger {
        ornament::khatam(
            p,
            rect.center(),
            rect.width().min(rect.height()) * 0.36,
            theme::wash::of(theme::custody_blocked(), theme::wash::GHOST),
            false,
        );
    } else {
        let gold = theme::alpha(theme::gold(), 128);
        ornament::corner_flourish(p, rect.left_top(), vec2(1.0, 1.0), 46.0, gold);
        ornament::corner_flourish(p, rect.right_bottom(), vec2(-1.0, -1.0), 46.0, gold);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TICKET: &str = "tzm1qyqszqgpqyqszqgpqyqszqgpqyqszqgp";

    #[test]
    fn sentence_reads_the_ticket_itself() {
        assert_eq!(
            ticket_sentence(TICKET, false),
            "invite ticket: tzm1qyqszqgpqyqszqgpqyqszqgpqyqszqgp"
        );
    }

    #[test]
    fn sentence_names_the_stub_when_the_qr_is_there() {
        assert_eq!(
            ticket_sentence(TICKET, true),
            "invite ticket: tzm1qyqszqgpqyqszqgpqyqszqgpqyqszqgp, QR code on the stub"
        );
    }

    #[test]
    fn an_empty_card_says_so_rather_than_naming_a_ticket_it_has_not_got() {
        assert_eq!(ticket_sentence("", false), TICKET_NONE);
        assert_eq!(ticket_sentence("   ", true), TICKET_NONE);
    }

    #[test]
    fn surrounding_whitespace_never_reaches_the_reader() {
        assert_eq!(
            ticket_sentence("  tzm1abc \n", false),
            "invite ticket: tzm1abc"
        );
    }
}
