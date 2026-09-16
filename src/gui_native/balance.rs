//! The conflict scales (الميزان): a painter-drawn balance for the app's most
//! delicate moment — tazamun preserved a copy rather than overwrite it, and
//! the user must weigh which bytes live. A beam on a khatam fulcrum tilts
//! toward the heavier side (log scale, so a 10x gap leans rather than slams),
//! a pan hangs from each end naming its weight, and a card beneath each pan
//! carries what that side is. Pure presentation over `theme` and `ornament` —
//! zero I/O, deterministic, total for degenerate inputs.

use eframe::egui;
use egui::{Color32, CornerRadius, FontFamily, Rect, Stroke, StrokeKind};
use egui::{pos2, vec2};

use super::{a11y, focusnav, ornament, theme};

/// What the figure is called when it is read out rather than seen.
const LEAD: &str = "scales weighing two copies";
/// The verdict when neither pan sinks. The beam is level, which a reader
/// cannot see, and "the same" is the answer that matters: no amount of bytes
/// decides this one.
const LEVEL: &str = "both sides weigh the same";

/// One pan of the scales: what it holds and how it should read.
pub struct Side<'a> {
    /// Short label above the pan, e.g. "preserved copy".
    pub title: &'a str,
    /// Bytes this side holds; drives the tilt and the weight caption.
    pub size: u64,
    /// Human size string already formatted by the caller, e.g. "17 B".
    pub size_text: &'a str,
    /// When this side came to be, already formatted, e.g. "kept 2026-07-17 20:08".
    pub when: &'a str,
    /// One short clause of context, e.g. the quarantine reason.
    pub note: &'a str,
    /// The side's accent — the custody colour of what this pan holds, at full
    /// strength (the card fades it where it needs to).
    pub accent: egui::Color32,
}

/// Draws the scales: a beam on a khatam fulcrum tilting toward the heavier
/// side, a hanging pan under each end, and a card beneath each pan carrying
/// its title, size, time, and note. Fills the available width; ~190px tall.
pub fn scales(ui: &mut egui::Ui, left: Side<'_>, right: Side<'_>) {
    let w = ui.available_width().max(280.0);
    // The tilt *is* the verdict, and a tilt is unreadable without eyes. The
    // figure takes a stop on the Tab route of its own so the weighing can be
    // heard before the three resolutions beneath it are chosen between.
    let (rect, resp) =
        ui.allocate_exact_size(vec2(w, 190.0), egui::Sense::focusable_noninteractive());
    a11y::describe(&resp, &scales_sentence(&left, &right));
    if !rect.is_finite() || !rect.is_positive() {
        return;
    }
    let p = ui.painter_at(rect);

    let fulcrum = pos2(rect.center().x, rect.top() + 46.0);
    let half = (w * 0.5 - 70.0).clamp(60.0, 200.0);
    let lean = beam_tilt(left.size, right.size);
    // Screen y grows downward, so the heavier side takes +dy and sinks. The
    // dip is capped so a steep lean on a long beam never drives the pans and
    // their captions into the cards below.
    let dy = (half * lean.sin()).clamp(-9.0, 9.0);
    let dx = half * lean.cos();
    let l_end = pos2(fulcrum.x - dx, fulcrum.y + dy);
    let r_end = pos2(fulcrum.x + dx, fulcrum.y - dy);

    let stand = Stroke::new(1.5, theme::alpha(theme::ink_faint(), 128));
    let foot_y = rect.top() + 78.0;
    p.line_segment([fulcrum, pos2(fulcrum.x, foot_y)], stand);
    p.line_segment(
        [
            pos2(fulcrum.x - 12.0, foot_y),
            pos2(fulcrum.x + 12.0, foot_y),
        ],
        stand,
    );

    p.line_segment(
        [l_end, r_end],
        Stroke::new(theme::RULE_ACCENT_W, theme::alpha(theme::gold(), 191)),
    );
    ornament::khatam(&p, fulcrum, 9.0, theme::gold(), true);

    pan(&p, l_end, left.accent, left.size_text);
    pan(&p, r_end, right.accent, right.size_text);

    // True-vertical plumb so the tilt reads; starts just below the khatam.
    p.line_segment(
        [
            pos2(fulcrum.x, fulcrum.y + 10.0),
            pos2(fulcrum.x, rect.top() + 92.0),
        ],
        Stroke::new(theme::RULE_W, theme::alpha(theme::ink_faint(), 64)),
    );

    let cw = (w - 30.0) * 0.5;
    let top = rect.top() + 96.0;
    side_card(
        &p,
        Rect::from_min_max(
            pos2(rect.left() + 10.0, top),
            pos2(rect.left() + 10.0 + cw, rect.bottom()),
        ),
        &left,
    );
    side_card(
        &p,
        Rect::from_min_max(
            pos2(rect.right() - 10.0 - cw, top),
            pos2(rect.right() - 10.0, rect.bottom()),
        ),
        &right,
    );

    focusnav::ring(ui, &resp);
}

