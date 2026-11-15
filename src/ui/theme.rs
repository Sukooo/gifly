//! Colors, sizes, fonts and the egui style they make up.

use std::sync::Arc;

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Id, Margin, Stroke, Ui, vec2,
};

pub(super) const BG: Color32 = Color32::from_rgb(0x1c, 0x1d, 0x22);
pub(super) const SURFACE: Color32 = Color32::from_rgb(0x27, 0x28, 0x2f);
pub(super) const RAISED: Color32 = Color32::from_rgb(0x33, 0x34, 0x3c);
pub(super) const RAISED_HI: Color32 = Color32::from_rgb(0x3d, 0x3e, 0x47);
pub(super) const LINE: Color32 = Color32::from_rgba_premultiplied(15, 15, 15, 15);
pub(super) const ACCENT: Color32 = Color32::from_rgb(0xc4, 0x7a, 0xd6);
pub(super) const BLURPLE: Color32 = Color32::from_rgb(0x58, 0x65, 0xf2);
pub(super) const GREEN: Color32 = Color32::from_rgb(0x5c, 0xcf, 0x8c);
pub(super) const RED: Color32 = Color32::from_rgb(0xf0, 0x72, 0x6c);
pub(super) const TEXT: Color32 = Color32::from_gray(228);
pub(super) const MUTED: Color32 = Color32::from_gray(140);
pub(super) const KEY: Color32 = Color32::from_gray(165);
pub(super) const FAINT: Color32 = Color32::from_gray(98);
/// Whether a button or menu item does something that can't be taken back.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Tone {
    Plain,
    Danger,
}

pub(super) const GAP: f32 = 6.0;
pub(super) const TARGET_W: f32 = 190.0;
pub(super) const RADIUS: u8 = 8;
pub(super) const FADE_SECS: f32 = 0.12;

pub fn style(ctx: &egui::Context) {
    fonts(ctx);
    ctx.set_theme(egui::Theme::Dark);
    ctx.style_mut_of(egui::Theme::Dark, |s| {
        use egui::TextStyle as T;
        for (t, size) in [(T::Heading, 18.0), (T::Body, 14.0), (T::Button, 14.0), (T::Small, 11.5)] {
            s.text_styles.insert(t, FontId::proportional(size));
        }
        s.spacing.item_spacing = vec2(6.0, 6.0);
        s.spacing.button_padding = vec2(10.0, 5.0);
        // A slim handle in its own gutter, so it never sits on top of the GIFs: faint until the
        // list is hovered, and a little wider while it's being used.
        s.spacing.scroll = egui::style::ScrollStyle {
            floating_allocated_width: 8.0,
            floating_width: 4.0,
            bar_width: 6.0,
            bar_outer_margin: 1.0,
            handle_min_length: 28.0,
            dormant_handle_opacity: 0.18,
            active_handle_opacity: 0.32,
            interact_handle_opacity: 0.55,
            active_background_opacity: 0.0,
            interact_background_opacity: 0.0,
            ..egui::style::ScrollStyle::floating()
        };
        s.spacing.menu_margin = Margin::same(5);

        let v = &mut s.visuals;
        v.panel_fill = BG;
        v.window_fill = SURFACE;
        v.extreme_bg_color = BG;
        v.faint_bg_color = SURFACE;
        v.window_stroke = Stroke::new(1.0, LINE);
        v.window_corner_radius = CornerRadius::same(10);
        v.menu_corner_radius = CornerRadius::same(10);
        v.popup_shadow = egui::Shadow { offset: [0, 8], blur: 24, spread: 0, color: Color32::from_black_alpha(120) };
        v.selection.bg_fill = ACCENT.gamma_multiply(0.45);
        v.selection.stroke = Stroke::new(1.0, ACCENT);
        v.text_cursor.stroke = Stroke::new(2.0, ACCENT);
        v.hyperlink_color = ACCENT;
        let w = &mut v.widgets;
        for (st, fill, text) in [
            (&mut w.noninteractive, SURFACE, 220),
            (&mut w.inactive, RAISED, 215),
            (&mut w.hovered, RAISED_HI, 245),
            (&mut w.active, RAISED_HI, 255),
            (&mut w.open, RAISED, 230),
        ] {
            st.corner_radius = CornerRadius::same(7);
            st.weak_bg_fill = fill;
            st.bg_fill = fill;
            st.bg_stroke = Stroke::NONE;
            st.fg_stroke = Stroke::new(1.0, Color32::from_gray(text));
            st.expansion = 0.0;
        }
        w.noninteractive.bg_stroke = Stroke::new(1.0, LINE); // separators
        w.hovered.bg_stroke = Stroke::new(1.0, LINE);
    });
}

/// Segoe UI (regular + semibold) when available; egui's bundled fonts stay as fallbacks.
fn fonts(ctx: &egui::Context) {
    let mut defs = FontDefinitions::default();
    let dir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
    let mut semibold = defs.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    for (name, file, is_semi) in [("segoe", "segoeui.ttf", false), ("segoe-sb", "seguisb.ttf", true)] {
        let Ok(bytes) = std::fs::read(format!(r"{dir}\Fonts\{file}")) else { continue };
        defs.font_data.insert(name.into(), Arc::new(FontData::from_owned(bytes)));
        semibold.insert(0, name.into());
        if !is_semi {
            defs.families.entry(FontFamily::Proportional).or_default().insert(0, name.into());
        }
    }
    defs.families.insert(FontFamily::Name("semibold".into()), semibold);
    ctx.set_fonts(defs);
}

pub(super) fn semi(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("semibold".into()))
}

pub(super) fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    a.lerp_to_gamma(b, t)
}

pub(super) fn fade(ui: &Ui, id: Id, on: bool) -> f32 {
    ui.ctx().animate_bool_with_time(id, on, FADE_SECS)
}
