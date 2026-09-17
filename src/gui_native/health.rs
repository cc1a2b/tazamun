//! Peer-health marks for the Peers view, drawn in the register's hand: a khatam
//! seal struck to the strength of the link, an RTT plot ruled like a ledger
//! column, and the two transfer rates entered above and below a line.
//!
//! Same discipline as `ornament` — every mark here is the house khatam or the
//! house diamond at hairline weight; this module defines no geometry of its own
//! and no glyphs. Pure presentation over `theme`: zero I/O, deterministic, total
//! for degenerate inputs, and correct in all three palettes (no
//! `linear_multiply`, which fades by darkening and so cannot tint warm stock).

use eframe::egui;

use super::{a11y, ornament, theme};

/// The struck pip and the struck inner square, as fractions of the seal's
/// radius. Ink coverage rises with the grade, so the mark ranks at a glance
/// instead of asking to be counted.
const PIP: f32 = 0.22;
const LOZENGE: f32 = 0.55;

/// The diamond that stamps the newest reading. Three quarters of the baseline
/// step, so the plot band is still a plot at the 24px height the Peers view
/// asks for.
const HEAD_R: f32 = theme::space::S * 0.75;

/// Interior columns the plot is ruled into — a reading every quarter of the
/// retained history.
const COLUMNS: usize = 4;

/// The em-dash [`super::telemetry::fmt_rate`] returns for an idle link.
const NO_RATE: &str = "—";

/// The house grades, indexed by the `lit` count [`super::telemetry::grade_lit`]
/// produces. Index 0 is the unstruck seal, which that mapping gives to
/// "Offline" and to any grade it does not recognise alike — an unstruck seal
/// means no measured link, and that is all it can honestly be read as.
const GRADES: [&str; 4] = ["offline", "poor", "fair", "good"];
/// What the seal, the plot and the rate marks are called when they are read
/// out rather than seen.
const SIGNAL_LEAD: &str = "link";
const TREND_LEAD: &str = "round-trip history";
/// The plot is drawn against each peer's own min and max, never against a
/// fixed scale, so the reading is a position in that peer's own span and has
/// to be announced as one — anything else would invent milliseconds.
const TREND_NONE: &str = "no readings yet";
const TREND_ONE: &str = "one reading so far";
const RISING: &str = "rising";
const FALLING: &str = "falling";
const STEADY: &str = "steady";
/// How far the second half of the trail must sit from the first, on the
/// normalized scale, before the plot is called anything but steady.
const TREND_SWING: f32 = 0.1;
const SENDING: &str = "sending";
const RECEIVING: &str = "receiving";
/// An idle direction. "Nothing" rather than "zero": the mark is an absence,
/// which is what the em-dash says on screen.
const RATE_IDLE: &str = "nothing";

/// Link-grade seal: the eight-point khatam, struck harder the better the link.
/// `lit` 0..=3 (from `telemetry::grade_lit`) reads as an empty seal matrix, a
/// struck pip, a struck lozenge, then the whole seal filled in `color`.
/// Allocates a square cell off the type scale, so it grows with the text size
/// control.
pub fn signal_arcs(ui: &mut egui::Ui, lit: u8, color: egui::Color32) {
    let side = seal_side();
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::hover());
    // Named, not focusable: the seal sits in a peer row beside that peer's
    // name, and three extra Tab stops per peer would cost a keyboard user more
    // than the mark is worth.
    a11y::describe(&resp, &signal_sentence(lit));
    if !rect.is_finite() || !rect.is_positive() {
        return;
    }
    let center = rect.center();
    let radius = side * 0.5 - theme::RULE_W;
    if radius <= 0.0 {
        return;
    }
    let p = ui.painter();
    match lit.min(3) {
        // The matrix keeps the mark's silhouette constant across grades, so a
        // row never shifts sideways as a link rises or falls.
        0 => ornament::khatam(p, center, radius, theme::ink_faint(), false),
        1 => {
            ornament::khatam(p, center, radius, theme::ink_faint(), false);
            ornament::diamond(p, center, radius * PIP, color);
        }
        2 => {
            ornament::khatam(p, center, radius, theme::ink_faint(), false);
            ornament::diamond(p, center, radius * LOZENGE, color);
        }
        _ => {
            ornament::khatam(p, center, radius, color, true);
            ornament::diamond(p, center, radius * PIP, color);
        }
    }
}

