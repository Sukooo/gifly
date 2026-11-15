//! Debug builds only: `GIFLY_SOAK=<rounds>` opens the picker off-screen without taking focus,
//! scrolls through Discord imports, hides it and waits, round after round, then quits. Memory is
//! sampled from outside; each step is printed with the time, to line the samples up with.
//!
//! `GIFLY_SOAK=tour` instead opens Home and then every view on it once, printing how long each
//! one's first screen took to load (thumbnails, then animations), and how many tiles didn't.
//!
//! `GIFLY_SOAK_HOVER=1` turns on "Play GIFs on hover" for the run, without saving it.

use std::{
    sync::atomic::Ordering::Relaxed,
    time::{Duration, Instant},
};

use crate::{db::GifId, platform};

use super::{App, View, loads::Slot};

const OPEN: Duration = Duration::from_secs(40);
const HIDDEN: Duration = Duration::from_secs(30);
const STEP: Duration = Duration::from_millis(500);
/// A view whose first screen hasn't loaded by then is counted as it is and left.
const TOUR_WAIT: Duration = Duration::from_secs(15);

pub(super) struct Soak {
    rounds: u32,
    round: u32,
    open: bool,
    since: Instant,
    step: Instant,
    /// Tour mode: the views still to visit, last first.
    tour: Option<Vec<View>>,
    /// The drawn frame the current tour view was opened on.
    frame: u64,
    hover_play: bool,
}

impl Soak {
    pub(super) fn from_env() -> Option<Self> {
        // Checked at run time, not compiled out: scripts/soak.ps1 builds release with debug
        // assertions on, so it measures the real build.
        let v = std::env::var("GIFLY_SOAK").ok().filter(|_| cfg!(debug_assertions))?;
        let tour = (v == "tour").then(|| vec![View::Home]);
        let rounds = if tour.is_some() { 0 } else { v.parse().ok()? };
        let now = Instant::now();
        mark("start", 0);
        Some(Self {
            rounds,
            round: 0,
            open: false,
            since: now,
            step: now,
            tour,
            frame: 0,
            hover_play: std::env::var_os("GIFLY_SOAK_HOVER").is_some(),
        })
    }
}

impl App {
    /// Steps the soak test. Called from `logic`, which runs whether or not the picker is shown.
    pub fn soak(&mut self) {
        if self.soak.as_ref().is_some_and(|s| s.hover_play) {
            self.prefs.hover_play = true;
        }
        if self.soak.as_ref().is_some_and(|s| s.tour.is_some()) {
            return self.tour();
        }
        let Some(s) = &mut self.soak else { return };
        self.ctx.request_repaint_after(STEP);
        let now = Instant::now();
        if s.open && now - s.since >= OPEN {
            (s.open, s.since) = (false, now);
            mark("hide", s.round);
            // The hotkey's way of closing it, so `hide` runs inside a drawn frame as it does for real.
            self.shared.hide_requested.store(true, Relaxed);
        } else if s.open && now - s.step >= STEP {
            s.step = now;
            if self.view != View::Imports {
                self.navigate(View::Imports);
            } else {
                self.grid.scroll_to = Some(self.grid.scroll_off + self.grid.view_h);
            }
        } else if !s.open && now - s.since >= HIDDEN {
            if s.round == s.rounds {
                mark("done", s.round);
                std::process::exit(0);
            }
            s.round += 1;
            (s.open, s.since, s.step) = (true, now, now);
            mark("open", s.round);
            self.shared.just_shown.store(true, Relaxed);
            platform::show_offscreen(self.shared.hwnd.get());
        }
    }
}

impl App {
    fn tour(&mut self) {
        self.ctx.request_repaint_after(Duration::from_millis(10));
        let (shown, waiting) = self.first_screen();
        let Some(s) = &mut self.soak else { return };
        let now = Instant::now();
        if !s.open {
            // Lets startup settle, as a real open would come well after it.
            if now - s.since < Duration::from_secs(5) {
                return;
            }
            s.open = true;
            self.shared.just_shown.store(true, Relaxed);
            platform::show_offscreen(self.shared.hwnd.get());
        } else {
            // A view is laid out in the frame after it's opened.
            let took = now - s.step;
            if self.frame < s.frame + 2 || (waiting > 0 && took < TOUR_WAIT) {
                return;
            }
            let ms = crate::clock::now_ms();
            eprintln!("tour {ms} {:?} {shown} tiles, {waiting} not loaded, {} ms", self.view, took.as_millis());
            if self.view == View::Home {
                let views = self.home.iter().map(|t| t.view).rev();
                s.tour.get_or_insert_default().splice(0..0, views);
            }
        }
        let Some(next) = s.tour.as_mut().and_then(Vec::pop) else {
            mark("done", 0);
            std::process::exit(0);
        };
        (s.step, s.frame) = (Instant::now(), self.frame);
        self.navigate(next);
    }

    /// How many GIFs the view shows on its first screen, and how many of those haven't finished
    /// loading: their thumbnail, and their animation once that's there.
    fn first_screen(&self) -> (usize, usize) {
        let ids: Vec<GifId> = if self.view == View::Home {
            self.home.iter().filter(|t| !t.hidden).filter_map(|t| t.cover).collect()
        } else {
            let screen = self.list.iter().zip(&self.grid.tiles).filter(|(_, t)| t.min.y < self.grid.view_h);
            screen.map(|(&id, _)| id).collect()
        };
        // With "Play GIFs on hover" on, animations load only when hovered, so stills are the whole screen.
        let waiting = ids.iter().filter(|id| match self.loads.thumbs.get(id) {
            Some(Slot::Failed(_)) => false,
            Some(Slot::Ready(..)) => {
                !self.prefs.hover_play && !matches!(self.loads.anims.get(id), Some(Slot::Ready(..) | Slot::Failed(_)))
            }
            _ => true,
        });
        (ids.len(), waiting.count())
    }
}

fn mark(what: &str, round: u32) {
    let ms = crate::clock::now_ms();
    eprintln!("soak {ms} {what} {round}");
}
