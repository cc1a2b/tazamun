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

use std::sync::Arc;

use eframe::egui;
use egui::{Color32, FontId, Galley, Pos2, Rect, Sense, Stroke};
use egui::{pos2, vec2};

use super::{ornament, telemetry, theme};
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
/// Vertical room the centre mark's own "you" label occupies beneath it.
const CENTRE_LABEL_BAND: f32 = 22.0;
/// Breathing room between the centre's label and the nearest star.
const CENTRE_CLEARANCE: f32 = 6.0;
/// How much of the draw-on is spent staggering outer stars behind inner ones.
const STAGGER: f32 = 0.35;
const WEAVE_WAVELENGTH: f32 = 13.0;
const WEAVE_AMP: f32 = 1.5;

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
/// index into `stars` of the peer under the pointer, if any.
pub fn sky(ui: &mut egui::Ui, rect: egui::Rect, stars: &[Star<'_>], t: f32) -> Option<usize> {
    if !rect.is_finite() || !rect.is_positive() || !t.is_finite() || t <= 0.0 {
        return None;
    }
    let ease = ease_out(t.clamp(0.0, 1.0));
    let centre = rect.center();
    let avail = rect.width().min(rect.height()) * 0.5 - CANVAS_MARGIN;
    let p = ui.painter().with_clip_rect(rect);
    let Some(band) = band_for(avail) else {
        // Too small for a sky: the centre mark alone, still deliberate.
        ornament::khatam(
            &p,
            centre,
            (avail * 0.5).clamp(4.0, 12.0),
            theme::GOLD.linear_multiply(ease),
            true,
        );
        return None;
    };

    // The rim: a dotted ring beyond the measured scale, where offline and
    // unmeasured stars sit.
    for shape in egui::Shape::dashed_line(
        &ring_points(centre, band.r_rim),
        Stroke::new(1.0, theme::FAINT.linear_multiply(0.30 * ease)),
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
        theme::GOLD.linear_multiply(ease),
        true,
    );
    let you = p.layout_no_wrap(
        "you".to_string(),
        FontId::new(10.5, theme::fam_medium()),
        theme::DIM.linear_multiply(ease),
    );
    let you_size = you.size();
    let you_pos = pos2(centre.x - you_size.x * 0.5, centre.y + band.centre_r + 4.0);
    p.galley(you_pos, you, theme::DIM);

    if stars.is_empty() {
        // A sky of one is the first-run case, not an error.
        let note = p.layout_no_wrap(
            "no peers yet".to_string(),
            FontId::new(10.5, theme::fam_medium()),
            theme::FAINT.linear_multiply(ease),
        );
        let size = note.size();
        p.galley(
            pos2(centre.x - size.x * 0.5, rect.bottom() - size.y - 6.0),
            note,
            theme::FAINT,
        );
        return None;
    }

    // Reference rings at the grade thresholds, so radius has stated meaning.
    let good_ms = GRADE_GOOD_MAX_RTT_MS as u32;
    let poor_ms = GRADE_POOR_MIN_RTT_MS as u32;
    let ring_marks = [good_ms, poor_ms].map(|ms| {
        let r = band.radius_of_frac(radial_frac(ms));
        p.add(egui::Shape::closed_line(
            ring_points(centre, r),
            Stroke::new(1.0, theme::INK.linear_multiply(0.035 * ease)),
        ));
        (r, format!("{ms} ms"))
    });

    let (placed, overflow) = place(stars, &band);
    let screen: Vec<(Pos2, f32)> = placed
        .iter()
        .map(|g| {
            (
                pos2(centre.x + g.dx, centre.y + g.dy),
                local_t(ease, g.frac),
            )
        })
        .collect();

    // Hover before drawing, so the ring paints with its star this frame.
    let resp = ui.interact(rect, ui.id().with("constellation-sky"), Sense::hover());
    let pointer = resp.hover_pos().filter(|q| rect.contains(*q));
    let hovered_slot = pointer.and_then(|q| {
        hit_at(
            placed
                .iter()
                .enumerate()
                .filter(|&(slot, _)| screen[slot].1 > 0.0),
            q.x - centre.x,
            q.y - centre.y,
        )
    });

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
                    Stroke::new(1.0, theme::DIM.linear_multiply(0.65 * local)),
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
                        Stroke::new(1.0, theme::DIM.linear_multiply(0.8 * local)),
                    ));
                }
            } else {
                // The taut band: a faint core with two strands woven over it.
                p.line_segment(
                    [a, b],
                    Stroke::new(1.0, theme::GOLD.linear_multiply(0.18 * local)),
                );
                for strand in weave_strands(a, b) {
                    p.add(egui::Shape::line(
                        strand,
                        Stroke::new(1.0, theme::GOLD.linear_multiply(0.45 * local)),
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
                color.linear_multiply((0.06 + 0.04 * lit) * local),
            );
            ornament::khatam(
                &p,
                pos,
                r,
                color.linear_multiply((0.7 + 0.1 * lit) * local),
                true,
            );
        } else {
            // Unlit: outline only, dim, no line — present but not pretending.
            ornament::khatam(
                &p,
                pos,
                r,
                theme::FAINT.linear_multiply(0.55 * local),
                false,
            );
        }
        if hovered_slot == Some(slot) {
            ornament::khatam(&p, pos, r + 4.0, theme::GOLD_HI.linear_multiply(0.8), false);
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
            FontId::new(10.0, egui::FontFamily::Monospace),
            theme::FAINT.linear_multiply(ease),
        );
        let size = g.size();
        let min = pos2(rect.right() - size.x - 8.0, rect.bottom() - size.y - 6.0);
        rects.push(Rect::from_min_size(min, size));
        p.galley(min, g, theme::FAINT);
        pre = 2;
    }
    let mut jobs: Vec<Vec<(Pos2, Arc<Galley>)>> = Vec::new();
    for (slot, g) in placed.iter().enumerate() {
        let (pos, local) = screen[slot];
        if local <= 0.15 {
            continue;
        }
        let star = &stars[g.idx];
        let name_color = if star.online { theme::INK } else { theme::DIM };
        let name = p.layout_no_wrap(
            elide(star.name),
            FontId::new(10.5, theme::fam_medium()),
            name_color.linear_multiply(0.9 * local),
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
            FontId::new(9.0, egui::FontFamily::Monospace),
            theme::FAINT.linear_multiply(local),
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
            FontId::new(8.5, egui::FontFamily::Monospace),
            theme::FAINT.linear_multiply(0.8 * ease),
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
                p.galley(at, galley, theme::DIM);
            }
        }
    }

    hovered_slot.map(|slot| placed[slot].idx)
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
    if !width.is_finite() || width <= 0.0 {
        return floor;
    }
    (width * 0.62).clamp(floor, 480.0)
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

