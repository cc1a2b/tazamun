//! The conflict scales (الميزان): a painter-drawn balance for the app's most
//! delicate moment — tazamun preserved a copy rather than overwrite it, and
//! the user must weigh which bytes live. A beam on a khatam fulcrum tilts
//! toward the heavier side (log scale, so a 10x gap leans rather than slams),
//! a pan hangs from each end naming its weight, and a card beneath each pan
//! carries what that side is. Pure presentation over `theme` and `ornament` —
//! zero I/O, deterministic, total for degenerate inputs.

use std::sync::Arc;

use eframe::egui;
use egui::text::Galley;
use egui::{Color32, CornerRadius, FontFamily, Rect, Stroke, StrokeKind};
use egui::{pos2, vec2};

use super::{a11y, focusnav, ornament, register, theme};

/// The apparatus, in the proportions a balance is drawn in. None of these is a
/// measure of type — they are where the beam pivots, how far it may dip, and
/// how the pan hangs off it — so they hold at every text scale. What the type
/// costs is measured, in [`apparatus_h`] and [`card_h`].
const PIVOT_Y: f32 = 46.0;
const MAX_DIP: f32 = 9.0;
const HANGER: f32 = 16.0;
const PAN_BOWL: f32 = 9.0;
const PAN_HALF: f32 = 22.0;
const STAND_H: f32 = 32.0;
/// How far the beam's end is held back from the figure's edge. A margin, but
/// never less than the pan and the weight written under it actually need.
const BEAM_INSET: f32 = 70.0;
/// Shortest figure the scales are drawn into at all.
const MIN_W: f32 = 280.0;

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
/// its title, size, time, and note. Fills the available width; as tall as the
/// apparatus plus the taller of the two cards under it.
pub fn scales(ui: &mut egui::Ui, left: Side<'_>, right: Side<'_>) {
    let w = ui.available_width().max(MIN_W);
    let cw = (w - theme::space::L * 3.0) * 0.5;

    // Everything is set before anything is claimed. The cards carry four lines
    // of type, one of them wrapped, and the figure cannot know how tall it is
    // until they have been laid out — which is exactly what a 190px figure with
    // the cards starting at 96 was guessing at, and getting wrong by a whole
    // line at 115%.
    let painter = ui.painter().clone();
    let laid = [
        Laid::of(&painter, &left, cw),
        Laid::of(&painter, &right, cw),
    ];
    let cards_h = laid[0].height().max(laid[1].height());
    let caption_w = laid[0].weight.size().x.max(laid[1].weight.size().x);
    let apparatus = apparatus_h(register::line_h(theme::step::META));

    // The tilt *is* the verdict, and a tilt is unreadable without eyes. The
    // figure takes a stop on the Tab route of its own so the weighing can be
    // heard before the three resolutions beneath it are chosen between.
    let (rect, resp) = ui.allocate_exact_size(
        vec2(w, apparatus + cards_h),
        egui::Sense::focusable_noninteractive(),
    );
    a11y::describe(&resp, &scales_sentence(&left, &right));
    if !rect.is_finite() || !rect.is_positive() {
        return;
    }
    let p = ui.painter_at(rect);

    let fulcrum = pos2(rect.center().x, rect.top() + PIVOT_Y);
    let half = (w * 0.5 - beam_inset(caption_w)).clamp(60.0, 200.0);
    let lean = beam_tilt(left.size, right.size);
    // Screen y grows downward, so the heavier side takes +dy and sinks. The
    // dip is capped so a steep lean on a long beam never drives the pans and
    // their captions into the cards below.
    let dy = (half * lean.sin()).clamp(-MAX_DIP, MAX_DIP);
    let dx = half * lean.cos();
    let l_end = pos2(fulcrum.x - dx, fulcrum.y + dy);
    let r_end = pos2(fulcrum.x + dx, fulcrum.y - dy);

    let stand = Stroke::new(1.5, theme::alpha(theme::ink_faint(), 128));
    let foot_y = fulcrum.y + STAND_H;
    p.line_segment([fulcrum, pos2(fulcrum.x, foot_y)], stand);
    p.line_segment(
        [
            pos2(fulcrum.x - theme::space::L, foot_y),
            pos2(fulcrum.x + theme::space::L, foot_y),
        ],
        stand,
    );

    p.line_segment(
        [l_end, r_end],
        Stroke::new(theme::RULE_ACCENT_W, theme::alpha(theme::gold(), 191)),
    );
    ornament::khatam(&p, fulcrum, MAX_DIP, theme::gold(), true);

    let cards_top = rect.top() + apparatus;
    pan(&p, l_end, left.accent, laid[0].weight.clone());
    pan(&p, r_end, right.accent, laid[1].weight.clone());

    // True-vertical plumb so the tilt reads; starts just below the khatam and
    // stops short of the cards rather than at a station of its own.
    p.line_segment(
        [
            pos2(fulcrum.x, fulcrum.y + theme::space::L),
            pos2(fulcrum.x, cards_top - theme::space::L),
        ],
        Stroke::new(theme::RULE_W, theme::alpha(theme::ink_faint(), 64)),
    );

    side_card(
        &p,
        Rect::from_min_max(
            pos2(rect.left() + theme::space::L, cards_top),
            pos2(rect.left() + theme::space::L + cw, rect.bottom()),
        ),
        &left,
        &laid[0],
    );
    side_card(
        &p,
        Rect::from_min_max(
            pos2(rect.right() - theme::space::L - cw, cards_top),
            pos2(rect.right() - theme::space::L, rect.bottom()),
        ),
        &right,
        &laid[1],
    );

    focusnav::ring(ui, &resp);
}

