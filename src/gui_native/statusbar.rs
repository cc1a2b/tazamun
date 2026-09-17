//! The window's bottom status strip: a quiet rail that closes the frame's
//! composition. A girih hairline along its top edge, aggregate device counts
//! separated by manuscript diamonds, an optional right-aligned note, and a
//! khatam seal that breathes while work is in flight.
//!
//! Everything here is painted, never allocated as widgets — the strip reports,
//! it never competes for layout or input. Pure presentation; no I/O, no state.
//! Total for degenerate input: zero counts, empty notes and hairline widths
//! clip and keep going. The only time-dependent behaviour is the busy breath,
//! which is also the only thing that ever schedules a repaint — idle, and
//! reduced motion, cost nothing.
//!
//! Painted galleys announce nothing, and this strip is the window's standing
//! report — the only place the aggregate counts are written at all. So it also
//! claims one focusable rect carrying [`strip_sentence`]: a reader reaches the
//! whole strip in one stop and hears one sentence, rather than losing the
//! counts entirely.

use eframe::egui;

use super::{a11y, focusnav, ornament, theme};

/// Everything the strip reports, gathered by the caller.
pub struct Status<'a> {
    pub sessions: usize,
    pub running: usize,
    pub peers_online: usize,
    pub conflicts: usize,
    /// A refresh or action is in flight — the seal breathes.
    pub busy: bool,
    /// Optional right-aligned note (the current action, or the selected
    /// session's name).
    pub note: Option<&'a str>,
    /// The running version, shown at the foot's right edge. It belongs here
    /// rather than under the session list: it describes the window, not the
    /// sessions, and the sidebar scrolls it out of sight once the list is long.
    pub version: Option<&'a str>,
}

/// Height the caller should give the bottom panel.
pub const STRIP_H: f32 = 26.0;

/// Bottom-corner radius when the window is restored. Mirrors `theme::R_WINDOW`
/// (and therefore `chrome::radius`) — the strip sits on the window's bottom
/// edge, so square corners here would break the frameless chrome's rounding.
const R_WINDOW_BOTTOM: u8 = theme::R_WINDOW;

/// Inset of the first cluster from the left edge.
const PAD_L: f32 = 14.0;
/// Air on each side of a separator diamond.
const SEP_AIR: f32 = 10.0;
/// Right edge shared by the counts' hard bound and the note's alignment —
/// leaves the seal its own air.
const CONTENT_RIGHT: f32 = 34.0;
/// Below this much free space the note is dropped rather than squeezed.
const NOTE_MIN_W: f32 = 60.0;
/// Breathing space between the last count and the note.
const NOTE_AIR: f32 = 8.0;
const SEAL_R: f32 = 6.0;

/// What the strip is called when it is read out rather than seen.
const LEAD: &str = "status";
/// The khatam seal, in words: filled and breathing while work is in flight,
/// outlined at rest. Both states are drawn, so both are announced — a reader
/// must be able to tell "nothing is happening" from "nobody told me".
const BUSY: &str = "working";
const IDLE: &str = "idle";

