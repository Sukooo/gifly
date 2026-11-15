//! Which job a worker takes next: the one nearest the view, dropping those it left far behind.

use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Condvar, Mutex},
};

use crossbeam_channel::Sender;

use super::{Event, Job, Kind, lock};
use crate::db::GifId;

/// Loads further than this many screens from the view are dropped, and asked for again if the
/// view comes back to them.
const FAR_SCREENS: f32 = 3.0;

/// A stretch of the grid, in its content coordinates.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct Span {
    pub top: f32,
    pub bottom: f32,
}

/// The part of the grid in view, and where each GIF it shows is laid out.
#[derive(Clone, Default)]
pub struct Viewport {
    view: Span,
    places: Arc<HashMap<GifId, Span>>,
}

impl Viewport {
    /// How far GIF `id`'s tile is from the view as the grid is laid out now (a search or a
    /// reflow moves it): 0 for loads outside the grid, infinite if the grid no longer shows it.
    pub fn gap(&self, id: GifId, grid: bool) -> f32 {
        if !grid {
            return 0.0;
        }
        let Some(p) = self.places.get(&id) else { return f32::INFINITY };
        (p.top - self.view.bottom).max(self.view.top - p.bottom).max(0.0)
    }

    pub fn too_far(&self, id: GifId, grid: bool) -> bool {
        self.gap(id, grid) > (self.view.bottom - self.view.top) * FAR_SCREENS
    }
}

/// What's in view, shared with the threads that pick what to load next.
#[derive(Clone, Default)]
pub struct Focus(Arc<Mutex<Viewport>>);

impl Focus {
    pub fn get(&self) -> Viewport {
        lock(&self.0).clone()
    }

    pub fn set(&self, view: Span) {
        lock(&self.0).view = view;
    }

    /// Where each GIF in the grid is, whenever its layout changes.
    pub fn set_places(&self, places: HashMap<GifId, Span>) {
        lock(&self.0).places = Arc::new(places);
    }
}

pub(super) type Queue = Arc<(Mutex<VecDeque<Job>>, Condvar)>;

/// Orders jobs nearest the view first, then by kind.
pub(super) fn priority(focus: &Viewport, j: &Job) -> (u32, Kind) {
    // Whole pixels are plenty, and make the order total.
    (focus.gap(j.id, j.grid) as u32, j.kind)
}

/// Takes the job nearest the view, after dropping the ones far from it. Ties go to the newest.
pub(super) fn next_job(q: &mut VecDeque<Job>, focus: &Viewport, tx: &Sender<Event>) -> Option<Job> {
    q.retain(|j| {
        let keep = !focus.too_far(j.id, j.grid);
        if !keep {
            let _ = tx.send(j.dropped());
        }
        keep
    });
    let best = q.iter().enumerate().min_by_key(|(_, j)| priority(focus, j))?.0;
    q.remove(best)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::Ticket;

    #[test]
    fn jobs_nearest_the_view_go_first_and_far_ones_are_dropped() {
        let job = |id, kind, grid| Job {
            id: GifId(id),
            ticket: Ticket(0),
            url: String::new(),
            media_link: None,
            size: (0, 0),
            px: 0,
            kind,
            grid,
        };
        let view = Span { top: 1000.0, bottom: 1500.0 };
        // GIF 6 isn't in the grid anymore; 8 isn't from it.
        let tops = [(1, 1600.0), (2, 0.0), (3, 1100.0), (4, 1200.0), (5, 5000.0), (9, 1300.0), (7, 800.0)];
        let places = tops.map(|(id, top)| (GifId(id), Span { top, bottom: top + 100.0 }));
        let viewport = Viewport { view, places: Arc::new(HashMap::from(places)) };
        let mut q: VecDeque<Job> = [
            job(1, Kind::Warm, true),
            job(2, Kind::Thumb, true),
            job(3, Kind::Anim, true),
            job(4, Kind::Thumb, true),
            job(5, Kind::Thumb, true),
            job(6, Kind::Thumb, true),
            job(9, Kind::Warm, true),
            job(7, Kind::Warm, true),
            job(8, Kind::Thumb, false),
        ]
        .into();
        let (tx, rx) = crossbeam_channel::unbounded();
        let order: Vec<GifId> = std::iter::from_fn(|| next_job(&mut q, &viewport, &tx)).map(|j| j.id).collect();
        assert_eq!(order, [4, 8, 3, 9, 1, 7, 2].map(GifId));
        let dropped: Vec<GifId> =
            rx.try_iter().filter_map(|m| if let Event::Dropped { id, .. } = m { Some(id) } else { None }).collect();
        assert_eq!(dropped, [5, 6].map(GifId));
    }
}
