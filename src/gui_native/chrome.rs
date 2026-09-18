//! P23 custom window chrome: the frameless, rounded, self-drawn window — root
//! container, title-bar painting, minimal window buttons, drag/double-click
//! handling, and edge-resize zones. The OS decorations are off; everything the
//! user sees is drawn here, which is what gives the app its rounded corners on
//! every platform.

use eframe::egui;
use egui::viewport::ResizeDirection;
use egui::{Color32, CornerRadius, CursorIcon, FontId, Rect, Sense, Stroke, ViewportCommand};

use super::{ornament, theme};

pub const TITLEBAR_H: f32 = 46.0;
const RESIZE_BAND: f32 = 6.0;
const CORNER_BAND: f32 = 14.0;

/// The wordmark: a gold `T` and the rest in ink, drawn rather than blitted,
/// in the engraved voice. `size` is in pixels — the caller supplies it already
/// scaled, e.g. `theme::sized(theme::step::DISPLAY)`.
///
/// It used to be a pre-rendered texture of the shaped Arabic `تزامُن`, because
/// egui has no bidi or shaping and would have drawn the letters disjoint. The
/// Latin form needs neither: it renders from the embedded serif at any size,
/// stays crisp on every display scale, and — the reason it changed — is simply
/// easier to read at UI size for the people using this.
pub fn wordmark(ui: &mut egui::Ui, size: f32) {
    let font = FontId::new(size, theme::fam_serif());
    let mut job = egui::text::LayoutJob::default();
    let mut part = |text: &str, color| {
        job.append(
            text,
            0.0,
            egui::TextFormat {
                font_id: font.clone(),
                color,
                ..Default::default()
            },
        );
    };
    part("T", theme::gold());
    part("azamun", theme::ink());
    ui.label(job);
}

/// The embedded window icon (raw RGBA baked from the P8 brand PNG).
pub fn window_icon() -> egui::IconData {
    egui::IconData {
        rgba: include_bytes!("../../assets/gui/icon-128.rgba").to_vec(),
        width: 128,
        height: 128,
    }
}

pub fn is_maximized(ui: &egui::Ui) -> bool {
    ui.input(|i| i.viewport().maximized.unwrap_or(false))
}

/// Window corner radius for the current state (square when maximized).
pub fn radius(maximized: bool) -> u8 {
    if maximized { 0 } else { theme::R_WINDOW }
}

/// Paints the window body: rounded base fill, hairline border, top highlight.
pub fn paint_root(ui: &egui::Ui, maximized: bool) {
    let rect = ui.max_rect();
    let r = radius(maximized);
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(r), theme::bg_deep());
    if !maximized {
        p.rect_stroke(
            rect.shrink(0.5),
            CornerRadius::same(r),
            Stroke::new(theme::RULE_W, theme::rule_divider()),
            egui::StrokeKind::Inside,
        );
    }
}

/// Where the girih band is ruled inside a title bar of `bar`: a band the height
/// of one space step, inset from both ends and standing clear of the seam
/// hairline at the bar's foot.
///
/// Pure, and separate from the painting because the band is the seam between
/// the title bar and everything under it: the menu heads above rule themselves
/// against the space it leaves, and a band that crept up into them would strike
/// the words out.
pub fn band_rect(bar: Rect) -> Rect {
    let h = theme::space::M;
    Rect::from_min_max(
        egui::pos2(
            bar.left() + theme::space::XXL,
            bar.bottom() - h - theme::space::XS,
        ),
        egui::pos2(
            bar.right() - theme::space::XXL,
            bar.bottom() - theme::space::XS,
        ),
    )
}

