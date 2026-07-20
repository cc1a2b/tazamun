//! Vertical rhythm and the running head for the native GUI: a 4px baseline
//! grid so views state their spacing in one currency (`space` in grid units,
//! `snap` for painter math) instead of scattered hand-picked pixel gaps; the
//! manuscript running head — the trail of names locating the reader, a
//! hairline rule beneath, a folio mark at the outer edge — as the house
//! answer to a breadcrumb bar; and the page-foot rule that closes a view.
//! Pure presentation over `theme` and `ornament`; zero I/O, no clock, total
//! for degenerate inputs.

use eframe::egui;
use egui::{FontFamily, FontId, Rangef, Rect, Sense};
use egui::{pos2, vec2};

use super::{ornament, theme};

/// The vertical unit every view's spacing is a multiple of.
pub const BASELINE: f32 = 4.0;

/// Running-head height: text band + rule + air, on the grid.
const HEAD_H: f32 = BASELINE * 6.0;
/// The band the trail and folio center in; the rule sits below it.
const TEXT_BAND: f32 = 18.0;
/// Crisp hairline offset from the head's top (drawn at the half-pixel).
const RULE_OFFSET: f32 = 20.0;
/// Width of the diamond separator cell between trail names.
const SEP_W: f32 = 16.0;
const SEP_R: f32 = 1.8;
/// Gap reserved between the elided trail and the folio mark.
const FOLIO_GAP: f32 = 12.0;
/// Front-elision marker. U+2026 is present in Inter (Regular/Medium/SemiBold)
/// and Hack — verified against the shipped font files; no tofu.
const ELIDE_MARK: &str = "…";

/// Snaps `y` down onto the baseline grid. Returns `y` unchanged if it is not
/// finite.
pub fn snap(y: f32) -> f32 {
    if !y.is_finite() {
        return y;
    }
    (y / BASELINE).floor() * BASELINE
}

/// Vertical space measured in baseline units rather than raw pixels, so a
/// view's rhythm is stated in the same currency everywhere.
pub fn space(ui: &mut egui::Ui, units: u16) {
    if units == 0 {
        return;
    }
    ui.add_space(units_to_px(units));
}