/// Paints the strip across `ui`'s full width. `maximized` follows the window
/// state so the bottom corners stay square when maximized and rounded when
/// not, matching the frameless chrome.
pub fn status_strip(ui: &mut egui::Ui, s: Status<'_>, maximized: bool) {
    let rect = ui.max_rect();
    if !rect.is_finite() || !rect.is_positive() {
        return;
    }

    let resp = ui.interact(
        rect,
        ui.id().with("status-strip"),
        egui::Sense::focusable_noninteractive(),
    );
    a11y::describe(&resp, &strip_sentence(&s));

    let p = ui.painter();

    // Surface: flat against the content above, rounded into the window below.
    let r = if maximized { 0 } else { R_WINDOW_BOTTOM };
    p.add(egui::Shape::rect_filled(
        rect,
        egui::CornerRadius {
            nw: 0,
            ne: 0,
            sw: r,
            se: r,
        },
        theme::bg_chrome(),
    ));

    // Top edge: a whisper of strapwork with the hairline over it keeping the
    // seam crisp. Intersected with the strip so a squeezed panel can't bleed.
    let band = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 12.0, rect.top() + 1.0),
        egui::pos2(rect.right() - 12.0, rect.top() + 6.0),
    )
    .intersect(rect);
    ornament::girih_band(p, band, theme::wash::of(theme::gold(), theme::wash::GHOST));
    p.hline(
        rect.x_range(),
        rect.top() + 0.5,
        egui::Stroke::new(theme::RULE_W, theme::rule_hair()),
    );

    // Counts are hard-clipped short of the seal, so no count can ever run
    // under the note or the khatam however narrow the window gets.
    let bound = rect.right() - CONTENT_RIGHT;
    let counts = p.with_clip_rect(egui::Rect::from_min_max(
        rect.left_top(),
        egui::pos2(bound, rect.bottom()),
    ));

    let font = theme::font(theme::step::META, theme::fam_medium());
    let cy = rect.center().y;
    let mut x = rect.left() + PAD_L;
    let mut drawn = 0usize;
    {
        let mut cluster = |text: String, color: egui::Color32| {
            if x >= bound {
                return;
            }
            if drawn > 0 {
                x += SEP_AIR;
                ornament::diamond(
                    &counts,
                    egui::pos2(x, cy),
                    2.2,
                    theme::alpha(theme::gold(), 115),
                );
                x += SEP_AIR;
            }
            let galley = counts.layout_no_wrap(text, font.clone(), color);
            let size = galley.size();
            counts.galley(egui::pos2(x, cy - size.y * 0.5), galley, color);
            x += size.x;
            drawn += 1;
        };

        cluster(
            format!(
                "{} {}",
                s.sessions,
                plural(s.sessions, "session", "sessions")
            ),
            theme::ink_muted(),
        );
        cluster(
            format!("{} running", s.running),
            if s.running > 0 {
                theme::custody_good()
            } else {
                theme::ink_muted()
            },
        );
        cluster(
            format!(
                "{} {} online",
                s.peers_online,
                plural(s.peers_online, "peer", "peers")
            ),
            if s.peers_online > 0 {
                theme::custody_peer()
            } else {
                theme::ink_muted()
            },
        );
        if s.conflicts > 0 {
            cluster(
                format!(
                    "{} preserved {}",
                    s.conflicts,
                    plural(s.conflicts, "copy", "copies")
                ),
                theme::custody_stale(),
            );
        }
    }

    // The version holds the right edge, and the note takes what is left of it.
    // When the window is too narrow for both the note goes first: which folder
    // is open is already named in the header above, whereas nothing else on
    // screen says which build is running.
    let mut bound = bound;
    if let Some(v) = s.version.map(str::trim).filter(|v| !v.is_empty())
        && bound - x >= NOTE_MIN_W
    {
        let galley = p.layout_no_wrap(v.to_owned(), font.clone(), theme::ink_faint());
        let size = galley.size();
        let left = (bound - size.x).max(x + NOTE_AIR);
        let clip = p.with_clip_rect(egui::Rect::from_min_max(
            egui::pos2(x + NOTE_AIR, rect.top()),
            egui::pos2(bound, rect.bottom()),
        ));
        clip.galley(
            egui::pos2(left, cy - size.y * 0.5),
            galley,
            theme::ink_faint(),
        );
        bound = left - NOTE_AIR;
    }

    // The note takes whatever the counts and the version left behind,
    // right-aligned; if that is cramped it is dropped whole rather than shown
    // as a stub.
    if let Some(note) = s.note
        && bound - x >= NOTE_MIN_W
    {
        let galley = p.layout_no_wrap(note.to_owned(), font, theme::ink_faint());
        let size = galley.size();
        let left = (bound - size.x).max(x + NOTE_AIR);
        let clip = p.with_clip_rect(egui::Rect::from_min_max(
            egui::pos2(x + NOTE_AIR, rect.top()),
            egui::pos2(bound, rect.bottom()),
        ));
        clip.galley(
            egui::pos2(left, cy - size.y * 0.5),
            galley,
            theme::ink_faint(),
        );
    }

    // The seal: outlined and still at rest, filled while busy — breathing only
    // when the user has not asked for stillness.
    let seal = egui::pos2(rect.right() - 16.0, cy);
    let breathing = s.busy && !theme::reduced_motion();
    if breathing {
        let phase = (ui.input(|i| i.time) * 2.2).sin() as f32 * 0.5 + 0.5;
        ornament::khatam(
            p,
            seal,
            SEAL_R,
            theme::alpha(theme::gold(), (115.0 + 140.0 * phase) as u8),
            true,
        );
        ui.ctx().request_repaint_after(theme::motion::CADENCE);
    } else if s.busy {
        ornament::khatam(p, seal, SEAL_R, theme::gold(), true);
    } else {
        ornament::khatam(p, seal, SEAL_R, theme::alpha(theme::gold(), 89), false);
    }

    // Last, so the house ring sits over the strip rather than under its fill.
    focusnav::ring(ui, &resp);
}

