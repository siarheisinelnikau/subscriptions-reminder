use egui::{Color32, Context, FontId, RichText, Stroke, Theme, ThemePreference, Visuals};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Preference {
    #[default]
    System,
    Dark,
    Light,
}

impl Preference {
    pub const ALL: [Self; 3] = [Self::System, Self::Dark, Self::Light];

    pub fn label(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::Dark => "Dark",
            Self::Light => "Light",
        }
    }

    pub fn apply(self, ctx: &Context) {
        ctx.set_theme(match self {
            Self::System => ThemePreference::System,
            Self::Dark => ThemePreference::Dark,
            Self::Light => ThemePreference::Light,
        });
        ctx.request_repaint();
    }

    pub fn window_theme(self) -> Option<winit::window::Theme> {
        match self {
            Self::System => None,
            Self::Dark => Some(winit::window::Theme::Dark),
            Self::Light => Some(winit::window::Theme::Light),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SubscriptionSize {
    #[default]
    Compact,
    Standard,
    Large,
}

impl SubscriptionSize {
    pub const ALL: [Self; 3] = [Self::Compact, Self::Standard, Self::Large];

    pub fn label(self) -> &'static str {
        match self {
            Self::Compact => "Compact",
            Self::Standard => "Standard",
            Self::Large => "Large",
        }
    }

    pub fn height(self) -> f32 {
        match self {
            Self::Compact => 48.0,
            Self::Standard => 64.0,
            Self::Large => 80.0,
        }
    }

    pub fn padding(self) -> i8 {
        match self {
            Self::Compact => 6,
            Self::Standard => 14,
            Self::Large => 22,
        }
    }
}

#[derive(Clone, Copy)]
pub struct Palette {
    pub background: Color32,
    pub card: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub accent: Color32,
    pub warning: Color32,
    pub danger: Color32,
    pub primary: Color32,
    pub primary_text: Color32,
    pub card_border: Color32,
    border: Color32,
    control: Color32,
    pub hovered: Color32,
    pressed: Color32,
    selected: Color32,
    input: Color32,
    link: Color32,
}

impl Palette {
    fn new(dark: bool) -> Self {
        let gray = Color32::from_gray;
        if dark {
            Self {
                background: Color32::from_rgb(23, 24, 27),
                card: Color32::from_rgb(32, 33, 38),
                text: Color32::from_rgb(239, 239, 242),
                muted: Color32::from_rgb(162, 164, 175),
                accent: Color32::from_rgb(137, 167, 237),
                warning: Color32::from_rgb(224, 181, 132),
                danger: Color32::from_rgb(244, 143, 143),
                primary: Color32::from_rgb(51, 66, 96),
                primary_text: Color32::from_rgb(239, 239, 242),
                card_border: Color32::from_rgb(51, 53, 60),
                border: Color32::from_rgb(62, 65, 76),
                control: Color32::from_rgb(43, 44, 51),
                hovered: Color32::from_rgb(53, 55, 64),
                pressed: Color32::from_rgb(58, 66, 88),
                selected: Color32::from_rgb(51, 66, 96),
                input: Color32::from_rgb(23, 24, 27),
                link: Color32::from_rgb(137, 167, 237),
            }
        } else {
            Self {
                background: gray(255),
                card: gray(247),
                text: gray(31),
                muted: gray(98),
                accent: gray(45),
                warning: Color32::from_rgb(132, 78, 0),
                danger: Color32::from_rgb(180, 35, 24),
                primary: gray(36),
                primary_text: gray(255),
                card_border: gray(225),
                border: gray(195),
                control: gray(242),
                hovered: gray(230),
                pressed: gray(217),
                selected: gray(221),
                input: gray(255),
                link: Color32::from_rgb(0, 95, 184),
            }
        }
    }

    pub fn for_ui(ui: &egui::Ui) -> Self {
        Self::new(ui.visuals().dark_mode)
    }

    pub fn for_context(ctx: &Context) -> Self {
        Self::new(ctx.style().visuals.dark_mode)
    }

    pub fn primary_button(self, label: &str) -> egui::Button<'_> {
        egui::Button::new(RichText::new(label).strong().color(self.primary_text))
            .fill(self.primary)
            .stroke(Stroke::NONE)
    }
}

/// Configure both styles once. Theme changes then select an existing style.
pub fn install(ctx: &Context, preference: Preference) {
    // Use the installed Windows font without redistributing its font file.
    if let Some(windows) = std::env::var_os("WINDIR")
        && let Ok(bytes) =
            std::fs::read(std::path::PathBuf::from(windows).join("Fonts/segoeui.ttf"))
    {
        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.insert(
            "windows-ui".into(),
            egui::FontData::from_owned(bytes).into(),
        );
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .insert(0, "windows-ui".into());
        ctx.set_fonts(fonts);
    }
    for mode in [Theme::Dark, Theme::Light] {
        let dark = mode == Theme::Dark;
        let colors = Palette::new(dark);
        let mut style = (*ctx.style_of(mode)).clone();
        style.visuals = if dark {
            Visuals::dark()
        } else {
            Visuals::light()
        };
        let visuals = &mut style.visuals;
        visuals.panel_fill = colors.background;
        visuals.window_fill = colors.card;
        visuals.override_text_color = Some(colors.text);
        visuals.weak_text_color = Some(colors.muted);
        visuals.selection.bg_fill = colors.selected;
        visuals.selection.stroke = Stroke::new(1.0, colors.text);
        visuals.hyperlink_color = colors.link;
        visuals.faint_bg_color = colors.card;
        visuals.extreme_bg_color = colors.input;
        visuals.text_edit_bg_color = Some(colors.input);
        visuals.warn_fg_color = colors.warning;
        visuals.error_fg_color = colors.danger;
        visuals.text_cursor.stroke = Stroke::new(1.5, colors.text);
        visuals.window_stroke = Stroke::new(1.0, colors.border);
        for widget in [
            &mut visuals.widgets.noninteractive,
            &mut visuals.widgets.inactive,
            &mut visuals.widgets.hovered,
            &mut visuals.widgets.active,
            &mut visuals.widgets.open,
        ] {
            widget.bg_fill = colors.control;
            widget.weak_bg_fill = colors.control;
            widget.bg_stroke = Stroke::new(1.0, colors.border);
            widget.fg_stroke = Stroke::new(1.0, colors.text);
            widget.corner_radius = egui::CornerRadius::same(3);
        }
        visuals.widgets.noninteractive.bg_fill = colors.card;
        visuals.widgets.noninteractive.weak_bg_fill = colors.card;
        visuals.widgets.hovered.bg_fill = colors.hovered;
        visuals.widgets.hovered.weak_bg_fill = colors.hovered;
        visuals.widgets.active.bg_fill = colors.pressed;
        visuals.widgets.active.weak_bg_fill = colors.pressed;
        visuals.widgets.open.bg_fill = colors.selected;
        visuals.widgets.open.weak_bg_fill = colors.selected;
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 5.0);
        style.spacing.interact_size.y = 25.0;
        style.spacing.icon_width = 15.0;
        style.spacing.scroll = egui::style::ScrollStyle::solid();
        style.spacing.scroll.bar_width = 6.0;
        for (kind, size) in [
            (egui::TextStyle::Body, 13.0),
            (egui::TextStyle::Button, 12.0),
            (egui::TextStyle::Small, 11.0),
            (egui::TextStyle::Heading, 20.0),
        ] {
            style.text_styles.insert(kind, FontId::proportional(size));
        }
        ctx.set_style_of(mode, style);
    }
    preference.apply(ctx);
}
