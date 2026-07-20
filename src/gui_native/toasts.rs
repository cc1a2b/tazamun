//! Bottom-centre toast stack for the native GUI. Replaces the single-slot
//! `Option<Toast>` the window used to hold: a burst of results (a keep-mine
//! resolution fires four IPC steps, each with something to say) used to
//! overwrite itself so only the last line was ever read. Here each toast lives
//! out its own [`TTL`], the stack is bounded to [`MAX_VISIBLE`], and identical
//! text refreshes in place instead of stacking duplicates.
//!
//! The queue is pure data with an injected clock — no egui, no I/O — so it is
//! exhaustively unit-testable; [`draw`] is the only part that touches a `Ui`.

use eframe::egui;

use super::{ornament, theme};

/// What a toast is announcing; drives its seal colour.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Info,
    Good,
    Warn,
    Bad,
}

impl Kind {
    fn color(self) -> egui::Color32 {
        match self {
            Kind::Info => theme::LAPIS,
            Kind::Good => theme::GOOD,
            Kind::Warn => theme::WARN,
            Kind::Bad => theme::BAD,
        }
    }
}

/// How long a toast lives, in seconds.
pub const TTL: f64 = 4.0;
/// How many toasts are shown at once; older ones expire first.
pub const MAX_VISIBLE: usize = 3;

/// Fade/rise-in duration, seconds.
const FADE_IN: f64 = 0.15;
/// Fade-out duration at the end of life, seconds.
const FADE_OUT: f64 = 0.4;
/// Pixels a toast travels upward as it fades in.
const RISE: f32 = 10.0;
/// Vertical pitch between stacked toasts, pixels.
const PITCH: f32 = 40.0;
/// Gap between the bottom edge and the newest toast, pixels.
const BOTTOM_INSET: f32 = 20.0;

struct Toast {
    text: String,
    kind: Kind,
    born: f64,
}

/// A bounded queue of live toasts, oldest first.
#[derive(Default)]
pub struct Queue {
    items: Vec<Toast>,
}

impl Queue {
    /// Adds a toast born at `now` (seconds, from `ui.input(|i| i.time)`).
    /// Identical consecutive text refreshes the existing toast's birth rather
    /// than stacking a duplicate.
    pub fn push(&mut self, text: String, kind: Kind, now: f64) {
        if let Some(existing) = self
            .items
            .iter_mut()
            .find(|t| t.text == text && now - t.born < TTL)
        {
            existing.born = now;
            existing.kind = kind;
            return;
        }
        self.items.push(Toast {
            text,
            kind,
            born: now,
        });
        self.trim();
    }

    /// Drops toasts older than [`TTL`] and trims to [`MAX_VISIBLE`], keeping
    /// the newest. Call once per frame before drawing.
    pub fn expire(&mut self, now: f64) {
        if !now.is_finite() {
            return;
        }
        self.items.retain(|t| now - t.born < TTL);
        self.trim();
    }

    /// True when nothing is live.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Live toast count. Test-only: the window asks whether the stack is empty,
    /// never how tall it is, but the trimming and de-duplication rules are only
    /// assertable by counting.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Drops the oldest entries until the stack fits [`MAX_VISIBLE`].
    fn trim(&mut self) {
        if self.items.len() > MAX_VISIBLE {
            let excess = self.items.len() - MAX_VISIBLE;
            self.items.drain(..excess);
        }
    }
}

