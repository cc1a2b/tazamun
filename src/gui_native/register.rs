//! The register: ruled entries under a column ruler, with a folio margin and a
//! seal for anything under lease.
//!
//! This is the structural replacement for the stacked card. A card puts a box
//! around one record and lets the next box disagree with it about where its
//! columns are; a register rules every entry against one ruler, so a column of
//! paths, sizes or custody can actually be read down. Five views in this
//! application are tabular — files, peers, versions, conflicts, audit — and all
//! five were stacks of boxes.
//!
//! Three things are deliberately built into the primitive rather than left to
//! each caller:
//!
//! * **[`Content`] has no "just draw nothing" case.** A view must say whether it
//!   is showing entries, is empty, is still loading, or *failed to read*. A read
//!   that failed used to fall through to the empty state, so an unreadable
//!   conflicts directory rendered as "no conflicts waiting" — the interface
//!   telling the user their preserved bytes were resolved when it had not
//!   managed to look. [`Content::Failed`] makes that unrepresentable.
//! * **Virtualisation.** Entries are drawn through [`egui::ScrollArea::show_rows`],
//!   so a thousand-entry register costs the same as a twenty-entry one. The
//!   price is one uniform row height: a register that carries two lines per
//!   entry declares it once, through [`Register::stacked_rows`], and every
//!   entry is ruled at that height.
//! * **The margin.** The folio number and the custody seal share one gutter, so
//!   every register in the application has its marks in the same place.
//!
//! Pure layout maths lives in [`lanes`] and [`measure`] and is unit-tested;
//! everything else is painting. Nothing here is sized by a pixel count that a
//! text scale can outgrow — heights come from the type scale through
//! [`line_h`], because the one that did not was sliced through the middle.

use eframe::egui;
use egui::{Align, Color32, Layout, Rect, Response, Sense, UiBuilder, Vec2, pos2};

use super::ornament;
use super::theme::{self, Custody};

/// The narrowest the margin carrying the folio number and the custody seal is
/// ever ruled.
const MARGIN_W: f32 = 34.0;
/// How much of the margin a folio must be able to fill: three figures of the
/// tabular face, which IBM Plex Mono advances at 0.6em each.
const FOLIO_EMS: f32 = 1.8;
/// How tall one laid-out line of type stands against its nominal step. Every
/// family stack in this window ends in Plex Sans Arabic, whose line box is
/// 1.5em where the Latin faces are 1.3em, and a path or a session name may be
/// Arabic — so anything sized for the Latin metric would slice the Arabic one.
const LINE_BOX: f32 = 1.5;
/// The shortest an empty register is drawn, so the watermark has a page.
const EMPTY_MIN_H: f32 = 96.0;
/// How many skeleton entries a loading register shows.
const LOADING_ROWS: usize = 6;

/// The width of the folio-and-seal margin at the type scale in force.
fn margin_w() -> f32 {
    (theme::sized(theme::step::META) * FOLIO_EMS + theme::space::M).max(MARGIN_W)
}

/// The radius of the seal drawn in the margin: half the caption line, so the
/// mark keeps its proportion to the folio beside it at every text scale.
fn seal_r() -> f32 {
    theme::sized(theme::step::META) * 0.5
}

// ─── columns ─────────────────────────────────────────────────────────────────

/// How a column claims horizontal space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ColW {
    /// This many points at 100% text, scaled with the reader's text size.
    ///
    /// A fixed column holds type, so it has to grow with the type: at 2.2 a
    /// literal 80pt column held about a third of the text it holds at 100% and
    /// the cell clipped the rest. The number is read as a measure of its
    /// content, not of the screen.
    Fixed(f32),
    /// A share of whatever is left after the fixed columns are served.
    Flex(f32),
}

/// One column of the ruler.
#[derive(Clone, Debug)]
pub struct Col {
    head: &'static str,
    width: ColW,
    align: Align,
    sort_key: Option<&'static str>,
}

impl Col {
    /// A left-aligned column.
    pub fn new(head: &'static str, width: ColW) -> Self {
        Self {
            head,
            width,
            align: Align::LEFT,
            sort_key: None,
        }
    }

    /// Right-aligned — every quantity, so the digits stack.
    pub fn right(mut self) -> Self {
        self.align = Align::RIGHT;
        self
    }

    /// Makes the header a sort control under `key`.
    pub fn sortable(mut self, key: &'static str) -> Self {
        self.sort_key = Some(key);
        self
    }
}

/// The narrowest a flexible column may be squeezed to: roughly a dozen
/// characters of the face such a column carries, so the record stays
/// identifiable rather than becoming two letters and an ellipsis.
fn flex_min() -> f32 {
    theme::sized(theme::step::LABEL) * 7.0
}

/// Resolves column widths against the space available for the columns
/// themselves — the margin and the inter-column gaps are already deducted by
/// the caller. Fixed columns are served first; whatever remains is split
/// between the flexible ones by weight. When the fixed columns alone overflow,
/// every column is scaled down proportionally rather than letting the last one
/// fall off the edge.
///
/// Pure: this is the part worth testing, and it has no `Ui` in it.
pub fn lanes(cols: &[Col], avail: f32) -> Vec<f32> {
    if cols.is_empty() {
        return Vec::new();
    }
    let avail = avail.max(0.0);
    let k = theme::scale();
    let declared: f32 = cols
        .iter()
        .filter_map(|c| match c.width {
            ColW::Fixed(w) => Some(w.max(0.0) * k),
            ColW::Flex(_) => None,
        })
        .sum();
    let fixed = declared;
    let weight: f32 = cols
        .iter()
        .filter_map(|c| match c.width {
            ColW::Flex(w) => Some(w.max(0.0)),
            ColW::Fixed(_) => None,
        })
        .sum();

    if fixed >= avail {
        // Over-subscribed: shrink everything by the same factor so the ruler
        // stays proportional instead of truncating the trailing columns.
        let shrink = if fixed > 0.0 { avail / fixed } else { 0.0 };
        return cols
            .iter()
            .map(|c| match c.width {
                ColW::Fixed(w) => w.max(0.0) * k * shrink,
                ColW::Flex(_) => 0.0,
            })
            .collect();
    }

    // A flexible column carries the record's own name — the path, the peer,
    // the event. It must not be squeezed to nothing so that fixed columns can
    // all have their full measure: at a large text scale the fixed columns
    // scale too, and without this floor they take the whole ruler and the
    // column the entry is actually identified by collapses to two letters.
    let flex_cols = cols
        .iter()
        .filter(|c| matches!(c.width, ColW::Flex(_)))
        .count();
    let floor = flex_min() * flex_cols as f32;
    let fixed = if flex_cols > 0 && avail - fixed < floor {
        // Give the flexible columns their floor and let the fixed ones share
        // what is left, in proportion, rather than the last one falling off.
        (avail - floor).max(0.0)
    } else {
        fixed
    };
    let spare = avail - fixed;
    // When the floor bit, the fixed columns share what the floor left them.
    let squeeze = if declared > 0.0 {
        (fixed / declared).min(1.0)
    } else {
        1.0
    };
    cols.iter()
        .map(|c| match c.width {
            ColW::Fixed(w) => w.max(0.0) * k * squeeze,
            ColW::Flex(w) => {
                if weight > 0.0 {
                    spare * (w.max(0.0) / weight)
                } else {
                    0.0
                }
            }
        })
        .collect()
}

