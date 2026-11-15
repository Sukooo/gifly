//! Everything drawn on screen. `App` holds the state; these modules paint it and turn clicks
//! and keys into `Action`s, which `App::apply` carries out once the frame is drawn.

mod chrome;
mod grid;
mod home;
mod icons;
mod menu;
mod settings;
mod theme;
mod welcome;
mod widgets;

use std::path::PathBuf;

use eframe::egui::{self, Align, Event, Id, Key, Layout, Margin, Modifiers, Ui, ViewportCommand};

use crate::{
    app::{Action, App, View},
    platform, urls,
};

use theme::BG;

pub use icons::icon_rgba;
pub use theme::style;

const HOTKEY_TAKEN: &str = "Another app is using Ctrl + Shift + G. Open gifly from its tray icon instead.";

fn search_id() -> Id {
    Id::unique("search")
}

impl eframe::App for App {
    fn logic(&mut self, _ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.pump();
        self.soak();
    }

    fn ui(&mut self, ui: &mut Ui, frame: &mut eframe::Frame) {
        self.frame += 1;
        self.sync_zoom();
        let ctx = ui.ctx().clone();
        self.uncloak_when_drawn();

        // Re-hide the start-hidden window eframe showed (see `window_options`), unless the hotkey
        // or tray opened it in the meantime.
        let shown = self.shared.take_just_shown();
        if self.win.start_hidden && self.picker_visible() {
            self.win.start_hidden = false;
            if !shown {
                self.hide();
                return;
            }
        }
        // Loads and clipboard captures still ask for frames while the picker is put away. Their
        // results were taken in `logic`; drawing would only load textures nobody can see.
        if self.win.hidden && !shown && !self.picker_visible() {
            // Drawn only for `paint_while_hidden`. Textures deleted in the last frame are kept by
            // the graphics driver until the GPU is done with that frame, which a hidden window
            // never shows it is: wait for it.
            if let Some(gl) = frame.gl() {
                // SAFETY: called on the UI thread, where eframe keeps this GL context current.
                unsafe { eframe::glow::HasContext::finish(&**gl) };
            }
            return;
        }
        if shown || self.win.hidden {
            self.on_show();
        } else if let Some(secs) = frame.info().cpu_usage {
            // The last frame's, which was drawn too.
            crate::stats::frame(secs);
        }
        if self.shared.take_hide_requested() {
            self.hide();
            return;
        }
        if ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            self.hide();
            return;
        }
        // Behave like a popup: clicking anywhere else closes it.
        match ctx.input(|i| i.viewport().focused) {
            Some(true) => self.win.had_focus = true,
            Some(false) if self.win.had_focus => {
                self.hide();
                return;
            }
            _ => {}
        }

        let full = ui.max_rect();
        let mut actions = Vec::new();
        let (back, fwd) = ctx.input(|i| {
            (
                i.pointer.button_pressed(egui::PointerButton::Extra1),
                i.pointer.button_pressed(egui::PointerButton::Extra2),
            )
        });
        if back {
            actions.push(Action::Back);
        }
        if fwd {
            actions.push(Action::Forward);
        }
        // Keys act on the list, so it has to be current first.
        if self.dirty {
            self.recompute();
        }
        self.keys(&ctx, &mut actions);

        egui::Frame::new().fill(BG).inner_margin(Margin { left: 10, right: 10, top: 0, bottom: 8 }).show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            self.header(ui, &mut actions);
            if self.dirty {
                self.recompute();
            }
            ui.add_space(6.0);
            if self.view == View::Settings {
                self.settings(ui, &mut actions);
                return;
            }
            ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                self.footer(ui, &mut actions);
                ui.add_space(2.0);
                ui.with_layout(Layout::top_down(Align::Min), |ui| {
                    if self.showing_grid() {
                        self.grid(ui, &mut actions);
                    } else {
                        self.home(ui, &mut actions);
                    }
                });
            });
        });

        Self::top_edge(ui, full);
        self.resize_grip(ui, full);
        if self.welcome {
            self.welcome(&ctx);
        }
        self.filing_panel(ui, full, &mut actions);
        if !self.welcome {
            let (dropped, hovering) = ctx.input(|i| {
                let files: Vec<PathBuf> = i
                    .raw
                    .dropped_files
                    .iter()
                    .map(|f| f.path().to_path_buf())
                    .filter(|p| !p.as_os_str().is_empty())
                    .collect();
                (files, !i.raw.hovered_files.is_empty())
            });
            if !dropped.is_empty() {
                actions.push(Action::AddFiles(dropped));
            }
            if hovering {
                self.drop_overlay(ui, full);
            }
        }
        self.paint_toast(ui, full, &mut actions);
        for a in actions {
            self.apply(a);
        }
        self.evict();
    }
}

impl App {
    fn showing_grid(&self) -> bool {
        match self.view {
            View::Settings => false,
            View::Home => !self.query.is_empty(),
            _ => true,
        }
    }