fn band_for(avail: f32) -> Option<Band> {
    if !avail.is_finite() || avail < MIN_SKY_R {
        return None;
    }
    let centre_r = (avail * 0.16).clamp(9.0, 15.0);
    // A peer at 0 ms sits exactly at `r_min`, and on a LAN that is the ordinary
    // case rather than a corner one. The centre mark carries its "you" label
    // underneath, so the innermost orbit has to clear the khatam, that label,
    // and the widest a star can be drawn — otherwise the nearest peer lands on
    // top of the centre with its round-trip written across it.
    let r_min = centre_r + CENTRE_LABEL_BAND + star_radius(3) + CENTRE_CLEARANCE;
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
    let mut order: Vec<usize> = (0..stars.len()).collect();
    order.sort_by(|&a, &b| class_key(&stars[a]).cmp(&class_key(&stars[b])));
    let overflow = stars.len().saturating_sub(MAX_STARS);
    order.truncate(MAX_STARS);
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
        "Good" => theme::GOOD,
        "Fair" => theme::WARN,
        "Poor" => theme::BAD,
        _ => theme::FAINT,
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

    #[test]
    fn band_rejects_tiny_and_non_finite() {
        assert!(band_for(f32::NAN).is_none());
        assert!(band_for(f32::INFINITY).is_none());
        assert!(band_for(-10.0).is_none());
        assert!(band_for(MIN_SKY_R - 1.0).is_none());
        assert!(band_for(MIN_SKY_R).is_some());
    }

    #[test]
    fn band_orders_radii() {
        let b = band_for(200.0).unwrap();
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
        assert!(approx(desired_height(600.0, 0), 372.0));
        assert!(approx(desired_height(300.0, 0), 186.0));
        assert!(
            approx(desired_height(300.0, 1), 234.0),
            "floor lifts with peers"
        );
        assert!(approx(desired_height(2000.0, 24), 480.0), "capped");
        assert!(
            approx(desired_height(100.0, usize::MAX), 326.0),
            "crowd floor caps"
        );
        assert!(desired_height(300.0, 0) <= desired_height(300.0, 1));
        assert!(desired_height(300.0, 1) <= desired_height(300.0, 24));
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

    #[test]
    fn the_innermost_orbit_clears_the_centre_and_its_label() {
        // A 0 ms peer is drawn at r_min; on a LAN that is the common case. It
        // must not overlap the centre khatam or the "you" label beneath it.
        for avail in [40.0f32, 80.0, 200.0, 600.0] {
            let Some(b) = band_for(avail) else { continue };
            let nearest = b.radius_of_frac(radial_frac(0));
            assert!(
                nearest - star_radius(3) > b.centre_r + CENTRE_LABEL_BAND,
                "a 0 ms peer overlaps the centre at avail={avail}"
            );
        }
    }

    #[test]
    fn star_radius_grows_with_lit_and_caps() {
        assert!(approx(star_radius(0), 4.0));
        assert!(star_radius(0) < star_radius(1));
        assert!(star_radius(1) < star_radius(2));
        assert!(star_radius(2) < star_radius(3));
        assert!(approx(star_radius(3), star_radius(9)), "lit caps at 3");
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
