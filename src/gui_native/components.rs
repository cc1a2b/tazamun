//! The shared component set: the blocks, marks and measures a view composes a
//! page out of — ledger figures, the colophon card with its 45-degree cut, the
//! kind chip, the primary plate, the ruled measure and the empty page.
//!
//! Pure presentation over `theme`, `ornament` and `register`; zero I/O, total
//! for degenerate inputs. Nothing here stamps a khatam: the star means an entry
//! is held, that meaning belongs to the register's margin, and a second place
//! drawing it would dilute it. The only star in this file is the watermark
//! behind an empty page, which is `ornament`'s.

use eframe::egui;
use egui::{Align2, Color32, FontFamily, Margin, Pos2, Rect, RichText, Sense, Stroke, StrokeKind};
use egui::{pos2, vec2};

use super::{ornament, register, theme};

/// The ledger head: each figure set in the tabular face over a gold rule as
/// wide as the figure itself, its name tracked underneath, and the house
/// diamond between one pair and the next. Figures, not tiles — the row reads
/// along one baseline the way a ledger total does.
pub fn ledger_stats(ui: &mut egui::Ui, items: &[(String, &'static str)]) {
    ui.horizontal(|ui| {
        for (i, (value, label)) in items.iter().enumerate() {
            if i > 0 {
                let (cell, _) = ui.allocate_exact_size(
                    vec2(theme::space::XL, theme::sized(theme::step::DISPLAY) * 2.0),
                    Sense::hover(),
                );
                ornament::diamond(ui.painter(), cell.center(), 3.0, theme::gold_deep());
            }
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = theme::space::XS;
                let galley = ui.painter().layout_no_wrap(
                    value.clone(),
                    theme::font(theme::step::DISPLAY, theme::fam_mono_medium()),
                    theme::ink(),
                );
                let figure_w = galley.size().x;
                let (slot, _) = ui.allocate_exact_size(galley.size(), Sense::hover());
                ui.painter().galley(slot.min, galley, theme::ink());
                let (rule, _) =
                    ui.allocate_exact_size(vec2(figure_w, theme::RULE_ACCENT_W), Sense::hover());
                ui.painter().rect_filled(rule, theme::R_NONE, theme::gold());
                ui.label(
                    RichText::new(*label)
                        .font(theme::font(theme::step::META, theme::fam_medium()))
                        .extra_letter_spacing(theme::HEADER_TRACKING)
                        .color(theme::ink_muted()),
                );
            });
        }
    });
}

/// The colophon block: a raised ground with one 45-degree cut at the top-right
/// corner, a hairline all round, and — when `thread` is given — a 3px bar of
/// that colour down the whole leading edge over a ground nudged 6% toward it.
/// The body is a convex 5-gon painted behind the contents through the
/// placeholder-then-set idiom, so the block sizes itself to whatever `add` put
/// in it.
pub fn notched_card(ui: &mut egui::Ui, thread: Option<Color32>, add: impl FnOnce(&mut egui::Ui)) {
    let bg = ui.painter().add(egui::Shape::Noop);
    let filament = ui.painter().add(egui::Shape::Noop);
    let rect = egui::Frame::new()
        .inner_margin(Margin::same(theme::density().block_pad() as i8))
        .show(ui, add)
        .response
        .rect;
    let notch = theme::space::L
        .min(rect.width().max(0.0))
        .min(rect.height().max(0.0));
    let body = snapped(rect);
    let points = vec![
        body.left_top(),
        pos2(body.right() - notch, body.top()),
        pos2(body.right(), body.top() + notch),
        body.right_bottom(),
        body.left_bottom(),
    ];
    let fill = match thread {
        Some(color) => theme::mix(theme::bg_raise(), color, 0.06),
        None => theme::bg_raise(),
    };
    ui.painter().set(
        bg,
        egui::Shape::convex_polygon(points, fill, Stroke::new(theme::RULE_W, theme::rule_hair())),
    );
    if let Some(color) = thread {
        let bar = Rect::from_min_size(
            body.min,
            vec2(theme::RULE_ACCENT_W + 1.0, body.height().max(0.0)),
        );
        ui.painter().set(
            filament,
            egui::Shape::rect_filled(bar, theme::R_NONE, color),
        );
    }
}

