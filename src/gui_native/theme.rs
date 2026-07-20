//! P23 design system for the native GUI: the brand palette (lapis + gold, from
//! the P8 identity), embedded typography (Inter UI + Noto Sans Arabic fallback,
//! both OFL — licenses in `assets/fonts/`), widget styling (radii, spacing,
//! hover states), and small reusable components. Pure presentation — no I/O.

use eframe::egui;
use egui::{
    Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Stroke, TextStyle,
};

// ─── palette (derived from the brand: lapis #0E2A47 · gold #C8A24B) ──────────

/// Window base — near-black with a lapis cast.
pub const BG0: Color32 = Color32::from_rgb(0x0a, 0x0f, 0x1e);
/// Chrome surfaces (title bar, sidebar).
pub const BG1: Color32 = Color32::from_rgb(0x0d, 0x14, 0x26);
/// Cards.
pub const BG2: Color32 = Color32::from_rgb(0x12, 0x1a, 0x30);
/// Hover / active surfaces.
pub const BG3: Color32 = Color32::from_rgb(0x18, 0x22, 0x40);
/// Inputs (slightly recessed).
pub const BG_INPUT: Color32 = Color32::from_rgb(0x08, 0x0c, 0x18);
pub const INK: Color32 = Color32::from_rgb(0xe9, 0xec, 0xf8);
pub const DIM: Color32 = Color32::from_rgb(0x8e, 0x97, 0xb3);
pub const FAINT: Color32 = Color32::from_rgb(0x5a, 0x64, 0x80);
/// The brand gold — primary accent.
pub const GOLD: Color32 = Color32::from_rgb(0xc8, 0xa2, 0x4b);
pub const GOLD_HI: Color32 = Color32::from_rgb(0xdb, 0xb8, 0x62);
/// Text on a gold fill.
pub const ON_GOLD: Color32 = Color32::from_rgb(0x17, 0x12, 0x05);
/// Secondary accent — lightened brand lapis (links, info).
pub const LAPIS: Color32 = Color32::from_rgb(0x5e, 0x8b, 0xd6);
pub const GOOD: Color32 = Color32::from_rgb(0x3f, 0xbf, 0x7f);
pub const WARN: Color32 = Color32::from_rgb(0xe0, 0x91, 0x2f);
pub const BAD: Color32 = Color32::from_rgb(0xe5, 0x60, 0x60);

/// Hairline stroke used on cards and dividers.
pub fn stroke_faint() -> Stroke {
    Stroke::new(1.0, Color32::from_rgba_unmultiplied(0xe9, 0xec, 0xf8, 14))
}

// ─── radii ───────────────────────────────────────────────────────────────────

pub const R_WINDOW: u8 = 14;
pub const R_CARD: u8 = 10;
pub const R_BUTTON: u8 = 8;
pub const R_INPUT: u8 = 6;

// ─── typography ──────────────────────────────────────────────────────────────

const INTER_REGULAR: &[u8] = include_bytes!("../../assets/fonts/Inter-Regular.ttf");
const INTER_MEDIUM: &[u8] = include_bytes!("../../assets/fonts/Inter-Medium.ttf");
const INTER_SEMIBOLD: &[u8] = include_bytes!("../../assets/fonts/Inter-SemiBold.ttf");
const ARABIC: &[u8] = include_bytes!("../../assets/fonts/NotoSansArabic.ttf");

/// Family name for medium-weight UI text.
pub fn fam_medium() -> FontFamily {
    FontFamily::Name("inter-medium".into())
}

/// Family name for semibold headings / emphasis.
pub fn fam_semibold() -> FontFamily {
    FontFamily::Name("inter-semibold".into())
}

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    fonts
        .font_data
        .insert("inter".into(), FontData::from_static(INTER_REGULAR).into());
    fonts.font_data.insert(
        "inter-medium".into(),
        FontData::from_static(INTER_MEDIUM).into(),
    );
    fonts.font_data.insert(
        "inter-semibold".into(),
        FontData::from_static(INTER_SEMIBOLD).into(),
    );
    // Arabic fallback so Arabic file/session names render glyphs instead of
    // tofu. egui has no bidi/shaping (upstream), so Arabic renders unshaped —
    // legible glyphs, not joined calligraphy; the shaped brand wordmark is a
    // pre-rendered texture for exactly this reason.
    fonts
        .font_data
        .insert("arabic".into(), FontData::from_static(ARABIC).into());

    let prop = fonts.families.entry(FontFamily::Proportional).or_default();
    prop.insert(0, "inter".into());
    prop.push("arabic".into());
    let mono = fonts.families.entry(FontFamily::Monospace).or_default();
    mono.push("arabic".into());
    fonts.families.insert(
        fam_medium(),
        vec!["inter-medium".into(), "arabic".into(), "inter".into()],
    );
    fonts.families.insert(
        fam_semibold(),
        vec!["inter-semibold".into(), "arabic".into(), "inter".into()],
    );
    ctx.set_fonts(fonts);
}

// ─── style install ───────────────────────────────────────────────────────────

