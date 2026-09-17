//! The keyboard sheet for the native GUI: one registry of every binding the
//! window answers to, and the ruled sheet that prints it — sections, each
//! binding shown as real key caps against a dotted leader to its meaning,
//! like the key-sheet at the back of a codex. Pure presentation over `theme`,
//! `ornament`, and `ceremony`; zero I/O, deterministic, total for degenerate
//! inputs.

use eframe::egui;
use egui::{FontFamily, RichText, Sense};
use egui::{pos2, vec2};

use super::{ceremony, ornament, theme};

/// One binding: the keys as they should appear on caps, what it does, and the
/// action the window takes when it fires.
pub struct Binding {
    /// Key cap labels in press order, e.g. `&["Ctrl", "K"]`.
    pub keys: &'static [&'static str],
    /// What the binding does, in the project's voice (lower case, no period).
    pub what: &'static str,
    /// What the window does. [`Action::Convention`] means egui or a list widget
    /// already answers this key and the global handler must not consume it.
    pub action: Action,
}

/// Everything the keyboard can ask the window to do.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    /// Toggle the command palette.
    Palette,
    /// Poll now rather than waiting for the tick.
    Refresh,
    /// Jump to the open session's settings.
    Settings,
    /// Toggle this sheet.
    Sheet,
    /// Mark every session in the sidebar.
    SelectAll,
    /// Jump to Files and focus its filter.
    FileFilter,
    /// Step the text scale.
    TextBigger,
    /// Step the text scale down.
    TextSmaller,
    /// Put the text scale back.
    TextReset,
    /// Jump to the nth tab of the open session, zero-based.
    Tab(usize),
    /// Handled elsewhere — by egui itself, by a list widget, or by whichever
    /// overlay is open. Listed so the sheet is complete, never consumed here.
    Convention,
}

