//! Crafted component set for the native GUI: the pieces that replace the
//! remaining generic-dashboard widgets (boxy stat tiles, uniform cards, plain
//! separators and empty rows) with brand-rooted forms — ledger stat rows,
//! notched colophon cards, seal marks, bevelled gold actions, illuminated day
//! rules and timeline rails. Pure presentation over `theme` and `ornament`;
//! zero I/O, total for degenerate inputs.

use eframe::egui;
use egui::{Color32, CornerRadius, FontFamily, FontId, Margin, Rect, RichText, Sense, Stroke};
use egui::{pos2, vec2};

use super::{ornament, theme};

/// The ledger row that replaces boxy stat tiles: values inline on one
/// baseline, each value/label pair separated by a small diamond. Value in
/// semibold 20 INK over an 18px gold underline tick; label 10.5 DIM below.
pub fn ledger_stats(ui: &mut egui::Ui, items: &[(String, &'static str)]) {
    ui.horizontal(|ui| {
        for (i, (value, label)) in items.iter().enumerate() {
            if i > 0 {
                let (cell, _) = ui.allocate_exact_size(vec2(18.0, 40.0), Sense::hover());
                ornament::diamond(
                    ui.painter(),
                    cell.center(),
                    3.0,
                    theme::GOLD.linear_multiply(0.5),
                );
            }
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 3.0;
                ui.label(
                    RichText::new(value.as_str())
                        .size(20.0)
                        .family(theme::fam_semibold())
                        .color(theme::INK),
                );
                let (tick, _) = ui.allocate_exact_size(vec2(18.0, 2.0), Sense::hover());
                ui.painter().rect_filled(tick, 1.0, theme::GOLD);
                ui.label(RichText::new(*label).size(10.5).color(theme::DIM));
            });
        }
    });
}

/// A card with one 45-degree notched corner (top-right), manuscript-colophon
/// style: BG2 fill, hairline stroke, and — when `accent` — a 2px gold filament
/// along the left edge and a fill nudged 6% toward gold. The notched body is a
/// convex 5-gon painted behind the contents (12px padding) via the
/// placeholder-then-set idiom.
/// `thread`: `None` = plain; `Some(color)` = a colored filament down the left
/// spine and a fill nudged 6% toward that color (gold for accent, amber for
/// warnings, and so on).
pub fn notched_card(ui: &mut egui::Ui, thread: Option<Color32>, add: impl FnOnce(&mut egui::Ui)) {
    const NOTCH: f32 = 12.0;
    let bg = ui.painter().add(egui::Shape::Noop);
    let filament = ui.painter().add(egui::Shape::Noop);
    let rect = egui::Frame::new()
        .inner_margin(Margin::same(12))
        .show(ui, add)
        .response
        .rect;
    let notch = NOTCH.min(rect.width().max(0.0)).min(rect.height().max(0.0));
    let points = vec![
        rect.left_top(),
        pos2(rect.right() - notch, rect.top()),
        pos2(rect.right(), rect.top() + notch),
        rect.right_bottom(),
        rect.left_bottom(),
    ];
    let fill = match thread {
        Some(color) => theme::lerp_color(theme::BG2, color, 0.06),
        None => theme::BG2,
    };
    ui.painter().set(
        bg,
        egui::Shape::convex_polygon(points, fill, theme::stroke_faint()),
    );
    if let Some(color) = thread {
        let h = (rect.height() - 16.0).max(0.0);
        let bar = Rect::from_center_size(pos2(rect.left() + 1.0, rect.center().y), vec2(2.0, h));
        ui.painter()
            .set(filament, egui::Shape::rect_filled(bar, 1.0, color));
    }
}

