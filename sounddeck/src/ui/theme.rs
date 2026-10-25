use std::sync::{OnceLock, RwLock};

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Painter,
    Pos2, Rect, Stroke, StrokeKind, Vec2,
};

use crate::model::AppTheme;

pub const UI_FONT: &str = "satoshi";
pub const UI_FONT_MEDIUM: &str = "satoshi-medium";

#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub is_light: bool,
    pub bg: Color32,
    pub panel: Color32,
    pub panel_deep: Color32,
    pub card: Color32,
    pub card_hover: Color32,
    pub field: Color32,
    pub border: Color32,
    pub border_soft: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub faint: Color32,
    pub accent: Color32,
    pub accent_soft: Color32,
    pub purple: Color32,
    pub amber: Color32,
    pub red: Color32,
}

fn hex(value: u32) -> Color32 {
    Color32::from_rgb(
        ((value >> 16) & 0xFF) as u8,
        ((value >> 8) & 0xFF) as u8,
        (value & 0xFF) as u8,
    )
}

fn blend(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let lerp = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t) as u8;
    Color32::from_rgba_unmultiplied(
        lerp(a.r(), b.r()),
        lerp(a.g(), b.g()),
        lerp(a.b(), b.b()),
        lerp(a.a(), b.a()),
    )
}

fn macchiato() -> Palette {
    let bg = hex(0x24273a);
    let accent = hex(0x0097ff);
    Palette {
        is_light: false,
        bg,
        panel: hex(0x1e2030),
        panel_deep: hex(0x181926),
        card: hex(0x363a4f),
        card_hover: hex(0x494d64),
        field: hex(0x181926),
        border: hex(0x5b6078),
        border_soft: hex(0x494d64),
        text: hex(0xcad3f5),
        muted: hex(0xa5adcb),
        faint: hex(0x8087a2),
        accent,
        accent_soft: blend(bg, accent, 0.20),
        purple: hex(0xc6a0f6),
        amber: hex(0xf5a97f),
        red: hex(0xed8796),
    }
}

fn mocha() -> Palette {
    let bg = hex(0x1e1e2e);
    let accent = hex(0x0097ff);
    Palette {
        is_light: false,
        bg,
        panel: hex(0x181825),
        panel_deep: hex(0x11111b),
        card: hex(0x313244),
        card_hover: hex(0x45475a),
        field: hex(0x11111b),
        border: hex(0x585b70),
        border_soft: hex(0x45475a),
        text: hex(0xcdd6f4),
        muted: hex(0xa6adc8),
        faint: hex(0x7f849c),
        accent,
        accent_soft: blend(bg, accent, 0.20),
        purple: hex(0xcba6f7),
        amber: hex(0xfab387),
        red: hex(0xf38ba8),
    }
}

fn frappe() -> Palette {
    let bg = hex(0x303446);
    let accent = hex(0x0097ff);
    Palette {
        is_light: false,
        bg,
        panel: hex(0x292c3c),
        panel_deep: hex(0x232634),
        card: hex(0x414559),
        card_hover: hex(0x51576d),
        field: hex(0x232634),
        border: hex(0x626880),
        border_soft: hex(0x51576d),
        text: hex(0xc6d0f5),
        muted: hex(0xa5adce),
        faint: hex(0x838ba7),
        accent,
        accent_soft: blend(bg, accent, 0.20),
        purple: hex(0xca9ee6),
        amber: hex(0xef9f76),
        red: hex(0xe78284),
    }
}

fn latte() -> Palette {
    let bg = hex(0xeff1f5);
    let accent = hex(0x0097ff);
    Palette {
        is_light: true,
        bg,
        panel: hex(0xe6e9ef),
        panel_deep: hex(0xdce0e8),
        card: hex(0xccd0da),
        card_hover: hex(0xbcc0cc),
        field: hex(0xe6e9ef),
        border: hex(0xacb0be),
        border_soft: hex(0xccd0da),
        text: hex(0x4c4f69),
        muted: hex(0x6c6f85),
        faint: hex(0x8c8fa1),
        accent,
        accent_soft: blend(bg, accent, 0.18),
        purple: hex(0x8839ef),
        amber: hex(0xdf8e1d),
        red: hex(0xd20f39),
    }
}

fn graphite() -> Palette {
    let bg = hex(0x0e1013);
    let accent = hex(0x0097ff);
    Palette {
        is_light: false,
        bg,
        panel: hex(0x15171b),
        panel_deep: hex(0x111316),
        card: hex(0x1b1e23),
        card_hover: hex(0x20242a),
        field: hex(0x0c0e11),
        border: hex(0x30343c),
        border_soft: hex(0x252930),
        text: hex(0xe8ecef),
        muted: hex(0x8a919a),
        faint: hex(0x5c636c),
        accent,
        accent_soft: blend(bg, accent, 0.25),
        purple: hex(0x8b78e6),
        amber: hex(0xd6a656),
        red: hex(0xde6969),
    }
}

