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

use super::{ornament, register, theme};

/// What a toast is announcing; drives its seal colour.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Info,
    Good,
    Warn,
    Bad,
}

impl Kind {
    /// Colour in this window means custody and nothing else, so a toast borrows
    /// the state it is reporting on rather than owning a second vocabulary.
    fn custody(self) -> theme::Custody {
        match self {
            Kind::Info => theme::Custody::Peer,
            Kind::Good => theme::Custody::Good,
            Kind::Warn => theme::Custody::Stale,
            Kind::Bad => theme::Custody::Blocked,
        }
    }
}

/// How long a toast lives, in seconds.
const TTL: f64 = 4.0;
/// How many toasts are shown at once; older ones expire first.
const MAX_VISIBLE: usize = 3;

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

/// Draws the stack bottom-centre, newest nearest the bottom. Each toast is a
/// square slip laid on the register: a hairline border, a custody-coloured bar
/// down its leading edge, the khatam seal, and the line itself. Requests a
/// repaint only while at least one toast is live.
pub fn draw(ui: &egui::Ui, q: &Queue, now: f64) {
    if q.is_empty() {
        return;
    }
    let pitch = pitch();
    // Index 0 sits at the bottom, so walk newest-first.
    for (i, toast) in q.items.iter().rev().enumerate() {
        let age = now - toast.born;
        let entered = ramp(age, theme::dur(theme::motion::STATE));
        let remaining = ramp(TTL - age, theme::dur(theme::motion::CEREMONY));
        let opacity = entered.min(remaining);
        let color = toast.kind.custody().color();
        let dy = -theme::space::XL - (i as f32) * pitch + (1.0 - entered) * theme::space::M;
        egui::Area::new(egui::Id::new(("toast", i)))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, dy))
            .show(ui.ctx(), |ui| {
                ui.set_opacity(opacity);
                let slip = egui::Frame::new()
                    .fill(theme::bg_raise())
                    .stroke(egui::Stroke::new(theme::RULE_W, theme::rule_emphasis()))
                    .corner_radius(egui::CornerRadius::same(theme::R_NONE))
                    .inner_margin(egui::Margin::symmetric(
                        theme::space::L as i8,
                        theme::space::M as i8,
                    ))
                    .shadow(theme::elevation::overlay())
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.x = theme::space::M;
                        ui.horizontal(|ui| {
                            let side = theme::sized(theme::step::LABEL);
                            let (seal, _) = ui
                                .allocate_exact_size(egui::vec2(side, side), egui::Sense::hover());
                            // Circumradius, so the star's points land inside the
                            // cell rather than on the text beside it.
                            ornament::khatam(ui.painter(), seal.center(), side * 0.4, color, true);
                            ui.label(
                                egui::RichText::new(&toast.text)
                                    .font(theme::font(theme::step::LABEL, theme::fam_medium()))
                                    .color(theme::ink()),
                            );
                        });
                    });
                let edge = slip.response.rect;
                ui.painter().rect_filled(
                    egui::Rect::from_min_max(
                        edge.left_top(),
                        egui::pos2(edge.left() + theme::RULE_ACCENT_W, edge.bottom()),
                    ),
                    egui::CornerRadius::same(theme::R_NONE),
                    color,
                );
            });
    }
    // This cadence drives expiry, not decoration: without it a slip would stay
    // on screen until the next input event, so reduced motion must not stop it.
    ui.ctx().request_repaint_after(theme::motion::CADENCE);
}

/// The tallest a slip can be drawn: the frame's symmetric margin plus the line
/// box its text lays out into. The seal beside the text is cut from the label
/// step, which that line box already covers.
fn slip_h() -> f32 {
    slip_box(register::line_h(theme::step::LABEL))
}

/// Pure: the slip around one line box.
fn slip_box(line_h: f32) -> f32 {
    line_h + theme::space::M * 2.0
}

/// How far one slip sits above the next. The density sets the rhythm, but a
/// slip is not a register entry and must never be allowed to grow past the step
/// between two of them — a stack that overlaps itself hides the line under it.
fn pitch() -> f32 {
    stack_pitch(theme::density().row_h(), slip_h())
}

/// Pure: the density's own step, floored at the slip plus a gap.
fn stack_pitch(row_h: f32, slip_h: f32) -> f32 {
    (row_h + theme::space::M).max(slip_h + theme::space::S)
}

/// A 0..=1 ramp over `over` seconds. A zero-length ramp — what [`theme::dur`]
/// returns under reduced motion — is already finished, so the slip appears at
/// once instead of rising into place.
fn ramp(elapsed: f64, over: f32) -> f32 {
    if over <= 0.0 || !elapsed.is_finite() {
        return 1.0;
    }
    (elapsed / f64::from(over)).clamp(0.0, 1.0) as f32
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

    // ── the stack's own measure ──

    /// Every text scale the control can actually reach.
    fn scales() -> impl Iterator<Item = f32> {
        (14..=44).map(|k| k as f32 / 20.0)
    }

    fn line_box(step: f32, scale: f32) -> f32 {
        step * scale * 1.5
    }

    /// Mirrors `theme::Density::row_h`: the base at the text scale, which the
    /// density deliberately stops following below 100%.
    fn row_h(base: f32, scale: f32) -> f32 {
        base * scale.clamp(1.0, 2.2)
    }

    /// Three slips are stacked bottom-up at one pitch. If the pitch is shorter
    /// than a slip, the newest toast is drawn over the one before it and the
    /// burst this queue exists to show becomes unreadable again.
    #[test]
    fn the_stack_never_laps_over_itself() {
        for base in [24.0, 28.0, 34.0] {
            for scale in scales() {
                let slip = slip_box(line_box(theme::step::LABEL, scale));
                let pitch = stack_pitch(row_h(base, scale), slip);
                assert!(
                    pitch > slip,
                    "density {base} at {scale}: {pitch} pitch under a {slip} slip"
                );
            }
        }
    }

    #[test]
    fn the_stack_grows_with_the_text_scale() {
        for base in [24.0, 28.0, 34.0] {
            let small = stack_pitch(
                row_h(base, 0.7),
                slip_box(line_box(theme::step::LABEL, 0.7)),
            );
            let large = stack_pitch(
                row_h(base, 2.2),
                slip_box(line_box(theme::step::LABEL, 2.2)),
            );
            assert!(large > small, "density {base}: {small} to {large} is flat");
        }
    }

    /// A roomier density spaces the stack out too, exactly as it does the
    /// register — the floor must not flatten the three densities into one.
    #[test]
    fn the_stack_follows_the_density() {
        let slip = slip_box(line_box(theme::step::LABEL, 2.2));
        let mut prev: Option<f32> = None;
        for base in [24.0, 28.0, 34.0] {
            let pitch = stack_pitch(row_h(base, 2.2), slip);
            if let Some(was) = prev {
                assert!(
                    pitch > was,
                    "density {base} did not open the stack: {pitch}"
                );
            }
            prev = Some(pitch);
        }
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
