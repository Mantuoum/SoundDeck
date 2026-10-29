//! Theme registry and the live palette.
//!
//! **Contract** (the one thing that keeps going wrong): a theme lives in
//! `config/themes/<id>.json`; the **file stem is the id** — the key
//! `settings.json` stores and every lookup matches — and the `"name"` field is
//! the display **name**. So renaming the file renames the theme: an id the
//! settings still point at no longer exists, and [`set_theme`] falls back to
//! Graphite **and says so** in its warnings instead of silently swapping the
//! user's colours out. A rename is still visible where it matters: the dropdown
//! lists the theme by its display name (`"name"`, or the id when that is empty),
//! and a vanished id is reported in the warnings.
//!
//! [`reload`] re-reads the folder and rebuilds the registry ([`reload_from`] is
//! the same pass over an explicit folder, for tests); a pre-0.6 `<id>.md` is
//! converted to JSON once on the way (see `migrate_markdown`). [`install`] is
//! the only entry point that touches egui (fonts + style). The active palette is
//! one `RwLock<Palette>` read through a generation-tagged thread-local snapshot,
//! so a theme switch is visible to every `palette()` caller on the next frame.

use std::cell::Cell;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{OnceLock, RwLock, RwLockReadGuard, RwLockWriteGuard};

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

/// The palette on screen and the id it was resolved from. Keeping the id next
/// to the palette is what lets [`reload_from`] notice that the file behind the
/// current colours is gone: the registry and the palette never disagree.
struct Current {
    id: Option<String>,
    palette: Palette,
}

static CURRENT: OnceLock<RwLock<Current>> = OnceLock::new();
static THEMES: OnceLock<RwLock<Vec<ThemeEntry>>> = OnceLock::new();

/// Bumped after every `CURRENT` write so the per-thread snapshot below can be
/// reused instead of re-acquiring the lock for each colour lookup.
static CURRENT_GENERATION: AtomicU64 = AtomicU64::new(0);

thread_local! {
    /// Last palette read on this thread, tagged with the generation it came
    /// from. `Palette` is `Copy`, so a hit is one atomic load and a 70-byte
    /// copy with no lock — widgets read several colours per frame, and a lock
    /// per colour per widget is pure churn.
    static PALETTE_SNAPSHOT: Cell<Option<(u64, Palette)>> = const { Cell::new(None) };
}

fn current_lock() -> &'static RwLock<Current> {
    CURRENT.get_or_init(|| {
        RwLock::new(Current {
            id: None,
            palette: theme_defaults::graphite(),
        })
    })
}

fn themes_lock() -> &'static RwLock<Vec<ThemeEntry>> {
    THEMES.get_or_init(|| RwLock::new(embedded_entries()))
}

