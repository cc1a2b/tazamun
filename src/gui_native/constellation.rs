//! The session's mesh drawn as a night-sky constellation: this node the
//! largest khatam at the centre, every peer a smaller khatam set out on a
//! radius that grows with round-trip time — near peers close, far peers out,
//! the scale log-compressed so a 400 ms straggler stays on the canvas while
//! 5 ms and 20 ms remain two different places. A direct path is a taut woven
//! band; a relayed path is a broken thread with a knot at the hop. Offline and
//! unmeasured peers sit on a dotted rim beyond the measured scale — present,
//! honest, never given a pretended distance. Angles come from a mixed hash of
//! the peer id, never from list order, so the sky holds still between frames.
//! Grading is the house vocabulary (`Good`/`Fair`/`Poor`/`Offline`) over the
//! same thresholds the daemon uses. Pure presentation over `theme` and
//! `ornament`: zero I/O, no clock but the caller's `t`, total for degenerate
//! inputs.
//!
//! All of that is carried in radius, thread and rim, which is to say in nothing
//! a screen reader can reach. So the figure also says itself: one sentence
//! ([`sky_sentence`]) naming every star it draws, in the order it draws them,
//! and a keyboard cursor ([`star_cursor`]) that steps between stars the way the
//! pointer hovers them — same slots, same order, same ring, same returned
//! selection.

use std::sync::Arc;

use eframe::egui;
use egui::{Color32, Galley, Key, Modifiers, Pos2, Rect, Sense, Stroke};
use egui::{pos2, vec2};

use super::{a11y, focusnav, ornament, register, telemetry, theme};
use crate::consts::{GRADE_GOOD_MAX_RTT_MS, GRADE_POOR_MIN_RTT_MS};

/// Log-compression knee: RTTs near this many ms use the scale's steep part.
const RTT_TAU_MS: f32 = 30.0;
/// RTTs at or above this many ms all sit at the measured band's outer edge.
const RTT_CAP_MS: f32 = 600.0;
/// Most stars drawn; the rest become the "+ N more" caption.
const MAX_STARS: usize = 24;
/// Longest name drawn under a star before elision.
const LABEL_MAX_CHARS: usize = 14;
/// Clearance demanded between two label blocks before one is dropped.
const LABEL_PAD: f32 = 2.0;
const CANVAS_MARGIN: f32 = 10.0;
/// Below this working radius there is no sky, only the centre mark.
/// Smallest half-extent that can still host a legible sky. It has to be large
/// enough that the innermost orbit clears the centre and its label — see
/// [`band_for`] — or a 0 ms peer would have nowhere to sit.
const MIN_SKY_R: f32 = 72.0;
/// Gap between the centre mark and the "you" label hanging under it.
const CENTRE_LABEL_GAP: f32 = theme::space::S;
/// Breathing room between the centre's label and the nearest star.
const CENTRE_CLEARANCE: f32 = 6.0;
/// How much of the draw-on is spent staggering outer stars behind inner ones.
const STAGGER: f32 = 0.35;
const WEAVE_WAVELENGTH: f32 = 13.0;
const WEAVE_AMP: f32 = 1.5;

/// What the sky is called when it is read out rather than seen, and the words
/// its marks stand for. The drawing says all of this in radius, thread and
/// rim; none of that survives without eyes, so the figure says it in a
/// sentence instead — see [`sky_sentence`].
const SKY_LEAD: &str = "peer mesh";
/// The note the sky paints for a session of one, in the same words.
const SKY_EMPTY: &str = "no peers yet";
const PATH_DIRECT: &str = "direct";
const PATH_RELAYED: &str = "relayed";
/// An online peer with no reading yet: the dash the sub-line paints, in words.
const RTT_UNMEASURED: &str = "round-trip unmeasured";
const STAR_OFFLINE: &str = "offline";
/// A peer that arrived with neither a name nor an id. Never seen in practice;
/// the alternative is announcing a bare grade with nothing to attach it to.
const STAR_UNNAMED: &str = "unnamed peer";

/// One peer as the sky needs it.
#[derive(Clone, Copy, Debug)]
pub struct Star<'a> {
    /// Stable peer id; drives the deterministic angle.
    pub id: &'a str,
    /// Short display name.
    pub name: &'a str,
    /// Round-trip time in milliseconds, when the peer is online and measured.
    pub rtt_ms: Option<u32>,
    /// True when the path is relayed rather than direct.
    pub relayed: bool,
    /// False for a peer that is known but not currently connected.
    pub online: bool,
    /// The caller's authoritative health grade, when it has one. The sky can
    /// only see round-trip time; the daemon also weighs jitter and flaps, so
    /// grading here independently would contradict the list on the same screen.
    pub grade: Option<&'a str>,
}