impl Action {
    /// The chord to consume: the modifier, and every key that spells it.
    /// More than one key where layouts disagree — Ctrl and the `+` key reports
    /// `Plus` on some and `Equals` on others, and consuming only one leaves the
    /// other pending.
    pub fn chord(self) -> Option<(egui::Modifiers, &'static [egui::Key])> {
        use egui::Key;
        let cmd = egui::Modifiers::COMMAND;
        let none = egui::Modifiers::NONE;
        Some(match self {
            Self::Palette => (cmd, &[Key::K] as &'static [Key]),
            Self::Refresh => (cmd, &[Key::R]),
            Self::Settings => (cmd, &[Key::Comma]),
            Self::Sheet => (none, &[Key::Questionmark]),
            Self::SelectAll => (cmd, &[Key::A]),
            Self::FileFilter => (cmd, &[Key::F]),
            Self::TextBigger => (cmd, &[Key::Plus, Key::Equals]),
            Self::TextSmaller => (cmd, &[Key::Minus]),
            Self::TextReset => (cmd, &[Key::Num0]),
            Self::Tab(0) => (cmd, &[Key::Num1]),
            Self::Tab(1) => (cmd, &[Key::Num2]),
            Self::Tab(2) => (cmd, &[Key::Num3]),
            Self::Tab(3) => (cmd, &[Key::Num4]),
            Self::Tab(4) => (cmd, &[Key::Num5]),
            Self::Tab(5) => (cmd, &[Key::Num6]),
            Self::Tab(6) => (cmd, &[Key::Num7]),
            Self::Tab(_) | Self::Convention => return None,
        })
    }

    /// Whether a focused text field should swallow this instead. A bare `?` is
    /// a character being typed, and Ctrl+A selects the field's text rather than
    /// every session.
    pub fn yields_to_typing(self) -> bool {
        matches!(self, Self::Sheet | Self::SelectAll)
    }

    /// Whether the binding only means something once a session is on screen.
    pub fn needs_session(self) -> bool {
        matches!(self, Self::Tab(_) | Self::Settings | Self::FileFilter)
    }
}

/// A titled group of bindings.
pub struct Section {
    pub title: &'static str,
    pub bindings: &'static [Binding],
}

// Caps carry the glyph a physical key shows ("," "+" "-"), never a spelled-out
// key name; "↑"/"↓" are U+2191/U+2193, present in every shipped Plex face.
const SECTIONS: &[Section] = &[
    Section {
        title: "Anywhere",
        bindings: &[
            Binding {
                keys: &["Ctrl", "K"],
                what: "open the command palette",
                action: Action::Palette,
            },
            Binding {
                keys: &["Ctrl", "R"],
                what: "refresh now",
                action: Action::Refresh,
            },
            Binding {
                keys: &["Ctrl", ","],
                what: "open settings for the selected session",
                action: Action::Settings,
            },
            Binding {
                keys: &["?"],
                what: "show this sheet",
                action: Action::Sheet,
            },
            Binding {
                // The bar consumes F10 itself — it owns which head opens and
                // where focus lands — so the global handler must not take it.
                keys: &["F10"],
                what: "open the menu bar",
                action: Action::Convention,
            },
            Binding {
                keys: &["Esc"],
                what: "close whatever is open",
                action: Action::Convention,
            },
        ],
    },
    Section {
        title: "Moving",
        bindings: &[
            Binding {
                keys: &["Tab"],
                what: "step to the next control",
                action: Action::Convention,
            },
            Binding {
                keys: &["Shift", "Tab"],
                what: "step back",
                action: Action::Convention,
            },
            Binding {
                keys: &["↑", "↓"],
                what: "move through a list",
                action: Action::Convention,
            },
            Binding {
                keys: &["Enter"],
                what: "activate what is focused",
                action: Action::Convention,
            },
        ],
    },
    Section {
        title: "Sessions",
        bindings: &[
            Binding {
                keys: &["Ctrl", "1"],
                what: "overview",
                action: Action::Tab(0),
            },
            Binding {
                keys: &["Ctrl", "2"],
                what: "peers",
                action: Action::Tab(1),
            },
            Binding {
                keys: &["Ctrl", "3"],
                what: "files",
                action: Action::Tab(2),
            },
            Binding {
                keys: &["Ctrl", "4"],
                what: "conflicts",
                action: Action::Tab(3),
            },
            Binding {
                keys: &["Ctrl", "5"],
                what: "history",
                action: Action::Tab(4),
            },
            Binding {
                keys: &["Ctrl", "6"],
                what: "audit",
                action: Action::Tab(5),
            },
            Binding {
                keys: &["Ctrl", "7"],
                what: "settings",
                action: Action::Tab(6),
            },
            Binding {
                keys: &["Ctrl", "A"],
                what: "mark every session",
                action: Action::SelectAll,
            },
            Binding {
                keys: &["Ctrl", "click"],
                what: "add one session to the marked set",
                action: Action::Convention,
            },
            Binding {
                keys: &["Shift", "click"],
                what: "mark a range of sessions",
                action: Action::Convention,
            },
        ],
    },
    Section {
        title: "Reading",
        bindings: &[
            Binding {
                keys: &["Ctrl", "F"],
                what: "jump to the file filter",
                action: Action::FileFilter,
            },
            Binding {
                keys: &["Ctrl", "+"],
                what: "larger text",
                action: Action::TextBigger,
            },
            Binding {
                keys: &["Ctrl", "-"],
                what: "smaller text",
                action: Action::TextSmaller,
            },
            Binding {
                keys: &["Ctrl", "0"],
                what: "text back to normal",
                action: Action::TextReset,
            },
        ],
    },
];

/// Every binding the window answers to, grouped for display.
///
/// This is the single source of truth. `App::keyboard` iterates this table and
/// dispatches on [`Binding::action`], and the sheet prints the same table, so a
/// binding cannot appear in one and not the other.
pub fn sections() -> &'static [Section] {
    SECTIONS
}

/// The sheet body: sections, each binding rendered as key caps followed by a
/// dotted leader to its meaning. Width-filling; the caller owns the frame.
pub fn sheet(ui: &mut egui::Ui) {
    let all = sections();
    for (i, section) in all.iter().enumerate() {
        ui.add_space(theme::space::S);
        ui.label(
            RichText::new(section.title)
                .size(theme::sized(theme::step::LABEL))
                .family(theme::fam_semibold())
                .color(theme::ink()),
        );
        for binding in section.bindings {
            binding_row(ui, binding);
        }
        if i + 1 < all.len() {
            ornament::rule_with_diamond(ui, theme::alpha(theme::gold(), 128));
        }
    }
}