/// The one poisoning policy for every theme lock: another thread's panic must
/// never lose the palette or take the UI down, so an unwound lock is read with
/// its last written value (`into_inner`). Degrade, never skip and never panic.
fn read_lock<T>(lock: &'static RwLock<T>) -> RwLockReadGuard<'static, T> {
    lock.read().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Write side of [`read_lock`]'s policy: the write always happens.
fn write_lock<T>(lock: &'static RwLock<T>) -> RwLockWriteGuard<'static, T> {
    lock.write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
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

/// Theme ids and dropdown labels for the settings combo, Graphite first.
///
/// The label is the display name (`name`, which the parser already falls back
/// to the id for) and nothing else: the dropdown shows the theme, not the file
/// it lives in. The dropdown draws the label as-is, so this is the one place
/// the rule lives.
pub fn available() -> Vec<(String, String)> {
    read_themes(|themes| {
        themes
            .iter()
            .map(|theme| (theme.id.clone(), theme.name.clone()))
            .collect()
    })
}

/// The active palette. Copy it once per paint function and read fields off the
/// copy; repeated calls are cheap (generation-tagged thread-local snapshot) but
/// a local still documents that the whole frame uses one consistent palette.
pub fn palette() -> Palette {
    let generation = CURRENT_GENERATION.load(Ordering::Acquire);
    PALETTE_SNAPSHOT.with(|snapshot| {
        if let Some((cached, palette)) = snapshot.get() {
            if cached == generation {
                return palette;
            }
        }
        let palette = read_lock(current_lock()).palette;
        snapshot.set(Some((generation, palette)));
        palette
    })
}

/// Publish a resolved palette as the one the UI paints with. The generation
/// bump happens after the write so snapshots taken before it read as stale.
fn publish(id: &str, palette: Palette) {
    {
        let mut current = write_lock(current_lock());
        current.id = Some(id.to_string());
        current.palette = palette;
    }
    CURRENT_GENERATION.fetch_add(1, Ordering::Release);
}

/// The one fallback rule: the entry for `id`, or Graphite plus a warning that
/// names the ids that do exist. Never silent — a silent swap is what makes a
/// user's edits look like they did not apply.
fn resolve(id: &str, themes: &[ThemeEntry]) -> (Palette, Vec<String>) {
    match themes.iter().find(|theme| theme.id == id) {
        Some(theme) => (theme.palette, Vec::new()),
        None => (
            theme_defaults::graphite(),
            vec![format!(
                "themes: theme \"{id}\" is not in config/themes (available: {}); using Graphite",
                themes
                    .iter()
                    .map(|theme| theme.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )],
        ),
    }
}

/// Select a parsed theme and publish it to the UI.
///
/// A missing id — the file was renamed or deleted — falls back to Graphite and
/// reports it in the returned warnings. Callers must log those: a silent
/// fallback is what makes a user's edits "not apply" and their theme "vanish".
#[must_use]
pub fn set_theme(id: &str) -> Vec<String> {
    let (palette, warnings) = read_themes(|themes| resolve(id, themes));
    publish(id, palette);
    warnings
}

/// Read the theme registry under the shared lock policy.
fn read_themes<T>(read: impl FnOnce(&[ThemeEntry]) -> T) -> T {
    read(&read_lock(themes_lock()))
}

/// Individual colour accessors kept for call-site brevity. Each one is a
/// snapshot hit after the first call on a thread; still, paint code that reads
/// three or more should hoist `let p = palette();` (see `volume::bar`).
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

/// Seed the five built-in themes only when the folder holds no theme file in
/// either format (JSON, or Markdown that is about to be converted). Existing
/// files are never overwritten.
fn seed_defaults(dir: &Path, warnings: &mut Vec<String>) {
    let has_themes = fs::read_dir(dir)
        .map(|entries| {
            entries.flatten().any(|entry| {
                let path = entry.path();
                theme_file::has_extension(&path, theme_file::THEME_EXTENSION)
                    || theme_file::has_extension(&path, theme_file::LEGACY_EXTENSION)
            })
        })
        .unwrap_or(false);
    if has_themes {
        return;
    }
    if let Err(err) = fs::create_dir_all(dir) {
        warnings.push(format!("themes: could not create {}: {err}", dir.display()));
        return;
    }
    for (id, name, palette) in theme_defaults::all() {
        let path = dir.join(format!("{id}.{}", theme_file::THEME_EXTENSION));
        if path.exists() {
            continue;
        }
        if let Err(err) = write_theme_file(&path, &theme_file::template(name, &palette)) {
            warnings.push(format!("themes: could not write {}: {err}", path.display()));
        }
    }
}

/// Bare file name for a warning line; the folder is already named by the message.
fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string())
}

/// Write one theme file without ever leaving a half-written one: the temp name
/// carries no theme extension, so a crash mid-write leaves an inert file instead
/// of a corrupt theme. (The config files' `.tmp`/`.bak` rotation belongs to
/// `storage`; the UI layer does not reach into it.)
fn write_theme_file(path: &Path, text: &str) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("{}.tmp", theme_file::THEME_EXTENSION));
    fs::write(&tmp, text)?;
    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = fs::remove_file(&tmp);
            Err(err)
        }
    }
}

