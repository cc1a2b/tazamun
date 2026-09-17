//! The token layer. Colours, type, space, motion, rules, elevation — and
//! nothing that draws. Every painter in `gui_native` resolves through here.
//!
//! # Brief
//!
//! **Subject.** Tazamun is a register of custody. A folder is shared, every file
//! in it is read-only, and exactly one person at a time may hold the pen. The
//! window answers three questions all day, at a glance: who holds this, may I
//! take it, and what happened to it before now.
//!
//! **Tone.** Custodial and exact. The reference object is not a dashboard; it is
//! a deed book — ruled entries, a column for the hand that signed, a seal beside
//! the line that is spoken for, a folio in the outer margin, a colophon at the
//! foot. The house pattern language (khatam stars, girih strapwork, the diamond)
//! already exists in `ornament`; this layer exists so that language can be the
//! *structure* of a page rather than a watermark behind one.
//!
//! **Density.** Data-dense. A register that shows five entries per screen is a
//! brochure. Entries are ruled, not boxed, and [`Density`] decides the pitch.
//!
//! **Type.** The IBM Plex superfamily, all OFL (license in `assets/fonts/`).
//! Serif for register titles, folio marks and the colophon — the engraved voice.
//! Sans for interface text. Mono for everything countable, because a ledger's
//! numerals must align. Sans Arabic so an Arabic name sits *in* the family
//! rather than beside it.
//!
//! **Palette.** Three modes over one token set: [`Mode::Night`] (iron-gall ink
//! on a dark desk), [`Mode::Paper`] (ink on warm stock), [`Mode::Contrast`]
//! (AAA). Colour carries custody meaning and nothing else — an entry is coloured
//! because someone holds it, is behind, or is contested, never for decoration.
//!
//! **Signature.** The rule and the khatam. Every entry sits on a hairline under
//! a folio column; an entry under lease is stamped in the margin with the
//! eight-point star from [`ornament::khatam`](super::ornament::khatam). This
//! module deliberately does **not** define a second star — there is one khatam
//! in this application and it lives in `ornament`.
//!
//! The only mutable state here is the active [`Mode`] and [`Density`], read as
//! relaxed atomics so a theme switch repaints without rebuilding the style.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use eframe::egui;
use egui::{
    Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Shadow, Stroke,
    TextStyle,
};

// ─── mode ────────────────────────────────────────────────────────────────────

/// Which palette the window is painted in.
///
/// [`Mode::Contrast`] is the default. It was built for low vision and bright
/// rooms, and it turned out to be the best expression of the whole design:
/// ink-black ground, white text, and the brand gold carrying every mark that
/// means something. Nothing is decorative enough to need a mid-tone, so the
/// palette with no mid-tones reads as the most deliberate of the three — and
/// the one that is legible to the most people is a good thing to open with.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Mode {
    /// Iron-gall ink on a dark desk.
    Night,
    /// Ink on warm stock.
    Paper,
    /// Maximum separation. The house default.
    #[default]
    Contrast,
}

impl Mode {
    /// The order the Settings control cycles through.
    pub const ALL: [Self; 3] = [Self::Night, Self::Paper, Self::Contrast];

    /// The value persisted in prefs.
    pub fn key(self) -> &'static str {
        match self {
            Self::Night => "night",
            Self::Paper => "paper",
            Self::Contrast => "contrast",
        }
    }

    /// Parses a persisted key; anything unknown falls back to the default.
    pub fn from_key(k: &str) -> Self {
        match k {
            "night" => Self::Night,
            "paper" => Self::Paper,
            _ => Self::default(),
        }
    }

    /// The label shown in Settings.
    pub fn label(self) -> &'static str {
        match self {
            Self::Night => "Night",
            Self::Paper => "Paper",
            Self::Contrast => "Contrast",
        }
    }

    /// True when the palette is light-on-dark. A few painters need this to know
    /// whether a wash should lighten or darken its ground.
    pub fn is_dark(self) -> bool {
        !matches!(self, Self::Paper)
    }

    fn tag(self) -> u8 {
        match self {
            Self::Night => 0,
            Self::Paper => 1,
            Self::Contrast => 2,
        }
    }
}

// Seeded with the default's tag so the very first frame — painted before prefs
// are read — is already the house palette rather than a flash of another one.
static MODE: AtomicU8 = AtomicU8::new(2);

/// The palette currently in force.
pub fn mode() -> Mode {
    match MODE.load(Ordering::Relaxed) {
        0 => Mode::Night,
        1 => Mode::Paper,
        _ => Mode::Contrast,
    }
}

/// Switches palette. Call [`restyle`] afterwards so egui's widget visuals follow.
pub fn set_mode(m: Mode) {
    MODE.store(m.tag(), Ordering::Relaxed);
}

// ─── palette ─────────────────────────────────────────────────────────────────

/// Every colour the interface may use. Nothing paints a literal.
pub struct Palette {
    /// Behind everything — the desk.
    pub bg_deep: Color32,
    /// Title bar, sidebar, status rail.
    pub bg_chrome: Color32,
    /// The page entries are ruled onto.
    pub bg_page: Color32,
    /// Hover and selection ground.
    pub bg_raise: Color32,
    /// Inputs, wells, quoted matter.
    pub bg_sunken: Color32,

    /// Primary text.
    pub ink: Color32,
    /// Secondary text — still readable at a glance.
    pub ink_muted: Color32,
    /// Tertiary text, inactive glyphs, ornament at rest.
    pub ink_faint: Color32,
    /// Text and strokes of a control that cannot be used.
    pub ink_disabled: Color32,

    /// The hairline that rules one entry from the next.
    pub rule_hair: Color32,
    /// The divider between column groups and blocks.
    pub rule_divider: Color32,
    /// The rule that opens or closes a section.
    pub rule_emphasis: Color32,

