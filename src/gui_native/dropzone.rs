//! Drag-and-drop onto the window: drag a folder from the OS file manager to
//! open it (if it is already a registered session) or to prefill "create a
//! session here". While files hover we paint a branded foreground overlay —
//! dimmed window, inset dashed gold frame, centered hint card. Presentation
//! and classification only; no I/O beyond fs metadata checks.

use eframe::egui;
use egui::{
    Color32, CornerRadius, FontFamily, FontId, LayerId, Order, Pos2, Rect, Shape, Stroke,
    StrokeKind,
};

use super::{chrome, copy, theme};

const BORDER_INSET: f32 = 14.0;
const DASH_LEN: f32 = 10.0;
const GAP_LEN: f32 = 7.0;

pub enum DropAction {
    /// The dropped folder is already a registered session — open it.
    OpenSession(String),
    /// A folder that is not a session yet — prefill the create form.
    PrefillInit(String),
    /// The drop could not be used; the &'static str is a short human reason.
    Rejected(&'static str),
}

/// Paints the hover overlay while the OS reports files being dragged over the
/// window. Call every frame after the panels; cheap no-op when nothing hovers.
pub fn overlay_if_hovering(ui: &egui::Ui) {
    if ui.input(|i| i.raw.hovered_files.is_empty()) {
        return;
    }
    let screen = ui.ctx().content_rect();
    // Layer painter only — never allocates a widget, so the overlay can never
    // steal input from whatever the drop lands on.
    let p = ui.ctx().layer_painter(LayerId::new(
        Order::Foreground,
        egui::Id::new("tzm-dropzone"),
    ));

    // Dim follows the window radius: the frameless window is transparent, so a
    // square fill would tint the empty rounded corners.
    let r = chrome::radius(chrome::is_maximized(ui));
    p.rect_filled(
        screen,
        CornerRadius::same(r),
        Color32::from_black_alpha(110),
    );

    let b = screen.shrink(BORDER_INSET);
    let dash = Stroke::new(1.5, theme::GOLD.linear_multiply(0.7));
    for [start, end] in [
        [b.left_top(), b.right_top()],
        [b.right_top(), b.right_bottom()],
        [b.right_bottom(), b.left_bottom()],
        [b.left_bottom(), b.left_top()],
    ] {
        p.extend(Shape::dashed_line(&[start, end], dash, DASH_LEN, GAP_LEN));
    }

    let title = p.layout_no_wrap(
        copy::DROP_OVERLAY_TITLE.to_owned(),
        FontId::new(16.0, theme::fam_semibold()),
        theme::INK,
    );
    let hint = p.layout_no_wrap(
        copy::DROP_OVERLAY_HINT.to_owned(),
        FontId::new(11.5, FontFamily::Proportional),
        theme::DIM,
    );
    let (t_sz, h_sz) = (title.size(), hint.size());
    let line_gap = 5.0;
    let pad = egui::vec2(18.0, 14.0);
    let inner = egui::vec2(t_sz.x.max(h_sz.x), t_sz.y + line_gap + h_sz.y);
    let card = Rect::from_center_size(screen.center(), inner + pad * 2.0);
    p.rect_filled(card, CornerRadius::same(theme::R_CARD + 2), theme::BG1);
    p.rect_stroke(
        card,
        CornerRadius::same(theme::R_CARD + 2),
        Stroke::new(1.0, theme::GOLD.linear_multiply(0.35)),
        StrokeKind::Inside,
    );
    let top = card.top() + pad.y;
    p.galley(
        Pos2::new(card.center().x - t_sz.x / 2.0, top),
        title,
        theme::INK,
    );
    p.galley(
        Pos2::new(card.center().x - h_sz.x / 2.0, top + t_sz.y + line_gap),
        hint,
        theme::DIM,
    );
}

/// Consumes a drop (the frame `dropped_files` is non-empty) and classifies it.
/// `registered` is the list of absolute session paths from the registry.
pub fn take_drop(ui: &egui::Ui, registered: &[String]) -> Option<DropAction> {
    let dropped = ui.input(|i| i.raw.dropped_files.clone());
    if dropped.is_empty() {
        return None;
    }
    let Some(path) = dropped.into_iter().find_map(|f| f.path) else {
        return Some(DropAction::Rejected(copy::DROP_REJECT_NO_PATH));
    };
    // absolute, not canonicalize: no symlink resolution, matching how the
    // registry stores paths as typed.
    let path = std::path::absolute(&path).unwrap_or(path);
    if path.is_dir() {
        let folder = path.to_string_lossy().into_owned();
        if registered.contains(&folder) {
            Some(DropAction::OpenSession(folder))
        } else {
            Some(DropAction::PrefillInit(folder))
        }
    } else {
        Some(DropAction::Rejected(copy::DROP_REJECT_NOT_FOLDER))
    }
}
