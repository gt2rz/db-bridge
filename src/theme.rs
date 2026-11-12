use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, FontTweak, Frame,
    Margin, RichText, Stroke, TextStyle, Theme, ThemePreference,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::model::ThemeMode;

/// Colores de la interfaz para un modo. La activa se elige en cada frame con `sync`.
struct Palette {
    bg: Color32,
    sidebar: Color32,
    surface: Color32,
    raised: Color32,
    hover: Color32,
    border: Color32,
    border_strong: Color32,
    border_hover: Color32,
    text: Color32,
    text_soft: Color32,
    muted: Color32,
    subtle: Color32,
    faint: Color32,
    accent: Color32,
    on_accent: Color32,
    success: Color32,
    warning: Color32,
    danger: Color32,
    number: Color32,
    keyword: Color32,
    string: Color32,
    ident: Color32,
}

const DARK: Palette = Palette {
    bg: Color32::from_rgb(0x0F, 0x11, 0x15),
    sidebar: Color32::from_rgb(0x13, 0x15, 0x1B),
    surface: Color32::from_rgb(0x16, 0x19, 0x20),
    raised: Color32::from_rgb(0x26, 0x2A, 0x33),
    hover: Color32::from_rgb(0x1F, 0x23, 0x2B),
    border: Color32::from_rgb(0x26, 0x2A, 0x33),
    border_strong: Color32::from_rgb(0x33, 0x38, 0x44),
    border_hover: Color32::from_rgb(0x46, 0x4D, 0x5B),
    text: Color32::from_rgb(0xE6, 0xE8, 0xEC),
    text_soft: Color32::from_rgb(0xC9, 0xCE, 0xD6),
    muted: Color32::from_rgb(0x9A, 0xA1, 0xAD),
    subtle: Color32::from_rgb(0x7D, 0x85, 0x93),
    faint: Color32::from_rgb(0x5C, 0x63, 0x70),
    accent: Color32::from_rgb(0x5B, 0x9B, 0xFF),
    on_accent: Color32::from_rgb(0x0B, 0x12, 0x20),
    success: Color32::from_rgb(0x3D, 0xD6, 0x8C),
    warning: Color32::from_rgb(0xF0, 0xA4, 0x3A),
    danger: Color32::from_rgb(0xF2, 0x6D, 0x6D),
    number: Color32::from_rgb(0xF7, 0x8C, 0x6C),
    keyword: Color32::from_rgb(0xC7, 0x92, 0xEA),
    string: Color32::from_rgb(0xC3, 0xE8, 0x8D),
    ident: Color32::from_rgb(0x7F, 0xDB, 0xCA),
};

/// Misma jerarquía que el oscuro: fondo gris claro, tarjetas blancas y colores de estado y de
/// sintaxis más oscuros para que mantengan el contraste sobre blanco.
const LIGHT: Palette = Palette {
    bg: Color32::from_rgb(0xF4, 0xF5, 0xF7),
    sidebar: Color32::from_rgb(0xEC, 0xEE, 0xF2),
    surface: Color32::from_rgb(0xFF, 0xFF, 0xFF),
    raised: Color32::from_rgb(0xE3, 0xE6, 0xEB),
    hover: Color32::from_rgb(0xEC, 0xEE, 0xF2),
    border: Color32::from_rgb(0xDD, 0xE1, 0xE7),
    border_strong: Color32::from_rgb(0xC8, 0xCD, 0xD5),
    border_hover: Color32::from_rgb(0xA9, 0xB0, 0xBC),
    text: Color32::from_rgb(0x1A, 0x1D, 0x23),
    text_soft: Color32::from_rgb(0x36, 0x3C, 0x46),
    muted: Color32::from_rgb(0x55, 0x5C, 0x69),
    subtle: Color32::from_rgb(0x6B, 0x72, 0x80),
    faint: Color32::from_rgb(0x9A, 0xA1, 0xAD),
    accent: Color32::from_rgb(0x25, 0x63, 0xEB),
    on_accent: Color32::from_rgb(0xFF, 0xFF, 0xFF),
    success: Color32::from_rgb(0x15, 0x80, 0x4D),
    warning: Color32::from_rgb(0xA8, 0x5D, 0x00),
    danger: Color32::from_rgb(0xC8, 0x2E, 0x2E),
    number: Color32::from_rgb(0xB4, 0x45, 0x0F),
    keyword: Color32::from_rgb(0x7C, 0x3A, 0xBF),
    string: Color32::from_rgb(0x3B, 0x76, 0x0F),
    ident: Color32::from_rgb(0x0E, 0x74, 0x6E),
};