/// RTT history as a ruled ledger column: a hairline cell with a quarter grid,
/// the trace hatched down to the baseline, and the newest reading stamped with
/// the house diamond. `samples` are pre-normalized 0..=1, oldest first; under
/// two samples the cell is ruled empty and marked nil. Allocates exactly `size`.
pub fn sparkline(ui: &mut egui::Ui, samples: &[f32], size: egui::Vec2, color: egui::Color32) {
    let size = if size.is_finite() {
        size.max(egui::Vec2::ZERO)
    } else {
        egui::Vec2::ZERO
    };
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::hover());
    a11y::describe(&resp, &trend_sentence(samples));
    if !rect.is_finite() || !rect.is_positive() {
        return;
    }
    let frame = rect.shrink(theme::space::XS);
    if !frame.is_positive() {
        return;
    }
    let p = ui.painter();
    rule_cell(p, frame);
    if samples.len() < 2 {
        ornament::diamond(p, frame.center(), theme::space::XS, theme::ink_faint());
        return;
    }
    // The head diamond hangs half over the value it marks, so the band that
    // carries values is inset by its radius top and bottom.
    let band = frame.shrink2(egui::vec2(0.0, HEAD_R));
    if !band.is_positive() {
        return;
    }
    let step = band.width() / (samples.len() - 1) as f32;
    let points: Vec<egui::Pos2> = samples
        .iter()
        .enumerate()
        .map(|(i, &s)| {
            egui::pos2(
                band.left() + step * i as f32,
                band.bottom() - level(s) * band.height(),
            )
        })
        .collect();
    let hatch = egui::Stroke::new(theme::RULE_W, theme::wash::of(color, theme::wash::TINT));
    for pt in &points {
        p.vline(pt.x, egui::Rangef::new(pt.y, frame.bottom()), hatch);
    }
    let newest = points.last().copied();
    p.add(egui::Shape::line(
        points,
        egui::Stroke::new(theme::RULE_W, color),
    ));
    if let Some(head) = newest {
        // A plumb line ties the reading to the baseline; the register's answer
        // to a glowing head dot.
        p.vline(
            head.x,
            egui::Rangef::new(head.y, frame.bottom()),
            egui::Stroke::new(theme::RULE_W, theme::wash::of(color, theme::wash::SELECT)),
        );
        ornament::diamond(p, head, HEAD_R, color);
    }
}

/// The two transfer rates entered against a line, ledger fashion: what this
/// device sends is stamped above it in the custody-self colour, what a peer
/// sends is stamped below it in the custody-peer colour. Values in mono at the
/// data step, so a column of peers aligns.
pub fn rate_arrows(ui: &mut egui::Ui, up: &str, down: &str) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = theme::space::S;
        rate_mark(ui, theme::Custody::Mine.color(), true);
        rate_value(ui, up, true);
        ui.add_space(theme::space::M);
        rate_mark(ui, theme::Custody::Peer.color(), false);
        rate_value(ui, down, false);
    });
}

/// The link grade in words, from the same `lit` count that strikes the seal.
fn signal_sentence(lit: u8) -> String {
    let top = GRADES.len() - 1;
    let lit = usize::from(lit).min(top);
    format!("{SIGNAL_LEAD} {}, {lit} of {top}", GRADES[lit])
}

/// The plot in words: how much history there is, where the newest reading sits
/// in this peer's own span, and which way the trail is going. Milliseconds are
/// never named here — the samples arrive normalized, and inventing a figure
/// from them would be a lie the drawn plot does not tell.
fn trend_sentence(samples: &[f32]) -> String {
    let parts = match samples.len() {
        0 => vec![TREND_NONE.to_owned()],
        1 => vec![TREND_ONE.to_owned()],
        n => {
            let newest = samples.last().copied().map_or(0.0, level);
            vec![
                format!("{n} readings"),
                format!(
                    "latest {} percent of its own range",
                    (newest * 100.0) as u32
                ),
                trend_of(samples).to_owned(),
            ]
        }
    };
    a11y::sentence(TREND_LEAD, &parts)
}

/// Which way the trail leans: the second half of the retained history against
/// the first. A flat ring normalizes to a straight line down the middle, which
/// is steady and must not be read as either direction.
fn trend_of(samples: &[f32]) -> &'static str {
    let half = samples.len() / 2;
    let (old, new) = samples.split_at(half);
    let mean = |slice: &[f32]| -> f32 {
        if slice.is_empty() {
            return 0.0;
        }
        slice.iter().copied().map(level).sum::<f32>() / slice.len() as f32
    };
    let swing = mean(new) - mean(old);
    if swing > TREND_SWING {
        RISING
    } else if swing < -TREND_SWING {
        FALLING
    } else {
        STEADY
    }
}

