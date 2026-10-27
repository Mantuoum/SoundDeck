use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Painter, Pos2,
    Rect, Stroke, StrokeKind, Vec2,
};

use super::theme_color::blend;
use super::theme_defaults;
use super::theme_file;

pub const UI_FONT: &str = "satoshi";
pub const UI_FONT_MEDIUM: &str = "satoshi-medium";

/// Corner-radius scale. Every rounded surface in the app picks one of these
/// so controls, cards and popups stay visually consistent.
///
/// - `SM`: nested inside a control (segmented pill, tiny badges)
/// - `MD`: every interactive control (buttons, fields, combos, chips, pills)
/// - `LG`: cards, drag ghosts, inner panels
/// - `XL`: modals, popups, the settings card
pub const RADIUS_SM: u8 = 5;
pub const RADIUS_MD: u8 = 8;
pub const RADIUS_LG: u8 = 12;
pub const RADIUS_XL: u8 = 14;

#[derive(Clone, Copy, Debug, PartialEq)]
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

pub struct ThemeEntry {
    pub id: String,
    pub name: String,
    pub palette: Palette,
}

static CURRENT: OnceLock<RwLock<Palette>> = OnceLock::new();
static THEMES: OnceLock<RwLock<Vec<ThemeEntry>>> = OnceLock::new();

fn current_lock() -> &'static RwLock<Palette> {
    CURRENT.get_or_init(|| RwLock::new(theme_defaults::graphite()))
}

fn themes_lock() -> &'static RwLock<Vec<ThemeEntry>> {
    THEMES.get_or_init(|| RwLock::new(embedded_entries()))
}

fn embedded_entries() -> Vec<ThemeEntry> {
    theme_defaults::all()
        .into_iter()
        .map(|(id, name, palette)| ThemeEntry {
            id: id.to_string(),
            name: name.to_string(),
            palette,
        })
        .collect()
}

/// Theme ids and display names for the settings dropdown, Graphite first.
pub fn available() -> Vec<(String, String)> {
    themes_lock()
        .read()
        .map(|themes| {
            themes
                .iter()
                .map(|theme| (theme.id.clone(), theme.name.clone()))
                .collect()
        })
        .unwrap_or_default()
}

pub fn palette() -> Palette {
    *current_lock()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Select a parsed theme. Unknown ids (or a deleted theme) fall back to Graphite.
pub fn set_theme(id: &str) {
    let palette = themes_lock()
        .read()
        .ok()
        .and_then(|themes| {
            themes
                .iter()
                .find(|theme| theme.id == id)
                .map(|theme| theme.palette)
        })
        .unwrap_or_else(theme_defaults::graphite);
    if let Ok(mut current) = current_lock().write() {
        *current = palette;
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

fn themes_dir() -> PathBuf {
    crate::model::config_dir().join("themes")
}

/// Seed the five built-in themes only when the folder has no Markdown files.
/// Existing files are never overwritten.
fn seed_defaults(dir: &Path, warnings: &mut Vec<String>) {
    let has_markdown = fs::read_dir(dir)
        .map(|entries| {
            entries.flatten().any(|entry| {
                entry
                    .path()
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .map(|ext| ext.eq_ignore_ascii_case("md"))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false);
    if has_markdown {
        return;
    }
    if let Err(err) = fs::create_dir_all(dir) {
        warnings.push(format!("themes: could not create {}: {err}", dir.display()));
        return;
    }
    for (id, name, palette) in theme_defaults::all() {
        let path = dir.join(format!("{id}.md"));
        if path.exists() {
            continue;
        }
        if let Err(err) = fs::write(&path, theme_file::template(name, &palette)) {
            warnings.push(format!("themes: could not write {}: {err}", path.display()));
        }
    }
}

/// Re-read `config/themes/*.md` and rebuild the in-memory registry.
pub fn reload() -> Vec<String> {
    let dir = themes_dir();
    let mut warnings = Vec::new();
    seed_defaults(&dir, &mut warnings);

    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| {
                    path.extension()
                        .and_then(|ext| ext.to_str())
                        .map(|ext| ext.eq_ignore_ascii_case("md"))
                        .unwrap_or(false)
                })
                .collect()
        })
        .unwrap_or_default();
    files.sort();

    let fallback = theme_defaults::graphite();
    let mut entries = Vec::new();
    for path in &files {
        let id = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default()
            .to_string();
        if id.is_empty() {
            continue;
        }
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) => {
                warnings.push(format!("themes: could not read {}: {err}", path.display()));
                continue;
            }
        };
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        let parsed = theme_file::parse(&id, text, &fallback);
        warnings.extend(parsed.warnings);
        entries.push(ThemeEntry {
            id,
            name: parsed.name,
            palette: parsed.palette,
        });
    }

    if files.is_empty() {
        // Missing or unwritable folder: keep the built-in list working.
        entries = embedded_entries();
    } else if !entries.iter().any(|theme| theme.id == "graphite") {
        entries.push(ThemeEntry {
            id: "graphite".into(),
            name: "Graphite".into(),
            palette: fallback,
        });
    }

    entries.sort_by(|a, b| {
        (a.id != "graphite", a.name.to_lowercase())
            .cmp(&(b.id != "graphite", b.name.to_lowercase()))
    });

    if let Ok(mut themes) = themes_lock().write() {
        *themes = entries;
    }
    warnings
}

/// Load the theme folder, apply the requested theme and return any file warnings.
pub fn install(ctx: &egui::Context, id: &str) -> Vec<String> {
    let warnings = reload();
    set_theme(id);
    install_fonts(ctx);
    apply_style(ctx);
    warnings
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
    visuals.window_corner_radius = CornerRadius::same(RADIUS_LG);
    visuals.menu_corner_radius = CornerRadius::same(RADIUS_MD);
    visuals.selection.bg_fill = alpha(p.accent, if p.is_light { 46 } else { 70 });
    visuals.selection.stroke = Stroke::new(1.0_f32, p.accent);
    visuals.hyperlink_color = p.accent;
    visuals.slider_trailing_fill = true;
    visuals.striped = false;

    visuals.widgets.noninteractive.bg_fill = p.panel;
    visuals.widgets.noninteractive.weak_bg_fill = p.panel;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, p.border_soft);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, p.muted);
    visuals.widgets.noninteractive.corner_radius = CornerRadius::same(RADIUS_MD);

    visuals.widgets.inactive.bg_fill = p.card;
    visuals.widgets.inactive.weak_bg_fill = p.card;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, p.border);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, p.text);
    visuals.widgets.inactive.corner_radius = CornerRadius::same(RADIUS_MD);

    visuals.widgets.hovered.bg_fill = p.card_hover;
    visuals.widgets.hovered.weak_bg_fill = p.card_hover;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, p.accent);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, p.text);
    visuals.widgets.hovered.corner_radius = CornerRadius::same(RADIUS_MD);

    visuals.widgets.active.bg_fill = p.accent_soft;
    visuals.widgets.active.weak_bg_fill = p.accent_soft;
    visuals.widgets.active.bg_stroke = Stroke::new(1.0_f32, p.accent);
    visuals.widgets.active.fg_stroke = Stroke::new(1.0_f32, p.text);
    visuals.widgets.active.corner_radius = CornerRadius::same(RADIUS_MD);

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
        let mid = (low + high).div_ceil(2);
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