static LIGHT_ACTIVE: AtomicBool = AtomicBool::new(false);

fn palette() -> &'static Palette {
    if LIGHT_ACTIVE.load(Ordering::Relaxed) {
        &LIGHT
    } else {
        &DARK
    }
}

/// Un acceso por color de la paleta activa: `theme::text()`, `theme::accent()`…
macro_rules! palette_colors {
    ($($name:ident),* $(,)?) => {
        $(
            pub fn $name() -> Color32 {
                palette().$name
            }
        )*
    };
}

palette_colors!(
    bg,
    sidebar,
    surface,
    raised,
    hover,
    border,
    border_strong,
    border_hover,
    text,
    text_soft,
    muted,
    subtle,
    faint,
    accent,
    on_accent,
    success,
    warning,
    danger,
    number,
    keyword,
    string,
    ident,
);

const SEMIBOLD: &str = "semibold";

pub fn install(ctx: &egui::Context, mode: ThemeMode) {
    install_fonts(ctx);
    for (theme, palette) in [(Theme::Dark, &DARK), (Theme::Light, &LIGHT)] {
        ctx.style_mut_of(theme, |style| apply_style(style, theme, palette));
    }
    set_mode(ctx, mode);
}

/// Cambia el modo; `Auto` sigue el tema claro u oscuro del sistema.
pub fn set_mode(ctx: &egui::Context, mode: ThemeMode) {
    ctx.set_theme(match mode {
        ThemeMode::Auto => ThemePreference::System,
        ThemeMode::Light => ThemePreference::Light,
        ThemeMode::Dark => ThemePreference::Dark,
    });
    sync(ctx);
}

/// Elige la paleta del tema en uso. Se llama al empezar cada frame porque en `Auto` el sistema
/// puede cambiar de tema con la app abierta.
pub fn sync(ctx: &egui::Context) {
    LIGHT_ACTIVE.store(ctx.theme() == Theme::Light, Ordering::Relaxed);
}

fn apply_style(style: &mut egui::Style, theme: Theme, palette: &Palette) {
    style.text_styles = [
        (TextStyle::Small, FontId::proportional(11.5)),
        (TextStyle::Body, FontId::proportional(13.5)),
        (TextStyle::Button, FontId::proportional(13.5)),
        (TextStyle::Heading, FontId::new(15.0, semibold())),
        (TextStyle::Monospace, FontId::monospace(12.5)),
    ]
    .into();

    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(12.0, 6.0);
    style.spacing.interact_size.y = 30.0;

    let visuals = &mut style.visuals;
    visuals.dark_mode = theme == Theme::Dark;
    visuals.panel_fill = palette.bg;
    visuals.window_fill = palette.surface;
    visuals.window_stroke = Stroke::new(1.0, palette.border);
    visuals.window_corner_radius = CornerRadius::same(10);
    visuals.menu_corner_radius = CornerRadius::same(8);
    visuals.extreme_bg_color = palette.bg;
    visuals.text_edit_bg_color = Some(palette.bg);
    visuals.faint_bg_color = palette.sidebar;
    visuals.code_bg_color = palette.hover;
    visuals.weak_text_color = Some(palette.subtle);
    visuals.hyperlink_color = palette.accent;
    visuals.warn_fg_color = palette.warning;
    visuals.error_fg_color = palette.danger;
    visuals.selection.bg_fill = palette.accent.gamma_multiply(0.28);
    visuals.selection.stroke = Stroke::new(1.0, palette.text);

    let radius = CornerRadius::same(7);
    let widgets = &mut visuals.widgets;
    widgets.noninteractive.bg_fill = palette.surface;
    widgets.noninteractive.weak_bg_fill = palette.surface;
    widgets.noninteractive.bg_stroke = Stroke::new(1.0, palette.border);
    widgets.noninteractive.fg_stroke = Stroke::new(1.0, palette.text);
    widgets.noninteractive.corner_radius = radius;

    widgets.inactive.bg_fill = palette.raised;
    widgets.inactive.weak_bg_fill = palette.surface;
    widgets.inactive.bg_stroke = Stroke::new(1.0, palette.border_strong);
    widgets.inactive.fg_stroke = Stroke::new(1.0, palette.text);
    widgets.inactive.corner_radius = radius;

    widgets.hovered.bg_fill = palette.border_strong;
    widgets.hovered.weak_bg_fill = palette.hover;
    widgets.hovered.bg_stroke = Stroke::new(1.0, palette.border_hover);
    widgets.hovered.fg_stroke = Stroke::new(1.5, palette.text);
    widgets.hovered.corner_radius = radius;
    widgets.hovered.expansion = 0.0;

    widgets.active.bg_fill = palette.border_strong;
    widgets.active.weak_bg_fill = palette.raised;
    widgets.active.bg_stroke = Stroke::new(1.0, palette.accent);
    widgets.active.fg_stroke = Stroke::new(1.5, palette.text);
    widgets.active.corner_radius = radius;
    widgets.active.expansion = 0.0;

    widgets.open.weak_bg_fill = palette.hover;
    widgets.open.bg_stroke = Stroke::new(1.0, palette.border_strong);
    widgets.open.corner_radius = radius;
}

