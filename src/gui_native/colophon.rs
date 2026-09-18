//! The colophon (About) panel body for the native GUI: the app's closing page
//! in manuscript spirit — the wordmark over a khatam,
//! build and engine facts, the embedded type with its licenses, and the Golden
//! Invariant as the closing seal under a diamond rule. Pure presentation over
//! `theme`, `ornament`, and `controls`: no I/O, no state. The caller owns the
//! surrounding overlay and frame; this body only fills the given width.

use eframe::egui;
use egui::text::LayoutJob;
use egui::{Align, Color32, RichText, Sense};
use egui::{pos2, vec2};

use super::{controls, ornament, theme};

/// A khatam's radius as a fraction of the cell it is struck in — the one part
/// of the mark that is a proportion rather than a measure, so it holds at every
/// text scale.
const SEAL_R: f32 = 0.375;

/// The colophon body: the wordmark under a khatam, build identity,
/// engine facts, type credits with their licenses, and the Golden Invariant
/// as the closing line under a diamond rule. Width-filling; the caller owns
/// the surrounding frame/overlay.
pub fn colophon(ui: &mut egui::Ui, version: &str) {
    ui.vertical_centered(|ui| {
        // The seal is set against the wordmark under it, so it is cut from the
        // same step: a fixed 40px mark over type that doubles is a wordmark
        // wearing a button.
        let cell = theme::sized(theme::step::DISPLAY) * 2.0;
        let (rect, _) = ui.allocate_exact_size(vec2(cell, cell), Sense::hover());
        ornament::khatam(
            ui.painter(),
            rect.center(),
            cell * SEAL_R,
            theme::gold(),
            false,
        );
        ui.add_space(theme::space::XS);
        super::chrome::wordmark(ui, theme::sized(theme::step::DISPLAY));
    });
    ui.add_space(theme::space::S);
    centered_text(
        ui,
        "strict-checkout folder sync between machines you trust — no server ever reads your files",
        theme::step::META,
        theme::ink_muted(),
    );

    ui.add_space(theme::space::M);
    ornament::rule_with_diamond(ui, theme::alpha(theme::gold(), 153));
    ui.add_space(theme::space::M);

    section_label(ui, "Build");
    ui.add_space(theme::space::XS);
    controls::leader_row(ui, "version", version);
    controls::leader_row(
        ui,
        "interface",
        "egui, drawn in-process — no webview, no runtime",
    );
    controls::leader_row(
        ui,
        "network",
        "iroh QUIC with NAT traversal, end-to-end encrypted",
    );
    controls::leader_row(ui, "hashing", "BLAKE3 content addressing");
    controls::leader_row(ui, "chunking", "FastCDC content-defined chunks");

    ui.add_space(theme::space::M);
    section_label(ui, "Type");
    ui.add_space(theme::space::XS);
    controls::leader_row(ui, "IBM Plex Sans", "SIL Open Font License 1.1");
    controls::leader_row(ui, "IBM Plex Sans Arabic", "SIL Open Font License 1.1");
    controls::leader_row(ui, "IBM Plex Serif", "SIL Open Font License 1.1");
    controls::leader_row(ui, "IBM Plex Mono", "SIL Open Font License 1.1");
    ui.add_space(theme::space::S);
    centered_text(
        ui,
        "license texts ship inside the repository under assets/fonts",
        theme::step::META,
        theme::ink_faint(),
    );

    ui.add_space(theme::space::M);
    ornament::rule_with_diamond(ui, theme::alpha(theme::gold(), 153));
    ui.add_space(theme::space::M);

    // Closing seal: a small filled khatam over the Golden Invariant promise.
    ui.vertical_centered(|ui| {
        let cell = theme::sized(theme::step::TITLE);
        let (rect, _) = ui.allocate_exact_size(vec2(cell, cell), Sense::hover());
        ornament::khatam(
            ui.painter(),
            rect.center(),
            cell * SEAL_R,
            theme::gold(),
            true,
        );
    });
    ui.add_space(theme::space::S);
    centered_text(
        ui,
        "Nothing here is ever overwritten unseen, and nothing is deleted unless you choose it. Every ambiguous moment keeps both copies and says so.",
        theme::step::META,
        theme::ink_muted(),
    );
    ui.add_space(theme::space::XS);
}

/// A left-aligned section label in the engraved voice this page is set in.
fn section_label(ui: &mut egui::Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .size(theme::sized(theme::step::LABEL))
            .family(theme::fam_serif())
            .color(theme::ink()),
    );
}

/// Center-aligned, width-wrapped body text: each wrapped row is centered
/// (halign) so a broken line stays symmetric on the page rather than ragged
/// against a centered block. Fills the width and allocates the galley's height.
fn centered_text(ui: &mut egui::Ui, text: &str, step: f32, color: Color32) {
    let width = ui.available_width().max(0.0);
    if width < 1.0 {
        return;
    }
    let mut job = LayoutJob::simple(
        text.to_owned(),
        theme::font(step, theme::fam_serif_text()),
        color,
        width,
    );
    job.halign = Align::Center;
    let galley = ui.painter().layout_job(job);
    let (rect, _) = ui.allocate_exact_size(vec2(width, galley.size().y), Sense::hover());
    ui.painter()
        .galley(pos2(rect.center().x, rect.top()), galley, color);
}