    fn keys(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        if self.welcome {
            return; // the welcome popup handles its own keys (Esc closes it, not the picker)
        }
        let focused = ctx.memory(egui::Memory::focused);
        if focused.is_some_and(|f| f != search_id()) {
            return; // another text field (rename, new collection, token) owns the keyboard
        }
        if egui::Popup::is_any_open(ctx) {
            return; // so is an open menu: Esc closes it, not the picker
        }
        if focused.is_none() && self.view != View::Settings {
            self.win.focus_search = true;
        }

        self.paste_keys(ctx, actions);

        if self.toast.as_ref().is_some_and(|t| !t.undo.is_empty())
            && ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Z))
        {
            return actions.push(Action::Undo);
        }
        const DIGITS: [Key; 9] =
            [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9];
        if let Some(n) = ctx.input_mut(|i| DIGITS.iter().position(|&k| i.consume_key(Modifiers::COMMAND, k))) {
            return actions.push(Action::Jump(n));
        }
        if self.lib.collections().iter().any(|c| c.hidden)
            && ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::H))
        {
            return actions.push(Action::Reveal);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            return self.escape(actions);
        }
        if self.query.is_empty()
            && self.view != View::Home
            && self.view != View::Settings
            && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Backspace))
        {
            return actions.push(Action::Open(View::Home));
        }
        if self.showing_grid() {
            self.grid_keys(ctx, actions);
        }
    }

    /// Ctrl+V with GIF links or files on the clipboard adds them, rather than typing into the search.
    fn paste_keys(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let links = ctx.input_mut(|i| {
            let at = i.events.iter().position(|e| matches!(e, Event::Paste(t) if picture_links(t).is_some()))?;
            let Event::Paste(t) = i.events.remove(at) else { return None };
            picture_links(&t)
        });
        if let Some(links) = links {
            actions.push(Action::AddLinks(links));
        }
        // Files copied in Explorer don't reach egui as a paste at all, so watch the keys.
        let held = platform::ctrl_v_down();
        if held
            && !self.win.paste_held
            && let Some(files) = clipboard_files()
        {
            actions.push(Action::AddFiles(files));
        }
        self.win.paste_held = held;
    }

    /// Esc backs out of one thing at a time, and finally hides the picker.
    fn escape(&mut self, actions: &mut Vec<Action>) {
        if self.grid.filing.is_some() {
            self.grid.filing = None;
        } else if self.editing.creating || self.editing.renaming.is_some() {
            self.editing.creating = false;
            self.editing.renaming = None;
        } else if !self.picked.is_empty() {
            self.picked.clear();
        } else if !self.query.is_empty() {
            self.clear_search();
        } else if self.view != View::Home {
            actions.push(Action::Open(View::Home));
        } else {
            actions.push(Action::Hide);
        }
    }

    /// Ctrl+A, Enter and the arrow keys in the grid.
    fn grid_keys(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::A)) {
            return actions.push(Action::PickAll);
        }
        const ARROWS: [(Key, Dir); 4] = [
            (Key::ArrowDown, Dir::Down),
            (Key::ArrowUp, Dir::Up),
            (Key::ArrowLeft, Dir::Left),
            (Key::ArrowRight, Dir::Right),
        ];
        let has_sel = self.grid.selected.is_some();
        let (enter, shift_enter, arrow) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::SHIFT, Key::Enter),
                // Left and right only move a selection; without one they stay with the search.
                // All of them are consumed, so a second press in the same frame can't reach the search.
                ARROWS
                    .into_iter()
                    .filter(|&(k, d)| {
                        (has_sel || matches!(d, Dir::Up | Dir::Down)) && i.consume_key(Modifiers::NONE, k)
                    })
                    .fold(None, |first, (_, d)| first.or(Some(d))),
            )
        });
        let n = self.list.len();
        if n == 0 {
            return;
        }
        if enter || shift_enter {
            let i = self.grid.selected.unwrap_or(0).min(n - 1);
            return actions.push(Action::Send { id: self.list[i], invert: shift_enter });
        }
        let before = self.grid.selected;
        self.grid.selected = match (before, arrow) {
            (None, Some(Dir::Down)) => Some(0),
            // Up from the top row leaves the grid for the search.
            (Some(s), Some(Dir::Up)) => self.step(s, Dir::Up),
            (Some(s), Some(d)) => self.step(s, d).or(Some(s)),
            (s, _) => s,
        };
        if self.grid.selected != before
            && let Some(t) = self.grid.tiles.get(self.grid.selected.unwrap_or(0))
        {
            if t.min.y < self.grid.scroll_off {
                self.grid.scroll_to = Some(t.min.y);
            } else if t.max.y > self.grid.scroll_off + self.grid.view_h {
                self.grid.scroll_to = Some(t.max.y - self.grid.view_h);
            }
        }
    }
}

/// A direction to move the grid's selection in.
#[derive(Clone, Copy)]
enum Dir {
    Up,
    Down,
    Left,
    Right,
}

/// The picture links in pasted text, one per line. None if any line is something else.
fn picture_links(text: &str) -> Option<Vec<String>> {
    let links: Vec<String> = text.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect();
    (!links.is_empty() && links.iter().all(|l| urls::is_picture_link(l))).then_some(links)
}

/// The files copied in Explorer, if that's what's on the clipboard.
fn clipboard_files() -> Option<Vec<PathBuf>> {
    arboard::Clipboard::new().ok()?.get().file_list().ok().filter(|f| !f.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Db, GifId};

    fn press(key: Key) -> Event {
        Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }
    }

    #[test]
    fn every_arrow_in_a_frame_is_taken_from_the_search_box() {
        let mut app = App::for_test(Db::open(std::path::Path::new(":memory:")).unwrap());
        app.view = View::Unsorted;
        app.list = vec![GifId(1), GifId(2)];
        let ctx = egui::Context::default();
        let input = egui::RawInput { events: vec![press(Key::ArrowDown), press(Key::ArrowUp)], ..Default::default() };
        ctx.begin_pass(input);
        app.grid_keys(&ctx, &mut Vec::new());
        assert_eq!(app.grid.selected, Some(0), "the first arrow moves the selection");
        assert!(ctx.input(|i| i.events.is_empty()), "the second one doesn't reach the search box");
        ctx.end_pass().textures_delta.clear();
    }
}