/// The file's kind, set small in the tabular face inside a square-cut chip
/// tinted in the peer blue: uppercase extension, up to five characters, the
/// house dot when there is nothing usable to show. Width is fixed to the
/// widest common extension so a column of these lines up.
pub fn ext_chip(ui: &mut egui::Ui, path: &str) {
    let ext = chip_ext(path);
    let c = theme::custody_peer();
    let galley =
        ui.painter()
            .layout_no_wrap(ext, theme::font(theme::step::CAPTION, theme::fam_mono()), c);
    let h = theme::sized(theme::step::META) + theme::space::S;
    let w = (galley.size().x + theme::space::M)
        .max(theme::sized(theme::step::DISPLAY) + theme::space::L);
    let (rect, _) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, theme::R_CHIP, theme::wash::of(c, theme::wash::TINT));
    p.rect_stroke(
        rect,
        theme::R_CHIP,
        Stroke::new(theme::RULE_W, theme::alpha(c, 90)),
        StrokeKind::Inside,
    );
    p.galley(centre(rect, galley.size()), galley, c);
}

fn chip_ext(path: &str) -> String {
    let ext = path.rsplit('.').next().unwrap_or("");
    let n = ext.chars().count();
    // Five characters still fit the well at caption size, and the common
    // web-asset extensions (woff2, jsonc, xhtml) are exactly five — falling
    // back to a bare dot for those looked like a missing chip.
    if path.contains('.') && (1..=5).contains(&n) && ext != path {
        ext.to_uppercase()
    } else {
        "·".to_owned()
    }
}

/// The single primary action: the gold plate.
pub fn bevel_primary(ui: &mut egui::Ui, label: &str) -> egui::Response {
    plate(
        ui,
        label,
        theme::gold(),
        theme::gold_bright(),
        theme::on_gold(),
    )
}

/// The filled-action form, shared by the gold plate and the danger plate so the
/// two buttons of a destructive confirm are the same object in two colours: a
/// rectangle cut at the top-right and bottom-left corners, its label centred in
/// `ink`.
///
/// * **rest** — flat `rest` fill, no edge.
/// * **hover** — the [`lift`] fill plus a hairline of `ink` at 27% around the
///   cut edge, so the change is not carried by colour alone.
/// * **focus** — the focus ring outside the plate *and* a hairline of `ink`
///   inset 3px inside it.
/// * **pressed** — back to the `rest` fill with a half-strength `ink` edge and
///   the label dropped one pixel, so the plate reads as struck.
/// * **disabled** — no fill at all: the cut outline and the label in
///   `ink_disabled`, no pointer cursor and no ring.
pub(super) fn plate(
    ui: &mut egui::Ui,
    label: &str,
    rest: Color32,
    hover: Color32,
    ink: Color32,
) -> egui::Response {
    let enabled = ui.is_enabled();
    let face = if enabled { ink } else { theme::ink_disabled() };
    let galley = ui.painter().layout_no_wrap(
        label.to_owned(),
        theme::font(theme::step::LABEL, theme::fam_medium()),
        face,
    );
    let size = vec2(
        galley.size().x + theme::space::XL * 2.0,
        (theme::sized(theme::step::LABEL) + theme::space::M * 2.0).max(galley.size().y),
    );
    let (rect, resp) = ui.allocate_at_least(size, Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, label));

    let pressed = enabled && resp.is_pointer_button_down_on();
    let hovered = enabled && resp.hovered();
    let focused = enabled && resp.has_focus();
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    let body = snapped(rect);
    let cut = theme::space::M
        .min(body.height().max(0.0) * 0.35)
        .min(body.width().max(0.0) * 0.25);
    let p = ui.painter();
    let (fill, edge) = if !enabled {
        (
            Color32::TRANSPARENT,
            Stroke::new(theme::RULE_W, theme::ink_disabled()),
        )
    } else if pressed {
        (rest, Stroke::new(theme::RULE_W, theme::alpha(ink, 130)))
    } else if hovered {
        (
            lift(rest, hover, ink),
            Stroke::new(theme::RULE_W, theme::alpha(ink, 70)),
        )
    } else {
        (rest, Stroke::NONE)
    };
    p.add(egui::Shape::convex_polygon(
        plate_points(body, cut),
        fill,
        edge,
    ));

    let drop = if pressed { 1.0 } else { 0.0 };
    let mut at = centre(rect, galley.size());
    at.y += drop;
    p.galley(at, galley, face);

    if focused {
        register::focus_ring(p, rect);
        let inner = body.shrink(3.0);
        if inner.is_positive() {
            p.add(egui::Shape::convex_polygon(
                plate_points(inner, cut * 0.5),
                Color32::TRANSPARENT,
                Stroke::new(theme::RULE_W, ink),
            ));
        }
    }
    resp
}