// ─── vertical measure ────────────────────────────────────────────────────────

/// The height of one laid-out line of type at `step`, at the scale in force.
/// The number a fixed pixel count keeps getting wrong: a step is the nominal
/// size, the line box is what the row has to clear.
pub fn line_h(step: f32) -> f32 {
    line_box(step, theme::scale())
}

fn line_box(step: f32, scale: f32) -> f32 {
    step * scale * LINE_BOX
}

/// The vertical measure of an entry whose cells stack lines of type.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stack {
    /// The type itself: every line box plus the gap between one line and the
    /// next.
    pub content: f32,
    /// The ruled height of the entry — the content with the leading a one-line
    /// entry already carries at this density.
    pub row: f32,
}

/// Measures an entry that stacks one line of type per entry in `steps`: a name
/// over its hint is `&[step::LABEL, step::META]`.
///
/// Everything here is derived from the type scale, so the measure is right at
/// 0.7 and at 2.2 rather than at the one scale it was tried on. A stack of one
/// [`theme::step::LABEL`] line measures exactly the density's own row height —
/// that is the reference the leading is taken from.
pub fn stacked(steps: &[f32]) -> Stack {
    measure(steps, theme::scale(), theme::density().row_h())
}

/// Pure: [`stacked`] against an explicit text scale and the height the density
/// rules an ordinary one-line entry at. No `Ui` and no globals, because this is
/// the arithmetic that must not regress.
fn measure(steps: &[f32], scale: f32, one_line: f32) -> Stack {
    // Filtered rather than trusted: a step that is not a positive, finite size
    // must cost the entry nothing, or one nonsense value would rule the whole
    // register — and therefore the scroll body — at an infinite height.
    let lines: f32 = steps
        .iter()
        .map(|s| line_box(*s, scale))
        .filter(|h| h.is_finite() && *h > 0.0)
        .sum();
    // What egui puts between two stacked labels, and therefore what a cell
    // drawing a name over a hint actually costs.
    let gaps = theme::space::S * steps.len().saturating_sub(1) as f32;
    let content = lines + gaps;
    // The air a one-line entry already has at this density and scale — taken
    // from the label line, which is the tallest type an ordinary entry carries
    // — so a stacked entry breathes like the register it sits in instead of by
    // a number of its own.
    let lead = (one_line - line_box(theme::step::LABEL, scale)).max(0.0);
    Stack {
        content,
        row: (content + lead).max(one_line),
    }
}

// ─── content ─────────────────────────────────────────────────────────────────

/// What the register has to show. There is deliberately no variant meaning
/// "nothing happened" — a caller must distinguish an empty source from one it
/// could not read.
pub enum Content<'a> {
    /// This many entries, drawn through the row closure.
    Entries(usize),
    /// The source was read and is genuinely empty.
    Empty {
        /// What is not here.
        title: &'a str,
        /// What the reader can do about it, if anything.
        hint: &'a str,
    },
    /// The source has not been read yet.
    Loading,
    /// The read failed. Never renders as empty, and always offers a retry.
    Failed {
        /// What could not be read, in the user's words.
        what: &'a str,
        /// The underlying error.
        because: &'a str,
    },
}

/// What the user did to the register this frame.
#[derive(Default)]
pub struct Out {
    /// The header of a sortable column was clicked.
    pub sort: Option<&'static str>,
    /// The retry offered by [`Content::Failed`] was clicked.
    pub retry: bool,
}

// ─── the register ────────────────────────────────────────────────────────────

/// A ruled register. Build it, then [`Register::show`] it.
pub struct Register<'a> {
    id_salt: &'a str,
    cols: &'a [Col],
    margin: bool,
    sort: Option<(&'a str, bool)>,
    max_height: Option<f32>,
    row: Option<f32>,
    stack: Option<f32>,
}

impl<'a> Register<'a> {
    /// A register over `cols`. `id_salt` must be unique within the view.
    pub fn new(id_salt: &'a str, cols: &'a [Col]) -> Self {
        Self {
            id_salt,
            cols,
            margin: true,
            sort: None,
            max_height: None,
            row: None,
            stack: None,
        }
    }

    /// Drops the folio-and-seal margin, for short registers where a folio would
    /// be noise (a settings table, a key/value block).
    pub fn no_margin(mut self) -> Self {
        self.margin = false;
        self
    }

    /// Marks `key` as the active sort, `descending` deciding the arrow.
    pub fn sorted_by(mut self, key: &'a str, descending: bool) -> Self {
        self.sort = Some((key, descending));
        self
    }

    /// Caps the scrolling body so the register can sit above other content.
    pub fn max_height(mut self, h: f32) -> Self {
        self.max_height = Some(h);
        self
    }