/// Una fuente instalada en el sistema. `weight` fija el eje `wght` en fuentes variables.
struct SystemFont {
    path: &'static str,
    weight: Option<f32>,
}

const fn font(path: &'static str, weight: Option<f32>) -> SystemFont {
    SystemFont { path, weight }
}

#[cfg(target_os = "macos")]
const SANS: &[SystemFont] = &[
    font("/System/Library/Fonts/SFNS.ttf", Some(400.0)),
    font("/System/Library/Fonts/Helvetica.ttc", None),
];
#[cfg(target_os = "macos")]
const SANS_SEMIBOLD: &[SystemFont] = &[font("/System/Library/Fonts/SFNS.ttf", Some(600.0))];
#[cfg(target_os = "macos")]
const MONO: &[SystemFont] = &[
    font("/System/Library/Fonts/SFNSMono.ttf", Some(400.0)),
    font("/System/Library/Fonts/Menlo.ttc", None),
];

#[cfg(target_os = "windows")]
const SANS: &[SystemFont] = &[font("C:\\Windows\\Fonts\\segoeui.ttf", None)];
#[cfg(target_os = "windows")]
const SANS_SEMIBOLD: &[SystemFont] = &[font("C:\\Windows\\Fonts\\seguisb.ttf", None)];
#[cfg(target_os = "windows")]
const MONO: &[SystemFont] = &[
    font("C:\\Windows\\Fonts\\CascadiaMono.ttf", Some(400.0)),
    font("C:\\Windows\\Fonts\\consola.ttf", None),
];

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const SANS: &[SystemFont] = &[
    font("/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf", None),
    font("/usr/share/fonts/noto/NotoSans-Regular.ttf", None),
    font("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", None),
];
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const SANS_SEMIBOLD: &[SystemFont] = &[
    font("/usr/share/fonts/truetype/noto/NotoSans-SemiBold.ttf", None),
    font("/usr/share/fonts/noto/NotoSans-SemiBold.ttf", None),
    font("/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf", None),
];
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const MONO: &[SystemFont] = &[
    font(
        "/usr/share/fonts/truetype/noto/NotoSansMono-Regular.ttf",
        None,
    ),
    font("/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf", None),
];

