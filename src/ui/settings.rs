//! The Settings view.

use eframe::egui::{
    self, Align, Align2, Button, Color32, CursorIcon, FontId, Id, Layout, Rect, RichText, ScrollArea, Sense, Stroke,
    Ui, pos2, vec2,
};

use crate::{
    app::{Action, App, HOTKEY_LABEL, ImportStatus, Setting},
    discord, update,
};

use super::{
    HOTKEY_TAKEN,
    icons::Icon,
    menu::secret_field,
    theme::{BG, BLURPLE, FAINT, GREEN, KEY, LINE, MUTED, RAISED_HI, RED, TEXT, semi},
    widgets::{card, divider, keycaps, primary, section, toggle_row},
};

impl App {
    pub(super) fn settings(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;

            section(ui, "PICKER");
            card(ui, |ui| self.picker_card(ui, actions));

            section(ui, "DISCORD FAVORITES");
            card(ui, |ui| self.discord_card(ui, actions));

            section(ui, "LIBRARY");
            card(ui, |ui| self.library_card(ui, actions));

            section(ui, "TROUBLESHOOTING");
            card(ui, |ui| self.troubleshooting_card(ui, actions));

            ui.add_space(6.0);
            footer(ui, actions);
        });
    }

    /// Hotkey, interface size and behavior toggles.
    fn picker_card(&self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::hover());
        ui.painter().text(r.left_center(), Align2::LEFT_CENTER, "Open picker", FontId::proportional(14.0), TEXT);
        keycaps(ui, r.right_center(), HOTKEY_LABEL);
        if !self.hotkey_ok {
            ui.label(RichText::new(HOTKEY_TAKEN).color(RED).size(12.0));
        }
        divider(ui);
        self.size_row(ui);
        let p = &self.prefs;
        let toggles = [
            (
                "Send right after pasting",
                "Ctrl+click or Shift+Enter does the opposite",
                p.auto_send,
                Setting::AutoSend as fn(bool) -> Setting,
            ),
            ("Save GIF links I copy", "Listed in Copied, new ones also in Unsorted", p.capture, Setting::Capture),
            (
                "Hide deleted GIFs",
                "Ones whose file was removed from where it was posted",
                p.hide_gone,
                Setting::HideGone,
            ),
            ("Play GIFs on hover", "Others stay still, which saves CPU", p.hover_play, Setting::HoverPlay),
            ("Start with Windows", "Waits in the tray, out of sight", p.autostart, Setting::Autostart),
        ];
        for (title, sub, mut on, setting) in toggles {
            divider(ui);
            if toggle_row(ui, title, sub, &mut on) {
                actions.push(Action::Set(setting(on)));
            }
        }
    }

    /// Import from the Discord app, or with a token, and the bot token that renews links.
    fn discord_card(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        ui.label(
            RichText::new(
                "Copies the favorites saved by the Discord app on this PC into “Discord imports”, \
                 apart from your library. Re-importing only adds new ones.",
            )
            .color(MUTED)
            .size(12.0),
        );
        ui.add_space(2.0);
        let running = matches!(self.form.import, ImportStatus::Running);
        let label = if running { "Importing…" } else { "Import from Discord" };
        if ui.add_enabled(!running, primary(label, BLURPLE).min_size(vec2(150.0, 32.0))).clicked() {
            actions.push(Action::Import(discord::Source::ThisPc));
        }
        if self.form.token_open {
            let submitted = secret_field(ui, &mut self.form.token, "Discord token", Icon::Key, "Import");
            if submitted && !running {
                actions.push(Action::Import(discord::Source::Token(std::mem::take(&mut self.form.token))));
            }
        } else {
            let link =
                Button::new(RichText::new("Use a token instead").color(MUTED).size(12.5).underline()).frame(false);
            if ui.add(link).on_hover_cursor(CursorIcon::PointingHand).clicked() {
                self.form.token_open = true;
            }
        }
        match &self.form.import {
            ImportStatus::Idle => {}
            ImportStatus::Running => {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(12.0).color(MUTED));
                    ui.label(RichText::new("Importing…").color(MUTED).size(12.5));
                });
            }
            ImportStatus::Done(s) => {
                ui.label(RichText::new(s).color(GREEN).size(12.5));
            }
            ImportStatus::Failed(s) => {
                ui.label(RichText::new(s).color(RED).size(12.5));
            }
        }
        divider(ui);
        self.bot_token_row(ui, actions);
    }

    /// The bot token that keeps imported Discord files loading after their links expire.
    fn bot_token_row(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        ui.label(RichText::new("Keep Discord files loading").color(TEXT).size(14.0));
        ui.label(
            RichText::new(
                "Discord's file links expire after a day. With a bot token, gifly renews them as they come into view.",
            )
            .color(MUTED)
            .size(12.0),
        );
        if self.has_bot_token() {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Bot token saved").color(GREEN).size(12.5));
                let remove = Button::new(RichText::new("Remove").color(MUTED).size(12.5).underline()).frame(false);
                if ui.add(remove).on_hover_cursor(CursorIcon::PointingHand).clicked() {
                    actions.push(Action::SetBotToken(String::new()));
                }
            });
        } else {
            if secret_field(ui, &mut self.form.bot_token, "Bot token", Icon::Key, "Save") {
                actions.push(Action::SetBotToken(std::mem::take(&mut self.form.bot_token)));
            }
            ui.label(
                RichText::new(
                    "Get one at discord.com/developers: New Application → Bot → Reset Token. The bot doesn't need to join any server.",
                )
                .color(MUTED)
                .size(11.5),
            );
        }
        if let Some(e) = &self.form.renew_error {
            ui.label(RichText::new(e).color(RED).size(12.5));
        }
    }

    fn library_card(&self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let imported = self.lib.gifs().iter().filter(|g| g.imported).count();
        let library = self.lib.gifs().iter().filter(|g| g.in_library()).count();
        let stats = [
            (library, "GIFs", TEXT),
            (self.lib.collections().len(), "collections", TEXT),
            (imported, "in Discord imports", if imported > 0 { BLURPLE } else { TEXT }),
        ];
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::hover());
        let w = r.width() / 3.0;
        let p = ui.painter();
        for (i, (n, label, c)) in stats.into_iter().enumerate() {
            let x = r.left() + w * i as f32;
            if i > 0 {
                p.vline(x, r.y_range().shrink(6.0), Stroke::new(1.0, LINE));
            }
            let cx = x + w / 2.0;
            p.text(pos2(cx, r.top() + 14.0), Align2::CENTER_CENTER, n.to_string(), semi(19.0), c);
            p.text(pos2(cx, r.bottom() - 8.0), Align2::CENTER_CENTER, label, FontId::proportional(11.5), MUTED);
        }

        divider(ui);
        if button_row(ui, "Back up library", "Saves a copy to Documents", "Back up") {
            actions.push(Action::Backup);
        }
        match &self.form.backup {
            Some(Ok(path)) => {
                let path = path.clone();
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("Saved {name}")).color(GREEN).size(12.5));
                    let show = Button::new(RichText::new("Show").color(TEXT).size(12.5).underline()).frame(false);
                    if ui.add(show).on_hover_cursor(CursorIcon::PointingHand).clicked() {
                        crate::platform::reveal(&path);
                    }
                });
            }
            Some(Err(e)) => {
                ui.label(RichText::new(format!("Backup failed: {e}")).color(RED).size(12.5));
            }
            None => {}
        }
    }

    /// Detailed logging, and what to send along when reporting a problem.
    fn troubleshooting_card(&self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let mut detailed = crate::log::detailed();
        let sub = "Records every download and renewal, to track down a problem";
        if toggle_row(ui, "Detailed logging", sub, &mut detailed) {
            actions.push(Action::Set(Setting::DetailedLog(detailed)));
        }
        divider(ui);
        if button_row(ui, "Copy diagnostics", "Version, settings and recent log, without tokens", "Copy") {
            actions.push(Action::CopyDiagnostics);
        }
        if let Some(path) = crate::log::path() {
            let show = Button::new(RichText::new("Show log file").color(MUTED).size(12.5).underline()).frame(false);
            if ui.add(show).on_hover_cursor(CursorIcon::PointingHand).clicked() {
                crate::platform::reveal(&path);
            }
        }
    }

    /// "Interface size" with a segmented 100–150% picker.
    pub(super) fn size_row(&self, ui: &mut Ui) {
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::hover());
        let p = ui.painter();
        p.text(
            pos2(r.left(), r.center().y - 8.0),
            Align2::LEFT_CENTER,
            "Interface size",
            FontId::proportional(14.0),
            TEXT,
        );
        p.text(
            pos2(r.left(), r.center().y + 9.0),
            Align2::LEFT_CENTER,
            "Ctrl + scroll to fine-tune",
            FontId::proportional(11.5),
            MUTED,
        );
        let opts = [1.0f32, 1.15, 1.3, 1.5];
        let seg = 42.0;
        let track =
            Rect::from_min_size(pos2(r.right() - seg * 4.0 - 4.0, r.center().y - 14.0), vec2(seg * 4.0 + 4.0, 28.0));
        p.rect_filled(track, 8, BG);
        for (i, z) in opts.into_iter().enumerate() {
            let sr = Rect::from_min_size(track.min + vec2(2.0 + seg * i as f32, 2.0), vec2(seg, 24.0));
            let resp =
                ui.interact(sr, Id::unique(("zoom", i)), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
            let on = (self.prefs.zoom - z).abs() < 0.01;
            if on {
                p.rect_filled(sr, 6, RAISED_HI);
            } else if resp.hovered() {
                p.rect_filled(sr, 6, Color32::from_white_alpha(8));
            }
            let c = if on {
                TEXT
            } else if resp.hovered() {
                KEY
            } else {
                MUTED
            };
            p.text(sr.center(), Align2::CENTER_CENTER, format!("{}%", (z * 100.0).round()), semi(12.0), c);
            if resp.clicked() {
                ui.ctx().set_zoom_factor(z);
            }
        }
    }
}