/// One direction's rate in words. The value is already formatted for the
/// column; this names the direction its diamond stands for and spells out the
/// unit the caption abbreviates.
fn rate_phrase(text: &str, outgoing: bool) -> String {
    let lead = if outgoing { SENDING } else { RECEIVING };
    let text = text.trim();
    // The em-dash and an empty caption say the same thing: no bytes moved.
    let value = if text == NO_RATE || text.is_empty() {
        RATE_IDLE.to_owned()
    } else {
        a11y::spoken_size(text)
    };
    format!("{lead} {value}")
}

/// A sample as the plot reads it: clamped into the band, a NaN resting on the
/// floor. Shared with [`trend_sentence`] so the spoken reading and the drawn
/// one can never come from two different numbers.
fn level(s: f32) -> f32 {
    if s.is_nan() { 0.0 } else { s.clamp(0.0, 1.0) }
}

/// The cell for the link-grade seal: the title step plus a baseline step of air,
/// so the seal reads beside a peer name without crowding it.
fn seal_side() -> f32 {
    theme::sized(theme::step::TITLE) + theme::space::S
}

/// Rules the plot cell: the axis along the foot, a lighter hairline along the
/// head, and a quarter grid inside.
fn rule_cell(p: &egui::Painter, frame: egui::Rect) {
    let axis = egui::Stroke::new(theme::RULE_W, theme::rule_divider());
    let grid = egui::Stroke::new(theme::RULE_W, theme::rule_hair());
    let span = egui::Rangef::new(frame.left(), frame.right());
    p.hline(span, theme::snap(frame.bottom()), axis);
    p.hline(span, theme::snap(frame.top()), grid);
    p.hline(span, theme::snap(frame.center().y), grid);
    let height = egui::Rangef::new(frame.top(), frame.bottom());
    for k in 1..COLUMNS {
        let x = frame.left() + frame.width() * k as f32 / COLUMNS as f32;
        p.vline(theme::snap(x), height, grid);
    }
}

/// One direction mark: a hairline with the house diamond stamped above it for
/// what leaves this device, below it for what arrives.
fn rate_mark(ui: &mut egui::Ui, color: egui::Color32, outgoing: bool) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(theme::space::M, theme::sized(theme::step::TITLE)),
        egui::Sense::hover(),
    );
    if !rect.is_finite() || !rect.is_positive() {
        return;
    }
    let y = theme::snap(rect.center().y);
    let p = ui.painter();
    p.hline(
        egui::Rangef::new(rect.left(), rect.right()),
        y,
        egui::Stroke::new(theme::RULE_W, theme::rule_divider()),
    );
    let offset = theme::space::XS * 2.0;
    let dy = if outgoing { -offset } else { offset };
    ornament::diamond(
        p,
        egui::pos2(rect.center().x, y + dy),
        theme::space::XS,
        color,
    );
}