/// One side's four lines, already set into the card's measure. The figure is
/// sized from these and then drawn from the same ones, so what was measured and
/// what is painted cannot drift apart.
struct Laid {
    title: Arc<Galley>,
    size: Arc<Galley>,
    when: Arc<Galley>,
    note: Arc<Galley>,
    weight: Arc<Galley>,
}

impl Laid {
    fn of(p: &egui::Painter, side: &Side<'_>, card_w: f32) -> Self {
        let wrap = (card_w - theme::space::L * 2.0).max(theme::space::L);
        Self {
            title: p.layout_no_wrap(
                side.title.to_owned(),
                theme::font(theme::step::LABEL, theme::fam_medium()),
                side.accent,
            ),
            size: p.layout_no_wrap(
                side.size_text.to_owned(),
                theme::font(theme::step::META, theme::fam_mono()),
                theme::alpha(side.accent, 217),
            ),
            when: p.layout_no_wrap(
                side.when.to_owned(),
                theme::font(theme::step::META, FontFamily::Proportional),
                theme::ink_faint(),
            ),
            note: p.layout(
                side.note.to_owned(),
                theme::font(theme::step::META, FontFamily::Proportional),
                theme::ink_muted(),
                wrap,
            ),
            weight: p.layout_no_wrap(
                side.size_text.to_owned(),
                theme::font(theme::step::META, theme::fam_mono()),
                side.accent,
            ),
        }
    }

    /// The card this side needs, from the lines it actually holds.
    fn height(&self) -> f32 {
        card_h(
            self.title.size().y,
            self.size.size().y.max(self.when.size().y),
            self.note.size().y,
        )
    }
}

/// Pure: the card's own arithmetic — top air, the title and its gap, the
/// size-and-when line and its gap, the wrapped note, bottom air. The one place
/// the card's height is written down, used both to rule the figure and to walk
/// down the card while drawing it.
fn card_h(title_h: f32, meta_h: f32, note_h: f32) -> f32 {
    theme::space::M
        + title_h
        + theme::space::S
        + meta_h
        + theme::space::S
        + note_h
        + theme::space::M
}

/// Pure: everything above the cards. The apparatus itself is fixed geometry,
/// but the weight written under the sinking pan is type — it grows with the
/// text scale while the beam does not, and it was the first thing to land on
/// the cards below.
fn apparatus_h(caption_h: f32) -> f32 {
    let lowest_ink = PIVOT_Y + MAX_DIP + HANGER + PAN_BOWL + theme::space::S + caption_h;
    lowest_ink.max(PIVOT_Y + STAND_H) + theme::space::M
}

