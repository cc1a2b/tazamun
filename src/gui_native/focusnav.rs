//! Focus visibility and keyboard navigation for the native GUI: the layer
//! that puts the corner-ticked gold ring (`fields::focus_ring`) on every
//! focusable widget, Enter/Space activation for painter-drawn rows,
//! Arrow/Home/End navigation for the sidebar and Files lists, and a
//! skip-to-content link that exists only while it holds keyboard focus.
//! Pure presentation over `fields` and `theme`; zero I/O, total for
//! degenerate inputs, and the only clock is `animate_bool_with_time`.

use eframe::egui;
use egui::vec2;
use egui::{CornerRadius, FontId, Key, Modifiers, Sense};

use super::{fields, theme};

const RING_FADE_S: f32 = 0.12;
const SKIP_W: f32 = 128.0;
const SKIP_H: f32 = 22.0;

/// Draws the crafted focus ring around `resp` when it holds keyboard focus,
/// fading in over ~0.12s. Call immediately after creating any focusable
/// widget; a no-op when the widget is not focused.
pub fn ring(ui: &egui::Ui, resp: &egui::Response) {
    let t = ui
        .ctx()
        .animate_bool_with_time(resp.id.with("focus"), resp.has_focus(), RING_FADE_S);
    if t <= 0.0 {
        return;
    }
    fields::focus_ring(ui, resp.rect, t);
}

/// Makes a painter-drawn row focusable and rings it: allocates nothing, but
/// registers `resp` for keyboard focus and returns true when the row was
/// activated this frame by Enter or Space while focused.
pub fn activate(ui: &egui::Ui, resp: &egui::Response) -> bool {
    ring(ui, resp);
    key_activated(ui, resp)
}

/// Arrow/Home/End navigation over a list of `len` items. Mutates `sel` in
/// place, clamping and wrapping at the ends, and returns true when it moved.
/// Consumes the keys it uses so they never leak into the views beneath.
pub fn list_nav(ui: &egui::Ui, len: usize, sel: &mut usize) -> bool {
    if len == 0 {
        return false;
    }
    let before = *sel;
    // A stale index from a shrunk list clamps before any arithmetic.
    *sel = (*sel).min(len - 1);
    let (down, up, home, end) = ui.input_mut(|i| {
        (
            i.consume_key(Modifiers::NONE, Key::ArrowDown),
            i.consume_key(Modifiers::NONE, Key::ArrowUp),
            i.consume_key(Modifiers::NONE, Key::Home),
            i.consume_key(Modifiers::NONE, Key::End),
        )
    });
    if down {
        *sel = if *sel + 1 == len { 0 } else { *sel + 1 };
    }
    if up {
        *sel = if *sel == 0 { len - 1 } else { *sel - 1 };
    }
    if home {
        *sel = 0;
    }
    if end {
        *sel = len - 1;
    }
    *sel != before
}

/// A "skip to content" affordance: invisible until it takes keyboard focus,
/// at which point it appears as a small gold-ringed chip at the given
/// position. Returns true when activated.
pub fn skip_link(ui: &mut egui::Ui, label: &str) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(SKIP_W, SKIP_H), Sense::click());
    // Unfocused it paints nothing; the reserved space keeps layout still.
    if resp.has_focus() {
        let p = ui.painter();
        p.rect_filled(rect, CornerRadius::same(theme::R_BUTTON), theme::BG3);
        let galley = p.layout_no_wrap(
            label.to_owned(),
            FontId::new(11.5, theme::fam_medium()),
            theme::INK,
        );
        p.galley(rect.center() - galley.size() * 0.5, galley, theme::INK);
        ring(ui, &resp);
    }
    resp.clicked() || key_activated(ui, &resp)
}

/// Enter or Space while `resp` holds focus. Focus is checked first and the
/// keys are consumed only then, so an unfocused row never swallows the
/// activation meant for another widget.
fn key_activated(ui: &egui::Ui, resp: &egui::Response) -> bool {
    if !resp.has_focus() {
        return false;
    }
    ui.input_mut(|i| {
        let enter = i.consume_key(Modifiers::NONE, Key::Enter);
        let space = i.consume_key(Modifiers::NONE, Key::Space);
        enter || space
    })
}