    /// The accent: the brand gold, read as a seal rather than a gradient.
    pub gold: Color32,
    /// Gold under the pointer.
    pub gold_bright: Color32,
    /// Gold at rest across a large field, and for ornament strokes.
    pub gold_deep: Color32,
    /// Text on a gold fill.
    pub on_gold: Color32,

    // Custody semantics. Colour in this window means exactly one of these.
    /// This device holds the lease.
    pub custody_self: Color32,
    /// Another device holds the lease.
    pub custody_peer: Color32,
    /// Unheld — the resting state of most entries.
    pub custody_free: Color32,
    /// The local copy is behind a peer's.
    pub custody_stale: Color32,
    /// Refused: offline, unreachable, or already held.
    pub custody_blocked: Color32,
    /// A copy preserved because two histories diverged.
    pub custody_quarantine: Color32,
    /// Verified, settled, in step.
    pub custody_good: Color32,

    /// Fill of a destructive control at rest.
    pub danger_fill: Color32,
    /// Fill of a destructive control under the pointer.
    pub danger_hover: Color32,
    /// Text on a destructive fill.
    pub on_danger: Color32,

    /// Focus ring — must clear both the page and the raised ground.
    pub focus: Color32,
    /// The colour shadows and scrims are mixed from.
    pub shade: Color32,
}

/// Iron-gall ink on a dark desk. Deliberately a desaturated blue-black rather
/// than the brand's saturated lapis: at full strength lapis reads as the stock
/// dark-SaaS navy, and the brand belongs on the seal, not on the desk.
const NIGHT: Palette = Palette {
    bg_deep: Color32::from_rgb(0x0a, 0x0c, 0x11),
    bg_chrome: Color32::from_rgb(0x0f, 0x12, 0x18),
    bg_page: Color32::from_rgb(0x13, 0x16, 0x1d),
    bg_raise: Color32::from_rgb(0x1c, 0x20, 0x29),
    bg_sunken: Color32::from_rgb(0x08, 0x0a, 0x0e),

    ink: Color32::from_rgb(0xe9, 0xe7, 0xe1),
    ink_muted: Color32::from_rgb(0xa5, 0xa4, 0x9d),
    ink_faint: Color32::from_rgb(0x73, 0x74, 0x72),
    ink_disabled: Color32::from_rgb(0x4e, 0x51, 0x59),

    rule_hair: Color32::from_rgb(0x24, 0x28, 0x31),
    rule_divider: Color32::from_rgb(0x31, 0x36, 0x41),
    rule_emphasis: Color32::from_rgb(0x46, 0x4c, 0x59),

    gold: Color32::from_rgb(0xc8, 0xa2, 0x4b),
    gold_bright: Color32::from_rgb(0xe2, 0xc2, 0x7f),
    gold_deep: Color32::from_rgb(0x87, 0x6b, 0x2f),
    on_gold: Color32::from_rgb(0x15, 0x11, 0x06),

    custody_self: Color32::from_rgb(0xc8, 0xa2, 0x4b),
    custody_peer: Color32::from_rgb(0x82, 0x9d, 0xd4),
    custody_free: Color32::from_rgb(0x73, 0x74, 0x72),
    custody_stale: Color32::from_rgb(0xb2, 0x71, 0x24),
    custody_blocked: Color32::from_rgb(0xd4, 0x6f, 0x68),
    custody_quarantine: Color32::from_rgb(0xb5, 0x84, 0xa0),
    custody_good: Color32::from_rgb(0x79, 0xb9, 0x8b),

    danger_fill: Color32::from_rgb(0x5c, 0x22, 0x22),
    danger_hover: Color32::from_rgb(0x77, 0x2b, 0x2b),
    on_danger: Color32::from_rgb(0xff, 0xdd, 0xd9),

    focus: Color32::from_rgb(0xe2, 0xc2, 0x7f),
    shade: Color32::from_rgb(0x00, 0x00, 0x00),
};

/// Ink on warm stock. The mode that makes the manuscript reading unmistakable.
const PAPER: Palette = Palette {
    bg_deep: Color32::from_rgb(0xc9, 0xc2, 0xb2),
    bg_chrome: Color32::from_rgb(0xe6, 0xdf, 0xd0),
    bg_page: Color32::from_rgb(0xf7, 0xf3, 0xe9),
    bg_raise: Color32::from_rgb(0xea, 0xe3, 0xd4),
    bg_sunken: Color32::from_rgb(0xfd, 0xfb, 0xf6),

    ink: Color32::from_rgb(0x22, 0x20, 0x1b),
    ink_muted: Color32::from_rgb(0x55, 0x51, 0x48),
    ink_faint: Color32::from_rgb(0x77, 0x72, 0x66),
    ink_disabled: Color32::from_rgb(0xa2, 0x9c, 0x8d),

    rule_hair: Color32::from_rgb(0xdb, 0xd4, 0xc4),
    rule_divider: Color32::from_rgb(0xc2, 0xba, 0xa7),
    rule_emphasis: Color32::from_rgb(0x9d, 0x94, 0x81),

    gold: Color32::from_rgb(0x7d, 0x5e, 0x1a),
    gold_bright: Color32::from_rgb(0x9c, 0x77, 0x28),
    gold_deep: Color32::from_rgb(0x59, 0x42, 0x10),
    on_gold: Color32::from_rgb(0xfd, 0xfb, 0xf6),

    custody_self: Color32::from_rgb(0x7d, 0x5e, 0x1a),
    custody_peer: Color32::from_rgb(0x2f, 0x4f, 0x8a),
    custody_free: Color32::from_rgb(0x77, 0x72, 0x66),
    custody_stale: Color32::from_rgb(0xb0, 0x6a, 0x12),
    custody_blocked: Color32::from_rgb(0x91, 0x30, 0x29),
    custody_quarantine: Color32::from_rgb(0x5a, 0x21, 0x50),
    custody_good: Color32::from_rgb(0x2c, 0x62, 0x3d),

    danger_fill: Color32::from_rgb(0x91, 0x30, 0x29),
    danger_hover: Color32::from_rgb(0x7a, 0x27, 0x21),
    on_danger: Color32::from_rgb(0xfd, 0xfb, 0xf6),

    focus: Color32::from_rgb(0x59, 0x42, 0x10),
    shade: Color32::from_rgb(0x46, 0x3e, 0x2e),
};

