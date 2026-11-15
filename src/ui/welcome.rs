//! The one-time welcome card shown after installing.

use eframe::egui::{self, Align2, Color32, FontId, Id, Key, Margin, RichText, Sense, Stroke, vec2};

use crate::app::{App, HOTKEY_LABEL};

use super::{
    HOTKEY_TAKEN,
    icons::paint_logo,
    theme::{ACCENT, KEY, MUTED, RED, SURFACE, TEXT, semi},
    widgets::{keycaps, primary},
};

impl App {
    /// The one-time popup after a fresh install. Esc, Enter or a click outside dismisses it.
    pub(super) fn welcome(&mut self, ctx: &egui::Context) {
        let frame = egui::Frame::new()
            .fill(SURFACE)
            .stroke(Stroke::new(1.0, Color32::from_white_alpha(14)))
            .corner_radius(14)
            .inner_margin(Margin::same(20))
            .shadow(egui::Shadow { offset: [0, 8], blur: 28, spread: 0, color: Color32::from_black_alpha(120) });
        let resp = egui::Modal::new(Id::unique("welcome"))
            .frame(frame)
            .backdrop_color(Color32::from_black_alpha(150))
            .show(ctx, |ui| {
                ui.set_width(300.0);
                ui.vertical_centered(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(44.0, 44.0), Sense::hover());
                    paint_logo(ui.painter(), r);
                    ui.add_space(10.0);
                    ui.label(RichText::new("You're all set").font(semi(17.0)).color(TEXT));
                    ui.add_space(2.0);
                    ui.label(
                        RichText::new("gifly lives in your tray and starts with Windows.").color(MUTED).size(12.5),
                    );
                });
                ui.add_space(14.0);
                for (i, (label, value)) in
                    [("Open gifly", None), ("Save a GIF", Some("Copy its link"))].into_iter().enumerate()
                {
                    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::hover());
                    let p = ui.painter();
                    if i > 0 {
                        p.hline(r.x_range(), r.top(), Stroke::new(1.0, Color32::from_white_alpha(8)));
                    }
                    p.text(r.left_center(), Align2::LEFT_CENTER, label, FontId::proportional(13.0), KEY);
                    match value {
                        Some(v) => _ = p.text(r.right_center(), Align2::RIGHT_CENTER, v, semi(12.5), TEXT),
                        None if !self.hotkey_ok => {
                            _ = p.text(r.right_center(), Align2::RIGHT_CENTER, "Click the tray icon", semi(12.5), TEXT);
                        }
                        None => keycaps(ui, r.right_center(), HOTKEY_LABEL),
                    }
                }
                if !self.hotkey_ok {
                    ui.label(RichText::new(HOTKEY_TAKEN).color(RED).size(12.0));
                }
                ui.add_space(14.0);
                ui.add_sized(vec2(ui.available_width(), 32.0), primary("Got it", ACCENT)).clicked()
                    || ui.input(|i| i.key_pressed(Key::Enter))
            });
        if resp.inner || resp.should_close() {
            self.welcome = false;
        }
    }
}