/// Lee la primera fuente disponible de la lista. No se distribuye ninguna fuente con la app:
/// se usan las del sistema operativo y, si no hay ninguna, las que trae egui.
fn load_first(candidates: &[SystemFont]) -> Option<FontData> {
    candidates.iter().find_map(|candidate| {
        let bytes = std::fs::read(candidate.path).ok()?;
        let mut tweak = FontTweak::default();
        if let Some(weight) = candidate.weight {
            tweak.coords.push(*b"wght", weight);
        }
        Some(FontData::from_owned(bytes).tweak(tweak))
    })
}

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    // Las fuentes por defecto de egui quedan detrás como respaldo para símbolos.
    let fallbacks = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();

    let mut semibold_chain = fallbacks.clone();
    for (name, candidates, family) in [
        ("system-sans", SANS, FontFamily::Proportional),
        ("system-mono", MONO, FontFamily::Monospace),
        ("system-sans-semibold", SANS_SEMIBOLD, semibold()),
    ] {
        let Some(data) = load_first(candidates) else {
            continue;
        };
        fonts.font_data.insert(name.to_string(), Arc::new(data));
        if family == semibold() {
            semibold_chain.insert(0, name.to_string());
        } else {
            fonts
                .families
                .entry(family)
                .or_default()
                .insert(0, name.to_string());
        }
    }
    // Sin semibold propia, el título usa la sans normal antes que la de egui.
    if !fonts.font_data.contains_key("system-sans-semibold")
        && fonts.font_data.contains_key("system-sans")
    {
        semibold_chain.insert(0, "system-sans".to_string());
    }
    fonts.families.insert(semibold(), semibold_chain);

    ctx.set_fonts(fonts);
}

pub fn semibold() -> FontFamily {
    FontFamily::Name(SEMIBOLD.into())
}

pub fn strong(text: impl Into<String>) -> RichText {
    RichText::new(text).family(semibold())
}

pub fn mono(text: impl Into<String>) -> RichText {
    RichText::new(text).monospace()
}

pub fn card() -> Frame {
    Frame::NONE
        .fill(surface())
        .stroke(Stroke::new(1.0, border()))
        .corner_radius(CornerRadius::same(10))
}

pub fn primary_button(text: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(strong(text).color(on_accent()))
        .fill(accent())
        .stroke(Stroke::NONE)
}

pub fn warning_button(text: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text).color(warning()))
        .fill(warning().gamma_multiply(0.12))
        .stroke(Stroke::new(1.0, warning().gamma_multiply(0.45)))
}

pub fn section_label(ui: &mut egui::Ui, text: &str) {
    ui.label(
        RichText::new(text.to_uppercase())
            .family(semibold())
            .size(11.0)
            .color(subtle()),
    );
}

pub fn badge(ui: &mut egui::Ui, text: &str, fg: Color32, bg: Color32) -> egui::Response {
    Frame::NONE
        .fill(bg)
        .corner_radius(CornerRadius::same(4))
        .inner_margin(Margin::symmetric(6, 1))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(11.0).color(fg));
        })
        .response
}

/// Campo con la etiqueta dentro del mismo marco que el control.
pub fn field<R>(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    Frame::NONE
        .fill(bg())
        .stroke(Stroke::new(1.0, border()))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(10, 2))
        .show(ui, |ui| {
            let widgets = &mut ui.visuals_mut().widgets;
            widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
            widgets.inactive.bg_stroke = Stroke::NONE;
            widgets.hovered.bg_stroke = Stroke::NONE;
            ui.horizontal(|ui| {
                ui.label(RichText::new(label).size(12.0).color(subtle()));
                add(ui)
            })
            .inner
        })
        .inner
}

pub fn dot(ui: &mut egui::Ui, color: Color32, radius: f32) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(radius * 2.0, radius * 2.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), radius, color);
}

/// Control segmentado: un grupo de opciones exclusivas dentro de un mismo marco.
pub fn segmented<T: PartialEq + Copy>(
    ui: &mut egui::Ui,
    current: &mut T,
    options: &[(T, &str)],
) -> bool {
    let mut changed = false;
    Frame::NONE
        .fill(bg())
        .stroke(Stroke::new(1.0, border()))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(3))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.horizontal(|ui| {
                for &(value, label) in options {
                    let selected = *current == value;
                    let text = if selected {
                        RichText::new(label).color(text())
                    } else {
                        RichText::new(label).color(muted())
                    };
                    let button = egui::Button::new(text)
                        .selected(selected)
                        .fill(if selected {
                            raised()
                        } else {
                            Color32::TRANSPARENT
                        })
                        .stroke(Stroke::NONE)
                        .min_size(egui::vec2(0.0, 26.0));
                    if ui.add(button).clicked() && !selected {
                        *current = value;
                        changed = true;
                    }
                }
            });
        });
    changed
}