/// Maximum separation. Every text pair here clears WCAG AAA.
const CONTRAST: Palette = Palette {
    bg_deep: Color32::from_rgb(0x00, 0x00, 0x00),
    bg_chrome: Color32::from_rgb(0x00, 0x00, 0x00),
    bg_page: Color32::from_rgb(0x00, 0x00, 0x00),
    bg_raise: Color32::from_rgb(0x1e, 0x1e, 0x1e),
    bg_sunken: Color32::from_rgb(0x0a, 0x0a, 0x0a),

    ink: Color32::from_rgb(0xff, 0xff, 0xff),
    ink_muted: Color32::from_rgb(0xe0, 0xe0, 0xe0),
    ink_faint: Color32::from_rgb(0xbd, 0xbd, 0xbd),
    ink_disabled: Color32::from_rgb(0x8a, 0x8a, 0x8a),

    rule_hair: Color32::from_rgb(0x6e, 0x6e, 0x6e),
    rule_divider: Color32::from_rgb(0x8f, 0x8f, 0x8f),
    rule_emphasis: Color32::from_rgb(0xc4, 0xc4, 0xc4),

    gold: Color32::from_rgb(0xff, 0xd1, 0x66),
    gold_bright: Color32::from_rgb(0xff, 0xe4, 0xa6),
    gold_deep: Color32::from_rgb(0xd0, 0xa3, 0x3c),
    on_gold: Color32::from_rgb(0x00, 0x00, 0x00),

    custody_self: Color32::from_rgb(0xff, 0xd1, 0x66),
    custody_peer: Color32::from_rgb(0x9e, 0xc6, 0xff),
    custody_free: Color32::from_rgb(0xbd, 0xbd, 0xbd),
    custody_stale: Color32::from_rgb(0xff, 0xc4, 0x52),
    custody_blocked: Color32::from_rgb(0xff, 0x8f, 0x87),
    custody_quarantine: Color32::from_rgb(0xf2, 0xa8, 0xe4),
    custody_good: Color32::from_rgb(0x72, 0xea, 0x94),

    danger_fill: Color32::from_rgb(0x7a, 0x14, 0x14),
    danger_hover: Color32::from_rgb(0x9c, 0x1c, 0x1c),
    on_danger: Color32::from_rgb(0xff, 0xff, 0xff),

    focus: Color32::from_rgb(0xff, 0xff, 0xff),
    shade: Color32::from_rgb(0x00, 0x00, 0x00),
};

/// The palette in force. Inlined so a colour lookup costs one relaxed load.
#[inline]
pub fn pal() -> &'static Palette {
    match MODE.load(Ordering::Relaxed) {
        0 => &NIGHT,
        1 => &PAPER,
        _ => &CONTRAST,
    }
}

macro_rules! tokens {
    ($($name:ident),+ $(,)?) => {
        $(
            #[inline]
            #[doc = concat!("The `", stringify!($name), "` colour of the active palette.")]
            pub fn $name() -> Color32 { pal().$name }
        )+
    };
}

tokens!(
    bg_deep,
    bg_chrome,
    bg_page,
    bg_raise,
    bg_sunken,
    ink,
    ink_muted,
    ink_faint,
    ink_disabled,
    rule_hair,
    rule_divider,
    rule_emphasis,
    gold,
    gold_bright,
    gold_deep,
    on_gold,
    custody_peer,
    custody_stale,
    custody_blocked,
    custody_quarantine,
    custody_good,
    danger_fill,
    danger_hover,
    on_danger,
    focus,
);

// ─── custody ─────────────────────────────────────────────────────────────────

/// Who holds an entry, and therefore how it is coloured and whether it is
/// sealed. The only vocabulary colour is allowed to express in this window.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Custody {
    /// This device holds the lease.
    Mine,
    /// Another device holds it.
    Peer,
    /// Unheld.
    Free,
    /// The local copy is behind.
    Stale,
    /// Refused — offline, unreachable, or already held.
    Blocked,
    /// A preserved divergent copy.
    Quarantined,
    /// Settled and verified.
    Good,
}

impl Custody {
    /// The colour this state paints in.
    pub fn color(self) -> Color32 {
        let p = pal();
        match self {
            Self::Mine => p.custody_self,
            Self::Peer => p.custody_peer,
            Self::Free => p.custody_free,
            Self::Stale => p.custody_stale,
            Self::Blocked => p.custody_blocked,
            Self::Quarantined => p.custody_quarantine,
            Self::Good => p.custody_good,
        }
    }

    /// Whether an entry in this state carries a khatam seal in its margin. Only
    /// a held entry is sealed — that is what the mark means.
    pub fn sealed(self) -> bool {
        matches!(self, Self::Mine | Self::Peer)
    }
}

// ─── type ────────────────────────────────────────────────────────────────────

const SANS: &[u8] = include_bytes!("../../assets/fonts/IBMPlexSans-Regular.ttf");
const SANS_MEDIUM: &[u8] = include_bytes!("../../assets/fonts/IBMPlexSans-Medium.ttf");
const SANS_SEMIBOLD: &[u8] = include_bytes!("../../assets/fonts/IBMPlexSans-SemiBold.ttf");
const MONO: &[u8] = include_bytes!("../../assets/fonts/IBMPlexMono-Regular.ttf");
const MONO_MEDIUM: &[u8] = include_bytes!("../../assets/fonts/IBMPlexMono-Medium.ttf");
const SERIF: &[u8] = include_bytes!("../../assets/fonts/IBMPlexSerif-Regular.ttf");
const SERIF_SEMIBOLD: &[u8] = include_bytes!("../../assets/fonts/IBMPlexSerif-SemiBold.ttf");
const ARABIC: &[u8] = include_bytes!("../../assets/fonts/IBMPlexSansArabic-Regular.ttf");
const ARABIC_MEDIUM: &[u8] = include_bytes!("../../assets/fonts/IBMPlexSansArabic-Medium.ttf");

