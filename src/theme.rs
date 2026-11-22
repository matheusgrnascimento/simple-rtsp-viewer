//! Windows 11 style look: light/dark palette following the system, Windows
//! accent color, Segoe fonts and Mica backdrop.

use egui::{Color32, FontFamily, FontId};

/// Environment variable that turns the Mica backdrop off when set to `0`,
/// `false` or `off` (for example, if the video driver does not composite the
/// transparent window well).
pub const MICA_ENV: &str = "RTSP_VIEWER_MICA";

/// Default Windows accent color, used when the system one cannot be read.
const DEFAULT_ACCENT: Color32 = Color32::from_rgb(0x00, 0x78, 0xD4);

/// Windows 11 status colors for dark backgrounds (status notices are drawn over
/// the video, in a dark pill, in both themes).
pub const SUCCESS: Color32 = Color32::from_rgb(0x6C, 0xCB, 0x5F);
pub const CAUTION: Color32 = Color32::from_rgb(0xFC, 0xE1, 0x00);
pub const CRITICAL: Color32 = Color32::from_rgb(0xFF, 0x99, 0xA4);

/// Translucent background of the labels and notices drawn over the video.
pub const OVERLAY_CHIP: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 170);

/// Glyphs of the Windows icon font (Segoe Fluent Icons / MDL2 Assets).
pub const ICON_VIDEO: &str = "\u{E714}";
pub const ICON_BACK_TO_WINDOW: &str = "\u{E73F}";
pub const ICON_SEARCH: &str = "\u{E721}";
pub const ICON_IMPORT: &str = "\u{E8B5}";
pub const ICON_KEY: &str = "\u{E8D7}";
pub const ICON_INFO: &str = "\u{E946}";
pub const ICON_WARNING: &str = "\u{E7BA}";
pub const ICON_SUCCESS: &str = "\u{E73E}";
pub const ICON_CLOSE: &str = "\u{E711}";
pub const ICON_REFRESH: &str = "\u{E72C}";
pub const ICON_ADD: &str = "\u{E710}";
pub const ICON_MENU: &str = "\u{E700}";
pub const ICON_VOLUME: &str = "\u{E767}";
pub const ICON_MUTE: &str = "\u{E74F}";

/// Semibold font family, used in titles.
const SEMIBOLD: &str = "semibold";
/// Windows icon font family.
const ICONS: &str = "icons";

/// Interface colors for one theme (light or dark).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    pub dark: bool,
    /// Window background when Mica is not active.
    pub window_background: Color32,
    /// Background of the camera tiles: bars around the video and while waiting
    /// for the first frame. Dark in both themes, as in video players.
    pub tile_background: Color32,
    pub tile_border: Color32,
    pub tile_border_hover: Color32,
    pub text: Color32,
    pub text_secondary: Color32,
    /// Windows accent color, adjusted to the theme as the system does (lighter
    /// in dark mode, darker in light mode).
    pub accent: Color32,
    /// Text on the accent color (primary button).
    pub on_accent: Color32,
    /// Dialog background.
    pub dialog_background: Color32,
    /// Buttons and controls: normal, hovered and pressed background, and
    /// border.
    pub control_fill: Color32,
    pub control_fill_hover: Color32,
    pub control_fill_pressed: Color32,
    pub control_stroke: Color32,
    /// Text field background.
    pub input_background: Color32,
    /// Success and error text on the theme background.
    pub success_text: Color32,
    pub critical_text: Color32,
    /// Backgrounds of the notice bar (InfoBar): info and error.
    pub notice_info: Color32,
    pub notice_error: Color32,
    /// Sidebar items: background under the pointer and when selected. They are
    /// translucent, like the Windows 11 navigation items, so they work both on
    /// Mica and on the solid fallback background.
    pub item_hover: Color32,
    pub item_selected: Color32,
    /// Cards inside the sidebar (the "Viewing" card).
    pub card_background: Color32,
    /// Thin lines between sidebar sections.
    pub divider: Color32,
    /// Off state of a switch: border, and the knob.
    pub switch_off_stroke: Color32,
    pub switch_off_knob: Color32,
}