/// The sentence the scales announce: both pans, then which one sank. The tilt
/// carries the verdict on screen and carries nothing at all to a reader, so the
/// verdict is stated — this is the figure behind the one decision in the app
/// that can cost the user bytes.
fn scales_sentence(left: &Side<'_>, right: &Side<'_>) -> String {
    let verdict = match left.size.cmp(&right.size) {
        std::cmp::Ordering::Greater => format!("{} is heavier", left.title),
        std::cmp::Ordering::Less => format!("{} is heavier", right.title),
        std::cmp::Ordering::Equal => LEVEL.to_owned(),
    };
    // Semicolons, not commas: each pan is already a comma-separated clause, and
    // the two must not run together into one list of six things.
    let body = [side_phrase(left), side_phrase(right), verdict]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("; ");
    format!("{LEAD}: {body}")
}

/// One pan in words: what it is, what it weighs, when it came to be, and the
/// clause of context under it — the card's lines, in the card's order, with the
/// unit spelled out. A missing weight (the synced version of a file that is no
/// longer in the folder) drops out rather than being announced as nothing.
fn side_phrase(side: &Side<'_>) -> String {
    a11y::clause(&[
        side.title.to_owned(),
        a11y::spoken_size(side.size_text),
        side.when.to_owned(),
        side.note.to_owned(),
    ])
}

/// Beam angle in radians, positive when the left side is heavier. Log scale
/// so magnitude differences lean the beam instead of slamming it; equal sizes
/// give a level beam.
fn beam_tilt(left: u64, right: u64) -> f32 {
    let ratio = ((left.max(1) as f32).ln() - (right.max(1) as f32).ln()) / 6.0;
    let tilt = ratio.clamp(-1.0, 1.0) * 0.20;
    if tilt.is_finite() { tilt } else { 0.0 }
}

/// A hanger dropping from the beam end, a shallow bowed pan, and the weight
/// caption centred beneath it.
fn pan(p: &egui::Painter, end: egui::Pos2, accent: Color32, size_text: &str) {
    let hang = pos2(end.x, end.y + 16.0);
    p.line_segment(
        [end, hang],
        Stroke::new(theme::RULE_W, theme::alpha(accent, 140)),
    );
    let points: Vec<egui::Pos2> = (0..=14)
        .map(|k| {
            let x = -22.0 + k as f32 * (44.0 / 14.0);
            let t = x / 22.0;
            pos2(hang.x + x, hang.y + 9.0 * (1.0 - t * t))
        })
        .collect();
    p.add(egui::Shape::line(points, Stroke::new(1.5, accent)));
    let galley = p.layout_no_wrap(
        size_text.to_owned(),
        theme::font(theme::step::META, theme::fam_mono()),
        accent,
    );
    let gw = galley.size().x;
    p.galley(pos2(hang.x - gw * 0.5, hang.y + 12.0), galley, accent);
}