/// Paints the title-bar surface (rounded top corners only).
pub fn paint_titlebar_bg(ui: &egui::Ui, maximized: bool) {
    let rect = ui.max_rect();
    let r = radius(maximized);
    let cr = CornerRadius {
        nw: r,
        ne: r,
        sw: 0,
        se: 0,
    };
    let p = ui.painter();
    p.rect_filled(rect, cr, theme::bg_chrome());
    // The brand signature, and the one piece of ornament always on screen:
    // girih strapwork ruled along the bar's foot, stopped at each end by the
    // house diamond so it reads as a drawn rule rather than a texture that ran
    // off the edge.
    //
    // Struck at a wash, not at full `gold_deep`. This ornament has failed in
    // both directions: at watermark alphas nobody could see it, and at full
    // strength it became the brightest thing in the window and out-shouted the
    // register it is supposed to introduce. It is a seam between the chrome and
    // the page — legible as weave, never as a chain.
    let band = band_rect(rect);
    if band.is_positive() {
        ornament::girih_band(
            p,
            band,
            theme::wash::of(theme::gold_deep(), theme::wash::SELECT),
        );
        // Half the band's height, so the diamond's points land exactly on its
        // two edges and the strapwork reads as stopped rather than as cut off.
        let r = band.height() * 0.5;
        ornament::diamond(
            p,
            egui::pos2(band.left(), band.center().y),
            r,
            theme::gold(),
        );
        ornament::diamond(
            p,
            egui::pos2(band.right(), band.center().y),
            r,
            theme::gold(),
        );
    }
    // The seam itself: one crisp gold hairline where the chrome ends and the
    // page begins.
    p.hline(
        rect.x_range(),
        theme::snap(rect.bottom() - theme::RULE_W),
        Stroke::new(theme::RULE_W, theme::gold_deep()),
    );
}

/// Paints the sidebar surface (rounded bottom-left corner only).
pub fn paint_sidebar_bg(ui: &egui::Ui, maximized: bool) {
    let rect = ui.max_rect();
    let cr = CornerRadius {
        nw: 0,
        ne: 0,
        sw: radius(maximized),
        se: 0,
    };
    ui.painter().rect_filled(rect, cr, theme::bg_chrome());
    ui.painter().vline(
        rect.right() - 0.5,
        rect.y_range(),
        Stroke::new(theme::RULE_W, theme::rule_hair()),
    );
}

/// Title-bar drag / double-click handling over `bar_rect`. Call BEFORE laying
/// widgets in the bar so buttons keep priority on clicks (egui gives later
/// widgets the hover, and we only start OS drags from empty bar space).
pub fn titlebar_interactions(ui: &mut egui::Ui, bar_rect: Rect) {
    let resp = ui.interact(
        bar_rect,
        egui::Id::new("tzm-titlebar"),
        Sense::click_and_drag(),
    );
    if resp.double_clicked() {
        let max = is_maximized(ui);
        ui.ctx().send_viewport_cmd(ViewportCommand::Maximized(!max));
    } else if resp.drag_started() {
        ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
    }
}

pub enum WinButton {
    Minimize,
    MaximizeRestore,
    Close,
}

/// One window button's cell.
pub const WIN_BUTTON_W: f32 = 30.0;
const WIN_BUTTON_H: f32 = 26.0;

/// How much of the bar's trailing edge the three window buttons take, given the
/// bar's horizontal item spacing.
///
/// Anything laid out in the bar before them — the menu bar especially — is
/// working from `available_width`, which still counts this strip because the
/// buttons are allocated afterwards in a right-to-left layout. A menu that
/// budgeted from the raw figure would run underneath Close, and a window whose
/// Close cannot be clicked cannot be shut.
pub fn window_buttons_width(spacing: f32) -> f32 {
    WIN_BUTTON_W * 3.0 + spacing.max(0.0) * 2.0
}

/// A minimal painter-drawn window button (28×28 hover pill, crisp 1.25px icon).
pub fn window_button(ui: &mut egui::Ui, kind: WinButton, maximized: bool) -> egui::Response {
    let size = egui::vec2(WIN_BUTTON_W, WIN_BUTTON_H);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let hovered = resp.hovered();
    let danger = matches!(kind, WinButton::Close);
    let t = ui
        .ctx()
        .animate_bool_with_time(resp.id, hovered, theme::dur(theme::motion::TOUCH));
    if t > 0.0 {
        let fill = if danger {
            theme::mix(
                Color32::TRANSPARENT,
                theme::alpha(theme::custody_blocked(), 230),
                t,
            )
        } else {
            theme::mix(Color32::TRANSPARENT, theme::bg_raise(), t)
        };
        ui.painter()
            .rect_filled(rect, CornerRadius::same(theme::R_CHIP), fill);
    }
    let ink = if hovered {
        theme::ink()
    } else {
        theme::ink_muted()
    };
    let s = Stroke::new(1.25, ink);
    let c = rect.center();
    let p = ui.painter();
    match kind {
        WinButton::Minimize => {
            p.hline(egui::Rangef::new(c.x - 5.0, c.x + 5.0), c.y + 0.5, s);
        }
        WinButton::MaximizeRestore => {
            if maximized {
                // Two offset squares (restore).
                let r1 = Rect::from_center_size(c + egui::vec2(-1.5, 1.5), egui::vec2(7.0, 7.0));
                let r2 = Rect::from_center_size(c + egui::vec2(1.5, -1.5), egui::vec2(7.0, 7.0));
                p.rect_stroke(r2, 1.5, s, egui::StrokeKind::Middle);
                p.rect_filled(r1.expand(0.8), 1.5, theme::bg_chrome());
                p.rect_stroke(r1, 1.5, s, egui::StrokeKind::Middle);
            } else {
                let r1 = Rect::from_center_size(c, egui::vec2(9.0, 9.0));
                p.rect_stroke(r1, 2.0, s, egui::StrokeKind::Middle);
            }
        }
        WinButton::Close => {
            let d = 4.5;
            p.line_segment([c + egui::vec2(-d, -d), c + egui::vec2(d, d)], s);
            p.line_segment([c + egui::vec2(-d, d), c + egui::vec2(d, -d)], s);
        }
    }
    resp
}