impl Palette {
    pub fn new(dark: bool, system_accent: Color32) -> Self {
        if dark {
            Self {
                dark,
                window_background: Color32::from_rgb(0x20, 0x20, 0x20),
                tile_background: Color32::from_rgb(0x12, 0x12, 0x12),
                tile_border: Color32::from_white_alpha(18),
                tile_border_hover: Color32::from_white_alpha(56),
                text: Color32::WHITE,
                text_secondary: Color32::from_rgb(0xC5, 0xC5, 0xC5),
                accent: mix(system_accent, Color32::WHITE, 0.4),
                on_accent: Color32::BLACK,
                dialog_background: Color32::from_rgb(0x2B, 0x2B, 0x2B),
                control_fill: Color32::from_rgb(0x37, 0x37, 0x37),
                control_fill_hover: Color32::from_rgb(0x3C, 0x3C, 0x3C),
                control_fill_pressed: Color32::from_rgb(0x31, 0x31, 0x31),
                control_stroke: Color32::from_white_alpha(20),
                input_background: Color32::from_rgb(0x1F, 0x1F, 0x1F),
                success_text: SUCCESS,
                critical_text: CRITICAL,
                notice_info: Color32::from_rgb(0x2B, 0x2B, 0x2B),
                notice_error: Color32::from_rgb(0x44, 0x27, 0x26),
                item_hover: Color32::from_white_alpha(12),
                item_selected: Color32::from_white_alpha(20),
                card_background: Color32::from_white_alpha(10),
                divider: Color32::from_white_alpha(20),
                switch_off_stroke: Color32::from_white_alpha(140),
                switch_off_knob: Color32::from_white_alpha(200),
            }
        } else {
            Self {
                dark,
                window_background: Color32::from_rgb(0xF3, 0xF3, 0xF3),
                tile_background: Color32::from_rgb(0x1C, 0x1C, 0x1C),
                tile_border: Color32::from_black_alpha(20),
                tile_border_hover: Color32::from_black_alpha(60),
                text: Color32::from_rgb(0x1A, 0x1A, 0x1A),
                text_secondary: Color32::from_rgb(0x5D, 0x5D, 0x5D),
                accent: mix(system_accent, Color32::BLACK, 0.15),
                on_accent: Color32::WHITE,
                dialog_background: Color32::from_rgb(0xF9, 0xF9, 0xF9),
                control_fill: Color32::from_rgb(0xFB, 0xFB, 0xFB),
                control_fill_hover: Color32::from_rgb(0xF5, 0xF5, 0xF5),
                control_fill_pressed: Color32::from_rgb(0xEE, 0xEE, 0xEE),
                control_stroke: Color32::from_black_alpha(28),
                input_background: Color32::WHITE,
                success_text: Color32::from_rgb(0x0F, 0x7B, 0x0F),
                critical_text: Color32::from_rgb(0xC4, 0x2B, 0x1C),
                notice_info: Color32::from_rgb(0xF6, 0xF6, 0xF6),
                notice_error: Color32::from_rgb(0xFD, 0xE7, 0xE9),
                item_hover: Color32::from_black_alpha(10),
                item_selected: Color32::from_black_alpha(16),
                card_background: Color32::from_rgba_unmultiplied(255, 255, 255, 150),
                divider: Color32::from_black_alpha(24),
                switch_off_stroke: Color32::from_black_alpha(130),
                switch_off_knob: Color32::from_black_alpha(160),
            }
        }
    }
}

/// Mixes `from` with `to` in the `amount` ratio (0 = only `from`, 1 = only
/// `to`).
pub fn mix(from: Color32, to: Color32, amount: f32) -> Color32 {
    let channel = |a: u8, b: u8| {
        let mixed = f32::from(a) + (f32::from(b) - f32::from(a)) * amount;
        mixed.round().clamp(0.0, 255.0) as u8
    };
    Color32::from_rgb(
        channel(from.r(), to.r()),
        channel(from.g(), to.g()),
        channel(from.b(), to.b()),
    )
}

/// Accent color chosen in Settings > Personalization > Colors.
pub fn system_accent() -> Color32 {
    read_system_accent().unwrap_or(DEFAULT_ACCENT)
}

