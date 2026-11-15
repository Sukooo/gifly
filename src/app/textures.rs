//! Thumbnail and animation textures: requesting them from `Media`, storing what comes back in
//! `Loads`, and evicting the least recently drawn ones to bound memory.

use std::{collections::HashMap, sync::Arc, time::Duration};

use eframe::egui::TextureHandle;

use crate::{
    db::GifId,
    media::{Anim, Fail, Job, Kind, Span, Ticket},
};

use super::{App, loads::Slot};

const MAX_THUMBS: usize = 240;
/// Thumbnails kept while hidden: enough for Home and the top of Recent to open instantly.
const HIDDEN_THUMBS: usize = 48;
/// Memory for animations: every visible tile animates, and off-screen ones are evicted
/// least-recently-seen first once they take a quarter more than this.
const ANIM_BYTES: usize = 160 << 20;
/// Animations change frame only on this shared clock, so many on screen at once redraw the
/// window at most 50 times a second rather than at the monitor's refresh rate. GIF delays come
/// in 10ms steps, nearly all of them even.
const ANIM_TICK_MS: u64 = 20;

impl App {
    /// The GIF's still thumbnail, requesting it the first time it's asked for. `grid` if it's for
    /// a tile in the grid, which loads by how near the view it is.
    pub fn thumb(&mut self, gi: usize, grid: bool) -> Option<TextureHandle> {
        let id = self.lib.gifs()[gi].id;
        match self.loads.thumbs.get_mut(&id) {
            Some(Slot::Ready(t, last)) => {
                *last = self.frame;
                Some(t.clone())
            }
            Some(_) => None,
            None => {
                self.request(gi, Kind::Thumb, grid);
                None
            }
        }
    }

    /// The GIF's current animation frame, requesting the animation the first time it's asked for.
    pub fn anim_frame(&mut self, gi: usize, grid: bool) -> Option<TextureHandle> {
        let id = self.lib.gifs()[gi].id;
        match self.loads.anims.get_mut(&id) {
            Some(Slot::Ready(a, last)) => {
                *last = self.frame;
                let now = (self.ctx.input(|i| i.time) * 1000.0) as u64;
                let tick = now - now % ANIM_TICK_MS;
                let (t, left) = a.frame_at(tick);
                let next = tick + left.div_ceil(ANIM_TICK_MS) * ANIM_TICK_MS;
                self.ctx.request_repaint_after(Duration::from_millis(next - now));
                Some(t.clone())
            }
            Some(_) => None,
            None => {
                self.request(gi, Kind::Anim, grid);
                None
            }
        }
    }

    fn request(&mut self, gi: usize, kind: Kind, grid: bool) {
        let ticket = self.loads.start(self.lib.gifs()[gi].id, kind);
        self.media.request(self.job(gi, ticket, kind, grid));
    }

    fn job(&self, gi: usize, ticket: Ticket, kind: Kind, grid: bool) -> Job {
        let g = &self.lib.gifs()[gi];
        Job {
            id: g.id,
            ticket,
            url: g.url.clone(),
            media_link: g.media_link.clone(),
            size: (g.w, g.h),
            px: self.grid.tile_px,
            kind,
            grid,
        }
    }

    /// Tells the loaders which part of the grid is in view.
    pub fn set_focus(&self, view: Span) {
        self.media.focus.set(view);
    }

    /// Tells the loaders where each GIF in the grid is, after its layout changed.
    pub fn set_places(&self, places: HashMap<GifId, Span>) {
        self.media.focus.set_places(places);
    }

    /// Gets the `i`th GIF of `list`, about to scroll into view, onto disk (once a session) so it
    /// shows up as soon as it does.
    pub fn warm(&mut self, i: usize) {
        let id = self.list[i];
        let Some(gi) = self.lib.index(id) else { return };
        if !self.loads.wants_warming(id) || self.media.busy() {
            return;
        }
        let g = &self.lib.gifs()[gi];
        if self.media.needs_renewing(&g.url, g.media_link.as_deref()) {
            // Warmed once the link is renewed. Without a token it can't be: it loads (or shows
            // as failed) when it scrolls into view.
            self.loads.warmed.insert(id);
            if self.ask_for_link(id, true) {
                self.ask_ahead(id);
            }
        } else if self.media.warm(self.job(gi, Ticket(0), Kind::Warm, true)) {
            self.loads.warmed.insert(id);
        }
    }

    /// Keeps the texture caches bounded while the picker is open.
    pub fn evict(&mut self) {
        self.loads.evict(MAX_THUMBS, ANIM_BYTES, self.frame, |a| a.bytes());
    }

    /// Frees most textures once the picker is hidden.
    pub(super) fn shrink_textures(&mut self) {
        self.loads.shrink(HIDDEN_THUMBS);
    }

    /// Invalidates unfinished or failed loads, e.g. after the GIF got a fresh link.
    pub(super) fn retry_load(&mut self, gi: usize) {
        let g = &self.lib.gifs()[gi];
        self.media.retry(&g.url);
        self.dirty |= self.loads.retry(g.id) && self.prefs.hide_gone;
    }

    /// Why the GIF doesn't show, for its menu. A still image's missing animation doesn't count.
    pub fn why_not_loading(&self, id: GifId) -> Option<String> {
        let f = self.loads.failure(id).filter(|f| **f != Fail::Still)?;
        Some(match f {
            Fail::Expired if !self.has_bot_token() => format!("{f} Add a bot token in Settings to renew it."),
            _ => f.to_string(),
        })
    }

    pub fn load_failed(&self, gi: usize) -> bool {
        self.loads.failure(self.lib.gifs()[gi].id).is_some()
    }

    pub fn is_gone(&self, id: GifId) -> bool {
        self.loads.gone.contains(&id)
    }

    pub(super) fn thumb_loaded(&mut self, id: GifId, ticket: Ticket, res: Result<TextureHandle, Fail>) {
        if !self.loads.is_pending(id, ticket, Kind::Thumb) {
            return;
        }
        // A load that finishes after its GIF was deleted is dropped.
        let Some(g) = self.lib.gif(id) else { return };
        // The first thumbnail tells us a GIF's shape if the import didn't.
        if let Ok(t) = &res
            && g.w == 0
        {
            let [w, h] = t.size().map(|v| u16::try_from(v).unwrap_or(u16::MAX));
            self.lib.set_size(id, w, h);
        }
        self.dirty |= self.loads.thumb_done(id, ticket, res, self.frame) && self.prefs.hide_gone;
    }

    pub(super) fn anim_loaded(&mut self, id: GifId, ticket: Ticket, res: Result<Arc<Anim>, Fail>) {
        if self.lib.gif(id).is_some() {
            self.dirty |= self.loads.anim_done(id, ticket, res, self.frame) && self.prefs.hide_gone;
        }
    }
}