/// Draws the mesh as a constellation into the given rect: this node at the
/// centre, each peer a khatam at a radius set by its round-trip time. `t` is
/// 0..=1 for the draw-on animation; nothing is drawn at t <= 0. Returns the
/// index into `stars` of the peer under the pointer — or of the star a keyboard
/// reader has stepped to, which reaches the same figure the same way.
pub fn sky(ui: &mut egui::Ui, rect: egui::Rect, stars: &[Star<'_>], t: f32) -> Option<usize> {
    if !rect.is_finite() || !rect.is_positive() || !t.is_finite() || t <= 0.0 {
        return None;
    }
    let ease = ease_out(t.clamp(0.0, 1.0));
    let centre = rect.center();
    let avail = rect.width().min(rect.height()) * 0.5 - CANVAS_MARGIN;

    // The figure claims its rect before any ink goes down. Placement is pure,
    // so the sentence the sky announces and the star a keyboard reader has
    // stepped to are both settled by the time the drawing wants them — and the
    // sky that is too small to draw still says what it holds.
    let resp = ui.interact(
        rect,
        ui.id().with("constellation-sky"),
        Sense::focusable_noninteractive(),
    );
    let placement = band_for(avail, register::line_h(theme::step::META)).map(|band| {
        let (placed, overflow) = place(stars, &band);
        (band, placed, overflow)
    });
    let cursor = star_cursor(
        ui,
        &resp,
        placement.as_ref().map_or(0, |(_, placed, _)| placed.len()),
    );
    let spoken = cursor
        .and_then(|slot| {
            placement
                .as_ref()
                .and_then(|(_, placed, _)| placed.get(slot))
        })
        .map_or_else(|| sky_sentence(stars), |g| star_phrase(&stars[g.idx]));
    a11y::describe(&resp, &spoken);

    let p = ui.painter().with_clip_rect(rect);
    let Some((band, placed, overflow)) = placement else {
        // Too small for a sky: the centre mark alone, still deliberate.
        ornament::khatam(
            &p,
            centre,
            (avail * 0.5).clamp(4.0, 12.0),
            theme::alpha(theme::gold(), (255.0 * ease) as u8),
            true,
        );
        focusnav::ring(ui, &resp);
        return None;
    };

    // The rim: a dotted ring beyond the measured scale, where offline and
    // unmeasured stars sit.
    for shape in egui::Shape::dashed_line(
        &ring_points(centre, band.r_rim),
        Stroke::new(
            theme::RULE_W,
            theme::alpha(theme::ink_faint(), (77.0 * ease) as u8),
        ),
        1.5,
        4.0,
    ) {
        p.add(shape);
    }

    // This node, and its label — seeded first into the collision list so
    // near stars defer to it.
    ornament::khatam(
        &p,
        centre,
        band.centre_r,
        theme::alpha(theme::gold(), (255.0 * ease) as u8),
        true,
    );
    let you = p.layout_no_wrap(
        "you".to_string(),
        theme::font(theme::step::META, theme::fam_medium()),
        theme::alpha(theme::ink_muted(), (255.0 * ease) as u8),
    );
    let you_size = you.size();
    let you_pos = pos2(
        centre.x - you_size.x * 0.5,
        centre.y + band.centre_r + CENTRE_LABEL_GAP,
    );
    p.galley(you_pos, you, theme::ink_muted());

    if stars.is_empty() {
        // A sky of one is the first-run case, not an error.
        let note = p.layout_no_wrap(
            "no peers yet".to_string(),
            theme::font(theme::step::META, theme::fam_medium()),
            theme::alpha(theme::ink_faint(), (255.0 * ease) as u8),
        );
        let size = note.size();
        p.galley(
            pos2(centre.x - size.x * 0.5, rect.bottom() - size.y - 6.0),
            note,
            theme::ink_faint(),
        );
        focusnav::ring(ui, &resp);
        return None;
    }

    // Reference rings at the grade thresholds, so radius has stated meaning.
    let good_ms = GRADE_GOOD_MAX_RTT_MS as u32;
    let poor_ms = GRADE_POOR_MIN_RTT_MS as u32;
    let ring_marks = [good_ms, poor_ms].map(|ms| {
        let r = band.radius_of_frac(radial_frac(ms));
        p.add(egui::Shape::closed_line(
            ring_points(centre, r),
            Stroke::new(
                theme::RULE_W,
                theme::alpha(theme::ink(), (9.0 * ease) as u8),
            ),
        ));
        (r, format!("{ms} ms"))
    });

    let screen: Vec<(Pos2, f32)> = placed
        .iter()
        .map(|g| {
            (
                pos2(centre.x + g.dx, centre.y + g.dy),
                local_t(ease, g.frac),
            )
        })
        .collect();

    // Hover before drawing, so the ring paints with its star this frame. The
    // keyboard cursor takes the same ring: a reader who stepped onto a star has
    // to be able to see where they are, and the figure already has a mark for
    // "this one".
    let pointer = resp.hover_pos().filter(|q| rect.contains(*q));
    let hovered_slot = pointer
        .and_then(|q| {
            hit_at(
                placed
                    .iter()
                    .enumerate()
                    .filter(|&(slot, _)| screen[slot].1 > 0.0),
                q.x - centre.x,
                q.y - centre.y,
            )
        })
        .or(cursor);

    // Threads first (under the stars), then the stars themselves.
    for (slot, g) in placed.iter().enumerate() {
        let (pos, local) = screen[slot];
        if local <= 0.0 {
            continue;
        }
        let star = &stars[g.idx];
        let color = grade_color(g.grade);
        let dist = centre.distance(pos);
        if star.online && dist > band.centre_r + g.star_r + 8.0 {
            let dir = (pos - centre) / dist;
            let a = centre + dir * (band.centre_r + 3.0);
            let b_full = pos - dir * (g.star_r + 3.0);
            let b = a + (b_full - a) * local;
            if star.relayed {
                // The broken thread: dim, dotted, with a knot at the hop.
                for shape in egui::Shape::dashed_line(
                    &[a, b],
                    Stroke::new(
                        theme::RULE_W,
                        theme::alpha(theme::ink_muted(), (166.0 * local) as u8),
                    ),
                    2.5,
                    4.5,
                ) {
                    p.add(shape);
                }
                if local >= 0.5 {
                    let mid = a + (b_full - a) * 0.5;
                    let k = 3.0;
                    p.add(egui::Shape::closed_line(
                        vec![
                            pos2(mid.x, mid.y - k),
                            pos2(mid.x + k, mid.y),
                            pos2(mid.x, mid.y + k),
                            pos2(mid.x - k, mid.y),
                        ],
                        Stroke::new(
                            theme::RULE_W,
                            theme::alpha(theme::ink_muted(), (204.0 * local) as u8),
                        ),
                    ));
                }
            } else {
                // The taut band: a faint core with two strands woven over it.
                p.line_segment(
                    [a, b],
                    Stroke::new(
                        theme::RULE_W,
                        theme::alpha(theme::gold(), (46.0 * local) as u8),
                    ),
                );
                for strand in weave_strands(a, b) {
                    p.add(egui::Shape::line(
                        strand,
                        Stroke::new(
                            theme::RULE_W,
                            theme::alpha(theme::gold(), (115.0 * local) as u8),
                        ),
                    ));
                }
            }
        }
        let r = g.star_r * (0.6 + 0.4 * local);
        if star.online {
            let lit = f32::from(telemetry::grade_lit(g.grade));
            p.circle_filled(
                pos,
                r * 1.9,
                theme::alpha(color, ((0.06 + 0.04 * lit) * local * 255.0) as u8),
            );
            ornament::khatam(
                &p,
                pos,
                r,
                theme::alpha(color, ((0.7 + 0.1 * lit) * local * 255.0) as u8),
                true,
            );
        } else {
            // Unlit: outline only, dim, no line — present but not pretending.
            ornament::khatam(
                &p,
                pos,
                r,
                theme::alpha(theme::ink_faint(), (140.0 * local) as u8),
                false,
            );
        }
        if hovered_slot == Some(slot) {
            ornament::khatam(
                &p,
                pos,
                r + 4.0,
                theme::alpha(theme::gold_bright(), 204),
                false,
            );
        }
    }

    // Labels. Priority is slot order (measured online first, then unmeasured,
    // then offline, id-ordered), after the pre-accepted "you" and overflow
    // marks; a label whose block would collide with an accepted one is dropped
    // whole — its star stays, and hover still names it. Ring captions come
    // last and yield to everything.
    let mut rects = vec![Rect::from_min_size(you_pos, you_size)];
    let mut pre = 1usize;
    if overflow > 0 {
        let g = p.layout_no_wrap(
            format!("+ {overflow} more"),
            theme::font(theme::step::CAPTION, theme::fam_mono()),
            theme::alpha(theme::ink_faint(), (255.0 * ease) as u8),
        );
        let size = g.size();
        let min = pos2(rect.right() - size.x - 8.0, rect.bottom() - size.y - 6.0);
        rects.push(Rect::from_min_size(min, size));
        p.galley(min, g, theme::ink_faint());
        pre = 2;
    }
    let mut jobs: Vec<Vec<(Pos2, Arc<Galley>)>> = Vec::new();
    for (slot, g) in placed.iter().enumerate() {
        let (pos, local) = screen[slot];
        if local <= 0.15 {
            continue;
        }
        let star = &stars[g.idx];
        let name_color = if star.online {
            theme::ink()
        } else {
            theme::ink_muted()
        };
        let name = p.layout_no_wrap(
            elide(star.name),
            theme::font(theme::step::META, theme::fam_medium()),
            theme::alpha(name_color, (230.0 * local) as u8),
        );
        // The sub-line is where honesty lives: an offline star says "offline",
        // never a stale round-trip; an unmeasured one says the dash.
        let sub_text = if star.online {
            match star.rtt_ms {
                Some(ms) => format!("{ms} ms"),
                None => "—".to_string(),
            }
        } else {
            "offline".to_string()
        };
        let sub = p.layout_no_wrap(
            sub_text,
            theme::font(theme::step::CAPTION, theme::fam_mono()),
            theme::alpha(theme::ink_faint(), (255.0 * local) as u8),
        );
        let (nsz, ssz) = (name.size(), sub.size());
        let w = nsz.x.max(ssz.x);
        let h = nsz.y + 1.0 + ssz.y;
        if w + 4.0 >= rect.width() {
            continue;
        }
        let mut top = pos.y + g.star_r + 4.0;
        if top + h > rect.bottom() - 2.0 {
            top = pos.y - g.star_r - 4.0 - h;
        }
        let half = w * 0.5 + 2.0;
        let cx = if rect.width() > 2.0 * half {
            pos.x.clamp(rect.left() + half, rect.right() - half)
        } else {
            pos.x
        };
        rects.push(Rect::from_min_size(pos2(cx - w * 0.5, top), vec2(w, h)));
        jobs.push(vec![
            (pos2(cx - nsz.x * 0.5, top), name),
            (pos2(cx - ssz.x * 0.5, top + nsz.y + 1.0), sub),
        ]);
    }
    for (r_ring, text) in ring_marks {
        let g = p.layout_no_wrap(
            text,
            theme::font(theme::step::CAPTION, theme::fam_mono()),
            theme::alpha(theme::ink_faint(), (204.0 * ease) as u8),
        );
        let size = g.size();
        let ang = -3.0 * std::f32::consts::FRAC_PI_4;
        let at = centre + vec2(ang.cos(), ang.sin()) * r_ring;
        let min = pos2(at.x - size.x - 3.0, at.y - size.y * 0.5);
        rects.push(Rect::from_min_size(min, size));
        jobs.push(vec![(min, g)]);
    }
    let accepted = accept_labels(&rects, pre);
    for (k, pieces) in jobs.into_iter().enumerate() {
        if accepted[pre + k] {
            for (at, galley) in pieces {
                p.galley(at, galley, theme::ink_muted());
            }
        }
    }

    focusnav::ring(ui, &resp);
    hovered_slot
        .and_then(|slot| placed.get(slot))
        .map(|g| g.idx)
}

// ─── what the sky says (pure) ────────────────────────────────────────────────

/// The whole sky in one sentence: how many peers it holds, then every star it
/// draws, in the order it draws them.
///
/// One sentence, because a reader must not have to assemble a mesh out of
/// fourteen fragments — and this one, because radius, thread and rim are the
/// only place the sky writes distance, path and presence down.
fn sky_sentence(stars: &[Star<'_>]) -> String {
    if stars.is_empty() {
        return a11y::sentence(SKY_LEAD, &[SKY_EMPTY]);
    }
    let (order, overflow) = order_stars(stars);
    let mut parts: Vec<String> = order.iter().map(|&idx| star_phrase(&stars[idx])).collect();
    if overflow > 0 {
        // The sky paints "+ N more"; the sentence owes the reader the same
        // admission that it is not naming everyone.
        parts.push(format!("{overflow} more not drawn"));
    }
    let lead = format!(
        "{SKY_LEAD}, {} {}",
        stars.len(),
        if stars.len() == 1 { "peer" } else { "peers" }
    );
    a11y::sentence(&lead, &parts)
}

/// One star in words: who it is, by what path, and how far. An offline peer
/// gets its name and "offline" and nothing else — the rim it sits on is the
/// drawing refusing to pretend a distance it no longer has, and the sentence
/// refuses in the same place.
fn star_phrase(star: &Star<'_>) -> String {
    let name = match star.name.trim() {
        "" => star.id.trim(),
        name => name,
    };
    let name = if name.is_empty() { STAR_UNNAMED } else { name };
    if !star.online {
        return format!("{name} {STAR_OFFLINE}");
    }
    let path = if star.relayed {
        PATH_RELAYED
    } else {
        PATH_DIRECT
    };
    match star.rtt_ms {
        Some(ms) => format!("{name} {path} {}", a11y::spoken_ms(u64::from(ms))),
        None => format!("{name} {path} {RTT_UNMEASURED}"),
    }
}

// ─── the keyboard's reach into the figure ────────────────────────────────────

/// Which star a keyboard reader has stepped to while the sky holds focus, or
/// `None` for the figure as a whole.
///
/// Hovering a star is a pointer's reach into the sky; this is the same reach
/// without one. Arrows walk the drawn slots in the drawn order, Home and End
/// take the ends, and Tab still leaves — the figure borrows the arrow keys, it
/// never traps them.
fn star_cursor(ui: &egui::Ui, resp: &egui::Response, slots: usize) -> Option<usize> {
    let key = resp.id.with("star-cursor");
    if slots == 0 || !resp.has_focus() {
        // The cursor does not outlive the focus that made it: leaving and
        // returning finds the whole sky again, not a star from last time.
        ui.ctx().data_mut(|d| d.remove::<usize>(key));
        return None;
    }
    ui.ctx().memory_mut(|m| {
        m.set_focus_lock_filter(
            resp.id,
            egui::EventFilter {
                tab: false,
                horizontal_arrows: true,
                vertical_arrows: true,
                escape: false,
            },
        );
    });
    // Bitwise, not short-circuiting: both keys of a pair must be consumed, or
    // the one left behind moves focus out of the figure a frame later.
    let (fwd, back, first, last) = ui.input_mut(|i| {
        (
            i.consume_key(Modifiers::NONE, Key::ArrowRight)
                | i.consume_key(Modifiers::NONE, Key::ArrowDown),
            i.consume_key(Modifiers::NONE, Key::ArrowLeft)
                | i.consume_key(Modifiers::NONE, Key::ArrowUp),
            i.consume_key(Modifiers::NONE, Key::Home),
            i.consume_key(Modifiers::NONE, Key::End),
        )
    });
    let next = step_cursor(
        ui.ctx().data(|d| d.get_temp::<usize>(key)),
        slots,
        fwd,
        back,
        first,
        last,
    );
    ui.ctx().data_mut(|d| match next {
        Some(slot) => {
            d.insert_temp(key, slot);
        }
        None => d.remove::<usize>(key),
    });
    next
}

/// Where the cursor lands. `held` is where it was — `None` meaning the figure
/// as a whole, which is where a reader arrives and where stepping back off the
/// first star returns to. Neither end wraps: a sky is not a list, and silently
/// jumping from the nearest peer to the furthest would lose a reader their
/// place in it.
fn step_cursor(
    held: Option<usize>,
    slots: usize,
    fwd: bool,
    back: bool,
    first: bool,
    last: bool,
) -> Option<usize> {
    if slots == 0 {
        return None;
    }
    // A stale slot from a mesh that has since shrunk clamps before it moves.
    let held = held.map(|slot| slot.min(slots - 1));
    if first {
        return Some(0);
    }
    if last {
        return Some(slots - 1);
    }
    if fwd {
        return Some(held.map_or(0, |slot| (slot + 1).min(slots - 1)));
    }
    if back {
        return match held {
            None | Some(0) => None,
            Some(slot) => Some(slot - 1),
        };
    }
    held
}

/// The height the sky wants for a given width and peer count, so the caller
/// can allocate before drawing.
pub fn desired_height(width: f32, peers: usize) -> f32 {
    let crowd = peers.min(MAX_STARS) as f32;
    let floor = if peers == 0 {
        150.0
    } else {
        230.0 + crowd * 4.0
    };
    // The ceiling rises with the crowd, not just with the window. A wide window
    // otherwise took the full 480 for a single centre mark, leaving the peer
    // table below it off the bottom of the screen — the sky is a figure about
    // the peers, so an empty one has nothing to be big about.
    let ceiling = (floor + crowd * 28.0).min(480.0);
    if !width.is_finite() || width <= 0.0 {
        return floor;
    }
    (width * 0.62).clamp(floor.min(ceiling), ceiling)
}

// ─── pure geometry ───────────────────────────────────────────────────────────

/// The sky's radii for a working half-extent. `None` when there is no room.
struct Band {
    centre_r: f32,
    r_min: f32,
    r_meas: f32,
    r_rim: f32,
}

impl Band {
    fn radius_of_frac(&self, f: f32) -> f32 {
        self.r_min + f.clamp(0.0, 1.0) * (self.r_meas - self.r_min)
    }
}

fn band_for(avail: f32, label_h: f32) -> Option<Band> {
    if !avail.is_finite() || avail < MIN_SKY_R || !label_h.is_finite() {
        return None;
    }
    let centre_r = (avail * 0.16).clamp(9.0, 15.0);
    // A peer at 0 ms sits exactly at `r_min`, and on a LAN that is the ordinary
    // case rather than a corner one. The centre mark carries its "you" label
    // underneath, so the innermost orbit has to clear the khatam, that label,
    // and the widest a star can be drawn — otherwise the nearest peer lands on
    // top of the centre with its round-trip written across it. The label is
    // type, so its room is measured and handed in: a fixed 22 was one line of
    // meta type at 100% and well under it by 150%.
    let r_min = centre_r + CENTRE_LABEL_GAP + label_h.max(0.0) + star_radius(3) + CENTRE_CLEARANCE;
    let r_meas = avail * 0.86;
    if r_min >= r_meas {
        return None;
    }
    Some(Band {
        centre_r,
        r_min,
        r_meas,
        r_rim: avail * 0.96,
    })
}

/// One chosen star, in offsets from the centre.
#[derive(Clone, Copy, Debug)]
struct PlacedGeo {
    idx: usize,
    dx: f32,
    dy: f32,
    /// 0..=1 across the measured band; 1.0 on the rim. Drives the stagger.
    frac: f32,
    star_r: f32,
    grade: &'static str,
}

/// Orders, caps, and places the stars. Measured online peers first, then
/// unmeasured online, then offline, id-ordered within each class — so when
/// the cap bites, the peers that matter most keep their place in the sky.
fn place(stars: &[Star<'_>], band: &Band) -> (Vec<PlacedGeo>, usize) {
    let (order, overflow) = order_stars(stars);
    let placed = order
        .into_iter()
        .map(|idx| {
            let s = &stars[idx];
            // The caller's grade wins when the star is lit, since it knows
            // jitter and flaps. An unlit star grades itself: the supplied grade
            // describes a live path this peer no longer has.
            let own = grade_of(s.online, s.relayed, s.rtt_ms);
            let grade = match (s.online, s.grade) {
                (true, Some(g)) => house_grade(g).unwrap_or(own),
                _ => own,
            };
            let angle = angle_of(s.id);
            // Offline or unmeasured: the rim — beyond the measured scale,
            // never a pretended distance. A stale rtt on an offline peer is
            // deliberately ignored.
            let (radius, frac) = match (s.online, s.rtt_ms) {
                (true, Some(ms)) => {
                    let f = radial_frac(ms);
                    (band.radius_of_frac(f), f)
                }
                _ => (band.r_rim, 1.0),
            };
            PlacedGeo {
                idx,
                dx: radius * angle.cos(),
                dy: radius * angle.sin(),
                frac,
                star_r: star_radius(telemetry::grade_lit(grade)),
                grade,
            }
        })
        .collect();
    (placed, overflow)
}

/// The order the sky takes the stars in, and how many the cap leaves out.
/// Split out of [`place`] because the sentence the figure announces walks the
/// same order without needing a band to place anything in — a reader hears the
/// peers in the order they are drawn, and hears the same overflow admitted.
fn order_stars(stars: &[Star<'_>]) -> (Vec<usize>, usize) {
    let mut order: Vec<usize> = (0..stars.len()).collect();
    order.sort_by(|&a, &b| class_key(&stars[a]).cmp(&class_key(&stars[b])));
    let overflow = stars.len().saturating_sub(MAX_STARS);
    order.truncate(MAX_STARS);
    (order, overflow)
}

fn class_key<'s>(s: &'s Star<'_>) -> (u8, &'s str) {
    let class = if !s.online {
        2
    } else if s.rtt_ms.is_none() {
        1
    } else {
        0
    };
    (class, s.id)
}

/// The house grading over the fields the sky is given — same thresholds as
/// the daemon's `PathStats::grade`, minus the jitter and flap terms it alone
/// can know.
fn grade_of(online: bool, relayed: bool, rtt_ms: Option<u32>) -> &'static str {
    if !online {
        return "Offline";
    }
    match rtt_ms {
        Some(ms) if f64::from(ms) >= GRADE_POOR_MIN_RTT_MS => "Poor",
        Some(ms) if !relayed && f64::from(ms) < GRADE_GOOD_MAX_RTT_MS => "Good",
        _ => "Fair",
    }
}

/// Maps a caller-supplied grade onto the house vocabulary, so a placed star
/// can hold a `'static` label whatever the grade was borrowed from. An
/// unrecognised string yields `None` and the caller falls back to the sky's own
/// reading, rather than being silently coloured as Fair.
fn house_grade(grade: &str) -> Option<&'static str> {
    match grade {
        "Good" => Some("Good"),
        "Fair" => Some("Fair"),
        "Poor" => Some("Poor"),
        "Offline" => Some("Offline"),
        _ => None,
    }
}

fn grade_color(grade: &str) -> Color32 {
    match grade {
        "Good" => theme::custody_good(),
        "Fair" => theme::custody_stale(),
        "Poor" => theme::custody_blocked(),
        _ => theme::ink_faint(),
    }
}

/// Compressed radial scale: `ln(1 + rtt/tau)` normalized to the cap, so the
/// low end stays spread while the far end folds in. Monotone, `0` at 0 ms,
/// `1` at the cap and beyond.
fn radial_frac(rtt_ms: u32) -> f32 {
    let x = rtt_ms as f32 / RTT_TAU_MS;
    let denom = (1.0 + RTT_CAP_MS / RTT_TAU_MS).ln();
    ((1.0 + x).ln() / denom).clamp(0.0, 1.0)
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// FNV's top bits barely move for short, similar ids (every `peer-NN` landed
/// in the same octant unmixed), so finish with a splitmix64 avalanche.
fn mix64(mut h: u64) -> u64 {
    h = (h ^ (h >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    h = (h ^ (h >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    h ^ (h >> 31)
}

/// Deterministic direction for a peer id, in `[0, TAU)`.
fn angle_of(id: &str) -> f32 {
    let mixed = mix64(fnv1a64(id.as_bytes()));
    let unit = (mixed >> 11) as f64 / (1u64 << 53) as f64;
    ((unit * std::f64::consts::TAU) as f32).rem_euclid(std::f32::consts::TAU)
}

/// Star size from the same `grade_lit` count that drives the signal arcs.
fn star_radius(lit: u8) -> f32 {
    4.0 + 1.6 * f32::from(lit.min(3))
}

fn hit_radius(star_r: f32) -> f32 {
    (star_r + 5.0).max(9.0)
}

fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t) * (1.0 - t)
}

/// Per-star reveal: inner stars appear first, the rim last, all done by
/// `ease == 1`.
fn local_t(ease: f32, frac: f32) -> f32 {
    (ease * (1.0 + STAGGER) - STAGGER * frac.clamp(0.0, 1.0)).clamp(0.0, 1.0)
}

/// Names longer than [`LABEL_MAX_CHARS`] chars keep their head and gain an
/// ellipsis; char-counted, so multi-byte names never split a boundary.
fn elide(name: &str) -> String {
    if name.chars().count() <= LABEL_MAX_CHARS {
        name.to_string()
    } else {
        let cut: String = name.chars().take(LABEL_MAX_CHARS - 1).collect();
        format!("{cut}…")
    }
}

/// First-accepted-wins over blocks in priority order: the leading
/// `preaccepted` rects always pass (and still block), every later rect is
/// dropped if it comes within [`LABEL_PAD`] of anything accepted before it.
fn accept_labels(rects: &[Rect], preaccepted: usize) -> Vec<bool> {
    let mut taken: Vec<Rect> = Vec::with_capacity(rects.len());
    let mut out = Vec::with_capacity(rects.len());
    for (i, r) in rects.iter().enumerate() {
        if !r.is_finite() {
            out.push(false);
            continue;
        }
        let padded = r.expand(LABEL_PAD);
        let free = i < preaccepted || taken.iter().all(|t| !t.intersects(padded));
        if free {
            taken.push(padded);
        }
        out.push(free);
    }
    out
}

/// Nearest candidate whose hit disc contains the pointer offset; ties keep
/// the earlier slot. Agrees with drawing because both use the placed offsets
/// and [`hit_radius`] of the drawn star size.
fn hit_at<'p>(
    cands: impl Iterator<Item = (usize, &'p PlacedGeo)>,
    px: f32,
    py: f32,
) -> Option<usize> {
    let mut best: Option<(usize, f32)> = None;
    for (slot, g) in cands {
        let d = (px - g.dx).hypot(py - g.dy);
        if d <= hit_radius(g.star_r) && best.is_none_or(|(_, bd)| d < bd) {
            best = Some((slot, d));
        }
    }
    best.map(|(slot, _)| slot)
}

/// Two strands woven about the chord `a -> b`, tapered to zero at both
/// anchors so the band reads taut. Empty for a degenerate chord.
fn weave_strands(a: Pos2, b: Pos2) -> [Vec<Pos2>; 2] {
    let len = a.distance(b);
    if !len.is_finite() || len <= f32::EPSILON {
        return [Vec::new(), Vec::new()];
    }
    let dir = (b - a) / len;
    let perp = vec2(-dir.y, dir.x);
    let n = ((len / 3.0).ceil() as usize).clamp(2, 512);
    let strand = |phase: f32| -> Vec<Pos2> {
        (0..=n)
            .map(|i| {
                let t01 = i as f32 / n as f32;
                let s = len * t01;
                let amp = WEAVE_AMP * (std::f32::consts::PI * t01).sin();
                let off = amp * (std::f32::consts::TAU * s / WEAVE_WAVELENGTH + phase).sin();
                a + dir * s + perp * off
            })
            .collect()
    };
    [strand(0.0), strand(std::f32::consts::PI)]
}

fn ring_points(centre: Pos2, r: f32) -> Vec<Pos2> {
    (0..=72)
        .map(|k| {
            let a = std::f32::consts::TAU * k as f32 / 72.0;
            pos2(centre.x + r * a.cos(), centre.y + r * a.sin())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    fn s<'a>(id: &'a str, rtt: Option<u32>, relayed: bool, online: bool) -> Star<'a> {
        Star {
            id,
            name: id,
            rtt_ms: rtt,
            relayed,
            online,
            grade: None,
        }
    }

    fn band() -> Band {
        Band {
            centre_r: 12.0,
            r_min: 40.0,
            r_meas: 160.0,
            r_rim: 180.0,
        }
    }

    fn radius_of(g: &PlacedGeo) -> f32 {
        g.dx.hypot(g.dy)
    }

    // ── radial scale ──

    #[test]
    fn radial_frac_is_monotone_and_bounded() {
        let samples = [0u32, 1, 5, 20, 80, 300, 400, 600];
        for w in samples.windows(2) {
            assert!(radial_frac(w[0]) < radial_frac(w[1]), "{w:?}");
        }
        for ms in samples.iter().chain(&[u32::MAX]) {
            let f = radial_frac(*ms);
            assert!((0.0..=1.0).contains(&f));
        }
    }

    #[test]
    fn radial_frac_pins_zero_and_cap() {
        assert!(approx(radial_frac(0), 0.0));
        assert!(approx(radial_frac(600), 1.0));
        assert!(approx(radial_frac(u32::MAX), 1.0));
        // Values cross-checked against the closed form ln(1+r/30)/ln(21).
        assert!((radial_frac(80) - 0.4268).abs() < 0.01);
        assert!((radial_frac(300) - 0.7876).abs() < 0.01);
    }

    #[test]
    fn radial_frac_keeps_near_peers_apart() {
        // 5 ms and 20 ms must remain two different places (0.117 apart).
        assert!(radial_frac(20) - radial_frac(5) > 0.06);
        // ...while 400 ms stays on the canvas.
        assert!(radial_frac(400) < 0.9);
    }

    // ── angles ──

    #[test]
    fn angle_is_deterministic_and_distinct() {
        assert_eq!(angle_of("alice").to_bits(), angle_of("alice").to_bits());
        assert!(!approx(angle_of("alice"), angle_of("bob")));
    }

    #[test]
    fn angle_is_always_in_range() {
        for id in ["", "a", "peer-00", "محطة", "a-very-long-peer-identifier"] {
            let a = angle_of(id);
            assert!((0.0..std::f32::consts::TAU).contains(&a), "{id}: {a}");
        }
    }

    #[test]
    fn angles_spread_across_the_sky() {
        // Distribution verified out-of-band for these exact ids:
        // octants [4,6,5,1,4,4,3,5] — the assertions leave wide margins.
        let mut buckets = [0usize; 8];
        for i in 0..32 {
            let a = angle_of(&format!("peer-{i:02}"));
            let b = ((a / std::f32::consts::TAU) * 8.0) as usize;
            buckets[b.min(7)] += 1;
        }
        let occupied = buckets.iter().filter(|&&c| c > 0).count();
        assert!(occupied >= 6, "sky clumped: {buckets:?}");
        assert!(
            buckets.iter().all(|&c| c <= 10),
            "octant overloaded: {buckets:?}"
        );
    }

    // ── grading ──

    #[test]
    fn caller_grade_wins_for_a_lit_star_but_never_for_an_unlit_one() {
        // The daemon weighs jitter and flaps the sky cannot see, so its grade
        // is authoritative while the star is lit — otherwise the sky and the
        // list beneath it would print two different verdicts for one peer.
        let mut lit = s("peer-a", Some(5), false, true);
        lit.grade = Some("Poor");
        let (placed, _) = place(&[lit], &band());
        assert_eq!(placed[0].grade, "Poor", "daemon grade wins when lit");

        // An unlit star grades itself: a supplied grade describes a live path
        // this peer no longer has.
        let mut unlit = s("peer-a", None, false, false);
        unlit.grade = Some("Good");
        let (placed, _) = place(&[unlit], &band());
        assert_eq!(placed[0].grade, "Offline");

        // An unrecognised grade falls back to the sky's own reading rather
        // than being silently coloured Fair.
        let mut odd = s("peer-a", Some(5), false, true);
        odd.grade = Some("Excellent");
        let (placed, _) = place(&[odd], &band());
        assert_eq!(placed[0].grade, "Good");
    }

    #[test]
    fn grade_matches_the_house_vocabulary() {
        assert_eq!(grade_of(true, false, Some(5)), "Good");
        assert_eq!(grade_of(true, false, Some(79)), "Good");
        assert_eq!(grade_of(true, false, Some(80)), "Fair");
        assert_eq!(grade_of(true, true, Some(5)), "Fair");
        assert_eq!(grade_of(true, false, Some(299)), "Fair");
        assert_eq!(grade_of(true, false, Some(300)), "Poor");
        assert_eq!(grade_of(true, true, Some(300)), "Poor");
        assert_eq!(grade_of(true, false, Some(u32::MAX)), "Poor");
        assert_eq!(grade_of(true, false, None), "Fair");
        assert_eq!(grade_of(false, false, None), "Offline");
    }

    #[test]
    fn offline_star_never_shows_a_live_rtt() {
        // The bug that must not return: a stale rtt on an offline peer.
        assert_eq!(grade_of(false, false, Some(5)), "Offline");
        let (placed, _) = place(&[s("ghost", Some(5), false, false)], &band());
        assert!(approx(radius_of(&placed[0]), 180.0), "must sit on the rim");
        assert!(radius_of(&placed[0]) > 179.0, "must not use the stale rtt");
    }

    // ── placement ──

    #[test]
    fn place_orders_measured_before_unmeasured_before_offline() {
        let stars = [
            s("a", None, false, false),
            s("b", None, false, true),
            s("c", Some(10), false, true),
        ];
        let (placed, overflow) = place(&stars, &band());
        assert_eq!(overflow, 0);
        let idxs: Vec<usize> = placed.iter().map(|g| g.idx).collect();
        assert_eq!(idxs, vec![2, 1, 0]);
    }

    #[test]
    fn place_caps_at_max_stars_and_reports_overflow() {
        let ids: Vec<String> = (0..30).map(|i| format!("s-{i:02}")).collect();
        let stars: Vec<Star<'_>> = ids.iter().map(|id| s(id, Some(20), false, true)).collect();
        let (placed, overflow) = place(&stars, &band());
        assert_eq!(placed.len(), MAX_STARS);
        assert_eq!(overflow, 6);
        let kept: Vec<&str> = placed.iter().map(|g| stars[g.idx].id).collect();
        let expected: Vec<&str> = ids.iter().take(MAX_STARS).map(String::as_str).collect();
        assert_eq!(kept, expected);
    }

    #[test]
    fn place_is_deterministic_under_input_permutation() {
        let ids: Vec<String> = (0..10).map(|i| format!("peer-{i:02}")).collect();
        let fwd: Vec<Star<'_>> = ids.iter().map(|id| s(id, Some(25), false, true)).collect();
        let mut rev = fwd.clone();
        rev.reverse();
        let (pa, _) = place(&fwd, &band());
        let (pb, _) = place(&rev, &band());
        for (ga, gb) in pa.iter().zip(&pb) {
            assert_eq!(fwd[ga.idx].id, rev[gb.idx].id);
            assert!(approx(ga.dx, gb.dx) && approx(ga.dy, gb.dy));
        }
    }

    #[test]
    fn placed_radius_is_monotone_in_rtt() {
        let b = band();
        let r_of = |ms: u32| {
            let (placed, _) = place(&[s("same-id", Some(ms), false, true)], &b);
            radius_of(&placed[0])
        };
        assert!(approx(r_of(0), 40.0), "0 ms hugs the inner edge");
        assert!(r_of(5) < r_of(20));
        assert!(r_of(20) < r_of(400));
        assert!(r_of(400) < r_of(600) + 1e-3);
        assert!(approx(r_of(u32::MAX), 160.0), "capped at the measured edge");
    }

    #[test]
    fn rim_holds_unmeasured_and_offline() {
        let b = band();
        for star in [s("p", None, false, true), s("p", None, false, false)] {
            let (placed, _) = place(&[star], &b);
            assert!(approx(radius_of(&placed[0]), b.r_rim));
            assert!(approx(placed[0].frac, 1.0));
        }
    }

    // ── band ──

    /// The room the "you" label needs at a given text scale — what
    /// `register::line_h(META)` returns, as arithmetic.
    fn label_h(scale: f32) -> f32 {
        theme::step::META * scale * 1.5
    }

    #[test]
    fn band_rejects_tiny_and_non_finite() {
        assert!(band_for(f32::NAN, label_h(1.0)).is_none());
        assert!(band_for(f32::INFINITY, label_h(1.0)).is_none());
        assert!(band_for(-10.0, label_h(1.0)).is_none());
        assert!(band_for(MIN_SKY_R - 1.0, label_h(1.0)).is_none());
        assert!(band_for(MIN_SKY_R, label_h(1.0)).is_some());
        assert!(band_for(200.0, f32::NAN).is_none());
        assert!(band_for(200.0, f32::INFINITY).is_none());
    }

    #[test]
    fn band_orders_radii() {
        let b = band_for(200.0, label_h(1.0)).unwrap();
        assert!(b.centre_r < b.r_min);
        assert!(b.r_min < b.r_meas);
        assert!(b.r_meas < b.r_rim);
        assert!(approx(b.radius_of_frac(0.0), b.r_min));
        assert!(approx(b.radius_of_frac(1.0), b.r_meas));
        assert!(approx(b.radius_of_frac(9.0), b.r_meas), "frac clamps");
    }

    // ── hit-testing ──

    fn geo(dx: f32, dy: f32, star_r: f32) -> PlacedGeo {
        PlacedGeo {
            idx: 0,
            dx,
            dy,
            frac: 0.5,
            star_r,
            grade: "Good",
        }
    }

    #[test]
    fn hit_finds_nearest_within_reach() {
        let placed = [geo(50.0, 0.0, 7.0), geo(100.0, 0.0, 7.0)];
        // hit_radius(7.0) == 12.0.
        assert_eq!(hit_at(placed.iter().enumerate(), 52.0, 1.0), Some(0));
        assert_eq!(hit_at(placed.iter().enumerate(), 111.0, 0.0), Some(1));
        // Overlapping discs: the nearer centre wins; exact ties keep slot 0.
        let stacked = [geo(0.0, 0.0, 7.0), geo(0.0, 0.0, 7.0)];
        assert_eq!(hit_at(stacked.iter().enumerate(), 3.0, 0.0), Some(0));
    }

    #[test]
    fn hit_misses_outside_reach() {
        let placed = [geo(50.0, 0.0, 7.0), geo(100.0, 0.0, 7.0)];
        assert_eq!(hit_at(placed.iter().enumerate(), 76.0, 0.0), None);
        let empty: [PlacedGeo; 0] = [];
        assert_eq!(hit_at(empty.iter().enumerate(), 0.0, 0.0), None);
    }

    #[test]
    fn hit_radius_floors_small_stars() {
        assert!(approx(hit_radius(star_radius(0)), 9.0));
        assert!(approx(hit_radius(star_radius(3)), 13.8));
    }

    // ── labels ──

    #[test]
    fn accept_labels_keeps_first_drops_overlappers() {
        let r = |x: f32| Rect::from_min_size(pos2(x, 0.0), vec2(30.0, 10.0));
        // A and B collide; C clears A once B is gone — chain does not cascade.
        let got = accept_labels(&[r(0.0), r(28.0), r(56.0)], 0);
        assert_eq!(got, vec![true, false, true]);
        let apart = accept_labels(&[r(0.0), r(100.0)], 0);
        assert_eq!(apart, vec![true, true]);
    }

    #[test]
    fn accept_labels_preaccepted_always_win() {
        let r = |x: f32| Rect::from_min_size(pos2(x, 0.0), vec2(30.0, 10.0));
        // Two colliding pre-accepted rects both pass, and both still block.
        let got = accept_labels(&[r(0.0), r(10.0), r(20.0)], 2);
        assert_eq!(got, vec![true, true, false]);
    }

    #[test]
    fn accept_labels_rejects_non_finite() {
        let bad = Rect::from_min_size(pos2(f32::NAN, 0.0), vec2(10.0, 10.0));
        let ok = Rect::from_min_size(pos2(0.0, 0.0), vec2(10.0, 10.0));
        assert_eq!(accept_labels(&[bad, ok], 1), vec![false, true]);
    }

    #[test]
    fn elide_keeps_short_and_trims_long() {
        assert_eq!(elide("laptop"), "laptop");
        assert_eq!(elide("workstation-of-hassan"), "workstation-o…");
        assert_eq!(elide("workstation-o…").chars().count(), LABEL_MAX_CHARS);
        // Multi-byte names trim on char boundaries, never mid-glyph.
        let arabic = elide("محطة-العمل-الرئيسية");
        assert_eq!(arabic.chars().count(), LABEL_MAX_CHARS);
        assert!(arabic.ends_with('…'));
    }

    // ── sizing and animation ──

    #[test]
    fn desired_height_for_zero_one_many_huge() {
        // An empty sky stays small however wide the window is. It used to take
        // the full ceiling for a single centre mark, pushing the peer table it
        // introduces off the bottom of the screen.
        assert!(approx(desired_height(600.0, 0), 150.0));
        assert!(approx(desired_height(2000.0, 0), 150.0));
        assert!(approx(desired_height(300.0, 0), 150.0));
        assert!(
            approx(desired_height(300.0, 1), 234.0),
            "one peer still gets a readable figure"
        );
        assert!(approx(desired_height(2000.0, 24), 480.0), "capped");
        assert!(
            desired_height(2000.0, usize::MAX) <= 480.0,
            "the cap holds for any crowd"
        );
        // Monotonic in the crowd at a fixed width.
        assert!(desired_height(300.0, 0) <= desired_height(300.0, 1));
        assert!(desired_height(300.0, 1) <= desired_height(300.0, 24));
        assert!(desired_height(2000.0, 1) <= desired_height(2000.0, 24));
    }

    #[test]
    fn desired_height_guards_non_finite_width() {
        assert!(approx(desired_height(f32::NAN, 0), 150.0));
        assert!(approx(desired_height(f32::INFINITY, 3), 242.0));
        assert!(approx(desired_height(-5.0, 0), 150.0));
        assert!(desired_height(f32::NAN, 7).is_finite());
    }

    #[test]
    fn local_t_bounds_and_stagger() {
        assert!(approx(ease_out(0.0), 0.0));
        assert!(approx(ease_out(1.0), 1.0));
        assert!(approx(ease_out(0.5), 0.75));
        assert!(approx(local_t(1.0, 0.0), 1.0));
        assert!(approx(local_t(1.0, 1.0), 1.0), "all revealed by the end");
        assert!(approx(local_t(0.0, 0.5), 0.0));
        assert!(local_t(0.5, 0.0) > local_t(0.5, 1.0), "inner stars first");
    }

    /// A 0 ms peer is drawn at r_min; on a LAN that is the common case. It must
    /// not overlap the centre khatam or the "you" label beneath it — at any
    /// text scale, which is what the fixed 22px band could not promise: at 2.2
    /// the label is 36 tall and a star used to be drawn through it.
    #[test]
    fn the_innermost_orbit_clears_the_centre_and_its_label() {
        for scale in (14..=44).map(|k| k as f32 / 20.0) {
            let label = label_h(scale);
            for avail in [40.0f32, 80.0, 200.0, 600.0] {
                let Some(b) = band_for(avail, label) else {
                    continue;
                };
                let nearest = b.radius_of_frac(radial_frac(0));
                assert!(
                    nearest - star_radius(3) > b.centre_r + label,
                    "a 0 ms peer overlaps the centre at avail={avail}, scale={scale}"
                );
            }
        }
    }

    /// A sky too cramped to keep the innermost orbit clear of the label is no
    /// sky at all — the centre mark is drawn alone rather than drawn over.
    #[test]
    fn a_sky_with_no_room_for_the_label_is_refused_outright() {
        // The label at 2.2 is taller than the whole measured band at the
        // smallest half-extent the sky is drawn into.
        assert!(band_for(MIN_SKY_R, label_h(2.2)).is_none());
        assert!(band_for(600.0, label_h(2.2)).is_some());
    }

    #[test]
    fn star_radius_grows_with_lit_and_caps() {
        assert!(approx(star_radius(0), 4.0));
        assert!(star_radius(0) < star_radius(1));
        assert!(star_radius(1) < star_radius(2));
        assert!(star_radius(2) < star_radius(3));
        assert!(approx(star_radius(3), star_radius(9)), "lit caps at 3");
    }

    // ── what the sky says ──

    fn named<'a>(
        id: &'a str,
        name: &'a str,
        rtt: Option<u32>,
        relayed: bool,
        online: bool,
    ) -> Star<'a> {
        Star {
            id,
            name,
            rtt_ms: rtt,
            relayed,
            online,
            grade: None,
        }
    }

    #[test]
    fn the_sky_announces_one_sentence_naming_every_star() {
        let stars = [
            named("a", "laptop", Some(12), false, true),
            named("b", "phone", Some(240), true, true),
            named("c", "desk", None, false, false),
        ];
        assert_eq!(
            sky_sentence(&stars),
            "peer mesh, 3 peers: laptop direct 12 milliseconds, \
             phone relayed 240 milliseconds, desk offline"
        );
    }

    #[test]
    fn a_sky_of_one_says_it_is_empty_in_the_words_it_paints() {
        assert_eq!(sky_sentence(&[]), "peer mesh: no peers yet");
    }

    #[test]
    fn one_peer_is_a_peer_not_peers() {
        let stars = [named("a", "laptop", Some(1), false, true)];
        assert_eq!(
            sky_sentence(&stars),
            "peer mesh, 1 peer: laptop direct 1 millisecond"
        );
    }

    #[test]
    fn an_offline_star_is_offline_and_carries_no_stale_round_trip() {
        // The same refusal the rim makes on screen: a peer that is not
        // connected has no distance, whatever number it last reported.
        let stale = named("ghost", "desk", Some(5), false, false);
        assert_eq!(star_phrase(&stale), "desk offline");
        assert!(!star_phrase(&stale).contains('5'));
        assert!(!star_phrase(&stale).contains(PATH_DIRECT));
        // ...including one that was relayed when it was last seen.
        let relayed = named("ghost", "desk", Some(400), true, false);
        assert_eq!(star_phrase(&relayed), "desk offline");
    }

    #[test]
    fn an_unmeasured_star_says_so_rather_than_guessing() {
        let star = named("a", "tablet", None, false, true);
        assert_eq!(star_phrase(&star), "tablet direct round-trip unmeasured");
        let relayed = named("a", "tablet", None, true, true);
        assert_eq!(
            star_phrase(&relayed),
            "tablet relayed round-trip unmeasured"
        );
    }

    #[test]
    fn the_units_are_spelled_out_for_a_reader() {
        // "12 ms" is read aloud as "twelve em ess"; the drawn sub-line keeps
        // the short form, the sentence does not.
        let said = star_phrase(&named("a", "laptop", Some(12), false, true));
        assert!(said.ends_with("12 milliseconds"), "{said}");
        assert!(!said.contains(" ms"), "{said}");
    }

    #[test]
    fn a_star_with_no_name_falls_back_to_its_id_then_to_a_word() {
        assert_eq!(
            star_phrase(&named("peer-7f2a", "", Some(9), false, true)),
            "peer-7f2a direct 9 milliseconds"
        );
        assert_eq!(
            star_phrase(&named("  ", "  ", None, false, false)),
            "unnamed peer offline"
        );
    }

    #[test]
    fn the_sentence_names_the_stars_in_the_order_the_sky_draws_them() {
        let stars = [
            named("c", "desk", None, false, false),
            named("b", "tablet", None, false, true),
            named("a", "laptop", Some(10), false, true),
        ];
        let (placed, _) = place(&stars, &band());
        let drawn: Vec<String> = placed.iter().map(|g| star_phrase(&stars[g.idx])).collect();
        let said = sky_sentence(&stars);
        let mut at = 0;
        for phrase in &drawn {
            let found = said[at..]
                .find(phrase.as_str())
                .unwrap_or_else(|| panic!("{phrase} is out of order in {said}"));
            at += found + phrase.len();
        }
    }

    #[test]
    fn the_sentence_admits_the_stars_the_cap_leaves_out() {
        let ids: Vec<String> = (0..30).map(|i| format!("s-{i:02}")).collect();
        let stars: Vec<Star<'_>> = ids.iter().map(|id| s(id, Some(20), false, true)).collect();
        let said = sky_sentence(&stars);
        assert!(said.starts_with("peer mesh, 30 peers:"), "{said}");
        assert!(said.ends_with("6 more not drawn"), "{said}");
        // Exactly the drawn stars are named, and not one more.
        assert_eq!(said.matches(" direct ").count(), MAX_STARS);
    }

    #[test]
    fn the_sentence_holds_for_a_mesh_that_is_entirely_dark() {
        let stars = [
            named("a", "laptop", None, false, false),
            named("b", "desk", None, false, false),
        ];
        // Every star is on the rim, so the tie falls to the id order the sky
        // places them in.
        assert_eq!(
            sky_sentence(&stars),
            "peer mesh, 2 peers: laptop offline, desk offline"
        );
    }

    // ── the keyboard's reach ──

    #[test]
    fn the_cursor_starts_on_the_figure_and_steps_into_it() {
        // A reader arrives at the whole sky and hears the whole sentence; the
        // first step forward lands on the first star the sky drew.
        assert_eq!(step_cursor(None, 3, false, false, false, false), None);
        assert_eq!(step_cursor(None, 3, true, false, false, false), Some(0));
        assert_eq!(step_cursor(Some(0), 3, true, false, false, false), Some(1));
    }

    #[test]
    fn stepping_back_off_the_first_star_returns_to_the_whole_sky() {
        assert_eq!(step_cursor(Some(1), 3, false, true, false, false), Some(0));
        assert_eq!(step_cursor(Some(0), 3, false, true, false, false), None);
        // ...and never wraps around to the far side of the mesh.
        assert_eq!(step_cursor(None, 3, false, true, false, false), None);
    }

    #[test]
    fn the_cursor_stops_at_the_far_end_rather_than_wrapping() {
        assert_eq!(step_cursor(Some(2), 3, true, false, false, false), Some(2));
        assert_eq!(step_cursor(Some(2), 3, false, false, true, false), Some(0));
        assert_eq!(step_cursor(Some(0), 3, false, false, false, true), Some(2));
    }

    #[test]
    fn a_shrunken_mesh_clamps_a_stale_cursor() {
        // The peer list is rebuilt every refresh; a slot from a bigger mesh
        // must land on a star that exists, not past the end of the sky.
        assert_eq!(step_cursor(Some(9), 3, false, false, false, false), Some(2));
        assert_eq!(step_cursor(Some(9), 3, true, false, false, false), Some(2));
        assert_eq!(step_cursor(Some(9), 3, false, true, false, false), Some(1));
        // An empty sky has nothing to point at, whatever was held before.
        assert_eq!(step_cursor(Some(4), 0, true, false, false, false), None);
        assert_eq!(step_cursor(None, 0, false, false, true, false), None);
    }

    #[test]
    fn two_directions_in_one_frame_resolve_the_same_way_every_time() {
        // Both arrows down in a single frame is reachable on a key repeat;
        // whatever it does, it must not depend on which key egui reports first.
        assert_eq!(step_cursor(Some(1), 3, true, true, false, false), Some(2));
        assert_eq!(step_cursor(Some(1), 3, true, true, true, false), Some(0));
        assert_eq!(step_cursor(Some(1), 3, false, false, true, true), Some(0));
    }

    #[test]
    fn weave_strands_anchor_their_ends() {
        let a = pos2(10.0, 10.0);
        let b = pos2(150.0, 90.0);
        let strands = weave_strands(a, b);
        for strand in &strands {
            assert!(strand.len() >= 3);
            assert!(strand[0].distance(a) < 1e-3, "taut at the centre anchor");
            assert!(
                strand[strand.len() - 1].distance(b) < 1e-3,
                "taut at the star anchor"
            );
            assert!(strand.iter().all(|q| q.x.is_finite() && q.y.is_finite()));
        }
        assert_eq!(strands[0].len(), strands[1].len());
        let [e0, e1] = weave_strands(a, a);
        assert!(
            e0.is_empty() && e1.is_empty(),
            "degenerate chord draws nothing"
        );
    }
}