/// A tiny uppercase file-extension chip ("SVG", "TXT", max 4 chars, "·" when
/// none): mono 9.5, LAPIS on a LAPIS-tinted pill, fixed 34x16.
pub fn ext_chip(ui: &mut egui::Ui, path: &str) {
    let ext = chip_ext(path);
    let (rect, _) = ui.allocate_exact_size(vec2(34.0, 16.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, 5.0, theme::LAPIS.linear_multiply(0.14));
    ui.put(
        rect,
        egui::Label::new(
            RichText::new(ext)
                .size(9.5)
                .family(FontFamily::Monospace)
                .color(theme::LAPIS),
        ),
    );
}

fn chip_ext(path: &str) -> String {
    let ext = path.rsplit('.').next().unwrap_or("");
    let n = ext.chars().count();
    // Five characters still fit the 34px well at 9.5pt monospace, and the
    // common web-asset extensions (woff2, jsonc, xhtml) are exactly five —
    // falling back to a bare dot for those looked like a missing chip.
    if path.contains('.') && (1..=5).contains(&n) && ext != path {
        ext.to_uppercase()
    } else {
        "·".to_owned()
    }
}

/// A seal mark for lease state: a small filled khatam (r = 6.5) in `color`;
/// `active` adds a soft halo circle behind. Allocates 16x16.
pub fn seal_dot(ui: &mut egui::Ui, color: Color32, active: bool) {
    let (rect, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
    let c = rect.center();
    if active {
        ui.painter()
            .circle_filled(c, 8.0, color.linear_multiply(0.15));
    }
    ornament::khatam(ui.painter(), c, 6.5, color, true);
}

/// Primary action button with crafted depth: gold fill, ON_GOLD label
/// (fam_medium 13.5), a 1px inner top-highlight line (white 18% alpha), hover
/// brightens to GOLD_HI, press flattens (no highlight). Min height 30.
pub fn bevel_primary(ui: &mut egui::Ui, label: &str) -> egui::Response {
    let resp = ui
        .scope(|ui| {
            ui.spacing_mut().interact_size.y = 30.0;
            let v = &mut ui.style_mut().visuals;
            for w in [
                &mut v.widgets.inactive,
                &mut v.widgets.hovered,
                &mut v.widgets.active,
            ] {
                w.fg_stroke = Stroke::new(1.0, theme::ON_GOLD);
                w.bg_stroke = Stroke::NONE;
                w.corner_radius = CornerRadius::same(theme::R_BUTTON);
                w.expansion = 0.0;
            }
            v.widgets.inactive.weak_bg_fill = theme::GOLD;
            v.widgets.hovered.weak_bg_fill = theme::GOLD_HI;
            v.widgets.active.weak_bg_fill = theme::GOLD;
            ui.add(egui::Button::new(
                RichText::new(label)
                    .size(13.5)
                    .family(theme::fam_medium())
                    .color(theme::ON_GOLD),
            ))
        })
        .inner;
    if !resp.is_pointer_button_down_on() && resp.rect.width() > 8.0 {
        ui.painter().hline(
            egui::Rangef::new(resp.rect.left() + 3.0, resp.rect.right() - 3.0),
            resp.rect.top() + 1.5,
            Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 46)),
        );
    }
    resp
}