#[cfg(windows)]
fn read_system_accent() -> Option<Color32> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let dwm = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(r"Software\Microsoft\Windows\DWM")
        .ok()?;
    // Stored as 0xAABBGGRR.
    let abgr: u32 = dwm.get_value("AccentColor").ok()?;
    let [red, green, blue, _alpha] = abgr.to_le_bytes();
    Some(Color32::from_rgb(red, green, blue))
}

#[cfg(not(windows))]
fn read_system_accent() -> Option<Color32> {
    None
}

/// The Mica backdrop was not turned off by `MICA_ENV`.
pub fn mica_requested() -> bool {
    !matches!(
        std::env::var(MICA_ENV).as_deref().map(str::trim),
        Ok("0" | "false" | "off")
    )
}

/// Applies the Mica backdrop to the window, with the title bar in the given
/// theme. Returns `false` where Mica does not exist (Windows 10, other
/// systems).
pub fn apply_mica_on_start(cc: &eframe::CreationContext<'_>, dark: bool) -> bool {
    window_vibrancy::apply_mica(cc, Some(dark)).is_ok()
}

/// Reapplies Mica (and the title bar theme) after a theme change.
pub fn apply_mica_on_frame(frame: &eframe::Frame, dark: bool) -> bool {
    window_vibrancy::apply_mica(frame, Some(dark)).is_ok()
}

/// Loads the Windows fonts: Segoe UI for text, Segoe UI Semibold for titles and
/// the system icon font. The fonts are read from the Windows fonts folder,
/// without embedding them in the executable; missing ones fall back to egui's
/// default fonts. Returns `true` if the icon font was loaded.
pub fn install_fonts(ctx: &egui::Context) -> bool {
    let (fonts, icons) = windows_font_definitions();
    ctx.set_fonts(fonts);
    icons
}

/// egui font definitions with the Windows fonts found, and whether the icon
/// font is among them.
fn windows_font_definitions() -> (egui::FontDefinitions, bool) {
    let mut fonts = egui::FontDefinitions::default();
    let defaults = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();

    let mut load = |name: &str, files: &[&str]| -> bool {
        match files.iter().find_map(|file| read_windows_font(file)) {
            Some(bytes) => {
                fonts
                    .font_data
                    .insert(name.to_owned(), egui::FontData::from_owned(bytes));
                true
            }
            None => false,
        }
    };

    let regular = load("Segoe UI", &["segoeui.ttf"]);
    let semibold = load("Segoe UI Semibold", &["seguisb.ttf"]);
    // Segoe Fluent Icons on Windows 11, Segoe MDL2 Assets on Windows 10.
    let icons = load("Segoe Icons", &["SegoeIcons.ttf", "segmdl2.ttf"]);

    let mut proportional = defaults.clone();
    if regular {
        proportional.insert(0, "Segoe UI".to_owned());
    }
    let mut semibold_family = proportional.clone();
    if semibold {
        semibold_family.insert(0, "Segoe UI Semibold".to_owned());
    }

    if icons {
        // The regular fonts are added as fallback: egui requires every family
        // to have the replacement character ('◻' or '?'), which the icon font
        // lacks. Without it, egui prints a warning to the console when the
        // family is used.
        let mut icon_family = vec!["Segoe Icons".to_owned()];
        icon_family.extend(proportional.iter().cloned());
        fonts
            .families
            .insert(FontFamily::Name(ICONS.into()), icon_family);
    }
    fonts
        .families
        .insert(FontFamily::Proportional, proportional);
    // The semibold family always exists (at worst, equal to the regular one),
    // so `semibold` never points to a family without fonts.
    fonts
        .families
        .insert(FontFamily::Name(SEMIBOLD.into()), semibold_family);

    (fonts, icons)
}

fn read_windows_font(file: &str) -> Option<Vec<u8>> {
    let windows = std::env::var_os("WINDIR")?;
    std::fs::read(std::path::Path::new(&windows).join("Fonts").join(file)).ok()
}

/// Semibold font (titles).
pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SEMIBOLD.into()))
}

