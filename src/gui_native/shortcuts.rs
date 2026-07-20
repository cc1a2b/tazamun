//! The keyboard sheet for the native GUI: one registry of every binding the
//! window answers to, and the ruled sheet that prints it — sections, each
//! binding shown as real key caps against a dotted leader to its meaning,
//! like the key-sheet at the back of a codex. Pure presentation over `theme`,
//! `ornament`, and `ceremony`; zero I/O, deterministic, total for degenerate
//! inputs.

use eframe::egui;
use egui::{FontFamily, FontId, RichText, Sense};
use egui::{pos2, vec2};

use super::{ceremony, ornament, theme};

/// One binding: the keys as they should appear on caps, and what it does.
pub struct Binding {
    /// Key cap labels in press order, e.g. `&["Ctrl", "K"]`.
    pub keys: &'static [&'static str],
    /// What the binding does, in the project's voice (lower case, no period).
    pub what: &'static str,
}

/// A titled group of bindings.
pub struct Section {
    pub title: &'static str,
    pub bindings: &'static [Binding],
}

// Caps carry the glyph a physical key shows ("," "+" "-"), never a spelled-out
// key name; "↑"/"↓" are U+2191/U+2193, present in both Inter and Hack.
const SECTIONS: &[Section] = &[
    Section {
        title: "Anywhere",
        bindings: &[
            Binding {
                keys: &["Ctrl", "K"],
                what: "open the command palette",
            },
            Binding {
                keys: &["Ctrl", "R"],
                what: "refresh now",
            },
            Binding {
                keys: &["Ctrl", ","],
                what: "open settings for the selected session",
            },
            Binding {
                keys: &["?"],
                what: "show this sheet",
            },
            Binding {
                keys: &["Esc"],
                what: "close whatever is open",
            },
        ],
    },
    Section {
        title: "Moving",
        bindings: &[
            Binding {
                keys: &["Tab"],
                what: "step to the next control",
            },
            Binding {
                keys: &["Shift", "Tab"],
                what: "step back",
            },
            Binding {
                keys: &["↑", "↓"],
                what: "move through a list",
            },
            Binding {
                keys: &["Enter"],
                what: "activate what is focused",
            },
        ],
    },
    Section {
        title: "Sessions",
        bindings: &[
            Binding {
                keys: &["Ctrl", "1"],
                what: "overview",
            },
            Binding {
                keys: &["Ctrl", "2"],
                what: "peers",
            },
            Binding {
                keys: &["Ctrl", "3"],
                what: "files",
            },
            Binding {
                keys: &["Ctrl", "4"],
                what: "conflicts",
            },
            Binding {
                keys: &["Ctrl", "5"],
                what: "history",
            },
            Binding {
                keys: &["Ctrl", "6"],
                what: "audit",
            },
            Binding {
                keys: &["Ctrl", "7"],
                what: "settings",
            },
            Binding {
                keys: &["Ctrl", "A"],
                what: "mark every session",
            },
            Binding {
                keys: &["Ctrl", "click"],
                what: "add one session to the marked set",
            },
            Binding {
                keys: &["Shift", "click"],
                what: "mark a range of sessions",
            },
        ],
    },
    Section {
        title: "Reading",
        bindings: &[
            Binding {
                keys: &["Ctrl", "F"],
                what: "jump to the file filter",
            },
            Binding {
                keys: &["Ctrl", "+"],
                what: "larger text",
            },
            Binding {
                keys: &["Ctrl", "-"],
                what: "smaller text",
            },
            Binding {
                keys: &["Ctrl", "0"],
                what: "text back to normal",
            },
        ],
    },
];

/// Every binding the window answers to, grouped for display. This is the
/// single source of truth: the integrator wires the same keys, and the sheet
/// renders from here, so the two can never drift.
pub fn sections() -> &'static [Section] {
    SECTIONS
}

/// The sheet body: sections, each binding rendered as key caps followed by a
/// dotted leader to its meaning. Width-filling; the caller owns the frame.
pub fn sheet(ui: &mut egui::Ui) {
    let all = sections();
    for (i, section) in all.iter().enumerate() {
        ui.add_space(6.0);
        ui.label(
            RichText::new(section.title)
                .size(12.5)
                .family(theme::fam_semibold())
                .color(theme::INK),
        );
        for binding in section.bindings {
            binding_row(ui, binding);
        }
        if i + 1 < all.len() {
            ornament::rule_with_diamond(ui, theme::GOLD.linear_multiply(0.5));
        }
    }
}

/// One ruled line: adjacent caps (4px apart, no plus signs), then leader dots
/// to the right-aligned meaning — `controls::leader_row`'s book-index look,
/// with the caps standing in as the entry.
fn binding_row(ui: &mut egui::Ui, binding: &Binding) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for key in binding.keys {
            ceremony::keycap(ui, key);
        }
        let (rect, _) =
            ui.allocate_exact_size(vec2(ui.available_width().max(0.0), 16.0), Sense::hover());
        if rect.width() <= 0.0 {
            return;
        }
        let p = ui.painter().with_clip_rect(rect);
        let galley = p.layout_no_wrap(
            binding.what.to_owned(),
            FontId::new(12.0, FontFamily::Proportional),
            theme::DIM,
        );
        let size = galley.size();
        let dots_from = rect.left() + 8.0;
        let dots_to = rect.right() - size.x - 8.0;
        // Leaders only when a real gap remains; a long meaning just clips.
        if dots_to - dots_from >= 12.0 {
            let y = rect.bottom() - 4.0;
            let count = (((dots_to - dots_from) / 4.0).floor() as usize + 1).min(2048);
            for k in 0..count {
                p.circle_filled(
                    pos2(dots_from + k as f32 * 4.0, y),
                    0.7,
                    theme::FAINT.linear_multiply(0.4),
                );
            }
        }
        p.galley(
            pos2(rect.right() - size.x, rect.center().y - size.y / 2.0),
            galley,
            theme::DIM,
        );
    });
}
