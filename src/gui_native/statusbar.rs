//! The window's bottom status strip: a quiet rail that closes the frame's
//! composition. A girih hairline along its top edge, aggregate device counts
//! separated by manuscript diamonds, an optional right-aligned note, and a
//! khatam seal that breathes while work is in flight.
//!
//! Everything here is painted, never allocated as widgets — the strip reports,
//! it never competes for layout or input. Pure presentation; no I/O, no state.
//! Total for degenerate input: zero counts, empty notes and hairline widths
//! clip and keep going. The only time-dependent behaviour is the busy breath,
//! which is also the only thing that ever schedules a repaint — idle costs
//! nothing.

use eframe::egui;

use super::{ornament, theme};

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
}

/// Height the caller should give the bottom panel.
pub const STRIP_H: f32 = 26.0;

/// Bottom-corner radius when the window is restored. Mirrors `theme::R_WINDOW`
/// (and therefore `chrome::radius`) — the strip sits on the window's bottom
/// edge, so square corners here would break the frameless chrome's rounding.
const R_WINDOW_BOTTOM: u8 = theme::R_WINDOW;

const FONT_PX: f32 = 11.0;
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

/// Paints the strip across `ui`'s full width. `maximized` follows the window
/// state so the bottom corners stay square when maximized and rounded when
/// not, matching the frameless chrome.
pub fn status_strip(ui: &mut egui::Ui, s: Status<'_>, maximized: bool) {
    let rect = ui.max_rect();
    if !rect.is_finite() || !rect.is_positive() {
        return;
    }
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
        theme::BG1,
    ));

    // Top edge: a whisper of strapwork with the hairline over it keeping the
    // seam crisp. Intersected with the strip so a squeezed panel can't bleed.
    let band = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 12.0, rect.top() + 1.0),
        egui::pos2(rect.right() - 12.0, rect.top() + 6.0),
    )
    .intersect(rect);
    ornament::girih_band(p, band, theme::GOLD.linear_multiply(0.08));
    p.hline(
        rect.x_range(),
        rect.top() + 0.5,
        egui::Stroke::new(1.0, theme::stroke_faint().color),
    );

    // Counts are hard-clipped short of the seal, so no count can ever run
    // under the note or the khatam however narrow the window gets.
    let bound = rect.right() - CONTENT_RIGHT;
    let counts = p.with_clip_rect(egui::Rect::from_min_max(
        rect.left_top(),
        egui::pos2(bound, rect.bottom()),
    ));

    let font = egui::FontId::new(FONT_PX, theme::fam_medium());
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
                    theme::GOLD.linear_multiply(0.45),
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
            theme::DIM,
        );
        cluster(
            format!("{} running", s.running),
            if s.running > 0 {
                theme::GOOD
            } else {
                theme::DIM
            },
        );
        cluster(
            format!(
                "{} {} online",
                s.peers_online,
                plural(s.peers_online, "peer", "peers")
            ),
            if s.peers_online > 0 {
                theme::LAPIS
            } else {
                theme::DIM
            },
        );
        if s.conflicts > 0 {
            cluster(
                format!(
                    "{} preserved {}",
                    s.conflicts,
                    plural(s.conflicts, "copy", "copies")
                ),
                theme::WARN,
            );
        }
    }

    // The note takes whatever the counts left behind, right-aligned; if that
    // is cramped it is dropped whole rather than shown as a stub.
    if let Some(note) = s.note
        && bound - x >= NOTE_MIN_W
    {
        let galley = p.layout_no_wrap(note.to_owned(), font, theme::FAINT);
        let size = galley.size();
        let left = (bound - size.x).max(x + NOTE_AIR);
        let clip = p.with_clip_rect(egui::Rect::from_min_max(
            egui::pos2(x + NOTE_AIR, rect.top()),
            egui::pos2(bound, rect.bottom()),
        ));
        clip.galley(egui::pos2(left, cy - size.y * 0.5), galley, theme::FAINT);
    }

    // The seal: outlined and still at rest, filled and breathing while busy.
    let seal = egui::pos2(rect.right() - 16.0, cy);
    if s.busy {
        let phase = (ui.input(|i| i.time) * 2.2).sin() as f32 * 0.5 + 0.5;
        ornament::khatam(
            p,
            seal,
            SEAL_R,
            theme::GOLD.linear_multiply(0.45 + 0.55 * phase),
            true,
        );
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(60));
    } else {
        ornament::khatam(p, seal, SEAL_R, theme::GOLD.linear_multiply(0.35), false);
    }
}

fn plural<'a>(n: usize, one: &'a str, many: &'a str) -> &'a str {
    if n == 1 { one } else { many }
}