fn rate_value(ui: &mut egui::Ui, s: &str, outgoing: bool) {
    // The em-dash is an absence, not a measurement, so it carries resting ink.
    let color = if s == NO_RATE {
        theme::ink_faint()
    } else {
        theme::ink()
    };
    let resp = ui.label(
        egui::RichText::new(s)
            .font(theme::font(theme::step::DATA, theme::fam_mono()))
            .color(color),
    );
    // The value is a real label and was always announced; which direction it
    // belongs to lived only in the diamond above or below the rule beside it.
    // Naming the pair on the value keeps the row at two announcements rather
    // than four, and neither of them is a bare figure.
    a11y::describe(&resp, &rate_phrase(s, outgoing));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui_native::telemetry;

    // ── link-grade seal ──

    #[test]
    fn signal_sentence_names_every_grade_the_house_has() {
        assert_eq!(
            signal_sentence(telemetry::grade_lit("Good")),
            "link good, 3 of 3"
        );
        assert_eq!(
            signal_sentence(telemetry::grade_lit("Fair")),
            "link fair, 2 of 3"
        );
        assert_eq!(
            signal_sentence(telemetry::grade_lit("Poor")),
            "link poor, 1 of 3"
        );
        assert_eq!(
            signal_sentence(telemetry::grade_lit("Offline")),
            "link offline, 0 of 3"
        );
    }

    #[test]
    fn signal_sentence_clamps_a_count_past_the_seal() {
        // The seal caps at 3; the sentence must cap with it rather than panic
        // on an index the matrix cannot draw.
        assert_eq!(signal_sentence(9), "link good, 3 of 3");
        assert_eq!(signal_sentence(u8::MAX), "link good, 3 of 3");
    }

    // ── round-trip plot ──

    #[test]
    fn trend_sentence_says_when_there_is_nothing_to_plot() {
        assert_eq!(trend_sentence(&[]), "round-trip history: no readings yet");
        assert_eq!(
            trend_sentence(&[0.4]),
            "round-trip history: one reading so far"
        );
    }

    #[test]
    fn trend_sentence_reads_the_newest_against_the_peers_own_range() {
        // Normalized samples are a position in this peer's own span, so the
        // announcement says so and never invents milliseconds.
        let said = trend_sentence(&[0.0, 0.25, 0.5, 1.0]);
        assert_eq!(
            said,
            "round-trip history: 4 readings, latest 100 percent of its own range, rising"
        );
        assert!(!said.contains("ms") && !said.contains("millisecond"));
    }

    #[test]
    fn trend_sentence_calls_a_flat_ring_steady() {
        // A link with no spread normalizes to 0.5 throughout — steady, and
        // neither rising nor falling.
        assert_eq!(
            trend_sentence(&[0.5; 6]),
            "round-trip history: 6 readings, latest 50 percent of its own range, steady"
        );
    }

    #[test]
    fn trend_sentence_reads_a_falling_trail() {
        let said = trend_sentence(&[1.0, 0.9, 0.2, 0.0]);
        assert!(said.ends_with("falling"), "{said}");
    }

    #[test]
    fn trend_sentence_survives_nonsense_samples() {
        // The plot floors a NaN and clamps out-of-range values; the sentence
        // reads the same numbers the plot does, through the same `level`.
        let unreadable = trend_sentence(&[f32::NAN; 4]);
        assert_eq!(
            unreadable,
            "round-trip history: 4 readings, latest 0 percent of its own range, steady"
        );
        // A trail that climbs past the top of the band is read at the band
        // edge, and is still rising rather than flattened into steady.
        let clamped = trend_sentence(&[0.0, 0.0, 5.0, 9.0]);
        assert!(clamped.contains("latest 100 percent"), "{clamped}");
        assert!(clamped.ends_with(RISING), "{clamped}");
        // A NaN in the middle of real readings floors to the bottom of the
        // band, exactly where the plot puts it.
        let mixed = trend_sentence(&[1.0, 1.0, f32::NAN, f32::NAN]);
        assert!(mixed.ends_with(FALLING), "{mixed}");
    }

    #[test]
    fn trend_swing_needs_a_real_move() {
        // A trail that barely drifts is steady: a reader should not be told a
        // link is degrading because of a rounding-sized wobble.
        assert_eq!(trend_of(&[0.50, 0.50, 0.55, 0.55]), STEADY);
        assert_eq!(trend_of(&[0.50, 0.50, 0.75, 0.75]), RISING);
        assert_eq!(trend_of(&[0.75, 0.75, 0.50, 0.50]), FALLING);
    }

    // ── transfer rates ──

    #[test]
    fn rate_phrase_names_the_direction_and_the_unit() {
        assert_eq!(
            rate_phrase(&telemetry::fmt_rate(1_300_000), true),
            "sending 1.2 megabytes per second"
        );
        assert_eq!(
            rate_phrase(&telemetry::fmt_rate(512), false),
            "receiving 512 bytes per second"
        );
    }

    #[test]
    fn an_idle_direction_is_an_absence_not_a_zero() {
        // `fmt_rate(0)` is the em-dash the column paints; read out it must be a
        // word, not a character name.
        assert_eq!(
            rate_phrase(&telemetry::fmt_rate(0), true),
            "sending nothing"
        );
        assert_eq!(
            rate_phrase(NO_RATE, false),
            format!("{RECEIVING} {RATE_IDLE}")
        );
    }

    #[test]
    fn rate_phrase_passes_an_unexpected_caption_through() {
        // An empty caption is an absence too, never a direction with a hanging
        // space after it.
        assert_eq!(rate_phrase("", true), "sending nothing");
        assert_eq!(rate_phrase("  ", false), "receiving nothing");
        assert_eq!(rate_phrase("stalled", false), "receiving stalled");
    }
}