/// A manuscript running head: the trail of names locating the reader (outer
/// first, e.g. `["photos", "Conflicts"]`), a hairline rule beneath, and an
/// optional folio mark set at the outer edge. Draws nothing for an empty
/// trail. The last name is where the reader *is* (INK, semibold); earlier
/// names are where they came from (DIM), separated by the house diamond. At
/// narrow widths the trail elides from the front behind an ellipsis, keeping
/// the current name legible; the folio owns its edge and is never overlapped.
pub fn running_head(ui: &mut egui::Ui, trail: &[&str], folio: Option<&str>) {
    if trail.is_empty() {
        return;
    }
    let width = ui.available_width().max(0.0);
    let (rect, _) = ui.allocate_exact_size(vec2(width, HEAD_H), Sense::hover());
    if !rect.is_finite() || rect.width() < 1.0 {
        return;
    }
    let p = ui.painter().with_clip_rect(rect);
    let text_mid = rect.top() + TEXT_BAND * 0.5;

    // Folio first: it owns the outer edge and the trail elides into what
    // remains. Reservation is capped at half the head so a pathological folio
    // clips (from its front) rather than starving the trail.
    let mut trail_right = rect.right();
    if let Some(mark) = folio
        && !mark.is_empty()
    {
        let galley = p.layout_no_wrap(
            mark.to_owned(),
            FontId::new(10.0, FontFamily::Monospace),
            theme::FAINT,
        );
        let size = galley.size();
        let zone_w = size.x.min(rect.width() * 0.5);
        let zone = Rect::from_min_max(
            pos2(rect.right() - zone_w, rect.top()),
            pos2(rect.right(), rect.top() + TEXT_BAND),
        );
        p.with_clip_rect(zone).galley(
            pos2(rect.right() - size.x, text_mid - size.y * 0.5),
            galley,
            theme::FAINT,
        );
        trail_right = zone.left() - FOLIO_GAP;
    }

    let budget = trail_right - rect.left();
    if budget >= 1.0 {
        let zone = Rect::from_min_max(
            pos2(rect.left(), rect.top()),
            pos2(trail_right, rect.top() + TEXT_BAND),
        );
        let tp = p.with_clip_rect(zone);
        let last = trail.len() - 1;
        let galleys: Vec<_> = trail
            .iter()
            .enumerate()
            .map(|(i, name)| {
                if i == last {
                    tp.layout_no_wrap(
                        (*name).to_owned(),
                        FontId::new(11.5, theme::fam_semibold()),
                        theme::INK,
                    )
                } else {
                    tp.layout_no_wrap(
                        (*name).to_owned(),
                        FontId::new(11.5, theme::fam_medium()),
                        theme::DIM,
                    )
                }
            })
            .collect();
        let marker = tp.layout_no_wrap(
            ELIDE_MARK.to_owned(),
            FontId::new(11.5, theme::fam_medium()),
            theme::DIM,
        );
        let widths: Vec<f32> = galleys.iter().map(|g| g.size().x).collect();
        let (start, lead_marker) = elide_plan(&widths, SEP_W, marker.size().x, budget);

        let mut items = Vec::new();
        if lead_marker {
            items.push((marker, theme::DIM));
        }
        for (i, galley) in galleys.into_iter().enumerate() {
            if i < start {
                continue;
            }
            let color = if i == last { theme::INK } else { theme::DIM };
            items.push((galley, color));
        }
        let mut x = rect.left();
        for (k, (galley, color)) in items.into_iter().enumerate() {
            if k > 0 {
                ornament::diamond(
                    &tp,
                    pos2(x + SEP_W * 0.5, text_mid),
                    SEP_R,
                    theme::GOLD.linear_multiply(0.45),
                );
                x += SEP_W;
            }
            let size = galley.size();
            tp.galley(pos2(x, text_mid - size.y * 0.5), galley, color);
            x += size.x;
        }
    }

    let rule_y = (rect.top() + RULE_OFFSET).floor() + 0.5;
    p.hline(
        Rangef::new(rect.left(), rect.right()),
        rule_y,
        theme::stroke_faint(),
    );
}

/// A page-foot rule: the mirror of the running head's rule, closing a view.
/// Centre-weighted with a diamond, matching `ornament::rule_with_diamond`.
pub fn foot_rule(ui: &mut egui::Ui) {
    ornament::rule_with_diamond(ui, theme::GOLD.linear_multiply(0.6));
}

/// Baseline units to pixels. Total by construction: `u16::MAX * BASELINE`
/// is far inside f32 range, so the result is always finite and non-negative.
fn units_to_px(units: u16) -> f32 {
    f32::from(units) * BASELINE
}