/// Pure: how far the beam's end is held back from the figure's edge. The margin
/// the figure is drawn to, unless the pan or the weight under it needs more.
fn beam_inset(caption_w: f32) -> f32 {
    let needs = PAN_HALF.max(caption_w * 0.5) + theme::space::M;
    BEAM_INSET.max(needs)
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
fn pan(p: &egui::Painter, end: egui::Pos2, accent: Color32, weight: Arc<Galley>) {
    let hang = pos2(end.x, end.y + HANGER);
    p.line_segment(
        [end, hang],
        Stroke::new(theme::RULE_W, theme::alpha(accent, 140)),
    );
    let points: Vec<egui::Pos2> = (0..=14)
        .map(|k| {
            let x = -PAN_HALF + k as f32 * (PAN_HALF * 2.0 / 14.0);
            let t = x / PAN_HALF;
            pos2(hang.x + x, hang.y + PAN_BOWL * (1.0 - t * t))
        })
        .collect();
    p.add(egui::Shape::line(points, Stroke::new(1.5, accent)));
    // Clear of the bowl it hangs under, from the bowl's own depth.
    let gw = weight.size().x;
    p.galley(
        pos2(hang.x - gw * 0.5, hang.y + PAN_BOWL + theme::space::S),
        weight,
        accent,
    );
}

/// The card under one pan: page fill, hairline stroke, an accent filament down
/// the left spine, then title / size-and-when / wrapped note, all painted and
/// clipped to the card so nothing steals layout or bleeds.
fn side_card(p: &egui::Painter, rect: Rect, side: &Side<'_>, laid: &Laid) {
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
    let bar_h = (rect.height() - theme::space::XL).max(0.0);
    let bar = Rect::from_center_size(
        pos2(rect.left() + theme::RULE_ACCENT_W * 0.5, rect.center().y),
        vec2(theme::RULE_ACCENT_W, bar_h),
    );
    p.rect_filled(bar, theme::R_CHIP, side.accent);

    let pc = p.with_clip_rect(rect);
    let x = rect.left() + theme::space::L;
    let mut y = rect.top() + theme::space::M;

    let size_color = theme::alpha(side.accent, 217);
    let meta_h = laid.size.size().y.max(laid.when.size().y);

    // The same walk `card_h` adds up, in the same order, so the card cannot be
    // taller than the figure ruled for it.
    pc.galley(pos2(x, y), laid.title.clone(), side.accent);
    y += laid.title.size().y + theme::space::S;

    let mut mx = x;
    if !side.size_text.is_empty() {
        mx += laid.size.size().x + theme::space::M;
        pc.galley(pos2(x, y), laid.size.clone(), size_color);
        if !side.when.is_empty() {
            ornament::diamond(
                &pc,
                pos2(mx, y + meta_h * 0.5),
                1.8,
                theme::alpha(theme::gold(), 128),
            );
            mx += theme::space::M;
        }
    }
    pc.galley(pos2(mx, y), laid.when.clone(), theme::ink_faint());
    y += meta_h + theme::space::S;

    pc.galley(pos2(x, y), laid.note.clone(), theme::ink_muted());
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

    // ── the figure's own measure ──

    /// Every text scale the control can actually reach.
    fn scales() -> impl Iterator<Item = f32> {
        (14..=44).map(|k| k as f32 / 20.0)
    }

    /// What `register::line_h` returns, as arithmetic. A quarantine reason is
    /// ordinary Arabic in this window, and 1.5em is what that lays out to.
    fn line_box(step: f32, scale: f32) -> f32 {
        step * scale * 1.5
    }

    /// The defect the user photographed, as arithmetic: the cards used to start
    /// at a fixed 96 from the top, and the weight caption under the sinking pan
    /// reached past that before the text scale even got to 100%.
    #[test]
    fn the_cards_start_below_the_lowest_ink_the_apparatus_puts_down() {
        for scale in scales() {
            let caption = line_box(theme::step::META, scale);
            let top = apparatus_h(caption);
            let caption_bottom = PIVOT_Y + MAX_DIP + HANGER + PAN_BOWL + theme::space::S + caption;
            assert!(
                top > caption_bottom,
                "cards start at {top}, the weight ends at {caption_bottom}, at {scale}"
            );
            assert!(
                top > PIVOT_Y + STAND_H,
                "cards start inside the stand at {scale}"
            );
        }
    }

    #[test]
    fn the_apparatus_grows_with_the_text_scale() {
        let small = apparatus_h(line_box(theme::step::META, 0.7));
        let large = apparatus_h(line_box(theme::step::META, 2.2));
        assert!(large > small, "{small} to {large} is not a text scale");
    }

    /// A card is as tall as the lines in it, whether the note came out as one
    /// line or as six.
    #[test]
    fn a_card_is_never_shorter_than_the_lines_in_it() {
        for scale in scales() {
            let title = line_box(theme::step::LABEL, scale);
            let meta = line_box(theme::step::META, scale);
            for note_lines in 1..=6 {
                let note = meta * note_lines as f32;
                let h = card_h(title, meta, note);
                assert!(
                    h > title + meta + note,
                    "{note_lines} note lines at {scale}: {h} holds {}",
                    title + meta + note
                );
            }
        }
        assert!(
            card_h(18.0, 16.0, 16.0 * 4.0) > card_h(18.0, 16.0, 16.0),
            "a wrapped note did not make the card taller"
        );
    }

    /// The whole figure: apparatus plus card, never smaller than either part,
    /// and never flat as the text grows.
    #[test]
    fn the_figure_grows_with_everything_in_it() {
        let mut prev: Option<(f32, f32)> = None;
        for scale in scales() {
            let title = line_box(theme::step::LABEL, scale);
            let meta = line_box(theme::step::META, scale);
            let h = apparatus_h(meta) + card_h(title, meta, meta * 2.0);
            if let Some((was, before)) = prev {
                assert!(h > before, "figure flat from {was} to {scale}");
            }
            prev = Some((scale, h));
        }
    }

    /// The beam is held back by the margin the figure is drawn to — unless the
    /// pan, or the weight written under it, needs more than the margin left.
    #[test]
    fn the_beam_yields_to_the_weight_written_under_it() {
        assert_eq!(beam_inset(0.0), BEAM_INSET, "a short weight moves nothing");
        assert_eq!(
            beam_inset(PAN_HALF),
            BEAM_INSET,
            "a weight narrower than the pan moves nothing"
        );
        // A long size at a large text scale: the caption is wider than the
        // margin, and the beam has to come in or the weight runs off the edge.
        let wide = BEAM_INSET * 3.0;
        assert!(
            beam_inset(wide) >= wide * 0.5,
            "a {wide} caption is drawn past the figure's edge"
        );
    }

    #[test]
    fn the_beam_inset_survives_a_caption_it_cannot_measure() {
        assert_eq!(beam_inset(-10.0), BEAM_INSET);
        assert_eq!(beam_inset(0.0), BEAM_INSET);
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