/// A setting with a title, a line under it and a button on the right. True when clicked.
fn button_row(ui: &mut Ui, title: &str, sub: &str, button: &str) -> bool {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::hover());
    let p = ui.painter();
    p.text(pos2(r.left(), r.center().y - 8.0), Align2::LEFT_CENTER, title, FontId::proportional(14.0), TEXT);
    p.text(pos2(r.left(), r.center().y + 9.0), Align2::LEFT_CENTER, sub, FontId::proportional(11.5), MUTED);
    let br = Rect::from_min_size(pos2(r.right() - 84.0, r.center().y - 15.0), vec2(84.0, 30.0));
    ui.put(br, primary(button, RAISED_HI)).clicked()
}

fn footer(ui: &mut Ui, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        let quit = Button::new(RichText::new("Quit gifly").color(MUTED).size(12.5)).frame(false);
        if ui.add(quit).clicked() {
            actions.push(Action::Quit);
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let version = concat!("v", env!("CARGO_PKG_VERSION"));
            let text = match update::status() {
                update::Status::Downloading => format!("{version} · downloading update"),
                update::Status::Ready => format!("{version} · update installs when closed"),
                update::Status::Idle => version.to_string(),
            };
            ui.label(RichText::new(text).color(FAINT).size(11.5));
        });
    });
}
