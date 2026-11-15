//! Showing and hiding the picker window.

use eframe::egui::ViewportCommand;

use crate::{
    db::Key,
    log::{self, Area},
    platform, update,
};

use super::{App, View};

impl App {
    /// Resets the picker to a fresh Home each time it opens.
    pub fn on_show(&mut self) {
        self.win.hidden = false;
        self.win.had_focus = false;
        self.reset_view(View::Home);
        self.reveal = false;
        self.grid.filing = None;
        // A tile drag cut short by hiding still keeps where the tile got to.
        if self.editing.drag.take().is_some() {
            self.save_order();
        }
        self.history = Default::default();
        self.retry_discord();
    }

    pub fn picker_visible(&self) -> bool {
        platform::is_visible(self.shared.hwnd.get())
    }

    /// Uncloaks the window once two frames are drawn. eframe shows it right after its first
    /// frame, often before that frame reaches the screen, which flashes white.
    pub fn uncloak_when_drawn(&mut self) {
        if !self.win.cloaked {
            return;
        }
        if self.frame > 2 {
            self.win.cloaked = false;
            platform::cloak(self.shared.hwnd.get(), false);
        } else {
            self.ctx.request_repaint();
        }
    }

    /// Hides the picker, remembers its size, and frees what it doesn't need while hidden.
    pub fn hide(&mut self) {
        self.save_size();
        platform::hide(self.shared.hwnd.get());
        update::touch();
        self.win.hidden = true;
        // Tokens being typed are only needed while their field is on screen.
        self.form.token.clear();
        self.form.bot_token.clear();
        self.form.token_open = false;
        self.shrink_textures();
        // Nothing in the grid is in view, so its queued loads are dropped and downloads stop
        // instead of decoding for nothing. The next draw places the grid again.
        self.grid.tiles.clear();
        self.set_places(Default::default());
        self.paint_while_hidden();
        platform::trim_memory_soon();
    }

    fn save_size(&self) {
        if let Some(r) = self.ctx.input(|i| i.viewport().inner_rect) {
            // Stored unzoomed, so the window comes back at the same size on screen.
            let (w, h) = (r.width() * self.prefs.zoom, r.height() * self.prefs.zoom);
            self.lib.db().set_setting(Key::Size, &format!("{} {}", w.round(), h.round()));
        }
    }

    /// Ends gifly: removes the tray icon (which Windows would otherwise leave behind until
    /// hovered) and writes out the log first.
    pub fn quit(&mut self) -> ! {
        if !self.win.hidden {
            self.save_size();
        }
        self._tray = None;
        log::info(Area::App, "quit");
        log::flush();
        std::process::exit(0);
    }

    /// Draws one empty frame while hidden. eframe doesn't draw a hidden window, and textures
    /// freed since its last frame are only let go of in the next one.
    pub(super) fn paint_while_hidden(&self) {
        self.ctx.send_viewport_cmd(ViewportCommand::RequestPaintWhileHidden);
        self.ctx.request_repaint();
    }
}