/// Installs the whole look: fonts, palette, widget visuals, spacing, radii.
pub fn install(ctx: &egui::Context) {
    install_fonts(ctx);

    let mut v = egui::Visuals::dark();
    v.override_text_color = Some(INK);
    v.panel_fill = Color32::TRANSPARENT; // panels paint their own rounded fills
    v.window_fill = BG2;
    v.window_stroke = stroke_faint();
    v.window_corner_radius = CornerRadius::same(R_CARD + 2);
    v.menu_corner_radius = CornerRadius::same(R_CARD);
    v.extreme_bg_color = BG_INPUT;
    v.faint_bg_color = Color32::from_rgba_unmultiplied(0xe9, 0xec, 0xf8, 6);
    v.hyperlink_color = LAPIS;
    v.selection.bg_fill = GOLD.linear_multiply(0.32);
    v.selection.stroke = Stroke::new(1.0, GOLD);

    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, INK);
    v.widgets.noninteractive.bg_stroke = stroke_faint();
    v.widgets.noninteractive.corner_radius = CornerRadius::same(R_BUTTON);

    v.widgets.inactive.bg_fill = BG2;
    v.widgets.inactive.weak_bg_fill = BG2;
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, INK);
    v.widgets.inactive.bg_stroke = stroke_faint();
    v.widgets.inactive.corner_radius = CornerRadius::same(R_BUTTON);

    v.widgets.hovered.bg_fill = BG3;
    v.widgets.hovered.weak_bg_fill = BG3;
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, INK);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, GOLD.linear_multiply(0.45));
    v.widgets.hovered.corner_radius = CornerRadius::same(R_BUTTON);
    v.widgets.hovered.expansion = 1.0;

    v.widgets.active.bg_fill = BG3;
    v.widgets.active.weak_bg_fill = BG3;
    v.widgets.active.fg_stroke = Stroke::new(1.0, GOLD_HI);
    v.widgets.active.bg_stroke = Stroke::new(1.0, GOLD);
    v.widgets.active.corner_radius = CornerRadius::same(R_BUTTON);

    v.widgets.open.bg_fill = BG3;
    v.widgets.open.weak_bg_fill = BG3;
    v.widgets.open.corner_radius = CornerRadius::same(R_BUTTON);

    // Tooltips and menus borrow the window dressing, so they inherit the card
    // language rather than egui's default box.
    v.window_shadow = egui::Shadow {
        offset: [0, 6],
        blur: 22,
        spread: 0,
        color: Color32::from_black_alpha(120),
    };
    v.popup_shadow = v.window_shadow;

    ctx.set_visuals(v);

    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = egui::vec2(10.0, 8.0);
        s.spacing.button_padding = egui::vec2(14.0, 6.0);
        s.spacing.interact_size.y = 30.0;
        s.spacing.window_margin = Margin::same(12);
        s.spacing.menu_margin = Margin::same(8);
        s.spacing.indent = 18.0;
        // A hairline rail that stays out of the way: floating, thin, and
        // invisible until the pointer is in the area — egui's default solid
        // bar is the last piece of stock chrome on screen.
        s.spacing.scroll = egui::style::ScrollStyle {
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
        // These five sizes and families are mirrored by `a11y::BASE_*`, which
        // re-derives this whole table whenever the user scales text. Change one,
        // change both, or the first Ctrl+0 silently resizes the window.
        s.text_styles = [
            (
                TextStyle::Heading,
                FontId::new(19.0, FontFamily::Name("inter-semibold".into())),
            ),
            (TextStyle::Body, FontId::new(13.5, FontFamily::Proportional)),
            (
                TextStyle::Button,
                FontId::new(13.5, FontFamily::Name("inter-medium".into())),
            ),
            (
                TextStyle::Small,
                FontId::new(11.5, FontFamily::Proportional),
            ),
            (
                TextStyle::Monospace,
                FontId::new(12.5, FontFamily::Monospace),
            ),
        ]
        .into();
    });
}

// ─── components ──────────────────────────────────────────────────────────────

/// A standard card frame: rounded, hairline stroke, comfortable padding.
pub fn card() -> egui::Frame {
    egui::Frame::new()
        .fill(BG2)
        .stroke(stroke_faint())
        .corner_radius(R_CARD)
        .inner_margin(Margin::same(12))
}

/// A small rounded status pill.
pub fn pill(ui: &mut egui::Ui, text: &str, color: Color32) {
    egui::Frame::new()
        .fill(color.linear_multiply(0.16))
        .corner_radius(9)
        .inner_margin(Margin::symmetric(8, 1))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(text)
                    .color(color)
                    .size(10.5)
                    .family(fam_medium()),
            );
        });
}

/// A status dot with a soft halo glow.
pub fn glow_dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
    let c = rect.center();
    let p = ui.painter();
    p.circle_filled(c, 6.0, color.linear_multiply(0.18));
    p.circle_filled(c, 3.2, color);
}

/// Section heading with the gold tick.
pub fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(egui::vec2(3.0, 14.0), egui::Sense::hover());
        ui.painter().rect_filled(r, 2.0, GOLD);
        ui.label(
            egui::RichText::new(title)
                .size(13.0)
                .family(fam_semibold())
                .color(INK),
        );
    });
    ui.add_space(2.0);
}

/// Skeleton placeholder bar (loading states).
pub fn skeleton(ui: &mut egui::Ui, width: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 12.0), egui::Sense::hover());
    let t = ui.input(|i| i.time);
    let pulse = ((t * 2.2).sin() * 0.5 + 0.5) as f32;
    let a = 10.0 + 14.0 * pulse;
    ui.painter().rect_filled(
        rect,
        6.0,
        Color32::from_rgba_unmultiplied(0xe9, 0xec, 0xf8, a as u8),
    );
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_millis(50));
}

/// Linear-interpolate two colors (for hover transitions).
pub fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| -> u8 { (x as f32 + (y as f32 - x as f32) * t).round() as u8 };
    Color32::from_rgba_unmultiplied(
        l(a.r(), b.r()),
        l(a.g(), b.g()),
        l(a.b(), b.b()),
        l(a.a(), b.a()),
    )
}