/// The fill a plate takes under the pointer. Normally the accent's `hover`
/// tone, but on a light palette the brighter tone can put the label under the
/// AA floor — `gold_bright` against `on_gold` reads 3.7:1 on paper — and a
/// legible plate matters more than a literal token, so those deepen instead.
fn lift(rest: Color32, hover: Color32, ink: Color32) -> Color32 {
    const AA_TEXT: f32 = 4.5;
    if theme::contrast(ink, hover) >= AA_TEXT {
        hover
    } else {
        theme::mix(rest, theme::pal().shade, 0.22)
    }
}

/// The outline of a plate: the rectangle with its top-right and bottom-left
/// corners cut back by `cut`. Pure, and clamped so a degenerate rect still
/// yields a finite hexagon.
fn plate_points(rect: Rect, cut: f32) -> Vec<Pos2> {
    let cut = if cut.is_finite() {
        cut.clamp(0.0, (rect.width().min(rect.height()) * 0.5).max(0.0))
    } else {
        0.0
    };
    vec![
        rect.left_top(),
        pos2(rect.right() - cut, rect.top()),
        pos2(rect.right(), rect.top() + cut),
        rect.right_bottom(),
        pos2(rect.left() + cut, rect.bottom()),
        pos2(rect.left(), rect.bottom() - cut),
    ]
}

/// A measure, ruled the way a scale is drawn rather than filled the way a
/// progress pill is: a baseline with a terminal serif at each end, quarter
/// ticks rising off it, and the measured part carried as a girih band over a
/// gold wash, stopped by a 2px gold head rule with the house diamond on top.
/// `frac` is clamped to 0..=1; anything non-finite measures nothing.
pub fn progress_gold(ui: &mut egui::Ui, frac: f32) {
    let frac = measure_frac(frac);
    let h = theme::sized(theme::step::CAPTION);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width().max(0.0), h), Sense::hover());
    if rect.width() < 4.0 || rect.height() < 3.0 {
        return;
    }
    let p = ui.painter();
    let base = theme::snap(rect.bottom());
    let rule = Stroke::new(theme::RULE_W, theme::rule_divider());
    p.hline(rect.x_range(), base, rule);
    for x in [rect.left(), rect.right() - theme::RULE_W] {
        p.vline(theme::snap(x), egui::Rangef::new(rect.top(), base), rule);
    }
    for k in 1..4 {
        let x = theme::snap(rect.left() + rect.width() * k as f32 / 4.0);
        p.vline(
            x,
            egui::Rangef::new(base - rect.height() * 0.35, base),
            Stroke::new(theme::RULE_W, theme::rule_hair()),
        );
    }
    if frac <= 0.0 {
        return;
    }
    let band = Rect::from_min_size(
        rect.min,
        vec2(
            (rect.width() * frac).max(theme::RULE_ACCENT_W),
            (rect.height() - theme::RULE_W).max(1.0),
        ),
    );
    p.rect_filled(
        band,
        theme::R_NONE,
        theme::wash::of(theme::gold(), theme::wash::GHOST),
    );
    ornament::girih_band(p, band, theme::alpha(theme::gold_deep(), 150));
    let head = theme::snap(band.right());
    p.vline(
        head,
        egui::Rangef::new(rect.top(), base),
        Stroke::new(theme::RULE_ACCENT_W, theme::gold()),
    );
    ornament::diamond(p, pos2(head, rect.top() + 2.0), 2.4, theme::gold());
}