/// Windows icon font. Can only be used if `install_fonts` returned `true`.
pub fn icon_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(ICONS.into()))
}

/// Applies the palette to the egui style: Windows 11 colors, rounded corners
/// and text sizes. With Mica, the panel background is transparent to let the
/// effect show through.
pub fn apply_style(ctx: &egui::Context, palette: &Palette, mica: bool) {
    let mut visuals = if palette.dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    visuals.panel_fill = if mica {
        Color32::TRANSPARENT
    } else {
        palette.window_background
    };
    // Dialogs and menus.
    visuals.window_fill = palette.dialog_background;
    visuals.window_stroke = egui::Stroke::new(1.0_f32, palette.control_stroke);
    visuals.window_rounding = egui::Rounding::same(8.0);
    visuals.menu_rounding = egui::Rounding::same(8.0);
    visuals.extreme_bg_color = palette.input_background;
    visuals.selection.bg_fill = palette.accent;
    visuals.selection.stroke = egui::Stroke::new(1.0_f32, palette.accent);
    visuals.hyperlink_color = palette.accent;
    for widget in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        widget.rounding = egui::Rounding::same(4.0);
    }
    // Buttons and controls: subtle background with a thin border, a little
    // lighter when hovered and darker when pressed, as in Windows 11.
    let control_stroke = egui::Stroke::new(1.0_f32, palette.control_stroke);
    visuals.widgets.inactive.weak_bg_fill = palette.control_fill;
    visuals.widgets.inactive.bg_fill = palette.control_fill;
    visuals.widgets.inactive.bg_stroke = control_stroke;
    visuals.widgets.hovered.weak_bg_fill = palette.control_fill_hover;
    visuals.widgets.hovered.bg_fill = palette.control_fill_hover;
    visuals.widgets.hovered.bg_stroke = control_stroke;
    visuals.widgets.active.weak_bg_fill = palette.control_fill_pressed;
    visuals.widgets.active.bg_fill = palette.control_fill_pressed;
    visuals.widgets.active.bg_stroke = control_stroke;
    visuals.widgets.open.weak_bg_fill = palette.control_fill_hover;
    ctx.set_visuals(visuals);

    ctx.style_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.spacing.button_padding = egui::vec2(12.0, 5.0);
        style.spacing.window_margin = egui::Margin::same(20.0);
        use egui::TextStyle;
        style
            .text_styles
            .insert(TextStyle::Body, FontId::proportional(14.0));
        style
            .text_styles
            .insert(TextStyle::Button, FontId::proportional(14.0));
        style
            .text_styles
            .insert(TextStyle::Small, FontId::proportional(12.0));
        style.text_styles.insert(TextStyle::Heading, semibold(20.0));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mix_interpolates_each_channel() {
        let from = Color32::from_rgb(0, 100, 200);
        assert_eq!(mix(from, Color32::WHITE, 0.0), from);
        assert_eq!(mix(from, Color32::WHITE, 1.0), Color32::WHITE);
        assert_eq!(
            mix(from, Color32::WHITE, 0.5),
            Color32::from_rgb(128, 178, 228)
        );
    }

    #[test]
    fn every_font_family_keeps_the_default_fonts_as_fallback() {
        // egui's default fonts have the replacement character; without them in
        // a family, egui warns "Failed to find replacement characters".
        let defaults = egui::FontDefinitions::default().families[&FontFamily::Proportional].clone();
        let (fonts, _icons) = windows_font_definitions();
        for (family, names) in &fonts.families {
            if *family == FontFamily::Monospace {
                continue;
            }
            for default in &defaults {
                assert!(
                    names.contains(default),
                    "family {family:?} does not have the default font {default}"
                );
            }
        }
    }

    #[test]
    fn accent_is_lighter_in_dark_theme_and_darker_in_light_theme() {
        let accent = Color32::from_rgb(0x00, 0x78, 0xD4);
        let brightness = |c: Color32| u32::from(c.r()) + u32::from(c.g()) + u32::from(c.b());
        assert!(brightness(Palette::new(true, accent).accent) > brightness(accent));
        assert!(brightness(Palette::new(false, accent).accent) < brightness(accent));
    }
}