/// One ruled line: adjacent caps (a hair apart, no plus signs), then leader
/// dots to the right-aligned meaning — `controls::leader_row`'s book-index
/// look, with the caps standing in as the entry.
fn binding_row(ui: &mut egui::Ui, binding: &Binding) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = theme::space::S;
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
            theme::font(theme::step::LABEL, FontFamily::Proportional),
            theme::ink_muted(),
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
                    theme::alpha(theme::ink_faint(), 102),
                );
            }
        }
        p.galley(
            pos2(rect.right() - size.x, rect.center().y - size.y / 2.0),
            galley,
            theme::ink_muted(),
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every chord the sheet advertises, one joined string each.
    fn advertised_chords() -> Vec<String> {
        SECTIONS
            .iter()
            .flat_map(|s| s.bindings.iter().map(|b| b.keys.join("+")))
            .collect()
    }

    /// The sheet is the only place these chords are written down for the user,
    /// so a duplicate means two different meanings are being advertised for one
    /// key press.
    #[test]
    fn no_chord_is_advertised_twice() {
        let chords = advertised_chords();
        let mut seen = std::collections::BTreeSet::new();
        for c in &chords {
            assert!(seen.insert(c.clone()), "{c} is advertised more than once");
        }
        assert!(!chords.is_empty());
    }

    /// An empty cap or meaning renders as a blank row with a dotted leader
    /// going nowhere.
    #[test]
    fn every_binding_is_complete() {
        for s in sections() {
            assert!(!s.title.trim().is_empty(), "a section has no title");
            assert!(!s.bindings.is_empty(), "section {} is empty", s.title);
            for b in s.bindings {
                assert!(!b.keys.is_empty(), "a binding in {} has no keys", s.title);
                assert!(
                    b.keys.iter().all(|k| !k.trim().is_empty()),
                    "a binding in {} has a blank cap",
                    s.title
                );
                assert!(
                    !b.what.trim().is_empty(),
                    "a binding in {} has no meaning",
                    s.title
                );
            }
        }
    }

    /// Every binding the window actually answers must resolve to a chord, or
    /// the sheet advertises a key press that does nothing.
    #[test]
    fn every_handled_binding_has_a_chord() {
        for sec in sections() {
            for b in sec.bindings {
                if b.action == Action::Convention {
                    assert!(
                        b.action.chord().is_none(),
                        "{:?} is a convention but claims a chord",
                        b.keys
                    );
                } else {
                    assert!(
                        b.action.chord().is_some(),
                        "{:?} is handled but has no chord",
                        b.keys
                    );
                }
            }
        }
    }

    /// Two bindings resolving to one chord means the second is unreachable —
    /// `consume_key` hands the press to whichever is matched first.
    #[test]
    fn no_two_bindings_claim_the_same_chord() {
        let mut seen: Vec<(egui::Modifiers, egui::Key)> = Vec::new();
        for sec in sections() {
            for b in sec.bindings {
                let Some((mods, keys)) = b.action.chord() else {
                    continue;
                };
                for k in keys {
                    assert!(
                        !seen.contains(&(mods, *k)),
                        "{:?} re-uses a chord already claimed",
                        b.keys
                    );
                    seen.push((mods, *k));
                }
            }
        }
    }

    /// Every action the window can take must be advertised. An action missing
    /// from the table is a key the user can press and never discover.
    #[test]
    fn every_action_is_advertised() {
        let listed: Vec<Action> = sections()
            .iter()
            .flat_map(|s| s.bindings.iter().map(|b| b.action))
            .collect();
        let expected = [
            Action::Palette,
            Action::Refresh,
            Action::Settings,
            Action::Sheet,
            Action::SelectAll,
            Action::FileFilter,
            Action::TextBigger,
            Action::TextSmaller,
            Action::TextReset,
            Action::Tab(0),
            Action::Tab(1),
            Action::Tab(2),
            Action::Tab(3),
            Action::Tab(4),
            Action::Tab(5),
            Action::Tab(6),
        ];
        for a in expected {
            assert!(listed.contains(&a), "{a:?} is handled but not on the sheet");
        }
    }

    /// The tab chords must address real tabs, in order.
    #[test]
    fn tab_actions_are_contiguous_from_zero() {
        let mut tabs: Vec<usize> = sections()
            .iter()
            .flat_map(|s| s.bindings.iter())
            .filter_map(|b| match b.action {
                Action::Tab(n) => Some(n),
                _ => None,
            })
            .collect();
        tabs.sort_unstable();
        assert_eq!(tabs, (0..tabs.len()).collect::<Vec<_>>());
    }

    /// A chord that fires while a field has the keyboard must be one that
    /// cannot be a character the user is typing.
    #[test]
    fn typing_guards_cover_the_bare_keys() {
        assert!(Action::Sheet.yields_to_typing());
        assert!(Action::SelectAll.yields_to_typing());
        assert!(!Action::Palette.yields_to_typing());
    }

    /// The project's voice for these: lower case, no trailing period.
    #[test]
    fn meanings_keep_the_house_voice() {
        for s in sections() {
            for b in s.bindings {
                assert!(!b.what.ends_with('.'), "{:?} ends with a period", b.what);
                let first = b.what.chars().next().unwrap_or('a');
                assert!(!first.is_uppercase(), "{:?} starts upper case", b.what);
            }
        }
    }
}