/// Convert pre-0.6 `<id>.md` files to `<id>.json`, once per file.
///
/// The Markdown file is renamed `<id>.md.migrated` (never deleted) so nothing
/// lingers as a live theme. A `<id>.json` that already exists wins and is not
/// touched; the old file is then left beside it, inert and still convertible if
/// the JSON is ever removed.
fn migrate_markdown(dir: &Path, fallback: &Palette, warnings: &mut Vec<String>) {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| theme_file::has_extension(path, theme_file::LEGACY_EXTENSION))
                .collect()
        })
        .unwrap_or_default();
    files.sort();

    for path in files {
        let id = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default()
            .to_string();
        if id.is_empty() {
            continue;
        }
        let target = dir.join(format!("{id}.{}", theme_file::THEME_EXTENSION));
        if target.exists() {
            continue;
        }
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) => {
                warnings.push(format!("themes: could not read {}: {err}", path.display()));
                continue;
            }
        };
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        let parsed = theme_file::parse_markdown(&id, text, fallback);
        warnings.extend(parsed.warnings);
        if let Err(err) = write_theme_file(
            &target,
            &theme_file::template(&parsed.name, &parsed.palette),
        ) {
            warnings.push(format!(
                "themes: could not write {}: {err}",
                target.display()
            ));
            continue;
        }
        let migrated = path.with_extension(theme_file::MIGRATED_SUFFIX);
        let converted = format!(
            "themes: converted {} to {}",
            file_name(&path),
            file_name(&target)
        );
        match fs::rename(&path, &migrated) {
            Ok(()) => warnings.push(format!("{converted} (kept as {})", file_name(&migrated))),
            Err(err) => warnings.push(format!(
                "{converted} but could not rename the old file ({err}); the JSON wins"
            )),
        }
    }
}

/// Re-read `config/themes/*.json` and rebuild the in-memory registry.
pub fn reload() -> Vec<String> {
    reload_from(&themes_dir())
}

/// Registry rebuild for one folder; `reload()` is the only caller that picks
/// the real `config/themes` path (tests point it at a temp folder).
pub(crate) fn reload_from(dir: &Path) -> Vec<String> {
    let mut warnings = Vec::new();
    let fallback = theme_defaults::graphite();
    migrate_markdown(dir, &fallback, &mut warnings);
    seed_defaults(dir, &mut warnings);

    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| theme_file::has_extension(path, theme_file::THEME_EXTENSION))
                .collect()
        })
        .unwrap_or_default();
    files.sort();

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
        match theme_file::parse(&id, text, &fallback) {
            Ok(parsed) => {
                warnings.extend(parsed.warnings);
                entries.push(ThemeEntry {
                    id,
                    name: parsed.name,
                    palette: parsed.palette,
                });
            }
            // Corrupt JSON: skip this file, keep every other theme.
            Err(warning) => warnings.push(warning),
        }
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

    *write_lock(themes_lock()) = entries;
    // The palette on screen must still resolve: a reload that drops the file
    // behind it falls back to Graphite here, with the same warning `set_theme`
    // would give, instead of leaving colours no file describes. (Bound first:
    // an `if let` scrutinee's guard lives for the whole block, and `publish`
    // needs the write lock.)
    let current_id = read_lock(current_lock()).id.clone();
    if let Some(id) = current_id {
        let (palette, missing) = read_themes(|themes| resolve(&id, themes));
        warnings.extend(missing);
        publish(&id, palette);
    }
    warnings
}