    /// Declares that every entry stacks one line of type per entry in `steps` —
    /// a name over its hint is `&[theme::step::LABEL, theme::step::META]` — and
    /// rules the register at the height those lines need, from [`stacked`].
    ///
    /// The taller height is still applied *uniformly*: the body is virtualised
    /// through [`egui::ScrollArea::show_rows`], which can only skip the entries
    /// it is not drawing while every entry is the same height. So this is a
    /// declaration about the register, never about one entry — a register whose
    /// entries disagree about their height is not a register this primitive can
    /// draw.
    ///
    /// The declared stack is also the band a cell's content is centred in, so a
    /// name over a hint sits on the middle of its rule rather than hanging from
    /// the top of it — and text in those cells is elided rather than wrapped,
    /// since a line the declaration did not pay for would be sliced by the rule
    /// below it.
    pub fn stacked_rows(mut self, steps: &[f32]) -> Self {
        let s = stacked(steps);
        self.stack = Some(s.content);
        self.row_height(s.row)
    }

    /// Rules the register at an explicit uniform row height.
    ///
    /// The escape hatch, for an entry carrying something that is not type — a
    /// thumbnail, a sparkline. For type, use [`Register::stacked_rows`]: a
    /// pixel count that happens to fit at 100% is sliced through the middle at
    /// 115%, which is the defect this exists to stop repeating.
    pub fn row_height(mut self, h: f32) -> Self {
        self.row = Some(h.max(0.0));
        self
    }

    /// The uniform height of one entry.
    fn row_h(&self) -> f32 {
        self.row.unwrap_or_else(|| theme::density().row_h())
    }

    /// Draws the ruler, then the body. `entry` is called once per visible entry
    /// and receives an [`Entry`] positioned on that row.
    pub fn show(
        self,
        ui: &mut egui::Ui,
        content: Content<'_>,
        mut entry: impl FnMut(&mut Entry<'_>),
    ) -> Out {
        let mut out = Out::default();
        let pad = theme::density().cell_pad();
        let margin_w = if self.margin { margin_w() } else { 0.0 };
        let gaps = pad * self.cols.len().saturating_sub(1) as f32;
        let full = ui.available_width();
        let widths = lanes(self.cols, (full - margin_w - gaps).max(0.0));

        out.sort = self.ruler(ui, &widths, margin_w, pad);

        match content {
            Content::Entries(n) => {
                let row_h = self.row_h();
                let mut area = egui::ScrollArea::vertical()
                    .id_salt(self.id_salt)
                    .auto_shrink([false, true]);
                if let Some(h) = self.max_height {
                    area = area.max_height(h);
                }
                area.show_rows(ui, row_h, n, |ui, range| {
                    for i in range {
                        let (rect, resp) = ui.allocate_exact_size(
                            Vec2::new(ui.available_width(), row_h),
                            Sense::click(),
                        );
                        let mut e = Entry {
                            ui,
                            index: i,
                            rect,
                            resp,
                            widths: &widths,
                            margin_w,
                            pad,
                            stack: self.stack,
                            lane: 0,
                            custody: None,
                            selected: false,
                            painted: false,
                            mark: true,
                            folio: None,
                            heavy_rule: false,
                            indent: 0.0,
                        };
                        entry(&mut e);
                        e.finish();
                    }
                });
            }
            Content::Empty { title, hint } => self.empty(ui, title, hint),
            Content::Loading => self.loading(ui, &widths, margin_w, pad),
            Content::Failed { what, because } => {
                out.retry = self.failed(ui, what, because);
            }
        }
        out
    }

    /// The column ruler: tracked headings over a divider rule.
    fn ruler(
        &self,
        ui: &mut egui::Ui,
        widths: &[f32],
        margin_w: f32,
        pad: f32,
    ) -> Option<&'static str> {
        let mut clicked = None;
        // A ruler with no headings is a blank ruled band above the entries: a
        // key/value register has columns but nothing to call them.
        if self.cols.iter().all(|c| c.head.trim().is_empty()) {
            return None;
        }
        // The caption step plus its gap, floored at the line box a heading
        // actually occupies: past about 1.5× text scale the gap alone no longer
        // clears the heading, and a tracked header would cross its own rule.
        let h = (theme::sized(theme::step::META) + theme::space::M).max(line_h(theme::step::META));
        let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), h), Sense::hover());
        let mut x = rect.left() + margin_w;

        for (col, w) in self.cols.iter().zip(widths) {
            let cell = Rect::from_min_size(pos2(x, rect.top()), Vec2::new(*w, rect.height()));
            if *w > 1.0 {
                self.heading(ui, col, cell, &mut clicked);
            }
            x += w + pad;
        }

        let y = theme::snap(rect.bottom());
        ui.painter().hline(
            rect.x_range(),
            y,
            egui::Stroke::new(theme::RULE_W, theme::rule_divider()),
        );
        ui.add_space(theme::space::S);
        clicked
    }

    fn heading(
        &self,
        ui: &mut egui::Ui,
        col: &Col,
        cell: Rect,
        clicked: &mut Option<&'static str>,
    ) {
        let active = self
            .sort
            .and_then(|(k, d)| col.sort_key.filter(|s| *s == k).map(|_| d));
        let color = if active.is_some() {
            theme::ink_muted()
        } else {
            theme::ink_faint()
        };

        let mut job = egui::text::LayoutJob {
            halign: col.align,
            ..Default::default()
        };
        job.append(
            col.head,
            0.0,
            egui::TextFormat {
                font_id: theme::font(theme::step::META, theme::fam_medium()),
                color,
                extra_letter_spacing: theme::HEADER_TRACKING,
                ..Default::default()
            },
        );
        let galley = ui.painter().layout_job(job);

        let anchor = match col.align {
            Align::RIGHT => pos2(cell.right(), cell.center().y),
            Align::Center => pos2(cell.center().x, cell.center().y),
            _ => pos2(cell.left(), cell.center().y),
        };
        let align2 = match col.align {
            Align::RIGHT => egui::Align2::RIGHT_CENTER,
            Align::Center => egui::Align2::CENTER_CENTER,
            _ => egui::Align2::LEFT_CENTER,
        };
        let text_rect = align2.anchor_size(anchor, galley.size());

        if let Some(key) = col.sort_key {
            let hit = text_rect.expand2(Vec2::new(theme::space::M, theme::space::S));
            let resp = ui.interact(hit, ui.id().with(("sort", key)), Sense::click());
            if resp.clicked() {
                *clicked = Some(key);
            }
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                let y = theme::snap(text_rect.bottom() + 1.0);
                ui.painter().hline(
                    text_rect.x_range(),
                    y,
                    egui::Stroke::new(theme::RULE_W, theme::gold_deep()),
                );
            }
            if resp.has_focus() {
                focus_ring(ui.painter(), hit);
            }
        }

        ui.painter().galley(text_rect.min, galley, color);

        // The active sort is marked with the house diamond rather than an
        // arrow glyph: the mark rides above or below the baseline to say which
        // way the column runs.
        if let Some(desc) = active {
            let dy = if desc { 2.5 } else { -2.5 };
            let cx = match col.align {
                Align::RIGHT => text_rect.left() - theme::space::S,
                _ => text_rect.right() + theme::space::S,
            };
            ornament::diamond(
                ui.painter(),
                pos2(cx, text_rect.center().y + dy),
                2.4,
                theme::gold(),
            );
        }
    }

    fn empty(&self, ui: &mut egui::Ui, title: &str, hint: &str) {
        let (title_h, hint_h) = (line_h(theme::step::BODY), line_h(theme::step::META));
        // How far the two centres stand apart: the nominal step and its gap,
        // floored at the two half line boxes, so the hint cannot ride up into
        // the title's descenders when the type is scaled up.
        let apart =
            (theme::sized(theme::step::BODY) + theme::space::M).max((title_h + hint_h) * 0.5);
        let h = (theme::density().row_h() * 4.0)
            .max(EMPTY_MIN_H)
            .max(apart + title_h + hint_h);
        let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), h), Sense::hover());
        ornament::watermark(ui.painter(), rect, theme::ink_faint());
        let mut cursor = rect.center().y - apart * 0.5;
        ui.painter().text(
            pos2(rect.center().x, cursor),
            egui::Align2::CENTER_CENTER,
            title,
            theme::font(theme::step::BODY, theme::fam_medium()),
            theme::ink_muted(),
        );
        cursor += apart;
        ui.painter().text(
            pos2(rect.center().x, cursor),
            egui::Align2::CENTER_CENTER,
            hint,
            theme::font(theme::step::META, egui::FontFamily::Proportional),
            theme::ink_faint(),
        );
    }

    /// Loading looks like entries not yet written: ruled blanks in the lanes the
    /// real entries will occupy, so the page does not jump when they land.
    fn loading(&self, ui: &mut egui::Ui, widths: &[f32], margin_w: f32, pad: f32) {
        // The register's own row height, not the density's: a skeleton drawn at
        // a different pitch would make the page jump when the entries land.
        let row_h = self.row_h();
        let t = ui.input(|i| i.time) as f32;
        for r in 0..LOADING_ROWS {
            let (rect, _) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), row_h), Sense::hover());
            let mut x = rect.left() + margin_w;
            // Each lane breathes a little out of phase so the block reads as
            // settling rather than blinking.
            let phase = if theme::reduced_motion() {
                0.5
            } else {
                ((t * 1.6 + r as f32 * 0.35).sin() * 0.5 + 0.5).clamp(0.0, 1.0)
            };
            for w in widths {
                if *w > 8.0 {
                    let y = theme::snap(rect.center().y);
                    let end = x + (w * 0.62).min(*w);
                    ui.painter().hline(
                        x..=end,
                        y,
                        egui::Stroke::new(
                            theme::RULE_W,
                            theme::mix(theme::rule_hair(), theme::ink_faint(), phase * 0.55),
                        ),
                    );
                }
                x += w + pad;
            }
            rule_under(ui.painter(), rect);
        }
        if !theme::reduced_motion() {
            ui.ctx().request_repaint_after(theme::motion::CADENCE);
        }
    }

    /// A failed read. Loud, specific, and never mistakable for an empty source.
    fn failed(&self, ui: &mut egui::Ui, what: &str, because: &str) -> bool {
        let mut retry = false;
        let c = Custody::Blocked.color();
        let frame = egui::Frame::new()
            .fill(theme::wash::of(c, theme::wash::TINT))
            .corner_radius(theme::R_NONE)
            .inner_margin(egui::Margin::symmetric(
                theme::space::L as i8,
                theme::space::L as i8,
            ));
        let resp = frame.show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                egui::RichText::new(what)
                    .font(theme::font(theme::step::BODY, theme::fam_semibold()))
                    .color(theme::ink()),
            );
            ui.add_space(theme::space::S);
            ui.label(
                egui::RichText::new(because)
                    .font(theme::font(theme::step::META, theme::fam_mono()))
                    .color(theme::ink_muted()),
            );
            ui.add_space(theme::space::M);
            retry = ui
                .button(egui::RichText::new("Try again").color(theme::ink()))
                .clicked();
        });
        // The leading edge bar names the severity without a coloured box.
        let r = resp.response.rect;
        ui.painter().rect_filled(
            Rect::from_min_size(r.min, Vec2::new(theme::RULE_ACCENT_W + 1.0, r.height())),
            theme::R_NONE,
            c,
        );
        retry
    }
}

