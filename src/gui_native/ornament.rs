//! Procedural Arabic-geometric ornament for the native GUI: the eight-point
//! khatam star (نجمة ثمانية — two squares, one turned 45 degrees), girih-style
//! strapwork bands, stepped corner flourishes, and the manuscript diamond used
//! as the house separator mark. Everything is painter-drawn at hairline
//! weights and low alphas so the marks read as craft, never clipart. Pure
//! geometry — no I/O, no state, no randomness; every function is total and
//! paints nothing for degenerate inputs.

use eframe::egui;

/// Eight-point star (two squares, one rotated 45 degrees). `filled` = filled
/// petals at low alpha + hairline outline; else outline only. Radius = outer
/// vertex. Strokes render at ~0.55 of `color` and fills at 0.10, so pass the
/// full-strength accent color; the overlap of the two translucent squares
/// deepens the center on its own.
pub fn khatam(
    p: &egui::Painter,
    center: egui::Pos2,
    radius: f32,
    color: egui::Color32,
    filled: bool,
) {
    if !center.is_finite() || !radius.is_finite() || radius <= 0.0 {
        return;
    }
    let axis = square_points(center, radius, std::f32::consts::FRAC_PI_4);
    let turned = square_points(center, radius, 0.0);
    let outline = egui::Stroke::new(1.0, color.linear_multiply(0.55));
    if filled {
        let fill = color.linear_multiply(0.10);
        p.add(egui::Shape::convex_polygon(axis, fill, outline));
        p.add(egui::Shape::convex_polygon(turned, fill, outline));
    } else {
        p.add(egui::Shape::closed_line(axis, outline));
        p.add(egui::Shape::closed_line(turned, outline));
    }
    p.circle_filled(center, radius * 0.08, color.linear_multiply(0.7));
}

/// A thin horizontal strapwork band clipped to `rect` (height <= ~8px works
/// best): a repeating interlace of diagonals and axis lines in the girih
/// spirit, stroke ~1px at the given color. Deterministic, no randomness.
pub fn girih_band(p: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    if !rect.is_finite() || !rect.is_positive() {
        return;
    }
    let unit = rect.height() * 2.6;
    if unit < 1.0 {
        return;
    }
    let p = p.with_clip_rect(rect);
    let stroke = egui::Stroke::new(1.0, color);
    let top = rect.top();
    let bottom = rect.bottom();
    let mid = rect.center().y;
    let gap = unit * 0.15;
    // Overdraw half a unit past both edges so the clip never shows a void;
    // the cap keeps pathological rects cheap.
    let start = rect.left() - unit * 0.5;
    let count = ((rect.width() / unit).ceil() as usize + 2).min(4096);
    for k in 0..count {
        let x = start + k as f32 * unit;
        let cx = x + unit * 0.5;
        p.line_segment([egui::pos2(x, top), egui::pos2(x + unit, bottom)], stroke);
        p.line_segment([egui::pos2(x, bottom), egui::pos2(x + unit, top)], stroke);
        // Axis strand between neighboring crossings, stopped short of each so
        // the diagonals read as passing over it — the interlace.
        p.line_segment(
            [egui::pos2(cx + gap, mid), egui::pos2(cx + unit - gap, mid)],
            stroke,
        );
    }
}