/// Given per-name widths, the separator cell width, the elision-marker width,
/// and the horizontal budget, decide which trail suffix to draw: returns
/// (index of the first drawn name, whether the marker leads it). Non-finite
/// input degrades to drawing everything — the clip rect still bounds paint.
/// When even the marker plus the last name cannot fit, the last name alone is
/// kept (and clips) so where-you-are stays the survivor.
fn elide_plan(widths: &[f32], sep_w: f32, marker_w: f32, budget: f32) -> (usize, bool) {
    if widths.is_empty() {
        return (0, false);
    }
    if !budget.is_finite()
        || !sep_w.is_finite()
        || !marker_w.is_finite()
        || widths.iter().any(|w| !w.is_finite())
    {
        return (0, false);
    }
    let width_from = |start: usize| -> f32 {
        let seps = (widths.len() - start).saturating_sub(1) as f32 * sep_w;
        widths[start..].iter().sum::<f32>() + seps
    };
    if width_from(0) <= budget {
        return (0, false);
    }
    for start in 1..widths.len() {
        if marker_w + sep_w + width_from(start) <= budget {
            return (start, true);
        }
    }
    (widths.len() - 1, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snap_is_identity_on_exact_multiples() {
        assert_eq!(snap(0.0), 0.0);
        assert_eq!(snap(4.0), 4.0);
        assert_eq!(snap(48.0), 48.0);
        assert_eq!(snap(-8.0), -8.0);
    }

    #[test]
    fn snap_floors_between_multiples() {
        assert_eq!(snap(1.0), 0.0);
        assert_eq!(snap(5.0), 4.0);
        assert_eq!(snap(7.999), 4.0);
        assert_eq!(snap(11.2), 8.0);
    }

    #[test]
    fn snap_floors_negatives_toward_negative_infinity() {
        assert_eq!(snap(-0.001), -4.0);
        assert_eq!(snap(-1.0), -4.0);
        assert_eq!(snap(-4.1), -8.0);
    }

    #[test]
    fn snap_passes_non_finite_through() {
        assert!(snap(f32::NAN).is_nan());
        assert_eq!(snap(f32::INFINITY), f32::INFINITY);
        assert_eq!(snap(f32::NEG_INFINITY), f32::NEG_INFINITY);
    }

    #[test]
    fn snap_survives_extremes() {
        assert!(snap(f32::MAX).is_finite());
        assert!(snap(f32::MIN).is_finite());
        assert!(snap(f32::MAX) <= f32::MAX);
    }

    #[test]
    fn units_convert_on_the_grid() {
        assert_eq!(units_to_px(0), 0.0);
        assert_eq!(units_to_px(1), BASELINE);
        assert_eq!(units_to_px(6), 24.0);
    }

    #[test]
    fn units_extreme_count_stays_finite_and_non_negative() {
        let px = units_to_px(u16::MAX);
        assert!(px.is_finite());
        assert!(px >= 0.0);
        assert_eq!(px, 65_535.0 * BASELINE);
    }

    #[test]
    fn plan_full_trail_when_it_fits() {
        assert_eq!(elide_plan(&[10.0, 10.0, 10.0], 5.0, 8.0, 100.0), (0, false));
        assert_eq!(elide_plan(&[10.0, 10.0, 10.0], 5.0, 8.0, 40.0), (0, false));
    }

    #[test]
    fn plan_drops_from_the_front_behind_the_marker() {
        // Full trail is 40; marker(8) + sep(5) + [10, 10] + sep(5) = 38.
        assert_eq!(elide_plan(&[10.0, 10.0, 10.0], 5.0, 8.0, 39.0), (1, true));
        // marker(8) + sep(5) + [10] = 23.
        assert_eq!(elide_plan(&[10.0, 10.0, 10.0], 5.0, 8.0, 24.0), (2, true));
    }

    #[test]
    fn plan_keeps_the_last_name_alone_when_nothing_fits() {
        assert_eq!(elide_plan(&[10.0, 10.0, 10.0], 5.0, 8.0, 20.0), (2, false));
        assert_eq!(elide_plan(&[50.0], 5.0, 8.0, 10.0), (0, false));
    }

    #[test]
    fn plan_empty_trail_is_a_no_op() {
        assert_eq!(elide_plan(&[], 5.0, 8.0, 100.0), (0, false));
    }

    #[test]
    fn plan_degrades_on_non_finite_input() {
        assert_eq!(elide_plan(&[10.0], 5.0, 8.0, f32::NAN), (0, false));
        assert_eq!(elide_plan(&[f32::NAN, 10.0], 5.0, 8.0, 50.0), (0, false));
        assert_eq!(elide_plan(&[10.0], f32::INFINITY, 8.0, 50.0), (0, false));
        assert_eq!(elide_plan(&[10.0], 5.0, f32::NAN, 50.0), (0, false));
    }
}