// ─── one entry ───────────────────────────────────────────────────────────────

/// A single ruled entry. Cells are filled left to right by [`Entry::cell`];
/// the margin mark and the row's ground are painted by [`Entry::finish`].
pub struct Entry<'u> {
    ui: &'u mut egui::Ui,
    index: usize,
    rect: Rect,
    resp: Response,
    widths: &'u [f32],
    margin_w: f32,
    pad: f32,
    stack: Option<f32>,
    lane: usize,
    custody: Option<Custody>,
    selected: bool,
    painted: bool,
    mark: bool,
    folio: Option<usize>,
    heavy_rule: bool,
    indent: f32,
}

impl Entry<'_> {
    /// This entry's zero-based position in the register.
    pub fn index(&self) -> usize {
        self.index
    }

    /// The row's response — click, hover, context menu.
    pub fn response(&self) -> &Response {
        &self.resp
    }

    /// Declares who holds this entry. Drives the margin seal and the row tint.
    /// An entry with no custody is an ordinary record and stays uncoloured.
    pub fn custody(&mut self, c: Custody) -> &mut Self {
        self.custody = Some(c);
        self
    }

    /// Marks the entry as the current selection.
    pub fn selected(&mut self, yes: bool) -> &mut Self {
        self.selected = yes;
        self
    }

    /// Leaves the margin blank. For entries that are not records in their own
    /// right — a group heading, a continuation line — where a folio number
    /// would be counting the wrong thing.
    pub fn no_mark(&mut self) -> &mut Self {
        self.mark = false;
        self
    }

    /// The folio this entry carries. Supplied by the caller because only the
    /// caller knows which rows are records: the register draws just the visible
    /// slice, so a counter kept here would restart at the top of every scroll.
    /// Without it an entry falls back to its row position.
    pub fn folio(&mut self, n: usize) -> &mut Self {
        self.folio = Some(n);
        self
    }

    /// Closes the entry with a divider rather than a hairline, to separate one
    /// run of entries from the next.
    pub fn divider(&mut self) -> &mut Self {
        self.heavy_rule = true;
        self
    }

    /// Indents the entry's first lane, for a line subordinate to the one above
    /// it (a version under its file).
    pub fn indent(&mut self, steps: f32) -> &mut Self {
        self.indent = steps * theme::space::XL;
        self
    }

    /// Fills the next lane. Called once per column, in column order; calling it
    /// more times than there are columns is a no-op rather than a panic, so a
    /// mismatched view degrades instead of bringing the window down.
    pub fn cell(&mut self, add: impl FnOnce(&mut egui::Ui)) -> &mut Self {
        self.ensure_ground();
        let Some(w) = self.widths.get(self.lane).copied() else {
            return self;
        };
        let lead = if self.lane == 0 { self.indent } else { 0.0 };
        let x = self.rect.left()
            + self.margin_w
            + self.widths[..self.lane].iter().sum::<f32>()
            + self.pad * self.lane as f32
            + lead;
        let w = (w - lead).max(0.0);
        self.lane += 1;
        if w <= 1.0 {
            return self;
        }
        let cell = Rect::from_min_size(pos2(x, self.rect.top()), Vec2::new(w, self.rect.height()));
        // A declared stack lays out in a band of its own height centred in the
        // entry: egui runs a nested vertical block down from the top of the
        // rect it is given, so without this a name over a hint would hang from
        // the top rule with all its air underneath.
        let band = match self.stack {
            Some(h) => Rect::from_center_size(cell.center(), Vec2::new(w, h.min(cell.height()))),
            None => cell,
        };
        let layout = Layout::left_to_right(Align::Center);
        let mut child = self.ui.new_child(
            UiBuilder::new()
                .max_rect(band)
                .layout(layout)
                .id_salt(("cell", self.index, self.lane)),
        );
        if self.stack.is_some() {
            // A declared stack is a promise about how many lines the entry is
            // ruled for, so text that would wrap past them is elided instead:
            // an ellipsis is a decision, a line sliced by the next rule is not.
            child.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
        }
        // Clipped to the whole entry rather than to the band: a long path still
        // may not bleed into the next column, but a cell that outgrows its
        // declaration spends the entry's leading before it is ever sliced.
        child.set_clip_rect(cell.intersect(self.ui.clip_rect()));
        add(&mut child);
        self
    }

    /// Fills the next lane with a right-aligned quantity in the tabular face.
    /// The one call every numeric column should use, so figures stack.
    pub fn figure(&mut self, text: &str, color: Color32) -> &mut Self {
        self.cell(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    egui::RichText::new(text)
                        .font(theme::font(theme::step::DATA, theme::fam_mono()))
                        .color(color),
                );
            });
        })
    }

    /// Paints the row ground before any cell draws over it. Idempotent.
    fn ensure_ground(&mut self) {
        if self.painted {
            return;
        }
        self.painted = true;
        let p = self.ui.painter();

        if self.selected {
            p.rect_filled(
                self.rect,
                theme::R_NONE,
                theme::wash::of(theme::gold(), theme::wash::SELECT),
            );
            p.rect_filled(
                Rect::from_min_size(
                    self.rect.min,
                    Vec2::new(theme::RULE_ACCENT_W, self.rect.height()),
                ),
                theme::R_NONE,
                theme::gold(),
            );
        } else if self.resp.hovered() {
            p.rect_filled(self.rect, theme::R_NONE, theme::bg_raise());
        } else if let Some(c) = self
            .custody
            .filter(|c| matches!(c, Custody::Stale | Custody::Blocked | Custody::Quarantined))
        {
            // Only the states that need attention tint their row. Held and free
            // are the normal condition of a register and stay quiet.
            p.rect_filled(
                self.rect,
                theme::R_NONE,
                theme::wash::of(c.color(), theme::wash::GHOST),
            );
        }
    }

    /// Rules the entry off and stamps its margin.
    fn finish(&mut self) {
        self.ensure_ground();
        let p = self.ui.painter();
        if self.heavy_rule {
            p.hline(
                self.rect.x_range(),
                theme::snap(self.rect.bottom()),
                egui::Stroke::new(theme::RULE_W, theme::rule_divider()),
            );
        } else {
            rule_under(p, self.rect);
        }

        if self.margin_w > 0.0 && self.mark {
            let cx = self.rect.left() + self.margin_w * 0.5;
            let cy = self.rect.center().y;
            match self.custody.filter(|c| c.sealed()) {
                // A held entry is sealed. That is the whole meaning of the mark,
                // so nothing else in the register may use it.
                Some(c) => ornament::khatam(p, pos2(cx, cy), seal_r(), c.color(), true),
                None => {
                    p.text(
                        pos2(self.rect.left() + self.margin_w - theme::space::M, cy),
                        egui::Align2::RIGHT_CENTER,
                        self.folio.unwrap_or(self.index + 1).to_string(),
                        theme::font(theme::step::META, theme::fam_mono()),
                        theme::ink_faint(),
                    );
                }
            }
        }

        if self.resp.has_focus() {
            focus_ring(self.ui.painter(), self.rect.shrink(1.0));
        }
    }
}