/// A corner ornament radiating from `corner` INTO the rect along `toward`
/// (unit-ish direction, e.g. vec2(1.0, 1.0) for a top-left corner): stepped
/// hairline arcs/segments + a small khatam accent. `size` = overall extent.
pub fn corner_flourish(
    p: &egui::Painter,
    corner: egui::Pos2,
    toward: egui::Vec2,
    size: f32,
    color: egui::Color32,
) {
    if !corner.is_finite() || !toward.is_finite() || !size.is_finite() || size <= 0.0 {
        return;
    }
    if toward.length() <= f32::EPSILON {
        return;
    }
    let sx = if toward.x >= 0.0 { 1.0 } else { -1.0 };
    let sy = if toward.y >= 0.0 { 1.0 } else { -1.0 };
    // Three stepped quarter arcs sweeping the quadrant `toward` points into,
    // fading as they widen.
    for (factor, alpha) in [(0.45, 0.5), (0.7, 0.35), (0.95, 0.2)] {
        let r = size * factor;
        let points: Vec<egui::Pos2> = (0..=14)
            .map(|k| {
                let t = k as f32 / 14.0 * std::f32::consts::FRAC_PI_2;
                egui::pos2(corner.x + sx * r * t.cos(), corner.y + sy * r * t.sin())
            })
            .collect();
        p.add(egui::Shape::line(
            points,
            egui::Stroke::new(1.0, color.linear_multiply(alpha)),
        ));
    }
    let accent = corner + toward.normalized() * (size * 0.5);
    khatam(p, accent, size * 0.16, color, false);
}

/// A tiny rotated square (the manuscript diamond) — the house separator mark.
pub fn diamond(p: &egui::Painter, center: egui::Pos2, r: f32, color: egui::Color32) {
    if !center.is_finite() || !r.is_finite() || r <= 0.0 {
        return;
    }
    let points = vec![
        egui::pos2(center.x, center.y - r),
        egui::pos2(center.x + r, center.y),
        egui::pos2(center.x, center.y + r),
        egui::pos2(center.x - r, center.y),
    ];
    p.add(egui::Shape::convex_polygon(
        points,
        color,
        egui::Stroke::NONE,
    ));
}

/// A full-width horizontal rule with a centered diamond and short fade-out
/// gaps around it — replaces plain separators. Allocates its own space.
pub fn rule_with_diamond(ui: &mut egui::Ui, color: egui::Color32) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 12.0), egui::Sense::hover());
    if !rect.is_finite() || rect.width() < 1.0 {
        return;
    }
    let p = ui.painter();
    let cx = rect.center().x;
    let y = rect.center().y.floor() + 0.5;
    let gap = 14.0;
    let main = egui::Stroke::new(1.0, color.linear_multiply(0.25));
    let fade = egui::Stroke::new(1.0, color.linear_multiply(0.10));
    // Main hairlines stop `gap` short of the diamond; a fainter stub carries
    // each halfway into the gap so the line dissolves rather than ends.
    if cx - gap > rect.left() {
        p.line_segment([egui::pos2(rect.left(), y), egui::pos2(cx - gap, y)], main);
        p.line_segment(
            [egui::pos2(cx - gap, y), egui::pos2(cx - gap * 0.5, y)],
            fade,
        );
    }
    if cx + gap < rect.right() {
        p.line_segment([egui::pos2(cx + gap, y), egui::pos2(rect.right(), y)], main);
        p.line_segment(
            [egui::pos2(cx + gap * 0.5, y), egui::pos2(cx + gap, y)],
            fade,
        );
    }
    diamond(p, egui::pos2(cx, y), 2.6, color.linear_multiply(0.8));
}

/// A large, very-low-alpha khatam watermark centered in `rect` (for empty
/// states and the Home hero). Chooses alpha from `color` times ~0.05.
pub fn watermark(p: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    if !rect.is_finite() || !rect.is_positive() {
        return;
    }
    let radius = rect.width().min(rect.height()) * 0.32;
    khatam(p, rect.center(), radius, color.linear_multiply(0.05), false);
}

/// Four vertices of a square with circumradius `radius`, rotated by `phase`.
fn square_points(center: egui::Pos2, radius: f32, phase: f32) -> Vec<egui::Pos2> {
    (0..4)
        .map(|k| {
            let a = phase + k as f32 * std::f32::consts::FRAC_PI_2;
            egui::pos2(center.x + radius * a.cos(), center.y + radius * a.sin())
        })
        .collect()
}