/// The one sentence the strip announces: the same counts it paints, in the same
/// order, plus the seal's state and the note. A count the strip drops — zero
/// preserved copies, a note squeezed out by a narrow window — is dropped here
/// too, except the note, which costs a reader nothing and is the only place the
/// current action is named.
fn strip_sentence(s: &Status<'_>) -> String {
    let mut parts = vec![
        format!(
            "{} {}",
            s.sessions,
            plural(s.sessions, "session", "sessions")
        ),
        format!("{} running", s.running),
        format!(
            "{} {} online",
            s.peers_online,
            plural(s.peers_online, "peer", "peers")
        ),
    ];
    if s.conflicts > 0 {
        parts.push(format!(
            "{} preserved {}",
            s.conflicts,
            plural(s.conflicts, "copy", "copies")
        ));
    }
    parts.push(if s.busy { BUSY } else { IDLE }.to_owned());
    if let Some(v) = s.version.map(str::trim).filter(|v| !v.is_empty()) {
        parts.push(format!("version {v}"));
    }
    if let Some(note) = s.note.map(str::trim).filter(|n| !n.is_empty()) {
        parts.push(note.to_owned());
    }
    a11y::sentence(LEAD, &parts)
}

fn plural<'a>(n: usize, one: &'a str, many: &'a str) -> &'a str {
    if n == 1 { one } else { many }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status() -> Status<'static> {
        Status {
            sessions: 4,
            running: 2,
            peers_online: 3,
            conflicts: 1,
            busy: true,
            note: Some("notes.txt"),
            version: None,
        }
    }

    #[test]
    fn sentence_reads_the_whole_strip() {
        assert_eq!(
            strip_sentence(&status()),
            "status: 4 sessions, 2 running, 3 peers online, 1 preserved copy, working, notes.txt"
        );
    }

    #[test]
    fn sentence_drops_the_clause_the_strip_does_not_paint() {
        // Zero conflicts paints no cluster, so it announces none either.
        let quiet = Status {
            conflicts: 0,
            busy: false,
            note: None,
            version: None,
            ..status()
        };
        assert_eq!(
            strip_sentence(&quiet),
            "status: 4 sessions, 2 running, 3 peers online, idle"
        );
    }

    #[test]
    fn sentence_singularizes_every_count() {
        let one = Status {
            sessions: 1,
            running: 1,
            peers_online: 1,
            conflicts: 1,
            busy: false,
            note: None,
            version: None,
        };
        assert_eq!(
            strip_sentence(&one),
            "status: 1 session, 1 running, 1 peer online, 1 preserved copy, idle"
        );
    }

    #[test]
    fn sentence_holds_at_zero_and_at_an_empty_note() {
        let empty = Status {
            sessions: 0,
            running: 0,
            peers_online: 0,
            conflicts: 0,
            busy: false,
            note: Some("   "),
            version: None,
        };
        assert_eq!(
            strip_sentence(&empty),
            "status: 0 sessions, 0 running, 0 peers online, idle"
        );
    }

    #[test]
    fn busy_and_idle_are_both_spoken() {
        let busy = strip_sentence(&Status {
            busy: true,
            ..status()
        });
        let idle = strip_sentence(&Status {
            busy: false,
            ..status()
        });
        assert!(busy.contains(BUSY) && !busy.contains(IDLE));
        assert!(idle.contains(IDLE) && !idle.contains(BUSY));
    }
}