/// Interface text at medium weight — controls, and the one value in a row that
/// carries more weight than its neighbours.
pub fn fam_medium() -> FontFamily {
    FontFamily::Name("sans-medium".into())
}

/// Headings and emphasis.
pub fn fam_semibold() -> FontFamily {
    FontFamily::Name("sans-semibold".into())
}

/// The engraved voice: register titles, folio marks, the colophon.
pub fn fam_serif() -> FontFamily {
    FontFamily::Name("serif-semibold".into())
}

/// Serif at text weight, for the few places that run to a sentence.
pub fn fam_serif_text() -> FontFamily {
    FontFamily::Name("serif".into())
}

/// Everything countable: paths, hashes, byte counts, timestamps, vectors.
/// Monospaced, therefore tabular — a column of these will always align, which
/// is what `figures::align` relies on.
pub fn fam_mono() -> FontFamily {
    FontFamily::Monospace
}

/// Mono at medium weight, for the one quantity in a row that matters most.
pub fn fam_mono_medium() -> FontFamily {
    FontFamily::Name("mono-medium".into())
}

/// The type scale. Nothing in the interface may pick a size outside this list;
/// every one of these resolves through [`scale`], which is what makes the text
/// size control actually move the window.
pub mod step {
    /// Register titles and the folio. Serif.
    pub const DISPLAY: f32 = 20.0;
    /// Section headings.
    pub const TITLE: f32 = 15.0;
    /// Running text.
    pub const BODY: f32 = 13.5;
    /// Controls and column values.
    pub const LABEL: f32 = 12.5;
    /// Countable values, in mono.
    pub const DATA: f32 = 12.0;
    /// Captions, column headers, secondary values.
    pub const META: f32 = 11.0;
    /// Ornamental marks and keycaps — the floor.
    pub const CAPTION: f32 = 10.0;
}

/// Extra tracking for the small tracked column headers. Plex Sans is tight at
/// caption sizes; without this a header reads as a smudge.
pub const HEADER_TRACKING: f32 = 1.15;

static SCALE: AtomicU8 = AtomicU8::new(20);

/// The user's text scale, 0.70 … 2.20. Stored as a u8 in twentieths so it can
/// live in an atomic beside the palette.
pub fn scale() -> f32 {
    SCALE.load(Ordering::Relaxed) as f32 / 20.0
}

/// Sets the text scale. Clamped; call [`restyle`] afterwards.
pub fn set_scale(s: f32) {
    SCALE.store((s.clamp(0.7, 2.2) * 20.0).round() as u8, Ordering::Relaxed);
}

/// A size from [`step`] at the user's current scale. Every `.size()` call in the
/// interface goes through this.
#[inline]
pub fn sized(step: f32) -> f32 {
    step * scale()
}

/// A [`FontId`] at a scaled step.
pub fn font(step: f32, family: FontFamily) -> FontId {
    FontId::new(sized(step), family)
}

// ─── space, motion, geometry ─────────────────────────────────────────────────

/// The spacing scale. Every gap in the interface is one of these. All are
/// multiples of the 4px baseline except [`space::XS`], which is the half-step
/// used inside a compound label.
pub mod space {
    /// Inside a compound label.
    pub const XS: f32 = 2.0;
    /// Between a glyph and its word.
    pub const S: f32 = 4.0;
    /// Between controls in a row.
    pub const M: f32 = 8.0;
    /// Between rows of unlike things.
    pub const L: f32 = 12.0;
    /// Between a heading and its register.
    pub const XL: f32 = 16.0;
    /// Between sections, and the page gutter.
    pub const XXL: f32 = 24.0;
}

/// Motion durations in seconds, and the repaint cadence for animations that
/// must be driven frame by frame. Anything slower than [`motion::CEREMONY`] is
/// a bug.
pub mod motion {
    use std::time::Duration;

    /// Pointer feedback — hover, press.
    pub const TOUCH: f32 = 0.10;
    /// A state that changed: open, close, select.
    pub const STATE: f32 = 0.16;
    /// A view that changed: tab, page, reveal.
    pub const VIEW: f32 = 0.24;
    /// Custody changed hands. The only motion allowed to be noticed.
    pub const CEREMONY: f32 = 0.42;

    /// How often a continuously animating mark asks for the next frame. One
    /// cadence for the whole window rather than the 33/50/60ms spread.
    pub const CADENCE: Duration = Duration::from_millis(33);
}

static REDUCED_MOTION: AtomicBool = AtomicBool::new(false);

/// Whether the user asked for reduced motion. Painters must consult this before
/// scheduling a repaint for a purely decorative animation.
pub fn reduced_motion() -> bool {
    REDUCED_MOTION.load(Ordering::Relaxed)
}

/// Sets the reduced-motion preference.
pub fn set_reduced_motion(on: bool) {
    REDUCED_MOTION.store(on, Ordering::Relaxed);
}

/// A duration, honouring reduced motion. Returns 0.0 when motion is off, so an
/// `animate_bool_with_time` call snaps instead of sliding and every call site
/// gets the preference for free.
#[inline]
pub fn dur(seconds: f32) -> f32 {
    if reduced_motion() { 0.0 } else { seconds }
}

