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
use egui::{Color32, FontFamily, Margin, Pos2, Rect, RichText, Sense, Stroke, StrokeKind};
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
                let (cell, _) =
                    ui.allocate_exact_size(vec2(theme::space::XL, figure_h()), Sense::hover());
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

/// The height of one ledger figure — the value, its gold rule, and the name
/// under it — so the separator between two figures is cut from the same measure
/// the column is set in rather than from a pixel count that agreed with it at
/// one text scale.
fn figure_h() -> f32 {
    figure_box(
        register::line_h(theme::step::DISPLAY),
        register::line_h(theme::step::META),
    )
}

/// Pure: the figure column's own arithmetic. The two line boxes, the rule
/// between them, and the half-step of air the column sets its items with.
fn figure_box(value_h: f32, label_h: f32) -> f32 {
    value_h + theme::space::XS + theme::RULE_ACCENT_W + theme::space::XS + label_h
}

/// The height of a chip carrying one line of type at `step` — the count tally,
/// the kind chip, a key cap. `galley_h` is what the text actually laid out to.
///
/// The one number three files used to write as `sized(META) + space::S`, which
/// is a nominal size plus a gap standing in for a line box: at 2.2 the type it
/// was meant to hold is taller than the chip drawn around it.
pub(super) fn chip_h(step: f32, galley_h: f32) -> f32 {
    chip_box(register::line_h(step), galley_h)
}

/// Pure: never shorter than the line box the type lays out into, and never
/// shorter than the galley it was actually handed.
fn chip_box(line_h: f32, galley_h: f32) -> f32 {
    let galley_h = if galley_h.is_finite() {
        galley_h.max(0.0)
    } else {
        0.0
    };
    line_h.max(galley_h + theme::space::XS)
}

/// The height of a [`plate`], for a caller that has to reserve the plate's room
/// before it has the plate — the bulk-action verb slots, which allocate a fixed
/// cell so the row does not reflow as verbs come and go.
///
/// Never less than the line box the label lays out into, so the slot fits the
/// plate at every text scale rather than at the one it was measured on.
pub(super) fn plate_h() -> f32 {
    plate_box(
        theme::sized(theme::step::LABEL),
        register::line_h(theme::step::LABEL),
    )
}