fn for_theme(theme: AppTheme) -> Palette {
    match theme {
        AppTheme::Macchiato => macchiato(),
        AppTheme::Mocha => mocha(),
        AppTheme::Frappe => frappe(),
        AppTheme::Latte => latte(),
        AppTheme::Graphite => graphite(),
    }
}

static CURRENT: OnceLock<RwLock<Palette>> = OnceLock::new();

fn current_lock() -> &'static RwLock<Palette> {
    CURRENT.get_or_init(|| RwLock::new(macchiato()))
}

pub fn palette() -> Palette {
    *current_lock()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub fn set_theme(theme: AppTheme) {
    if let Ok(mut current) = current_lock().write() {
        *current = for_theme(theme);
    }
}

pub fn bg() -> Color32 {
    palette().bg
}
pub fn panel() -> Color32 {
    palette().panel
}
pub fn panel_deep() -> Color32 {
    palette().panel_deep
}
pub fn card() -> Color32 {
    palette().card
}
pub fn card_hover() -> Color32 {
    palette().card_hover
}
pub fn field() -> Color32 {
    palette().field
}
pub fn border() -> Color32 {
    palette().border
}
pub fn border_soft() -> Color32 {
    palette().border_soft
}
pub fn text() -> Color32 {
    palette().text
}
pub fn muted() -> Color32 {
    palette().muted
}
pub fn faint() -> Color32 {
    palette().faint
}
pub fn accent() -> Color32 {
    palette().accent
}
pub fn purple() -> Color32 {
    palette().purple
}
pub fn amber() -> Color32 {
    palette().amber
}
pub fn red() -> Color32 {
    palette().red
}

// Fixed meter zones: an instrument, not a theme accent, so the levels stay
// readable in every palette and green is never used anywhere else in the UI.
pub fn meter_low() -> Color32 {
    Color32::from_rgb(0x5F, 0xD3, 0x7A)
}
pub fn meter_mid() -> Color32 {
    Color32::from_rgb(0xE8, 0xB8, 0x4B)
}
pub fn meter_high() -> Color32 {
    Color32::from_rgb(0xE5, 0x48, 0x4D)
}

pub fn ui_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

pub fn medium(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(UI_FONT_MEDIUM.into()))
}


pub fn install(ctx: &egui::Context, theme: AppTheme) {
    set_theme(theme);
    install_fonts(ctx);
    apply_style(ctx);
}

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        UI_FONT.to_owned(),
        std::sync::Arc::new(FontData::from_static(include_bytes!(
            "../../assets/Satoshi-Regular.ttf"
        ))),
    );
    fonts.font_data.insert(
        UI_FONT_MEDIUM.to_owned(),
        std::sync::Arc::new(FontData::from_static(include_bytes!(
            "../../assets/Satoshi-Medium.ttf"
        ))),
    );
    if let Some(family) = fonts.families.get_mut(&FontFamily::Proportional) {
        family.insert(0, UI_FONT.to_owned());
    }
    fonts.families.insert(
        FontFamily::Name(UI_FONT_MEDIUM.into()),
        vec![UI_FONT_MEDIUM.to_owned(), UI_FONT.to_owned()],
    );
    ctx.set_fonts(fonts);
}