/// A register is ruled, not boxed: the default corner radius is square.
pub const R_NONE: u8 = 0;
/// A small mark — chip, keycap, count.
pub const R_CHIP: u8 = 2;
/// Buttons and inputs: enough to read as a control, not enough to read as a pill.
pub const R_CONTROL: u8 = 3;
/// True overlays: menus, modals, toasts.
pub const R_OVERLAY: u8 = 6;
/// The OS window itself.
pub const R_WINDOW: u8 = 12;

/// Hairline weight. Snapped by [`snap`] before painting.
pub const RULE_W: f32 = 1.0;
/// The weight of an accent rule that marks a selection or a section.
pub const RULE_ACCENT_W: f32 = 2.0;

/// Snaps a coordinate onto the pixel grid so a 1px rule renders as one crisp
/// line rather than two grey ones. Three files used to hand-roll this.
#[inline]
pub fn snap(v: f32) -> f32 {
    v.floor() + 0.5
}

/// How tight the register is packed. Persisted in prefs.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Density {
    /// Maximum entries per screen.
    Compact,
    /// The default.
    #[default]
    Regular,
    /// Roomier lines for long sessions.
    Relaxed,
}

impl Density {
    /// The order the Settings control cycles through.
    pub const ALL: [Self; 3] = [Self::Compact, Self::Regular, Self::Relaxed];

    /// The height of one ruled entry, at the user's text scale.
    pub fn row_h(self) -> f32 {
        let base = match self {
            Self::Compact => 24.0,
            Self::Regular => 28.0,
            Self::Relaxed => 34.0,
        };
        base * scale().clamp(1.0, 2.2)
    }

    /// Horizontal padding inside a register cell.
    pub fn cell_pad(self) -> f32 {
        match self {
            Self::Compact => space::M,
            Self::Regular => space::L,
            Self::Relaxed => space::XL,
        }
    }

    /// Vertical padding inside a block.
    pub fn block_pad(self) -> f32 {
        match self {
            Self::Compact => space::M,
            Self::Regular => space::L,
            Self::Relaxed => space::XL,
        }
    }

    /// The persisted key.
    pub fn key(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Regular => "regular",
            Self::Relaxed => "relaxed",
        }
    }

    /// Parses a persisted key; anything unknown falls back to [`Density::Regular`].
    pub fn from_key(k: &str) -> Self {
        match k {
            "compact" => Self::Compact,
            "relaxed" => Self::Relaxed,
            _ => Self::Regular,
        }
    }

    /// The label shown in Settings.
    pub fn label(self) -> &'static str {
        match self {
            Self::Compact => "Compact",
            Self::Regular => "Regular",
            Self::Relaxed => "Relaxed",
        }
    }
}

static DENSITY: AtomicU8 = AtomicU8::new(1);

/// The density in force.
pub fn density() -> Density {
    match DENSITY.load(Ordering::Relaxed) {
        0 => Density::Compact,
        2 => Density::Relaxed,
        _ => Density::Regular,
    }
}

/// Sets the register density. Call [`restyle`] afterwards.
pub fn set_density(d: Density) {
    DENSITY.store(
        match d {
            Density::Compact => 0,
            Density::Regular => 1,
            Density::Relaxed => 2,
        },
        Ordering::Relaxed,
    );
}

// ─── elevation ───────────────────────────────────────────────────────────────

/// The register never floats. Only these three things do.
pub mod elevation {
    use super::{Shadow, alpha, mode, pal};

    /// Menus, popups, toasts.
    pub fn overlay() -> Shadow {
        Shadow {
            offset: [0, 6],
            blur: 18,
            spread: 0,
            color: alpha(pal().shade, if mode().is_dark() { 130 } else { 48 }),
        }
    }

    /// A modal dialog over a scrim.
    pub fn modal() -> Shadow {
        Shadow {
            offset: [0, 10],
            blur: 28,
            spread: 0,
            color: alpha(pal().shade, if mode().is_dark() { 165 } else { 66 }),
        }
    }
}

/// The scrim behind a modal. One value, so four dialogs stop disagreeing.
pub fn scrim() -> Color32 {
    alpha(pal().shade, if mode().is_dark() { 150 } else { 96 })
}

// ─── install ─────────────────────────────────────────────────────────────────

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let mut add = |name: &str, bytes: &'static [u8]| {
        fonts
            .font_data
            .insert(name.to_owned(), FontData::from_static(bytes).into());
    };
    add("sans", SANS);
    add("sans-medium", SANS_MEDIUM);
    add("sans-semibold", SANS_SEMIBOLD);
    add("mono", MONO);
    add("mono-medium", MONO_MEDIUM);
    add("serif", SERIF);
    add("serif-semibold", SERIF_SEMIBOLD);
    // Arabic file and session names must render glyphs, not tofu. egui has no
    // bidi or shaping upstream, so Arabic renders unshaped — legible letters,
    // not joined calligraphy. That is why the brand wordmark is a pre-rendered
    // texture rather than live text. Plex Sans Arabic is chosen over Noto so an
    // Arabic name sits in the same superfamily as the Latin beside it.
    add("arabic", ARABIC);
    add("arabic-medium", ARABIC_MEDIUM);

    let prop = fonts.families.entry(FontFamily::Proportional).or_default();
    prop.insert(0, "sans".into());
    prop.push("arabic".into());

    let mono = fonts.families.entry(FontFamily::Monospace).or_default();
    mono.insert(0, "mono".into());
    mono.push("arabic".into());

    let mut named = |family: FontFamily, stack: &[&str]| {
        fonts
            .families
            .insert(family, stack.iter().map(|s| (*s).to_owned()).collect());
    };
    named(
        fam_medium(),
        &["sans-medium", "arabic-medium", "sans", "arabic"],
    );
    named(
        fam_semibold(),
        &["sans-semibold", "arabic-medium", "sans", "arabic"],
    );
    named(fam_serif(), &["serif-semibold", "serif", "arabic", "sans"]);
    named(fam_serif_text(), &["serif", "arabic", "sans"]);
    named(fam_mono_medium(), &["mono-medium", "mono", "arabic"]);

    ctx.set_fonts(fonts);
}