/// Pure: the plate's own arithmetic — the nominal size with its symmetric
/// padding, floored at the line box that size lays out into.
fn plate_box(nominal: f32, line_h: f32) -> f32 {
    (nominal + theme::space::M * 2.0).max(line_h)
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
    let h = chip_h(theme::step::CAPTION, galley.size().y);
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
        plate_h().max(galley.size().y),
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
///
/// Both lines are laid out into the page's own measure before anything is
/// allocated, so a long hint wraps inside the page instead of running off its
/// edge, and the page is as tall as the two lines came out rather than as tall
/// as they were once assumed to be.
pub fn empty_state(ui: &mut egui::Ui, title: &str, hint: &str) {
    let width = ui.available_width().max(0.0);
    if width <= 0.0 {
        return;
    }
    // A measure, not the full width: centred prose set edge to edge is a page
    // with no margins, and the wrap is what keeps a long hint on the page.
    let wrap = (width - theme::space::XXL * 2.0).max(width * 0.5);
    let p = ui.painter();
    let title_g = p.layout(
        title.to_owned(),
        theme::font(theme::step::BODY, theme::fam_medium()),
        theme::ink_muted(),
        wrap,
    );
    let hint_g = p.layout(
        hint.to_owned(),
        theme::font(theme::step::META, FontFamily::Proportional),
        theme::ink_faint(),
        wrap,
    );
    let block = empty_block_h(title_g.size().y, hint_g.size().y);
    let h = (theme::density().row_h() * 4.0).max(block + theme::density().block_pad() * 2.0);
    let (rect, _) = ui.allocate_exact_size(vec2(width, h), Sense::hover());
    if !rect.is_positive() {
        return;
    }
    ornament::watermark(&ui.painter_at(rect), rect, theme::gold());
    let p = ui.painter();
    let mut y = rect.center().y - block * 0.5;
    for (galley, color) in [(title_g, theme::ink_muted()), (hint_g, theme::ink_faint())] {
        let size = galley.size();
        p.galley(
            pos2((rect.center().x - size.x * 0.5).round(), y.round()),
            galley,
            color,
        );
        y += size.y + theme::space::M;
    }
}

/// Pure: the two laid-out lines and the air between them.
fn empty_block_h(title_h: f32, hint_h: f32) -> f32 {
    title_h + theme::space::M + hint_h
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

    /// Every text scale the control can actually reach — stored in twentieths,
    /// so this is the whole set rather than a sample of it.
    fn scales() -> impl Iterator<Item = f32> {
        (14..=44).map(|k| k as f32 / 20.0)
    }

    /// The line box `register::line_h` returns, as arithmetic: Plex Sans Arabic
    /// is 1.5em and every stack in this window ends in it.
    fn line_box(step: f32, scale: f32) -> f32 {
        step * scale * 1.5
    }

    /// What Plex's Latin faces actually lay out to — the galley a chip is
    /// handed when its text is Latin.
    fn latin(step: f32, scale: f32) -> f32 {
        step * scale * 1.3
    }

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

    // ── measures that used to be pixel counts ──

    /// The defect this replaced, stated as arithmetic: a chip ruled at
    /// `sized(META) + space::S` is shorter than the caption inside it once the
    /// text scale passes roughly 1.9, and shorter still for Arabic.
    #[test]
    fn a_chip_is_never_shorter_than_the_type_in_it() {
        let step = theme::step::CAPTION;
        for scale in scales() {
            let line = line_box(step, scale);
            for galley in [latin(step, scale), line, 0.0] {
                let h = chip_box(line, galley);
                assert!(h >= galley, "chip {h} holds a {galley} galley at {scale}");
                assert!(h >= line, "chip {h} under the line box {line} at {scale}");
            }
            // The formula this replaced: a nominal size one step up, plus a
            // gap, standing in for a line box it stops covering past ~1.9.
            let was = theme::step::META * scale + theme::space::S;
            if scale >= 2.0 {
                assert!(
                    chip_box(line, latin(step, scale)) > was,
                    "the old pixel count is still the taller of the two at {scale}"
                );
            }
        }
    }

    #[test]
    fn a_chip_grows_with_the_text_scale() {
        let step = theme::step::CAPTION;
        let mut prev: Option<(f32, f32)> = None;
        for scale in scales() {
            let h = chip_box(line_box(step, scale), latin(step, scale));
            if let Some((was, before)) = prev {
                assert!(h > before, "chip flat from {was} to {scale}");
            }
            prev = Some((scale, h));
        }
    }

    /// A nonsense galley height must cost the chip nothing rather than ruling
    /// the row it sits in at an infinite height.
    #[test]
    fn a_chip_survives_a_galley_it_cannot_measure() {
        let line = line_box(theme::step::CAPTION, 1.0);
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -50.0] {
            assert_eq!(chip_box(line, bad), line, "{bad} escaped the chip");
        }
    }

    /// The slot a bulk verb is reserved in is the plate's own height, so the
    /// plate cannot outgrow the cell that was allocated for it.
    #[test]
    fn a_plate_slot_clears_the_plate_at_every_scale() {
        for scale in scales() {
            let step = theme::step::LABEL;
            let line = line_box(step, scale);
            let h = plate_box(step * scale, line);
            assert!(
                h >= line,
                "a {line} line box does not fit the plate at {scale}"
            );
            assert!(h >= latin(step, scale), "latin label clipped at {scale}");
        }
    }

    /// The separator between two ledger figures is cut from the column's own
    /// measure, so it can never be tuned against a column it no longer matches.
    #[test]
    fn a_ledger_separator_spans_the_figure_beside_it() {
        let mut prev: Option<(f32, f32)> = None;
        for scale in scales() {
            let h = figure_box(
                line_box(theme::step::DISPLAY, scale),
                line_box(theme::step::META, scale),
            );
            assert!(
                h > line_box(theme::step::DISPLAY, scale) + line_box(theme::step::META, scale),
                "the rule between value and name is unaccounted for at {scale}"
            );
            if let Some((was, before)) = prev {
                assert!(h > before, "figure height flat from {was} to {scale}");
            }
            prev = Some((scale, h));
        }
    }

    /// An empty page is as tall as the two lines came out, whether they wrapped
    /// to one line each or to five.
    #[test]
    fn an_empty_page_follows_the_lines_it_holds() {
        let one = empty_block_h(20.0, 16.0);
        let wrapped = empty_block_h(20.0, 16.0 * 5.0);
        assert!(wrapped > one, "a wrapped hint did not make the page taller");
        assert!(one >= 20.0 + 16.0, "the air between the lines went missing");
        for scale in scales() {
            let block = empty_block_h(
                line_box(theme::step::BODY, scale),
                line_box(theme::step::META, scale),
            );
            assert!(
                block >= line_box(theme::step::BODY, scale) + line_box(theme::step::META, scale),
                "block {block} clips its own lines at {scale}"
            );
        }
    }
}