// ─── surfaces and marks ──────────────────────────────────────────────────────

/// A well: recessed and square, for matter that is *quoted* rather than
/// authored — an invite ticket, an activity line, command output.
pub fn well() -> egui::Frame {
    egui::Frame::new()
        .fill(theme::bg_sunken())
        .stroke(egui::Stroke::new(theme::RULE_W, theme::rule_hair()))
        .corner_radius(theme::R_CONTROL)
        .inner_margin(egui::Margin::same(theme::space::L as i8))
}

/// The only surface permitted to float: menus, dialogs, toasts.
pub fn overlay() -> egui::Frame {
    egui::Frame::new()
        .fill(theme::bg_chrome())
        .stroke(egui::Stroke::new(theme::RULE_W, theme::rule_emphasis()))
        .corner_radius(theme::R_OVERLAY)
        .inner_margin(egui::Margin::same(theme::space::XL as i8))
        .shadow(theme::elevation::modal())
}

/// A section mark: the engraved serif title over an emphasis rule. Replaces the
/// gold tick that opened every block with the same gesture.
pub fn heading(ui: &mut egui::Ui, title: &str) {
    ui.add_space(theme::space::L);
    ui.label(
        egui::RichText::new(title)
            .font(theme::font(theme::step::TITLE, theme::fam_serif()))
            .color(theme::ink()),
    );
    ui.add_space(theme::space::XS);
    let w = ui.available_width();
    let (r, _) = ui.allocate_exact_size(Vec2::new(w, theme::RULE_W), Sense::hover());
    ui.painter().hline(
        r.x_range(),
        theme::snap(r.center().y),
        egui::Stroke::new(theme::RULE_W, theme::rule_emphasis()),
    );
    ui.add_space(theme::space::S);
}

