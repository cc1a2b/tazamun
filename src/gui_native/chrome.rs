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

/// The wordmark: a gold `T` and the rest in ink, drawn rather than blitted.
///
/// It used to be a pre-rendered texture of the shaped Arabic `تزامُن`, because
/// egui has no bidi or shaping and would have drawn the letters disjoint. The
/// Latin form needs neither: it renders from the embedded Inter at any size,
/// stays crisp on every display scale, and — the reason it changed — is simply
/// easier to read at UI size for the people using this.
pub fn wordmark(ui: &mut egui::Ui, size: f32) {
    let font = FontId::new(size, super::theme::fam_semibold());
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
    part("T", super::theme::GOLD);
    part("azamun", super::theme::INK);
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
    p.rect_filled(rect, CornerRadius::same(r), theme::BG0);
    if !maximized {
        p.rect_stroke(
            rect.shrink(0.5),
            CornerRadius::same(r),
            Stroke::new(1.0, Color32::from_rgba_unmultiplied(0xe9, 0xec, 0xf8, 26)),
            egui::StrokeKind::Inside,
        );
    }
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
    p.rect_filled(rect, cr, theme::BG1);
    // The brand signature: a whisper of girih strapwork along the bar's foot,
    // with the hairline underneath keeping the edge crisp.
    let band = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 10.0, rect.bottom() - 7.0),
        egui::pos2(rect.right() - 10.0, rect.bottom() - 1.5),
    );
    ornament::girih_band(p, band, theme::GOLD.linear_multiply(0.10));
    p.hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        Stroke::new(1.0, theme::GOLD.linear_multiply(0.16)),
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
    ui.painter().rect_filled(rect, cr, theme::BG1);
    ui.painter()
        .vline(rect.right() - 0.5, rect.y_range(), theme::stroke_faint());
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

/// A minimal painter-drawn window button (28×28 hover pill, crisp 1.25px icon).
pub fn window_button(ui: &mut egui::Ui, kind: WinButton, maximized: bool) -> egui::Response {
    let size = egui::vec2(30.0, 26.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let hovered = resp.hovered();
    let danger = matches!(kind, WinButton::Close);
    let t = ui.ctx().animate_bool_with_time(resp.id, hovered, 0.10);
    if t > 0.0 {
        let fill = if danger {
            theme::lerp_color(Color32::TRANSPARENT, theme::BAD.linear_multiply(0.9), t)
        } else {
            theme::lerp_color(Color32::TRANSPARENT, theme::BG3, t)
        };
        ui.painter().rect_filled(rect, 7.0, fill);
    }
    let ink = if danger && hovered {
        Color32::WHITE
    } else if hovered {
        theme::INK
    } else {
        theme::DIM
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
                p.rect_filled(r1.expand(0.8), 1.5, theme::BG1);
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