/// The card under one pan: page fill, hairline stroke, an accent filament down
/// the left spine, then title / size-and-when / wrapped note, all painted and
/// clipped to the card so nothing steals layout or bleeds.
fn side_card(p: &egui::Painter, rect: Rect, side: &Side<'_>) {
    if !rect.is_finite() || !rect.is_positive() {
        return;
    }
    p.rect_filled(rect, CornerRadius::same(theme::R_NONE), theme::bg_page());
    p.rect_stroke(
        rect,
        CornerRadius::same(theme::R_NONE),
        Stroke::new(theme::RULE_W, theme::rule_hair()),
        StrokeKind::Inside,
    );
    let bar_h = (rect.height() - 16.0).max(0.0);
    let bar = Rect::from_center_size(pos2(rect.left() + 1.0, rect.center().y), vec2(2.0, bar_h));
    p.rect_filled(bar, 1.0, side.accent);

    let pc = p.with_clip_rect(rect);
    let x = rect.left() + 12.0;
    let mut y = rect.top() + 10.0;

    let title = pc.layout_no_wrap(
        side.title.to_owned(),
        theme::font(theme::step::LABEL, theme::fam_medium()),
        side.accent,
    );
    let title_h = title.size().y;
    pc.galley(pos2(x, y), title, side.accent);
    y += title_h.max(14.0) + 3.0;

    let size_color = theme::alpha(side.accent, 217);
    let size_g = pc.layout_no_wrap(
        side.size_text.to_owned(),
        theme::font(theme::step::META, theme::fam_mono()),
        size_color,
    );
    let when_g = pc.layout_no_wrap(
        side.when.to_owned(),
        theme::font(theme::step::META, FontFamily::Proportional),
        theme::ink_faint(),
    );
    let line_h = size_g.size().y.max(when_g.size().y).max(12.0);
    let mut mx = x;
    if !side.size_text.is_empty() {
        mx += size_g.size().x + 7.0;
        pc.galley(pos2(x, y), size_g, size_color);
        if !side.when.is_empty() {
            ornament::diamond(
                &pc,
                pos2(mx, y + line_h * 0.5),
                1.8,
                theme::alpha(theme::gold(), 128),
            );
            mx += 9.0;
        }
    }
    pc.galley(pos2(mx, y), when_g, theme::ink_faint());
    y += line_h + 6.0;

    let wrap = (rect.width() - 24.0).max(10.0);
    let note = pc.layout(
        side.note.to_owned(),
        theme::font(theme::step::META, FontFamily::Proportional),
        theme::ink_muted(),
        wrap,
    );
    pc.galley(pos2(x, y), note, theme::ink_muted());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kept(size: u64, size_text: &'static str) -> Side<'static> {
        Side {
            title: "preserved copy",
            size,
            size_text,
            when: "kept 2026-07-17 20:08",
            note: "edited while offline",
            accent: Color32::WHITE,
        }
    }

    fn live(size: u64, size_text: &'static str) -> Side<'static> {
        Side {
            title: "synced version",
            size,
            size_text,
            when: "in the folder now",
            note: "this is what your peers hold today",
            accent: Color32::WHITE,
        }
    }

    #[test]
    fn sentence_reads_both_pans_and_the_verdict() {
        assert_eq!(
            scales_sentence(&kept(17, "17 B"), &live(1536, "1.5 KB")),
            "scales weighing two copies: \
             preserved copy, 17 bytes, kept 2026-07-17 20:08, edited while offline; \
             synced version, 1.5 kilobytes, in the folder now, this is what your peers hold today; \
             synced version is heavier"
        );
    }

    #[test]
    fn sentence_names_the_heavier_side_either_way() {
        let heavier_left = scales_sentence(&kept(4096, "4.0 KB"), &live(17, "17 B"));
        assert!(
            heavier_left.ends_with("preserved copy is heavier"),
            "{heavier_left}"
        );
        let heavier_right = scales_sentence(&kept(17, "17 B"), &live(4096, "4.0 KB"));
        assert!(
            heavier_right.ends_with("synced version is heavier"),
            "{heavier_right}"
        );
    }

    #[test]
    fn equal_weights_announce_a_level_beam() {
        // A level beam is the one state a tilt cannot show a reader at all.
        let level = scales_sentence(&kept(17, "17 B"), &live(17, "17 B"));
        assert!(level.ends_with(LEVEL), "{level}");
        // Zero on both sides is level too, not "the left one is heavier".
        let nothing = scales_sentence(&kept(0, ""), &live(0, ""));
        assert!(nothing.ends_with(LEVEL), "{nothing}");
    }

    #[test]
    fn a_missing_side_drops_its_weight_but_keeps_its_card() {
        // The synced version of a file that is no longer in the folder: no size
        // text, and the note carries the truth.
        let mut gone = live(0, "");
        gone.when = "";
        gone.note = "nothing is in the folder under this name";
        let said = scales_sentence(&kept(17, "17 B"), &gone);
        assert!(
            said.contains("synced version, nothing is in the folder under this name;"),
            "{said}"
        );
        assert!(said.ends_with("preserved copy is heavier"), "{said}");
    }

    #[test]
    fn sentence_survives_a_side_with_nothing_on_it() {
        let blank = Side {
            title: "",
            size: 0,
            size_text: "",
            when: "",
            note: "",
            accent: Color32::WHITE,
        };
        // Degenerate, but it must still be one sentence and never a run of
        // punctuation with nothing between it.
        assert_eq!(
            scales_sentence(&blank, &blank),
            "scales weighing two copies: both sides weigh the same"
        );
    }

    #[test]
    fn the_beam_never_leans_against_the_spoken_verdict() {
        // The verdict follows the bytes, because the bytes are what the user is
        // about to lose. The beam is coarser — it floors both sides at one byte
        // before taking logs, so 0 against 1 draws level — but it must never
        // lean one way while the sentence names the other.
        for (l, r) in [(17u64, 1536u64), (1536, 17), (17, 17), (0, 0), (0, 1)] {
            let tilt = beam_tilt(l, r);
            let said = scales_sentence(&kept(l, "x"), &live(r, "y"));
            match l.cmp(&r) {
                std::cmp::Ordering::Greater => {
                    assert!(
                        said.ends_with("preserved copy is heavier"),
                        "{l}/{r}: {said}"
                    );
                    assert!(tilt >= 0.0, "{l}/{r}: beam leans against the verdict");
                }
                std::cmp::Ordering::Less => {
                    assert!(
                        said.ends_with("synced version is heavier"),
                        "{l}/{r}: {said}"
                    );
                    assert!(tilt <= 0.0, "{l}/{r}: beam leans against the verdict");
                }
                std::cmp::Ordering::Equal => {
                    assert!(said.ends_with(LEVEL), "{l}/{r}: {said}");
                    assert!(tilt.abs() <= f32::EPSILON, "{l}/{r}: beam is not level");
                }
            }
        }
    }
}