/// A tag: the word in its custody colour inside a hairline box. Square — the
/// rounded capsule is the dashboard vocabulary this design replaces, and it
/// carried state, identity, counts and transport all at the same weight.
pub fn tag(ui: &mut egui::Ui, text: &str, color: Color32) {
    egui::Frame::new()
        .fill(theme::wash::of(color, theme::wash::TINT))
        .stroke(egui::Stroke::new(theme::RULE_W, theme::alpha(color, 120)))
        .corner_radius(theme::R_NONE)
        .inner_margin(egui::Margin::symmetric(theme::space::S as i8, 1))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(text)
                    .font(theme::font(theme::step::META, theme::fam_medium()))
                    .color(color),
            );
        });
}

/// A status mark: the house diamond in the state's colour, ringed when live.
/// Replaces the glowing dot. The khatam is not used here — that mark means
/// custody and nothing else.
pub fn status_mark(ui: &mut egui::Ui, color: Color32) {
    let side = theme::sized(theme::step::META) + theme::space::XS;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(side, side), Sense::hover());
    ornament::diamond(ui.painter(), rect.center(), side * 0.26, color);
}

/// A blank waiting to be written: one ruled line that breathes. Loading in a
/// register should look like an entry not yet entered, not like a grey pill.
pub fn skeleton_line(ui: &mut egui::Ui, width: f32) {
    let h = theme::sized(theme::step::BODY);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, h), Sense::hover());
    let phase = if theme::reduced_motion() {
        0.5
    } else {
        let t = ui.input(|i| i.time) as f32;
        ((t * 1.6).sin() * 0.5 + 0.5).clamp(0.0, 1.0)
    };
    ui.painter().hline(
        rect.x_range(),
        theme::snap(rect.center().y + 2.0),
        egui::Stroke::new(
            theme::RULE_W,
            theme::mix(theme::rule_hair(), theme::ink_faint(), phase * 0.55),
        ),
    );
    if !theme::reduced_motion() {
        ui.ctx().request_repaint_after(theme::motion::CADENCE);
    }
}

/// A band calling out one condition across the register — offline, refused,
/// contested. Square, with the custody colour carried on its leading edge
/// rather than as a rounded pastel box.
pub fn notice(ui: &mut egui::Ui, kind: Custody, add: impl FnOnce(&mut egui::Ui)) {
    let c = kind.color();
    let resp = egui::Frame::new()
        .fill(theme::wash::of(c, theme::wash::TINT))
        .corner_radius(theme::R_NONE)
        .inner_margin(egui::Margin::symmetric(
            theme::space::L as i8,
            theme::space::M as i8,
        ))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
    let r = resp.response.rect;
    ui.painter().rect_filled(
        Rect::from_min_size(r.min, Vec2::new(theme::RULE_ACCENT_W + 1.0, r.height())),
        theme::R_NONE,
        c,
    );
}

fn rule_under(p: &egui::Painter, rect: Rect) {
    p.hline(
        rect.x_range(),
        theme::snap(rect.bottom()),
        egui::Stroke::new(theme::RULE_W, theme::rule_hair()),
    );
}