/// Installs fonts and style. Call once at startup.
pub fn install(ctx: &egui::Context) {
    install_fonts(ctx);
    restyle(ctx);
}

/// Rebuilds visuals and the text table from the [`Mode`], [`Density`] and text
/// [`scale`] currently in force. The single definition of the style — `a11y`
/// no longer keeps a parallel copy of the type table, it sets the scale and
/// calls this.
pub fn restyle(ctx: &egui::Context) {
    let p = pal();
    let dark = mode().is_dark();
    let mut v = if dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };

    v.override_text_color = Some(p.ink);
    // Panels paint their own ground so a rule can bleed to the panel edge
    // without a seam.
    v.panel_fill = Color32::TRANSPARENT;
    v.window_fill = p.bg_chrome;
    v.window_stroke = Stroke::new(RULE_W, p.rule_emphasis);
    v.window_corner_radius = CornerRadius::same(R_OVERLAY);
    v.menu_corner_radius = CornerRadius::same(R_OVERLAY);
    v.extreme_bg_color = p.bg_sunken;
    v.faint_bg_color = alpha(p.ink, if dark { 8 } else { 14 });
    v.hyperlink_color = p.custody_peer;
    v.selection.bg_fill = alpha(p.gold, if dark { 46 } else { 52 });
    v.selection.stroke = Stroke::new(RULE_W, p.gold);

    let r = CornerRadius::same(R_CONTROL);

    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, p.ink);
    v.widgets.noninteractive.bg_stroke = Stroke::new(RULE_W, p.rule_hair);
    v.widgets.noninteractive.bg_fill = Color32::TRANSPARENT;
    v.widgets.noninteractive.weak_bg_fill = Color32::TRANSPARENT;
    v.widgets.noninteractive.corner_radius = r;

    // A control at rest is outlined, not filled. A filled rectangle in a
    // register reads as a stamp, and the stamp is reserved for custody.
    v.widgets.inactive.bg_fill = Color32::TRANSPARENT;
    v.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, p.ink_muted);
    v.widgets.inactive.bg_stroke = Stroke::new(RULE_W, p.rule_divider);
    v.widgets.inactive.corner_radius = r;

    v.widgets.hovered.bg_fill = p.bg_raise;
    v.widgets.hovered.weak_bg_fill = p.bg_raise;
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, p.ink);
    v.widgets.hovered.bg_stroke = Stroke::new(RULE_W, p.gold_deep);
    v.widgets.hovered.corner_radius = r;
    // No expansion: a control that grows on hover breaks the register's rules.
    v.widgets.hovered.expansion = 0.0;

    v.widgets.active.bg_fill = p.bg_raise;
    v.widgets.active.weak_bg_fill = p.bg_raise;
    v.widgets.active.fg_stroke = Stroke::new(1.0, p.gold_bright);
    v.widgets.active.bg_stroke = Stroke::new(RULE_W, p.gold);
    v.widgets.active.corner_radius = r;
    v.widgets.active.expansion = 0.0;

    v.widgets.open.bg_fill = p.bg_raise;
    v.widgets.open.weak_bg_fill = p.bg_raise;
    v.widgets.open.fg_stroke = Stroke::new(1.0, p.ink);
    v.widgets.open.bg_stroke = Stroke::new(RULE_W, p.rule_divider);
    v.widgets.open.corner_radius = r;

    v.window_shadow = elevation::modal();
    v.popup_shadow = elevation::overlay();

    ctx.set_visuals(v);

    let d = density();
    ctx.all_styles_mut(|st| {
        // egui has no reduced-motion support of its own; a zero animation time
        // makes every `animate_bool*` snap, which covers the widget transitions
        // the app does not drive by hand.
        st.animation_time = dur(motion::STATE);
        st.spacing.item_spacing = egui::vec2(space::M, space::S);
        st.spacing.button_padding = egui::vec2(space::L, space::S);
        st.spacing.interact_size.y = d.row_h();
        st.spacing.window_margin = Margin::same(space::L as i8);
        st.spacing.menu_margin = Margin::same(space::M as i8);
        st.spacing.indent = space::XL;
        // A hairline rail that stays out of the way: floating, thin, invisible
        // until the pointer is in the area. egui's default solid bar is the last
        // piece of stock chrome that would survive on screen.
        st.spacing.scroll = egui::style::ScrollStyle {
            floating: true,
            bar_width: 6.0,
            handle_min_length: 24.0,
            bar_inner_margin: 4.0,
            bar_outer_margin: 2.0,
            floating_width: 6.0,
            floating_allocated_width: 0.0,
            foreground_color: true,
            dormant_background_opacity: 0.0,
            active_background_opacity: 0.0,
            interact_background_opacity: 0.0,
            dormant_handle_opacity: 0.0,
            active_handle_opacity: 0.5,
            interact_handle_opacity: 0.9,
            ..Default::default()
        };
        st.text_styles = [
            (TextStyle::Heading, font(step::TITLE, fam_semibold())),
            (TextStyle::Body, font(step::BODY, FontFamily::Proportional)),
            (TextStyle::Button, font(step::LABEL, fam_medium())),
            (TextStyle::Small, font(step::META, FontFamily::Proportional)),
            (
                TextStyle::Monospace,
                font(step::DATA, FontFamily::Monospace),
            ),
            (
                TextStyle::Name("display".into()),
                font(step::DISPLAY, fam_serif()),
            ),
        ]
        .into();
    });
}

// ─── colour maths ────────────────────────────────────────────────────────────