pub fn apply_style(ctx: &egui::Context) {
    let p = palette();
    let mut style = (*ctx.style()).clone();
    let mut visuals = if p.is_light {
        egui::Visuals::light()
    } else {
        egui::Visuals::dark()
    };

    visuals.panel_fill = p.bg;
    visuals.window_fill = p.panel;
    visuals.faint_bg_color = p.panel_deep;
    visuals.extreme_bg_color = p.field;
    visuals.override_text_color = Some(p.text);
    visuals.window_stroke = Stroke::new(1.0_f32, p.border);
    visuals.window_corner_radius = CornerRadius::same(12);
    visuals.menu_corner_radius = CornerRadius::same(8);
    visuals.selection.bg_fill = alpha(p.accent, if p.is_light { 46 } else { 70 });
    visuals.selection.stroke = Stroke::new(1.0_f32, p.accent);
    visuals.hyperlink_color = p.accent;
    visuals.slider_trailing_fill = true;
    visuals.striped = false;

    visuals.widgets.noninteractive.bg_fill = p.panel;
    visuals.widgets.noninteractive.weak_bg_fill = p.panel;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, p.border_soft);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, p.muted);
    visuals.widgets.noninteractive.corner_radius = CornerRadius::same(8);

    visuals.widgets.inactive.bg_fill = p.card;
    visuals.widgets.inactive.weak_bg_fill = p.card;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, p.border);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, p.text);
    visuals.widgets.inactive.corner_radius = CornerRadius::same(8);

    visuals.widgets.hovered.bg_fill = p.card_hover;
    visuals.widgets.hovered.weak_bg_fill = p.card_hover;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, p.accent);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, p.text);
    visuals.widgets.hovered.corner_radius = CornerRadius::same(8);

    visuals.widgets.active.bg_fill = p.accent_soft;
    visuals.widgets.active.weak_bg_fill = p.accent_soft;
    visuals.widgets.active.bg_stroke = Stroke::new(1.0_f32, p.accent);
    visuals.widgets.active.fg_stroke = Stroke::new(1.0_f32, p.text);
    visuals.widgets.active.corner_radius = CornerRadius::same(8);

    visuals.widgets.open.bg_fill = p.card_hover;
    visuals.widgets.open.bg_stroke = Stroke::new(1.0_f32, p.border);
    visuals.widgets.open.fg_stroke = Stroke::new(1.0_f32, p.text);

    style.visuals = visuals;
    style.spacing.item_spacing = Vec2::new(8.0, 8.0);
    style.spacing.button_padding = Vec2::new(10.0, 5.0);
    style.spacing.interact_size = Vec2::new(28.0, 30.0);
    style.spacing.slider_width = 150.0;
    style.spacing.slider_rail_height = 5.0;
    style.spacing.scroll.bar_width = 7.0;
    style.spacing.scroll.floating = true;
    style.spacing.scroll.bar_inner_margin = 4.0;
    style.spacing.menu_margin = egui::Margin::symmetric(6, 6);
    style.spacing.window_margin = egui::Margin::same(0);
    style.text_styles.insert(
        egui::TextStyle::Body,
        FontId::new(14.5, FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Button,
        FontId::new(14.5, FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Small,
        FontId::new(11.5, FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Heading,
        FontId::new(20.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Monospace,
        FontId::new(12.5, FontFamily::Monospace),
    );
    ctx.set_style(style);
}

pub fn alpha(color: Color32, a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), a)
}

pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    blend(a, b, t)
}

/// Draw a dashed rounded-ish outline (edges only, corners left open) without
/// allocating extra widgets.
pub fn chain_icon(painter: &Painter, rect: Rect, color: Color32, linked: bool) {
    let stroke = Stroke::new(1.8_f32, color);
    let c = rect.center();
    let w = rect.width().min(rect.height()) * 0.36;
    let h = rect.height() * 0.22;
    let a = Rect::from_center_size(Pos2::new(c.x - w * 0.35, c.y), Vec2::new(w, h * 2.0));
    let b = Rect::from_center_size(Pos2::new(c.x + w * 0.35, c.y), Vec2::new(w, h * 2.0));
    painter.rect_stroke(a, CornerRadius::same(4), stroke, StrokeKind::Inside);
    painter.rect_stroke(b, CornerRadius::same(4), stroke, StrokeKind::Inside);
    if linked {
        painter.line_segment(
            [
                Pos2::new(c.x - w * 0.25, c.y),
                Pos2::new(c.x + w * 0.25, c.y),
            ],
            Stroke::new(2.0_f32, color),
        );
    }
}

pub fn speaker_icon(painter: &Painter, rect: Rect, color: Color32) {
    let c = rect.center();
    let s = rect.height().min(rect.width()) * 0.5;
    let x = c.x - s * 0.55;
    let body = Rect::from_min_max(
        Pos2::new(x, c.y - s * 0.22),
        Pos2::new(x + s * 0.30, c.y + s * 0.22),
    );
    painter.rect_filled(body, CornerRadius::same(1), color);
    painter.add(egui::Shape::convex_polygon(
        vec![
            Pos2::new(x + s * 0.30, c.y - s * 0.22),
            Pos2::new(c.x + s * 0.05, c.y - s * 0.48),
            Pos2::new(c.x + s * 0.05, c.y + s * 0.48),
            Pos2::new(x + s * 0.30, c.y + s * 0.22),
        ],
        color,
        Stroke::NONE,
    ));
    painter.circle_stroke(
        Pos2::new(c.x + s * 0.28, c.y),
        s * 0.30,
        Stroke::new(1.2_f32, color),
    );
}

pub fn truncate(painter: &Painter, text: &str, font: FontId, max_width: f32) -> String {
    if max_width <= 4.0 {
        return String::new();
    }
    let measure_color = palette().text;
    let full = painter.layout_no_wrap(text.to_owned(), font.clone(), measure_color);
    if full.size().x <= max_width {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut low = 0usize;
    let mut high = chars.len();
    while low < high {
        let mid = (low + high + 1) / 2;
        let candidate: String = chars[..mid].iter().collect::<String>() + "...";
        let width = painter
            .layout_no_wrap(candidate, font.clone(), measure_color)
            .size()
            .x;
        if width <= max_width {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    if low == 0 {
        "...".to_string()
    } else {
        chars[..low].iter().collect::<String>() + "..."
    }
}

