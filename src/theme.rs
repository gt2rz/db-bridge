use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, FontTweak, Frame,
    Margin, RichText, Stroke, TextStyle, Theme,
};
use std::sync::Arc;

pub const BG: Color32 = Color32::from_rgb(0x0F, 0x11, 0x15);
pub const SIDEBAR: Color32 = Color32::from_rgb(0x13, 0x15, 0x1B);
pub const SURFACE: Color32 = Color32::from_rgb(0x16, 0x19, 0x20);
pub const RAISED: Color32 = Color32::from_rgb(0x26, 0x2A, 0x33);
pub const HOVER: Color32 = Color32::from_rgb(0x1F, 0x23, 0x2B);
pub const BORDER: Color32 = Color32::from_rgb(0x26, 0x2A, 0x33);
pub const BORDER_STRONG: Color32 = Color32::from_rgb(0x33, 0x38, 0x44);

pub const TEXT: Color32 = Color32::from_rgb(0xE6, 0xE8, 0xEC);
pub const TEXT_SOFT: Color32 = Color32::from_rgb(0xC9, 0xCE, 0xD6);
pub const MUTED: Color32 = Color32::from_rgb(0x9A, 0xA1, 0xAD);
pub const SUBTLE: Color32 = Color32::from_rgb(0x7D, 0x85, 0x93);
pub const FAINT: Color32 = Color32::from_rgb(0x5C, 0x63, 0x70);

pub const ACCENT: Color32 = Color32::from_rgb(0x5B, 0x9B, 0xFF);
pub const ON_ACCENT: Color32 = Color32::from_rgb(0x0B, 0x12, 0x20);
pub const SUCCESS: Color32 = Color32::from_rgb(0x3D, 0xD6, 0x8C);
pub const WARNING: Color32 = Color32::from_rgb(0xF0, 0xA4, 0x3A);
pub const DANGER: Color32 = Color32::from_rgb(0xF2, 0x6D, 0x6D);
pub const NUMBER: Color32 = Color32::from_rgb(0xF7, 0x8C, 0x6C);
pub const KEYWORD: Color32 = Color32::from_rgb(0xC7, 0x92, 0xEA);
pub const STRING: Color32 = Color32::from_rgb(0xC3, 0xE8, 0x8D);
pub const IDENT: Color32 = Color32::from_rgb(0x7F, 0xDB, 0xCA);

const SEMIBOLD: &str = "semibold";

pub fn install(ctx: &egui::Context) {
    install_fonts(ctx);
    ctx.set_theme(Theme::Dark);
    ctx.style_mut_of(Theme::Dark, |style| {
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
        visuals.dark_mode = true;
        visuals.panel_fill = BG;
        visuals.window_fill = SURFACE;
        visuals.window_stroke = Stroke::new(1.0, BORDER);
        visuals.window_corner_radius = CornerRadius::same(10);
        visuals.menu_corner_radius = CornerRadius::same(8);
        visuals.extreme_bg_color = BG;
        visuals.text_edit_bg_color = Some(BG);
        visuals.faint_bg_color = SIDEBAR;
        visuals.code_bg_color = HOVER;
        visuals.weak_text_color = Some(SUBTLE);
        visuals.hyperlink_color = ACCENT;
        visuals.warn_fg_color = WARNING;
        visuals.error_fg_color = DANGER;
        visuals.selection.bg_fill = ACCENT.gamma_multiply(0.28);
        visuals.selection.stroke = Stroke::new(1.0, TEXT);

        let radius = CornerRadius::same(7);
        let widgets = &mut visuals.widgets;
        widgets.noninteractive.bg_fill = SURFACE;
        widgets.noninteractive.weak_bg_fill = SURFACE;
        widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
        widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
        widgets.noninteractive.corner_radius = radius;

        widgets.inactive.bg_fill = RAISED;
        widgets.inactive.weak_bg_fill = SURFACE;
        widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER_STRONG);
        widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
        widgets.inactive.corner_radius = radius;

        widgets.hovered.bg_fill = BORDER_STRONG;
        widgets.hovered.weak_bg_fill = HOVER;
        widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(0x46, 0x4D, 0x5B));
        widgets.hovered.fg_stroke = Stroke::new(1.5, TEXT);
        widgets.hovered.corner_radius = radius;
        widgets.hovered.expansion = 0.0;

        widgets.active.bg_fill = BORDER_STRONG;
        widgets.active.weak_bg_fill = RAISED;
        widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
        widgets.active.fg_stroke = Stroke::new(1.5, TEXT);
        widgets.active.corner_radius = radius;
        widgets.active.expansion = 0.0;

        widgets.open.weak_bg_fill = HOVER;
        widgets.open.bg_stroke = Stroke::new(1.0, BORDER_STRONG);
        widgets.open.corner_radius = radius;
    });
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
        .fill(SURFACE)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(CornerRadius::same(10))
}

pub fn primary_button(text: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(strong(text).color(ON_ACCENT))
        .fill(ACCENT)
        .stroke(Stroke::NONE)
}

pub fn warning_button(text: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text).color(WARNING))
        .fill(WARNING.gamma_multiply(0.12))
        .stroke(Stroke::new(1.0, WARNING.gamma_multiply(0.45)))
}

pub fn section_label(ui: &mut egui::Ui, text: &str) {
    ui.label(
        RichText::new(text.to_uppercase())
            .family(semibold())
            .size(11.0)
            .color(SUBTLE),
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
        .fill(BG)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(10, 2))
        .show(ui, |ui| {
            let widgets = &mut ui.visuals_mut().widgets;
            widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
            widgets.inactive.bg_stroke = Stroke::NONE;
            widgets.hovered.bg_stroke = Stroke::NONE;
            ui.horizontal(|ui| {
                ui.label(RichText::new(label).size(12.0).color(SUBTLE));
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
        .fill(BG)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(3))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.horizontal(|ui| {
                for &(value, label) in options {
                    let selected = *current == value;
                    let text = if selected {
                        RichText::new(label).color(TEXT)
                    } else {
                        RichText::new(label).color(MUTED)
                    };
                    let button = egui::Button::new(text)
                        .selected(selected)
                        .fill(if selected {
                            RAISED
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