/// Load the theme folder, apply the requested theme and return any warnings —
/// file problems and a missing stored id both reach the activity log from here.
pub fn install(ctx: &egui::Context, id: &str) -> Vec<String> {
    let mut warnings = reload();
    warnings.extend(set_theme(id));
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
    // Chrome text is not the user's text: the title bar, the credit, captions,
    // About rows, activity-log rows and card names must never paint a selection
    // (G2). Typed fields (search, rename, gain) are `TextEdit`s and keep their
    // own selection — this switch only reaches `Label`.
    style.interaction.selectable_labels = false;
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

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, MutexGuard};

    use super::*;

    /// The registry and the current palette are process-global, so every test
    /// that rebuilds them takes this lock and restores the built-in list on the
    /// way out — otherwise parallel tests read each other's temp folders.
    static REGISTRY_LOCK: Mutex<()> = Mutex::new(());

    struct RegistryGuard {
        _lock: MutexGuard<'static, ()>,
    }

    impl Drop for RegistryGuard {
        fn drop(&mut self) {
            *write_lock(themes_lock()) = embedded_entries();
            let mut current = write_lock(current_lock());
            current.id = None;
            current.palette = theme_defaults::graphite();
        }
    }

    fn registry_guard() -> RegistryGuard {
        let lock = REGISTRY_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        RegistryGuard { _lock: lock }
    }

    /// A throwaway theme folder under the system temp dir, removed on drop.
    struct TempThemeDir(PathBuf);

    impl TempThemeDir {
        fn new(tag: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default();
            let dir = std::env::temp_dir().join(format!(
                "sounddeck-theme-test-{tag}-{}-{nanos}",
                std::process::id()
            ));
            fs::create_dir_all(&dir).expect("create temp theme dir");
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempThemeDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// A hand-written theme file, in the shape a user would edit.
    fn write_theme(path: &Path, name: &str, rows: &[(&str, &str)]) {
        let mut body = format!("\"name\": \"{name}\"");
        for (token, value) in rows {
            body.push_str(&format!(",\n  \"{token}\": \"{value}\""));
        }
        fs::write(path, format!("{{\n  {body}\n}}\n")).expect("write theme file");
    }

    /// A real 0.5.0 theme file, verbatim — what a migration has to convert.
    fn write_markdown_theme(path: &Path, heading: &str, rows: &[(&str, &str)]) {
        let mut text = format!("# {heading}\n\n| token | value |\n| --- | --- |\n");
        for (token, value) in rows {
            text.push_str(&format!("| {token} | {value} |\n"));
        }
        fs::write(path, text).expect("write Markdown theme file");
    }

    fn files_with_extension(dir: &Path, extension: &str) -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = fs::read_dir(dir)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.path())
                    .filter(|path| theme_file::has_extension(path, extension))
                    .collect()
            })
            .unwrap_or_default();
        files.sort();
        files
    }

    /// Editing a colour value in an existing theme file and reloading must reach
    /// the palette the UI paints with — the whole `file → registry → current
    /// palette → `palette()` chain, not just the parse step.
    #[test]
    fn edited_colour_reaches_the_palette_after_reload() {
        let _registry = registry_guard();
        let dir = TempThemeDir::new("edit-colour");
        let path = dir.path().join("custom.json");
        write_theme(&path, "Custom", &[("bg", "#123456")]);
        reload_from(dir.path());
        let _ = set_theme("custom");
        assert_eq!(palette().bg, Color32::from_rgb(0x12, 0x34, 0x56));

        // Second edit, same id: Reload alone must repaint the new colour — no
        // reselect in the dropdown, no restart.
        write_theme(&path, "Custom", &[("bg", "#654321")]);
        let warnings = reload_from(dir.path());
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(palette().bg, Color32::from_rgb(0x65, 0x43, 0x21));
        assert_eq!(set_theme("custom"), Vec::<String>::new());
        assert_eq!(palette().bg, Color32::from_rgb(0x65, 0x43, 0x21));

        // Last hop: the egui style the app paints with is rebuilt from that
        // palette, so the edit is on screen and not just in the registry.
        let ctx = eframe::egui::Context::default();
        apply_style(&ctx);
        assert_eq!(
            ctx.style().visuals.panel_fill,
            Color32::from_rgb(0x65, 0x43, 0x21)
        );
        assert_eq!(
            ctx.style().visuals.override_text_color,
            Some(palette().text)
        );
    }

    /// The `name` field is the display name, so editing it must show up in the
    /// list the Appearance dropdown draws.
    #[test]
    fn name_change_updates_the_display_name() {
        let _registry = registry_guard();
        let dir = TempThemeDir::new("heading");
        let path = dir.path().join("mine.json");
        write_theme(&path, "First Name", &[("bg", "#101010")]);
        reload_from(dir.path());
        assert!(
            available()
                .iter()
                .any(|(id, label)| id == "mine" && label == "First Name"),
            "{:?}",
            available()
        );

        write_theme(&path, "Second Name", &[("bg", "#101010")]);
        reload_from(dir.path());
        assert!(
            available()
                .iter()
                .any(|(id, label)| id == "mine" && label == "Second Name"),
            "{:?}",
            available()
        );
    }

    /// The file stem is the id: renaming the file renames the theme.
    #[test]
    fn renaming_a_theme_file_changes_its_id() {
        let _registry = registry_guard();
        let dir = TempThemeDir::new("rename");
        let old = dir.path().join("old-id.json");
        write_theme(&old, "Mine", &[("bg", "#101010")]);
        reload_from(dir.path());
        assert!(available().iter().any(|(id, _)| id == "old-id"));

        fs::rename(&old, dir.path().join("new-id.json")).expect("rename theme file");
        reload_from(dir.path());
        let ids: Vec<String> = available().into_iter().map(|(id, _)| id).collect();
        assert!(ids.iter().any(|id| id == "new-id"), "{ids:?}");
        assert!(!ids.iter().any(|id| id == "old-id"), "{ids:?}");
    }

    /// The Reload button path: when the file behind the current theme is
    /// renamed away, reload must say so and fall back — not keep painting
    /// colours no file describes while the dropdown shows something else.
    #[test]
    fn reload_warns_when_the_current_theme_file_disappears() {
        let _registry = registry_guard();
        let dir = TempThemeDir::new("vanish");
        let path = dir.path().join("mine.json");
        write_theme(&path, "Mine", &[("bg", "#112233")]);
        reload_from(dir.path());
        assert_eq!(set_theme("mine"), Vec::<String>::new());
        assert_eq!(palette().bg, Color32::from_rgb(0x11, 0x22, 0x33));

        fs::rename(&path, dir.path().join("renamed.json")).expect("rename theme file");
        let warnings = reload_from(dir.path());
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("\"mine\"") && w.contains("using Graphite")),
            "{warnings:?}"
        );
        assert_eq!(palette(), theme_defaults::graphite());
    }

    /// Seeding is a first-run convenience: a folder that already holds a theme
    /// belongs to the user and must never be overwritten or added to.
    #[test]
    fn reload_keeps_user_theme_files_when_the_folder_has_themes() {
        let _registry = registry_guard();
        let dir = TempThemeDir::new("no-overwrite");
        let path = dir.path().join("mine.json");
        let original = "{\n  \"name\": \"Mine\",\n  \"bg\": \"#010203\"\n}\n";
        fs::write(&path, original).expect("write theme file");

        reload_from(dir.path());

        assert_eq!(
            fs::read_to_string(&path).expect("read theme file"),
            original
        );
        assert_eq!(
            files_with_extension(dir.path(), theme_file::THEME_EXTENSION),
            vec![path.clone()],
            "reload must not seed into a folder that already has themes"
        );
    }

    /// A folder with no theme file at all gets the five built-ins, as JSON.
    #[test]
    fn reload_seeds_json_files_into_an_empty_folder() {
        let _registry = registry_guard();
        let dir = TempThemeDir::new("seed");
        reload_from(dir.path());

        let seeded = files_with_extension(dir.path(), theme_file::THEME_EXTENSION);
        assert_eq!(seeded.len(), theme_defaults::all().len(), "{seeded:?}");
        assert!(
            files_with_extension(dir.path(), theme_file::LEGACY_EXTENSION).is_empty(),
            "seeding must write JSON, not Markdown"
        );
        let graphite = seeded
            .iter()
            .find(|path| path.file_stem().is_some_and(|stem| stem == "graphite"))
            .expect("graphite seeded");
        let text = fs::read_to_string(graphite).expect("read seeded theme");
        let parsed = theme_file::parse("graphite", &text, &theme_defaults::graphite())
            .expect("seeded file parses");
        assert_eq!(parsed.palette, theme_defaults::graphite());
    }

    /// A pre-0.6 Markdown theme is converted once: the JSON becomes the live
    /// theme and the old file is kept as `.md.migrated`, never deleted.
    #[test]
    fn markdown_theme_is_migrated_to_json_and_kept() {
        let _registry = registry_guard();
        let dir = TempThemeDir::new("migrate");
        let md = dir.path().join("legacy.md");
        write_markdown_theme(&md, "Legacy", &[("bg", "#123456")]);

        let warnings = reload_from(dir.path());
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("converted") && w.contains("legacy.md")),
            "one line per conversion: {warnings:?}"
        );

        let json = dir.path().join("legacy.json");
        assert!(json.exists(), "converted JSON missing");
        assert!(!md.exists(), "the old Markdown file must not stay live");
        let migrated = dir.path().join("legacy.md.migrated");
        assert!(migrated.exists(), "the old file must be kept, not deleted");
        assert_eq!(
            fs::read_to_string(&migrated).expect("read kept file"),
            "# Legacy\n\n| token | value |\n| --- | --- |\n| bg | #123456 |\n"
        );

        // The converted theme is the live one, with the same name and colour.
        assert!(
            available()
                .iter()
                .any(|(id, label)| id == "legacy" && label == "Legacy"),
            "{:?}",
            available()
        );
        assert_eq!(set_theme("legacy"), Vec::<String>::new());
        assert_eq!(palette().bg, Color32::from_rgb(0x12, 0x34, 0x56));

        // Second reload: the migration is finished and quiet.
        assert!(reload_from(dir.path()).is_empty());
    }

    /// Both formats present → the JSON wins and is never overwritten; the old
    /// Markdown file is left beside it, inert.
    #[test]
    fn json_wins_when_both_formats_exist() {
        let _registry = registry_guard();
        let dir = TempThemeDir::new("json-wins");
        let json = dir.path().join("both.json");
        let md = dir.path().join("both.md");
        write_theme(&json, "From JSON", &[("bg", "#010203")]);
        let md_text = "# From Markdown\n\n| token | value |\n| --- | --- |\n| bg | #040506 |\n";
        fs::write(&md, md_text).expect("write Markdown theme");

        let warnings = reload_from(dir.path());
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            fs::read_to_string(&json).expect("read json"),
            "{\n  \"name\": \"From JSON\",\n  \"bg\": \"#010203\"\n}\n",
            "the JSON file must not be overwritten"
        );
        assert_eq!(fs::read_to_string(&md).expect("read md"), md_text);
        assert_eq!(set_theme("both"), Vec::<String>::new());
        assert_eq!(palette().bg, Color32::from_rgb(0x01, 0x02, 0x03));
    }

    /// Corrupt JSON is skipped with a warning; the other themes still load.
    #[test]
    fn corrupt_theme_is_skipped_and_the_others_load() {
        let _registry = registry_guard();
        let dir = TempThemeDir::new("corrupt");
        fs::write(dir.path().join("broken.json"), "{ \"bg\": ").expect("write broken theme");
        write_theme(&dir.path().join("good.json"), "Good", &[("bg", "#0A0B0C")]);

        let warnings = reload_from(dir.path());
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("broken.json") && w.contains("not valid JSON")),
            "{warnings:?}"
        );
        let ids: Vec<String> = available().into_iter().map(|(id, _)| id).collect();
        assert!(!ids.iter().any(|id| id == "broken"), "{ids:?}");
        assert_eq!(set_theme("good"), Vec::<String>::new());
        assert_eq!(palette().bg, Color32::from_rgb(0x0A, 0x0B, 0x0C));
    }

    /// A stored id whose file was renamed must fall back to Graphite *loudly*:
    /// the warning names the missing id and the ids that are actually there.
    #[test]
    fn missing_theme_id_falls_back_to_graphite_with_a_warning() {
        let _registry = registry_guard();
        let dir = TempThemeDir::new("missing-id");
        write_theme(&dir.path().join("kept.json"), "Kept", &[("bg", "#101010")]);
        reload_from(dir.path());

        let warnings = set_theme("renamed-away");
        assert_eq!(palette(), theme_defaults::graphite());
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("renamed-away") && w.contains("using Graphite")),
            "{warnings:?}"
        );
        assert!(
            warnings.iter().any(|w| w.contains("kept")),
            "the warning should list the ids that do exist: {warnings:?}"
        );

        // A known id stays quiet.
        assert!(set_theme("kept").is_empty());
        assert_eq!(palette().bg, Color32::from_rgb(0x10, 0x10, 0x10));
    }

    /// The dropdown shows the display name and nothing else: no file name, no
    /// id suffix. A name that is missing or empty is the id (G4).
    #[test]
    fn dropdown_label_is_the_display_name() {
        let _registry = registry_guard();
        let dir = TempThemeDir::new("label");
        write_theme(
            &dir.path().join("mine.json"),
            "My Theme",
            &[("bg", "#101010")],
        );
        write_theme(&dir.path().join("same.json"), "same", &[("bg", "#101010")]);
        write_theme(&dir.path().join("unnamed.json"), "", &[("bg", "#101010")]);
        reload_from(dir.path());

        let choices = available();
        assert!(
            choices
                .iter()
                .any(|(id, label)| id == "mine" && label == "My Theme"),
            "the label is the name, with no id suffix: {choices:?}"
        );
        assert!(
            choices
                .iter()
                .any(|(id, label)| id == "same" && label == "same"),
            "a name that already spells the id reads the same: {choices:?}"
        );
        assert!(
            choices
                .iter()
                .any(|(id, label)| id == "unnamed" && label == "unnamed"),
            "an empty name falls back to the id: {choices:?}"
        );
        assert!(
            !choices.iter().any(|(_, label)| label.contains('(')),
            "no label may carry a parenthesised id: {choices:?}"
        );
    }

    /// G2: chrome text the user did not type must not be selectable. The switch
    /// lives on the app's own style, so every `Label` picks it up; `TextEdit`
    /// (search, rename, gain) is a separate widget and keeps its selection.
    #[test]
    fn static_labels_are_not_selectable() {
        let _registry = registry_guard();
        let ctx = eframe::egui::Context::default();
        assert!(
            ctx.style().interaction.selectable_labels,
            "egui's default is selectable, so this test can only pass through apply_style"
        );
        apply_style(&ctx);
        assert!(!ctx.style().interaction.selectable_labels);
    }

    /// The snapshot cache must not hide a theme switch made on the same thread.
    #[test]
    fn palette_snapshot_follows_theme_changes() {
        let _registry = registry_guard();
        let builtins = theme_defaults::all();
        let (id, _, expected) = &builtins[1];
        let _ = set_theme(id);
        assert_eq!(palette(), *expected);
        assert_eq!(bg(), expected.bg);
        let _ = set_theme("graphite");
        assert_eq!(palette(), theme_defaults::graphite());
        assert_eq!(bg(), theme_defaults::graphite().bg);
    }
}
