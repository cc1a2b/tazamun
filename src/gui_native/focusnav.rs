//! Focus visibility and keyboard navigation for the native GUI: the layer
//! that puts the corner-ticked focus ring (`fields::focus_ring`) on every
//! focusable widget, Enter/Space activation for painter-drawn rows,
//! Arrow/Home/End navigation for the sidebar and Files lists, and a
//! skip-to-content link that exists only while it holds keyboard focus.
//! Pure presentation over `fields` and `theme`; zero I/O, total for
//! degenerate inputs, and the only clock is `animate_bool_with_time`.

use eframe::egui;
use egui::vec2;
use egui::{CornerRadius, Key, Modifiers, Sense};

use super::{fields, register, theme};

/// Draws the crafted focus ring around `resp` when it holds keyboard focus.
/// Call immediately after creating any focusable widget; a no-op when the
/// widget is not focused.
pub fn ring(ui: &egui::Ui, resp: &egui::Response) {
    let t = ui.ctx().animate_bool_with_time(
        resp.id.with("focus"),
        resp.has_focus(),
        theme::dur(theme::motion::STATE),
    );
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
/// at which point it appears as a small ringed chip at the given position.
/// Returns true when activated.
pub fn skip_link(ui: &mut egui::Ui, label: &str) -> bool {
    // Laid out before the chip is claimed, and whether or not it is focused:
    // the reserved space has to be the size of the words in it at this text
    // scale, and it has to be the same size in both states or the layout under
    // it shifts the moment a keyboard reaches this stop.
    let galley = ui.painter().layout_no_wrap(
        label.to_owned(),
        theme::font(theme::step::META, theme::fam_medium()),
        theme::ink(),
    );
    let size = chip_size(galley.size());
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    // Unfocused it paints nothing; the reserved space keeps layout still.
    if resp.has_focus() {
        let p = ui.painter();
        p.rect_filled(
            rect,
            CornerRadius::same(theme::R_CONTROL),
            theme::bg_raise(),
        );
        p.galley(rect.center() - galley.size() * 0.5, galley, theme::ink());
        ring(ui, &resp);
    }
    resp.clicked() || key_activated(ui, &resp)
}

/// The chip around the skip link's words: the galley with symmetric padding,
/// never shorter than the line box that type lays out into.
fn chip_size(galley: egui::Vec2) -> egui::Vec2 {
    skip_box(galley, register::line_h(theme::step::META))
}

/// Pure: the chip's arithmetic.
fn skip_box(galley: egui::Vec2, line_h: f32) -> egui::Vec2 {
    vec2(
        galley.x + theme::space::L * 2.0,
        galley.y.max(line_h) + theme::space::S * 2.0,
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Every text scale the control can actually reach.
    fn scales() -> impl Iterator<Item = f32> {
        (14..=44).map(|k| k as f32 / 20.0)
    }

    fn line_box(step: f32, scale: f32) -> f32 {
        step * scale * 1.5
    }

    /// The chip was 128 x 22, which held "Skip to content" at 100% and clipped
    /// it in both directions well before 200%.
    #[test]
    fn the_chip_holds_the_words_in_it() {
        for scale in scales() {
            let line = line_box(theme::step::META, scale);
            // What the words lay out to: Plex's Latin faces are 1.3em, and the
            // link is fifteen characters of them.
            let galley = vec2(theme::step::META * scale * 0.55 * 15.0, line / 1.5 * 1.3);
            let box_ = skip_box(galley, line);
            assert!(box_.x > galley.x, "the words overflow the chip at {scale}");
            assert!(box_.y > galley.y, "the words are sliced at {scale}");
            assert!(box_.y > line, "the line box does not fit at {scale}");
        }
    }

    #[test]
    fn the_chip_grows_with_the_text_scale() {
        let mut prev: Option<(f32, egui::Vec2)> = None;
        for scale in scales() {
            let line = line_box(theme::step::META, scale);
            let got = skip_box(vec2(80.0 * scale, line), line);
            if let Some((was, before)) = prev {
                assert!(got.x > before.x, "chip width flat from {was} to {scale}");
                assert!(got.y > before.y, "chip height flat from {was} to {scale}");
            }
            prev = Some((scale, got));
        }
    }

    /// A galley shorter than the line box still gets the line box, so the chip
    /// does not shrink around a word with no descenders in it.
    #[test]
    fn the_chip_never_drops_below_the_line_box() {
        let line = line_box(theme::step::META, 1.0);
        assert_eq!(
            skip_box(vec2(50.0, 0.0), line).y,
            skip_box(vec2(50.0, line), line).y
        );
    }
}
