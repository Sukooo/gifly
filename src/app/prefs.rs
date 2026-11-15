//! The Settings toggles, interface zoom and library backups.

use crate::{
    db::{Db, Key},
    platform,
};

use super::{App, MAX_ZOOM, MIN_ZOOM};

/// The settings kept in the library.
pub struct Prefs {
    pub auto_send: bool,
    pub capture: bool,
    pub autostart: bool,
    /// Leaves GIFs whose file is gone out of every view.
    pub hide_gone: bool,
    /// GIFs stay still until hovered or selected, so only those are decoded and drawn.
    pub hover_play: bool,
    pub zoom: f32,
}

impl Prefs {
    pub(super) fn load(db: &Db) -> Self {
        Self {
            auto_send: db.flag(Key::AutoSend, true),
            capture: db.flag(Key::Capture, true),
            autostart: db.flag(Key::Autostart, true),
            hide_gone: db.flag(Key::HideGone, false),
            hover_play: db.flag(Key::HoverPlay, false),
            zoom: db.setting(Key::Zoom).and_then(|s| s.parse().ok()).unwrap_or(1.0f32).clamp(MIN_ZOOM, MAX_ZOOM),
        }
    }
}

/// A Settings toggle, turned on or off.
#[derive(Clone, Copy, Debug)]
pub enum Setting {
    AutoSend(bool),
    Capture(bool),
    HideGone(bool),
    HoverPlay(bool),
    Autostart(bool),
    DetailedLog(bool),
}

impl App {
    pub(super) fn set(&mut self, s: Setting) {
        let (key, on) = match s {
            Setting::AutoSend(on) => {
                self.prefs.auto_send = on;
                (Key::AutoSend, on)
            }
            Setting::Capture(on) => {
                self.prefs.capture = on;
                self.shared.set_capture(on);
                (Key::Capture, on)
            }
            Setting::HideGone(on) => {
                self.prefs.hide_gone = on;
                if on {
                    self.find_gone();
                }
                self.dirty = true;
                (Key::HideGone, on)
            }
            Setting::HoverPlay(on) => {
                self.prefs.hover_play = on;
                (Key::HoverPlay, on)
            }
            Setting::Autostart(on) => {
                self.prefs.autostart = on;
                if !cfg!(debug_assertions) && !platform::set_autostart(on) {
                    self.toast("Couldn't change whether gifly starts with Windows.");
                }
                (Key::Autostart, on)
            }
            Setting::DetailedLog(on) => {
                crate::log::set_detailed(on);
                (Key::DetailedLog, on)
            }
        };
        self.lib.db().set_flag(key, on);
    }

    /// Marks the GIFs already known to be gone, so they're hidden before they're scrolled to.
    pub(super) fn find_gone(&mut self) {
        let gone = self.lib.gifs().iter().filter(|g| self.media.is_gone(&g.url, g.media_link.as_deref()));
        self.loads.gone.extend(gone.map(|g| g.id));
    }

    /// Applies Ctrl+scroll, clamps the UI scale, and resizes the window to match, since egui's
    /// zoom (Ctrl +/-/0, Settings) only rescales the content.
    pub fn sync_zoom(&mut self) {
        let delta = self.ctx.input(eframe::egui::InputState::zoom_delta);
        let z = (self.ctx.zoom_factor() * delta).clamp(MIN_ZOOM, MAX_ZOOM);
        if z != self.ctx.zoom_factor() {
            self.ctx.set_zoom_factor(z);
        }
        if (z - self.prefs.zoom).abs() > 1e-3 {
            platform::scale_window(self.shared.hwnd.get(), z / self.prefs.zoom);
            self.prefs.zoom = z;
            self.lib.db().set_setting(Key::Zoom, &format!("{z:.3}"));
        }
    }

    /// Saves a copy of the library to Documents\gifly backups.
    pub(super) fn backup(&mut self) {
        let dir = directories::UserDirs::new()
            .and_then(|u| u.document_dir().map(|d| d.join("gifly backups")))
            .ok_or_else(|| "Couldn't find your Documents folder".to_string());
        self.form.backup = Some(dir.and_then(|dir| {
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            self.lib.db().backup_to(&dir)
        }));
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::app::Action;

    #[test]
    fn a_setting_is_kept_for_next_start() {
        let mut app = App::for_test(Db::open(Path::new(":memory:")).unwrap());
        assert!(app.prefs.auto_send && !app.prefs.hover_play);
        app.apply(Action::Set(Setting::AutoSend(false)));
        app.apply(Action::Set(Setting::HoverPlay(true)));
        let saved = Prefs::load(app.lib.db());
        assert!(!app.prefs.auto_send && app.prefs.hover_play);
        assert!(!saved.auto_send && saved.hover_play);
    }
}