/// The colour at a given opacity. Preferred over `linear_multiply`, which
/// darkens as well as fades and therefore cannot make a translucent wash over a
/// light ground — the bug that would otherwise hit every tint the moment
/// [`Mode::Paper`] is selected.
///
/// `Color32` is premultiplied, so the stored channels are scaled by `a`; what
/// is preserved is the colour's *hue over any ground*, which is the property
/// the washes rely on.
#[inline]
pub fn alpha(c: Color32, a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a)
}

/// A wash of `c` over the page, at one of the named strengths.
pub mod wash {
    use super::{Color32, alpha, mode};

    /// Barely there — a watermark.
    pub const GHOST: u8 = 14;
    /// A tint that names a row's state without competing with its text.
    pub const TINT: u8 = 30;
    /// A selected row.
    pub const SELECT: u8 = 52;

    /// `c` at `strength`, nudged up on light grounds where the same alpha
    /// reads weaker.
    pub fn of(c: Color32, strength: u8) -> Color32 {
        let a = if mode().is_dark() {
            strength
        } else {
            strength.saturating_add(strength / 4)
        };
        alpha(c, a)
    }
}

/// Linear interpolation between two colours, alpha included.
pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let l = |x: u8, y: u8| -> u8 { (x as f32 + (y as f32 - x as f32) * t).round() as u8 };
    Color32::from_rgba_unmultiplied(
        l(a.r(), b.r()),
        l(a.g(), b.g()),
        l(a.b(), b.b()),
        l(a.a(), b.a()),
    )
}