/// The focus ring. Drawn outside the widget so it never shifts layout, in
/// [`theme::focus`] so it clears both the page and the raised ground.
pub fn focus_ring(p: &egui::Painter, rect: Rect) {
    p.rect_stroke(
        rect.expand(2.0),
        egui::CornerRadius::same(theme::R_CONTROL),
        egui::Stroke::new(2.0, theme::focus()),
        egui::StrokeKind::Outside,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols(spec: &[ColW]) -> Vec<Col> {
        spec.iter().map(|w| Col::new("h", *w)).collect()
    }

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    /// A name over its hint: the two-line settings cell the register is ruled
    /// for, and the one that used to be sliced through the middle.
    const NAME_OVER_HINT: [f32; 2] = [theme::step::LABEL, theme::step::META];

    /// The three densities' one-line bases, restated as data. Every scale and
    /// density can then be exercised without writing the process-global style
    /// the rest of the window is reading.
    const DENSITY_BASES: [f32; 3] = [24.0, 28.0, 34.0];

    /// Mirrors `theme::Density::row_h`: the base at the text scale, which the
    /// density deliberately stops following below 100%.
    fn one_line(base: f32, scale: f32) -> f32 {
        base * scale.clamp(1.0, 2.2)
    }

    /// Every text scale the control can actually reach — it is stored in
    /// twentieths, so this is the whole set, not a sample of it.
    fn scales() -> impl Iterator<Item = f32> {
        (14..=44).map(|k| k as f32 / 20.0)
    }

    /// A line box has to clear the tallest face in the family stack. Plex's
    /// Latin faces are 1.3em; Plex Sans Arabic, which every stack in this
    /// window ends in, is 1.5em — and an Arabic path is an ordinary entry here.
    #[test]
    fn a_line_box_clears_the_tallest_face_in_the_stack() {
        for scale in scales() {
            let step = theme::step::BODY;
            let b = line_box(step, scale);
            assert!(
                b >= step * scale * 1.3,
                "latin metric not cleared at {scale}"
            );
            assert!(
                approx(b, step * scale * 1.5),
                "arabic metric lost at {scale}"
            );
        }
    }

    /// The reference the leading is taken from: one line of label type measures
    /// exactly the entry the density already rules, so an unstacked register is
    /// untouched by any of this.
    #[test]
    fn one_line_of_label_measures_the_density_row() {
        for base in DENSITY_BASES {
            for scale in scales() {
                let one = one_line(base, scale);
                let m = measure(&[theme::step::LABEL], scale, one);
                assert!(approx(m.row, one), "{base} at {scale}: {m:?} vs {one}");
            }
        }
    }

    /// The defect, stated as arithmetic: two lines of type do not fit the rule
    /// one line is ruled at, at any density or text scale.
    #[test]
    fn a_stacked_entry_is_taller_than_a_one_line_entry() {
        for base in DENSITY_BASES {
            for scale in scales() {
                let one = one_line(base, scale);
                let two = measure(&NAME_OVER_HINT, scale, one);
                assert!(
                    two.row > one,
                    "{base} at {scale}: two lines ruled at {} inside {one}",
                    two.row
                );
            }
        }
    }

    /// The whole point: whatever the scale, the rule clears the type inside it,
    /// with exactly the leading an ordinary entry has. 2.2 is the scale the
    /// fixed row height failed hardest at.
    #[test]
    fn an_entry_is_never_shorter_than_its_content() {
        for base in DENSITY_BASES {
            for scale in scales() {
                for lines in 1..=4 {
                    let steps = vec![theme::step::LABEL; lines];
                    let one = one_line(base, scale);
                    let m = measure(&steps, scale, one);
                    assert!(m.row >= m.content, "{lines} lines at {scale}/{base}: {m:?}");
                    assert!(m.row >= one, "{lines} lines at {scale}/{base}: {m:?}");
                    let air = one - line_box(theme::step::LABEL, scale);
                    assert!(
                        approx(m.row - m.content, air),
                        "{lines} lines at {scale}/{base}: {m:?} leaves {} air, not {air}",
                        m.row - m.content
                    );
                }
            }
        }
    }

    /// Text size has to move the rule, or the control is a lie the second time
    /// a row carries two lines.
    #[test]
    fn the_measure_grows_with_the_text_scale() {
        for base in DENSITY_BASES {
            let mut prev: Option<(f32, Stack, Stack)> = None;
            for scale in scales() {
                let one = one_line(base, scale);
                let two = measure(&NAME_OVER_HINT, scale, one);
                let single = measure(&[theme::step::LABEL], scale, one);
                if let Some((was, prev_two, prev_single)) = prev {
                    assert!(
                        two.row > prev_two.row,
                        "{base}: stacked row flat from {was} to {scale}"
                    );
                    assert!(
                        two.content > prev_two.content,
                        "{base}: stacked content flat from {was} to {scale}"
                    );
                    // A one-line entry can only follow the density, which holds
                    // still below 100% on purpose.
                    assert!(
                        single.row >= prev_single.row,
                        "{base}: one-line row shrank from {was} to {scale}"
                    );
                }
                prev = Some((scale, two, single));
            }
        }
    }

    /// A roomier density must rule a roomier stacked entry, exactly as it does
    /// a one-line one.
    #[test]
    fn the_measure_grows_with_density() {
        for scale in scales() {
            let mut prev: Option<f32> = None;
            for base in DENSITY_BASES {
                let m = measure(&NAME_OVER_HINT, scale, one_line(base, scale));
                if let Some(was) = prev {
                    assert!(m.row > was, "density {base} at {scale}: {} vs {was}", m.row);
                }
                prev = Some(m.row);
            }
        }
    }

    /// A register that declares nothing is ruled exactly as it was.
    #[test]
    fn no_steps_measures_an_ordinary_entry() {
        for base in DENSITY_BASES {
            let one = one_line(base, 1.0);
            let m = measure(&[], 1.0, one);
            assert!(approx(m.content, 0.0));
            assert!(approx(m.row, one), "{m:?} vs {one}");
        }
    }

    /// A nonsense step must fall back to an ordinary entry rather than rule the
    /// register at a NaN height, which would take the whole body with it.
    #[test]
    fn a_nonsense_step_falls_back_to_an_ordinary_entry() {
        let one = one_line(28.0, 1.0);
        for step in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -40.0] {
            let m = measure(&[step, theme::step::META], 1.0, one);
            assert!(m.row.is_finite() && m.content.is_finite(), "{step}: {m:?}");
            assert!(m.row >= one, "{step}: {m:?}");
        }
    }

    /// The explicit override wins over the density, and cannot go negative.
    #[test]
    fn an_explicit_row_height_wins() {
        let r = Register::new("t", &[]).row_height(96.0);
        assert!(approx(r.row_h(), 96.0));
        let r = Register::new("t", &[]).row_height(-12.0);
        assert!(approx(r.row_h(), 0.0));
        let r = Register::new("t", &[]).row_height(f32::NAN);
        assert!(r.row_h().is_finite());
        // Declared first, overridden after: the last word is the caller's.
        let r = Register::new("t", &[])
            .stacked_rows(&NAME_OVER_HINT)
            .row_height(200.0);
        assert!(approx(r.row_h(), 200.0));
    }

    /// The declaration reaches the rule: the register is ruled at the measured
    /// height, and the band its cells lay out in fits inside that rule.
    #[test]
    fn stacked_rows_rules_the_register_at_the_measured_height() {
        let r = Register::new("t", &[]).stacked_rows(&NAME_OVER_HINT);
        let Some(band) = r.stack else {
            panic!("a stacked register must carry its content band");
        };
        assert!(band > 0.0 && band.is_finite(), "band {band}");
        assert!(r.row_h() > band, "ruled at {} for a {band} band", r.row_h());
    }

    #[test]
    fn empty_ruler_has_no_lanes() {
        assert!(lanes(&[], 500.0).is_empty());
    }

    /// A fixed column measures its *content*, so it has to track the reader's
    /// text size. Asserted against the scale in force rather than by setting
    /// one: the scale is a process-global that the a11y tests also write.
    #[test]
    fn a_fixed_column_tracks_the_text_scale() {
        let k = theme::scale();
        let c = cols(&[ColW::Fixed(80.0), ColW::Fixed(40.0)]);
        let w = lanes(&c, 4_000.0);
        assert!(approx(w[0], 80.0 * k), "{} vs {}", w[0], 80.0 * k);
        assert!(approx(w[1], 40.0 * k));
        // And the ratio the caller declared survives whatever the scale is.
        assert!(approx(w[0] / w[1], 2.0));
    }

    #[test]
    fn fixed_columns_take_exactly_their_width() {
        let k = theme::scale();
        let c = cols(&[ColW::Fixed(100.0), ColW::Fixed(60.0)]);
        let w = lanes(&c, 4_000.0);
        assert!(approx(w[0], 100.0 * k) && approx(w[1], 60.0 * k));
    }

    #[test]
    fn flex_splits_the_remainder_by_weight() {
        let c = cols(&[ColW::Fixed(100.0), ColW::Flex(3.0), ColW::Flex(1.0)]);
        let w = lanes(&c, 500.0);
        assert!(approx(w[0], 100.0));
        assert!(approx(w[1], 300.0), "got {}", w[1]);
        assert!(approx(w[2], 100.0), "got {}", w[2]);
    }

    /// The lanes must fill the ruler exactly — a rounding drift here shows up
    /// as a column of figures that does not line up with its heading.
    /// The column the entry is identified by must survive a crowded ruler.
    /// Before this, scaled fixed columns took the whole width and the path
    /// column collapsed to two letters.
    #[test]
    fn a_flexible_column_is_never_squeezed_to_nothing() {
        let c = cols(&[
            ColW::Flex(3.0),
            ColW::Fixed(46.0),
            ColW::Fixed(78.0),
            ColW::Fixed(132.0),
            ColW::Fixed(86.0),
        ]);
        // A ruler barely wider than the fixed columns themselves.
        let fixed_total = (46.0 + 78.0 + 132.0 + 86.0) * theme::scale();
        let w = lanes(&c, fixed_total + 8.0);
        assert!(w[0] >= flex_min(), "path collapsed to {}", w[0]);
        let sum: f32 = w.iter().sum();
        assert!(approx(sum, fixed_total + 8.0), "summed to {sum}");
        // The fixed columns gave way in proportion rather than one vanishing.
        assert!(
            w[1] > 0.0 && w[2] > 0.0 && w[3] > 0.0 && w[4] > 0.0,
            "{w:?}"
        );
        assert!(approx(w[3] / w[1], 132.0 / 46.0), "ratio lost: {w:?}");
    }

    /// With room to spare nothing is squeezed and the floor does not bite.
    #[test]
    fn a_roomy_ruler_is_unaffected_by_the_floor() {
        let k = theme::scale();
        let c = cols(&[ColW::Flex(1.0), ColW::Fixed(80.0)]);
        let w = lanes(&c, 4_000.0);
        assert!(approx(w[1], 80.0 * k));
        assert!(approx(w[0], 4_000.0 - 80.0 * k));
    }

    #[test]
    fn lanes_sum_to_the_available_width() {
        let c = cols(&[ColW::Fixed(80.0), ColW::Flex(2.0), ColW::Flex(1.0)]);
        for avail in [200.0_f32, 333.7, 1024.0, 4000.0] {
            let sum: f32 = lanes(&c, avail).iter().sum();
            assert!(approx(sum, avail), "avail {avail} summed to {sum}");
        }
    }

    /// A window narrower than the fixed columns must shrink them all in step,
    /// not drop the trailing ones off the edge.
    #[test]
    fn over_subscribed_ruler_scales_proportionally() {
        let c = cols(&[ColW::Fixed(200.0), ColW::Fixed(100.0)]);
        let w = lanes(&c, 150.0);
        let sum: f32 = w.iter().sum();
        assert!(approx(sum, 150.0), "summed to {sum}");
        assert!(approx(w[0] / w[1], 2.0), "ratio lost: {:?}", w);
    }

    #[test]
    fn flex_gets_nothing_when_fixed_fills_the_ruler() {
        let c = cols(&[ColW::Fixed(300.0), ColW::Flex(1.0)]);
        let w = lanes(&c, 200.0);
        assert!(approx(w[1], 0.0));
    }

    #[test]
    fn zero_width_ruler_yields_zero_lanes() {
        let c = cols(&[ColW::Fixed(50.0), ColW::Flex(1.0)]);
        for w in lanes(&c, 0.0) {
            assert!(approx(w, 0.0));
        }
    }

    #[test]
    fn negative_width_is_treated_as_zero() {
        let c = cols(&[ColW::Fixed(50.0), ColW::Flex(1.0)]);
        let w = lanes(&c, -100.0);
        assert!(w.iter().all(|v| approx(*v, 0.0)), "{w:?}");
    }

    /// Weights of zero must not produce NaN lanes.
    #[test]
    fn zero_weight_flex_is_finite() {
        let c = cols(&[ColW::Flex(0.0), ColW::Flex(0.0)]);
        for w in lanes(&c, 400.0) {
            assert!(w.is_finite(), "non-finite lane");
        }
    }

    #[test]
    fn negative_declared_widths_are_clamped() {
        let c = cols(&[ColW::Fixed(-50.0), ColW::Flex(-1.0)]);
        let w = lanes(&c, 300.0);
        assert!(w.iter().all(|v| v.is_finite() && *v >= 0.0), "{w:?}");
    }

    #[test]
    fn column_builders_set_their_fields() {
        let c = Col::new("size", ColW::Fixed(80.0)).right().sortable("size");
        assert_eq!(c.head, "size");
        assert_eq!(c.align, Align::RIGHT);
        assert_eq!(c.sort_key, Some("size"));
    }

    /// Only a held entry is sealed — the mark must not leak onto the states
    /// that merely need attention, or it stops meaning custody.
    #[test]
    fn only_held_entries_are_sealed() {
        assert!(Custody::Mine.sealed());
        assert!(Custody::Peer.sealed());
        for c in [
            Custody::Free,
            Custody::Stale,
            Custody::Blocked,
            Custody::Quarantined,
            Custody::Good,
        ] {
            assert!(!c.sealed(), "{c:?} must not carry a seal");
        }
    }
}