/// Progress: a 6px rounded BG_INPUT track, gold fill with a 5px brighter head
/// dot at the leading edge; `frac` clamped to 0..=1 (NaN treated as 0).
pub fn progress_gold(ui: &mut egui::Ui, frac: f32) {
    let frac = if frac.is_finite() {
        frac.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (rect, _) =
        ui.allocate_exact_size(vec2(ui.available_width().max(0.0), 6.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 3.0, theme::BG_INPUT);
    if frac > 0.0 && rect.width() > 0.0 {
        let min_w = 6.0_f32.min(rect.width());
        let w = (rect.width() * frac).clamp(min_w, rect.width());
        let fill = Rect::from_min_size(rect.min, vec2(w, rect.height()));
        p.rect_filled(fill, 3.0, theme::GOLD);
        p.circle_filled(
            pos2(fill.right() - 3.0, rect.center().y),
            2.5,
            theme::GOLD_HI,
        );
    }
}

/// A centered date rule for grouped lists: hairlines left and right, the label
/// (10.5, FAINT, fam_medium) in the middle with 10px gaps, diamonds at the
/// outer ends of both hairlines.
pub fn day_rule(ui: &mut egui::Ui, label: &str) {
    let (rect, _) =
        ui.allocate_exact_size(vec2(ui.available_width().max(0.0), 18.0), Sense::hover());
    let text_w = ui
        .painter()
        .layout_no_wrap(
            label.to_owned(),
            FontId::new(10.5, theme::fam_medium()),
            theme::FAINT,
        )
        .size()
        .x;
    ui.put(
        rect,
        egui::Label::new(
            RichText::new(label)
                .size(10.5)
                .family(theme::fam_medium())
                .color(theme::FAINT),
        ),
    );
    let y = rect.center().y;
    let d = 2.5;
    let left_end = rect.center().x - text_w / 2.0 - 10.0;
    let right_start = rect.center().x + text_w / 2.0 + 10.0;
    let p = ui.painter();
    let hair = theme::stroke_faint();
    let dcol = theme::GOLD.linear_multiply(0.45);
    if left_end > rect.left() + 2.0 * d + 2.0 {
        p.hline(egui::Rangef::new(rect.left() + 2.0 * d, left_end), y, hair);
        ornament::diamond(p, pos2(rect.left() + d, y), d, dcol);
    }
    if right_start < rect.right() - 2.0 * d - 2.0 {
        p.hline(
            egui::Rangef::new(right_start, rect.right() - 2.0 * d),
            y,
            hair,
        );
        ornament::diamond(p, pos2(rect.right() - d, y), d, dcol);
    }
}

/// One row of a vertical timeline (audit view): a continuous 1px rule at
/// x = 8 (FAINT at 25%), a node dot (r = 3.5, `color`) on the rule aligned
/// with the first content line, `time` (mono 10.5 FAINT) after the rail, then
/// the `add` contents. The rule spans the full row height, measured after
/// layout; it is painted first so the dot sits on top.
pub fn timeline_row(
    ui: &mut egui::Ui,
    color: Color32,
    time: &str,
    add: impl FnOnce(&mut egui::Ui),
) {
    let origin = ui.cursor().min;
    // Start the rail one item-gap early so consecutive rows form one
    // continuous line instead of 8px stitches.
    let lead = ui.spacing().item_spacing.y;
    let rail = ui
        .horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let (cell, _) = ui.allocate_exact_size(vec2(16.0, 17.0), Sense::hover());
            ui.label(
                RichText::new(time)
                    .size(10.5)
                    .family(FontFamily::Monospace)
                    .color(theme::FAINT),
            );
            ui.vertical(add);
            cell
        })
        .inner;
    let bottom = ui.min_rect().bottom();
    let x = origin.x + 8.0;
    let p = ui.painter();
    p.vline(
        x,
        egui::Rangef::new(origin.y - lead, bottom),
        Stroke::new(1.0, theme::FAINT.linear_multiply(0.25)),
    );
    p.circle_filled(pos2(x, rail.center().y), 3.5, color);
}

/// Crafted empty state: a low-alpha khatam watermark behind a centered title
/// (fam_semibold 14.5 DIM) and hint (11.5 FAINT); height 140.
pub fn empty_state(ui: &mut egui::Ui, title: &str, hint: &str) {
    let (rect, _) =
        ui.allocate_exact_size(vec2(ui.available_width().max(0.0), 140.0), Sense::hover());
    if rect.width() <= 0.0 {
        return;
    }
    ornament::watermark(&ui.painter_at(rect), rect, theme::GOLD);
    let c = rect.center();
    let title_rect = Rect::from_center_size(pos2(c.x, c.y - 9.0), vec2(rect.width(), 20.0));
    let hint_rect = Rect::from_center_size(pos2(c.x, c.y + 13.0), vec2(rect.width(), 16.0));
    ui.put(
        title_rect,
        egui::Label::new(
            RichText::new(title)
                .size(14.5)
                .family(theme::fam_semibold())
                .color(theme::DIM),
        ),
    );
    ui.put(
        hint_rect,
        egui::Label::new(RichText::new(hint).size(11.5).color(theme::FAINT)),
    );
}

#[cfg(test)]
mod tests {
    use super::chip_ext;

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
}