/// Relative luminance per WCAG 2.1.
pub fn luminance(c: Color32) -> f32 {
    let f = |v: u8| {
        let s = v as f32 / 255.0;
        if s <= 0.039_28 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * f(c.r()) + 0.7152 * f(c.g()) + 0.0722 * f(c.b())
}

/// WCAG contrast ratio between two opaque colours, 1.0 … 21.0.
pub fn contrast(a: Color32, b: Color32) -> f32 {
    let (x, y) = (luminance(a), luminance(b));
    let (hi, lo) = if x > y { (x, y) } else { (y, x) };
    (hi + 0.05) / (lo + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The palette, density and scale live in process-global atomics, so every
    /// test that writes one must hold this or they interleave.
    static STYLE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn lock() -> std::sync::MutexGuard<'static, ()> {
        STYLE_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn each_mode(f: impl Fn(Mode, &'static Palette)) {
        let _g = lock();
        for m in Mode::ALL {
            set_mode(m);
            f(m, pal());
        }
        set_mode(Mode::default());
    }

    /// Body and secondary text must clear WCAG AA on every ground they are
    /// painted on, in every palette. A palette edit that breaks this is a
    /// regression, not a matter of taste.
    #[test]
    fn text_clears_aa_on_every_ground() {
        each_mode(|m, p| {
            for (name, ground) in [
                ("page", p.bg_page),
                ("chrome", p.bg_chrome),
                ("raise", p.bg_raise),
                ("sunken", p.bg_sunken),
            ] {
                let c = contrast(p.ink, ground);
                assert!(c >= 4.5, "{m:?}: ink on {name} is {c:.2}:1");
                let c = contrast(p.ink_muted, ground);
                assert!(c >= 4.5, "{m:?}: ink_muted on {name} is {c:.2}:1");
            }
        });
    }

    /// Tertiary ink may be quieter, but never below the 3:1 floor that large
    /// text and non-text marks require.
    #[test]
    fn faint_ink_clears_the_non_text_floor() {
        each_mode(|m, p| {
            let c = contrast(p.ink_faint, p.bg_page);
            assert!(c >= 3.0, "{m:?}: ink_faint on page is {c:.2}:1");
        });
    }

    /// Custody colour is the interface's only vocabulary of meaning, so each
    /// state must be distinguishable from the page it is read on.
    #[test]
    fn custody_colors_clear_the_page() {
        each_mode(|m, p| {
            for (name, c) in [
                ("self", p.custody_self),
                ("peer", p.custody_peer),
                ("free", p.custody_free),
                ("stale", p.custody_stale),
                ("blocked", p.custody_blocked),
                ("quarantine", p.custody_quarantine),
                ("good", p.custody_good),
            ] {
                let r = contrast(c, p.bg_page);
                assert!(r >= 3.0, "{m:?}: custody {name} on page is {r:.2}:1");
            }
        });
    }

    /// Custody states must be told apart from each other, not just from the
    /// page — otherwise "held" and "behind" look the same to the user.
    #[test]
    fn custody_states_are_mutually_distinct() {
        each_mode(|m, p| {
            let named = [
                ("self", p.custody_self),
                ("peer", p.custody_peer),
                ("stale", p.custody_stale),
                ("blocked", p.custody_blocked),
                ("quarantine", p.custody_quarantine),
                ("good", p.custody_good),
            ];
            for (i, (an, a)) in named.iter().enumerate() {
                for (bn, b) in named.iter().skip(i + 1) {
                    let d = (luminance(*a) - luminance(*b)).abs();
                    let hue_gap = (a.r() as i32 - b.r() as i32).abs()
                        + (a.g() as i32 - b.g() as i32).abs()
                        + (a.b() as i32 - b.b() as i32).abs();
                    assert!(
                        d > 0.02 || hue_gap > 90,
                        "{m:?}: custody {an} and {bn} are not distinguishable"
                    );
                }
            }
        });
    }

    /// Text dropped on a filled mark must be legible in all three palettes.
    #[test]
    fn fills_carry_legible_text() {
        each_mode(|m, p| {
            let c = contrast(p.on_gold, p.gold);
            assert!(c >= 4.5, "{m:?}: on_gold over gold is {c:.2}:1");
            let c = contrast(p.on_danger, p.danger_fill);
            assert!(c >= 4.5, "{m:?}: on_danger over danger_fill is {c:.2}:1");
            let c = contrast(p.on_danger, p.danger_hover);
            assert!(c >= 4.5, "{m:?}: on_danger over danger_hover is {c:.2}:1");
        });
    }

    /// The three rule weights must be ordered, or they are three names for one
    /// line.
    #[test]
    fn rules_are_ordered_by_weight() {
        each_mode(|m, p| {
            let (h, d, e) = (
                contrast(p.rule_hair, p.bg_page),
                contrast(p.rule_divider, p.bg_page),
                contrast(p.rule_emphasis, p.bg_page),
            );
            assert!(
                h < d && d < e,
                "{m:?}: rules not ordered ({h:.2} {d:.2} {e:.2})"
            );
        });
    }

    /// A disabled control must read as unavailable but still be visible.
    #[test]
    fn disabled_ink_is_quieter_than_muted_but_visible() {
        each_mode(|m, p| {
            let dis = contrast(p.ink_disabled, p.bg_page);
            let mut_ = contrast(p.ink_muted, p.bg_page);
            assert!(dis < mut_, "{m:?}: disabled is not quieter than muted");
            assert!(dis >= 1.9, "{m:?}: disabled is invisible at {dis:.2}:1");
        });
    }

    #[test]
    fn contrast_mode_clears_aaa() {
        let _g = lock();
        set_mode(Mode::Contrast);
        let p = pal();
        assert!(contrast(p.ink, p.bg_page) >= 7.0);
        assert!(contrast(p.ink_muted, p.bg_page) >= 7.0);
        assert!(contrast(p.ink_faint, p.bg_page) >= 7.0);
        set_mode(Mode::default());
    }

    #[test]
    fn mode_keys_round_trip() {
        for m in Mode::ALL {
            assert_eq!(Mode::from_key(m.key()), m);
        }
        assert_eq!(Mode::from_key("nonsense"), Mode::default());
    }

    #[test]
    fn density_keys_round_trip() {
        for d in Density::ALL {
            assert_eq!(Density::from_key(d.key()), d);
        }
        assert_eq!(Density::from_key("nonsense"), Density::Regular);
    }

    /// Compact must actually be denser than relaxed, or the control is a lie.
    #[test]
    fn density_is_ordered() {
        let _g = lock();
        set_scale(1.0);
        assert!(Density::Compact.row_h() < Density::Regular.row_h());
        assert!(Density::Regular.row_h() < Density::Relaxed.row_h());
        assert!(Density::Compact.cell_pad() < Density::Relaxed.cell_pad());
    }

    /// The type scale must be strictly ordered, so "display" never renders
    /// smaller than "title".
    #[test]
    fn type_scale_is_ordered() {
        let s = [
            step::CAPTION,
            step::META,
            step::DATA,
            step::LABEL,
            step::BODY,
            step::TITLE,
            step::DISPLAY,
        ];
        for w in s.windows(2) {
            assert!(w[0] < w[1], "type scale out of order at {:?}", w);
        }
    }

    /// Text scale must reach every step — this is the property that was broken
    /// while 125 call sites pinned absolute sizes.
    #[test]
    fn scale_moves_every_step() {
        let _g = lock();
        set_scale(1.0);
        let base = sized(step::BODY);
        set_scale(2.0);
        assert!((sized(step::BODY) - base * 2.0).abs() < 0.01);
        set_scale(0.1);
        assert!(scale() >= 0.7, "scale must clamp at the floor");
        set_scale(9.0);
        assert!(scale() <= 2.2, "scale must clamp at the ceiling");
        set_scale(1.0);
    }

    #[test]
    fn reduced_motion_zeroes_durations() {
        let _g = lock();
        set_reduced_motion(true);
        assert_eq!(dur(motion::CEREMONY), 0.0);
        set_reduced_motion(false);
        assert_eq!(dur(motion::CEREMONY), motion::CEREMONY);
    }

    #[test]
    fn snap_lands_on_half_pixels() {
        assert_eq!(snap(10.0), 10.5);
        assert_eq!(snap(10.9), 10.5);
        assert_eq!(snap(-0.2), -0.5);
        assert_eq!(snap(-1.2), -1.5);
    }

    #[test]
    fn mix_endpoints_are_exact_and_clamped() {
        let a = Color32::from_rgb(0, 0, 0);
        let b = Color32::from_rgb(255, 255, 255);
        assert_eq!(mix(a, b, 0.0), a);
        assert_eq!(mix(a, b, 1.0), b);
        assert_eq!(mix(a, b, 2.0), b);
        assert_eq!(mix(a, b, -1.0), a);
    }

    #[test]
    fn alpha_sets_opacity_and_stays_premultiplied() {
        let c = Color32::from_rgb(0x12, 0x34, 0x56);
        let f = alpha(c, 77);
        assert_eq!(f.a(), 77);
        // Premultiplied: no channel may exceed the alpha it was scaled by.
        assert!(
            f.r() <= 77 && f.g() <= 77 && f.b() <= 77,
            "{f:?} not premultiplied"
        );
        assert_eq!(alpha(c, 255), c, "full opacity must be the identity");
        assert_eq!(alpha(c, 0), Color32::TRANSPARENT);
    }

    /// A wash must stay ordered, and the light-ground boost must strengthen it
    /// without wrapping — the same alpha reads weaker on paper than on ink.
    #[test]
    fn washes_are_ordered_and_boosted_on_paper() {
        let _g = lock();
        const _: () = assert!(wash::GHOST < wash::TINT);
        const _: () = assert!(wash::TINT < wash::SELECT);

        for strength in [wash::GHOST, wash::TINT, wash::SELECT] {
            set_mode(Mode::Night);
            let dark = wash::of(gold(), strength).a();
            set_mode(Mode::Paper);
            let light = wash::of(gold(), strength).a();
            assert_eq!(dark, strength, "night must use the strength as given");
            assert!(
                light >= dark,
                "paper must not weaken a wash ({light} < {dark})"
            );
        }
        set_mode(Mode::default());
    }
}