fn measure_frac(frac: f32) -> f32 {
    if frac.is_finite() {
        frac.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// A page with nothing written on it: the khatam watermark from `ornament`
/// behind the reason it is empty and what to do about it.
pub fn empty_state(ui: &mut egui::Ui, title: &str, hint: &str) {
    let h = (theme::density().row_h() * 4.0).max(96.0);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width().max(0.0), h), Sense::hover());
    if rect.width() <= 0.0 {
        return;
    }
    ornament::watermark(&ui.painter_at(rect), rect, theme::gold());
    let p = ui.painter();
    let mut y = rect.center().y - theme::sized(theme::step::BODY);
    p.text(
        pos2(rect.center().x, y),
        Align2::CENTER_CENTER,
        title,
        theme::font(theme::step::BODY, theme::fam_medium()),
        theme::ink_muted(),
    );
    y += theme::sized(theme::step::BODY) + theme::space::M;
    p.text(
        pos2(rect.center().x, y),
        Align2::CENTER_CENTER,
        hint,
        theme::font(theme::step::META, FontFamily::Proportional),
        theme::ink_faint(),
    );
}

/// A rect on the pixel grid, so a hairline renders as one crisp line instead of
/// two grey ones.
pub(super) fn snapped(rect: Rect) -> Rect {
    Rect::from_min_max(
        pos2(theme::snap(rect.left()), theme::snap(rect.top())),
        pos2(theme::snap(rect.right()), theme::snap(rect.bottom())),
    )
}

/// Where to put a galley of `size` so it sits in the middle of `rect`, on whole
/// pixels.
pub(super) fn centre(rect: Rect, size: egui::Vec2) -> Pos2 {
    pos2(
        (rect.center().x - size.x * 0.5).round(),
        (rect.center().y - size.y * 0.5).round(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_extensions_get_a_chip() {
        for (path, want) in [
            ("a/button.tsx", "TSX"),
            ("sprite.png", "PNG"),
            ("README.md", "MD"),
            ("tokens/palette.json", "JSON"),
            // Five characters: these used to fall back to a bare dot, which
            // read as a missing chip rather than a deliberate one.
            ("tokens/icons.woff2", "WOFF2"),
            ("page.xhtml", "XHTML"),
        ] {
            assert_eq!(chip_ext(path), want, "for {path}");
        }
    }

    #[test]
    fn anything_without_a_usable_extension_gets_the_dot() {
        for path in [
            "Makefile",            // no dot at all
            ".gitignore",          // dotfile: the extension is the whole name
            "archive.tar.gzipped", // too long to fit the well
            "trailing.",           // nothing after the dot
        ] {
            assert_eq!(chip_ext(path), "·", "for {path}");
        }
    }

    #[test]
    fn a_plate_is_cut_at_two_opposite_corners() {
        let r = Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 30.0));
        let pts = plate_points(r, 8.0);
        assert_eq!(pts.len(), 6);
        assert!(pts.iter().all(|p| p.is_finite()));
        assert!(pts.iter().all(|p| r.contains(*p)), "{pts:?}");
        assert!(!pts.contains(&r.right_top()), "top-right was not cut");
        assert!(!pts.contains(&r.left_bottom()), "bottom-left was not cut");
        assert!(pts.contains(&r.left_top()) && pts.contains(&r.right_bottom()));
    }

    /// A cut deeper than the plate would fold the outline inside out.
    #[test]
    fn an_oversized_cut_is_clamped_to_the_plate() {
        let r = Rect::from_min_size(pos2(0.0, 0.0), vec2(40.0, 20.0));
        for cut in [1000.0, f32::INFINITY, f32::NAN, -5.0] {
            let pts = plate_points(r, cut);
            assert!(
                pts.iter().all(|p| p.is_finite() && r.contains(*p)),
                "cut {cut} escaped the plate: {pts:?}"
            );
        }
    }

    #[test]
    fn a_measure_never_reads_past_its_ends() {
        assert_eq!(measure_frac(-1.0), 0.0);
        assert_eq!(measure_frac(0.0), 0.0);
        assert_eq!(measure_frac(0.5), 0.5);
        assert_eq!(measure_frac(2.0), 1.0);
        assert_eq!(measure_frac(f32::NAN), 0.0);
        assert_eq!(measure_frac(f32::INFINITY), 0.0);
    }
}