/// Draws the stack bottom-centre: newest nearest the bottom, each fading and
/// rising into place, with a seal dot in its kind's colour. Requests a repaint
/// only while at least one toast is live.
pub fn draw(ui: &egui::Ui, q: &Queue, now: f64) {
    if q.is_empty() {
        return;
    }
    // Index 0 sits at the bottom, so walk newest-first.
    for (i, toast) in q.items.iter().rev().enumerate() {
        let age = now - toast.born;
        let t_in = ((age / FADE_IN).min(1.0)) as f32;
        let t_out = (((TTL - age) / FADE_OUT).min(1.0)) as f32;
        let alpha = t_in.min(t_out).max(0.0);
        let rise = (1.0 - t_in) * RISE;
        let color = toast.kind.color();
        let dy = -BOTTOM_INSET - (i as f32) * PITCH + rise;
        egui::Area::new(egui::Id::new(("toast", i)))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, dy))
            .show(ui.ctx(), |ui| {
                ui.set_opacity(alpha);
                egui::Frame::new()
                    .fill(theme::BG1)
                    .stroke(egui::Stroke::new(1.0, color.linear_multiply(0.55)))
                    .corner_radius(20)
                    .inner_margin(egui::Margin::symmetric(14, 9))
                    .shadow(egui::Shadow {
                        offset: [0, 4],
                        blur: 18,
                        spread: 0,
                        color: egui::Color32::from_black_alpha(110),
                    })
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let (seal, _) = ui
                                .allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                            ornament::khatam(ui.painter(), seal.center(), 5.0, color, true);
                            ui.label(
                                egui::RichText::new(&toast.text)
                                    .size(12.5)
                                    .family(theme::fam_medium())
                                    .color(theme::INK),
                            );
                        });
                    });
            });
    }
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_millis(33));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push(q: &mut Queue, text: &str, now: f64) {
        q.push(text.to_string(), Kind::Info, now);
    }

    fn texts(q: &Queue) -> Vec<&str> {
        q.items.iter().map(|t| t.text.as_str()).collect()
    }

    #[test]
    fn push_makes_one_live_toast() {
        let mut q = Queue::default();
        assert!(q.is_empty());
        push(&mut q, "saved", 0.0);
        assert_eq!(q.len(), 1);
        assert!(!q.is_empty());
    }

    #[test]
    fn duplicate_text_refreshes_instead_of_stacking() {
        let mut q = Queue::default();
        push(&mut q, "pulling", 0.0);
        push(&mut q, "pulling", 3.0);
        assert_eq!(q.len(), 1, "identical live text must not stack");

        // The refresh reset the clock, so it outlives the original TTL.
        q.expire(4.5);
        assert_eq!(
            q.len(),
            1,
            "refreshed toast should survive past original TTL"
        );
        q.expire(7.5);
        assert!(
            q.is_empty(),
            "refreshed toast expires TTL after its refresh"
        );
    }

    #[test]
    fn duplicate_after_expiry_is_a_new_toast() {
        let mut q = Queue::default();
        push(&mut q, "pulling", 0.0);
        // Dead by now, so the same text starts a fresh toast rather than
        // reviving a corpse.
        push(&mut q, "pulling", 10.0);
        assert_eq!(q.len(), 2);
        q.expire(10.0);
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn duplicate_refresh_adopts_the_new_kind() {
        let mut q = Queue::default();
        q.push("step".into(), Kind::Info, 0.0);
        q.push("step".into(), Kind::Bad, 1.0);
        assert_eq!(q.len(), 1);
        assert_eq!(q.items[0].kind, Kind::Bad);
    }

    #[test]
    fn expire_drops_only_the_aged() {
        let mut q = Queue::default();
        push(&mut q, "old", 0.0);
        push(&mut q, "new", 3.0);
        q.expire(4.5);
        assert_eq!(texts(&q), vec!["new"]);
    }

    #[test]
    fn expire_keeps_a_toast_exactly_at_its_birth() {
        let mut q = Queue::default();
        push(&mut q, "fresh", 12.0);
        q.expire(12.0);
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn trim_keeps_the_newest_max_visible() {
        let mut q = Queue::default();
        for (i, name) in ["a", "b", "c", "d", "e"].iter().enumerate() {
            push(&mut q, name, i as f64 * 0.1);
        }
        assert_eq!(q.len(), MAX_VISIBLE);
        assert_eq!(texts(&q), vec!["c", "d", "e"]);
    }

    #[test]
    fn expire_trims_as_well_as_ages() {
        let mut q = Queue::default();
        // Bypass push's trim to prove expire enforces the bound on its own.
        for (i, name) in ["a", "b", "c", "d"].iter().enumerate() {
            q.items.push(Toast {
                text: (*name).to_string(),
                kind: Kind::Info,
                born: i as f64 * 0.1,
            });
        }
        assert_eq!(q.len(), 4);
        q.expire(0.5);
        assert_eq!(texts(&q), vec!["b", "c", "d"]);
    }

    #[test]
    fn is_empty_transitions_both_ways() {
        let mut q = Queue::default();
        assert!(q.is_empty());
        push(&mut q, "hello", 1.0);
        assert!(!q.is_empty());
        q.expire(1.0 + TTL - 0.001);
        assert!(!q.is_empty());
        q.expire(1.0 + TTL);
        assert!(q.is_empty());
    }

    #[test]
    fn non_finite_now_drops_nothing() {
        let mut q = Queue::default();
        push(&mut q, "a", 0.0);
        push(&mut q, "b", 1.0);
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            q.expire(bad);
            assert_eq!(
                q.len(),
                2,
                "non-finite now must be treated as no time passed"
            );
        }
    }
}