/// Edge/corner resize zones for the frameless window. Uses raw pointer input
/// (not widgets), so it never fights panel contents; the bands sit in the outer
/// few pixels where nothing interactive is laid out.
pub fn resize_zones(ui: &egui::Ui) {
    if is_maximized(ui) {
        return;
    }
    let rect = ui.max_rect();
    let Some(pos) = ui.input(|i| i.pointer.interact_pos()) else {
        return;
    };
    let l = pos.x - rect.left() <= RESIZE_BAND;
    let r = rect.right() - pos.x <= RESIZE_BAND;
    let t = pos.y - rect.top() <= RESIZE_BAND;
    let b = rect.bottom() - pos.y <= RESIZE_BAND;
    let lc = pos.x - rect.left() <= CORNER_BAND;
    let rc = rect.right() - pos.x <= CORNER_BAND;
    let tc = pos.y - rect.top() <= CORNER_BAND;
    let bc = rect.bottom() - pos.y <= CORNER_BAND;

    let dir = if (t && lc) || (l && tc) {
        Some(ResizeDirection::NorthWest)
    } else if (t && rc) || (r && tc) {
        Some(ResizeDirection::NorthEast)
    } else if (b && lc) || (l && bc) {
        Some(ResizeDirection::SouthWest)
    } else if (b && rc) || (r && bc) {
        Some(ResizeDirection::SouthEast)
    } else if l {
        Some(ResizeDirection::West)
    } else if r {
        Some(ResizeDirection::East)
    } else if t {
        Some(ResizeDirection::North)
    } else if b {
        Some(ResizeDirection::South)
    } else {
        None
    };
    let Some(dir) = dir else { return };
    let cursor = match dir {
        ResizeDirection::North | ResizeDirection::South => CursorIcon::ResizeVertical,
        ResizeDirection::East | ResizeDirection::West => CursorIcon::ResizeHorizontal,
        ResizeDirection::NorthWest | ResizeDirection::SouthEast => CursorIcon::ResizeNwSe,
        ResizeDirection::NorthEast | ResizeDirection::SouthWest => CursorIcon::ResizeNeSw,
    };
    ui.ctx().set_cursor_icon(cursor);
    if ui.input(|i| i.pointer.primary_pressed()) {
        ui.ctx()
            .send_viewport_cmd(ViewportCommand::BeginResize(dir));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(w: f32) -> Rect {
        Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(w, TITLEBAR_H))
    }

    /// The band belongs to the bar's foot and to nothing above it: the menu
    /// heads rule themselves in the space it leaves.
    #[test]
    fn the_band_sits_clear_at_the_foot_of_the_bar() {
        let bar = bar(900.0);
        let band = band_rect(bar);
        assert!(band.is_positive());
        assert!(band.bottom() < bar.bottom(), "{band:?}");
        assert!(band.top() > bar.center().y, "{band:?}");
        assert!(band.left() > bar.left() && band.right() < bar.right());
    }

    /// A window squeezed narrower than the band's own insets must not paint a
    /// band inside out.
    #[test]
    fn a_bar_too_narrow_for_the_band_yields_nothing_to_paint() {
        assert!(!band_rect(bar(8.0)).is_positive());
        assert!(!band_rect(bar(0.0)).is_positive());
    }
}
