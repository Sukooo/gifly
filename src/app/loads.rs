//! What's known about loading each GIF: its textures (loading, loaded or failed and why), whether
//! it was downloaded ahead or is waiting on a fresh link, and whether its file is gone. Changes
//! that touch more than one of these go through here, so they can't disagree.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::{
    db::GifId,
    media::{Fail, Kind, Ticket},
};

/// A texture that's loading, loaded (with the frame it was last drawn on), or failed to load.
pub enum Slot<T> {
    Pending(Ticket),
    Ready(T, u64),
    Failed(Fail),
}

impl<T> Slot<T> {
    fn is_ready(&self) -> bool {
        matches!(self, Self::Ready(..))
    }

    fn failure(&self) -> Option<&Fail> {
        if let Self::Failed(f) = self { Some(f) } else { None }
    }
}

/// Load state per GIF id. Generic over the texture types so it's testable without a GPU.
pub struct Loads<T, A> {
    next: Ticket,
    pub(super) thumbs: HashMap<GifId, Slot<T>>,
    pub(super) anims: HashMap<GifId, Slot<A>>,
    /// GIFs already downloaded ahead of scrolling this session.
    pub(super) warmed: HashSet<GifId>,
    /// GIFs waiting on a fresh link.
    pub(super) renewing: HashSet<GifId>,
    /// GIFs whose file was deleted, outlined in the grid.
    pub(super) gone: HashSet<GifId>,
}

impl<T, A> Default for Loads<T, A> {
    fn default() -> Self {
        Self {
            next: Ticket(0),
            thumbs: HashMap::new(),
            anims: HashMap::new(),
            warmed: HashSet::new(),
            renewing: HashSet::new(),
            gone: HashSet::new(),
        }
    }
}

impl<T, A> Loads<T, A> {
    /// Starts a load with a ticket that distinguishes it from an earlier attempt.
    pub fn start(&mut self, id: GifId, kind: Kind) -> Ticket {
        self.next = Ticket(self.next.0.wrapping_add(1));
        let ticket = self.next;
        match kind {
            Kind::Thumb => {
                self.thumbs.insert(id, Slot::Pending(ticket));
            }
            Kind::Anim => {
                self.anims.insert(id, Slot::Pending(ticket));
            }
            Kind::Warm => {}
        }
        ticket
    }

    /// Whether this is still the attempt the UI is waiting for.
    pub fn is_pending(&self, id: GifId, ticket: Ticket, kind: Kind) -> bool {
        let pending = |slot: Option<&Ticket>| slot == Some(&ticket);
        match kind {
            Kind::Thumb => pending(self.thumbs.get(&id).and_then(pending_ticket)),
            Kind::Anim => pending(self.anims.get(&id).and_then(pending_ticket)),
            Kind::Warm => false,
        }
    }

    /// Stores a finished thumbnail load. True if it found the GIF's file newly gone.
    pub fn thumb_done(&mut self, id: GifId, ticket: Ticket, res: Result<T, Fail>, frame: u64) -> bool {
        done(&mut self.thumbs, &mut self.gone, id, ticket, res, frame)
    }

    /// Stores a finished animation load. True if it found the GIF's file newly gone.
    pub fn anim_done(&mut self, id: GifId, ticket: Ticket, res: Result<A, Fail>, frame: u64) -> bool {
        done(&mut self.anims, &mut self.gone, id, ticket, res, frame)
    }

    /// Why the GIF failed to load, if it did: its thumbnail's reason first.
    pub fn failure(&self, id: GifId) -> Option<&Fail> {
        failed(&self.thumbs, id).or_else(|| failed(&self.anims, id))
    }

    pub fn loaded(&self) -> (usize, usize) {
        (self.thumbs.values().filter(|s| s.is_ready()).count(), self.anims.values().filter(|s| s.is_ready()).count())
    }

    /// How many loads failed, by reason.
    pub fn failures(&self) -> BTreeMap<String, usize> {
        let mut by = BTreeMap::new();
        let fails = self.thumbs.values().filter_map(Slot::failure);
        for f in fails.chain(self.anims.values().filter_map(Slot::failure)) {
            *by.entry(format!("{f:?}")).or_default() += 1;
        }
        by
    }

    /// Worth downloading ahead: not loaded, loading or downloaded yet.
    pub fn wants_warming(&self, id: GifId) -> bool {
        !self.thumbs.contains_key(&id) && !self.warmed.contains(&id)
    }

    /// Settles loads that were waiting for a fresh link: asked for again if it came, else failed.
    pub fn settle(&mut self, id: GifId, renewed: bool) {
        self.renewing.remove(&id);
        settle(&mut self.thumbs, id, renewed);
        settle(&mut self.anims, id, renewed);
    }

    /// A queued load was dropped before it ran: forget it so it's asked for again.
    pub fn dropped(&mut self, id: GifId, ticket: Ticket, kind: Kind) {
        match kind {
            Kind::Thumb => remove_pending(&mut self.thumbs, id, Some(ticket)),
            Kind::Anim => remove_pending(&mut self.anims, id, Some(ticket)),
            Kind::Warm => {
                self.warmed.remove(&id);
            }
        }
    }

    /// A renewal was dropped unsent: everything waiting on it is asked for again when needed.
    pub fn renewals_dropped(&mut self, id: GifId) {
        self.renewing.remove(&id);
        remove_pending(&mut self.thumbs, id, None);
        remove_pending(&mut self.anims, id, None);
        self.warmed.remove(&id);
    }

    /// A replaced renewer no longer owns any pending loads.
    pub fn cancel_renewals(&mut self) {
        for id in std::mem::take(&mut self.renewing) {
            self.renewals_dropped(id);
        }
    }

    /// Invalidates unfinished or failed attempts so the GIF can retry. True if it was marked gone.
    pub fn retry(&mut self, id: GifId) -> bool {
        if !self.thumbs.get(&id).is_some_and(Slot::is_ready) {
            self.thumbs.remove(&id);
        }
        if !self.anims.get(&id).is_some_and(Slot::is_ready) {
            self.anims.remove(&id);
        }
        self.gone.remove(&id)
    }

    /// Drops everything about a GIF, e.g. after it's deleted.
    pub fn forget(&mut self, id: GifId) {
        self.thumbs.remove(&id);
        self.anims.remove(&id);
        self.warmed.remove(&id);
        self.renewing.remove(&id);
        self.gone.remove(&id);
    }

    /// Keeps the texture caches bounded: `thumbs` many, animations to `anim_bytes` by `bytes`.
    /// Nothing drawn on frame `now` goes.
    pub fn evict(&mut self, thumbs: usize, anim_bytes: usize, now: u64, bytes: impl Fn(&A) -> usize) {
        evict_lru(&mut self.thumbs, thumbs, now, |_| 1);
        evict_lru(&mut self.anims, anim_bytes, now, bytes);
    }

    /// Frees every animation and most thumbnails, e.g. once the picker is hidden. Remembers which
    /// GIFs turned out to be still images, so they aren't decoded again on every open.
    pub fn shrink(&mut self, thumbs: usize) {
        self.anims.retain(|_, s| matches!(s, Slot::Failed(Fail::Still)));
        evict_lru(&mut self.thumbs, thumbs, u64::MAX, |_| 1);
    }
}

fn done<T>(
    m: &mut HashMap<GifId, Slot<T>>,
    gone: &mut HashSet<GifId>,
    id: GifId,
    ticket: Ticket,
    res: Result<T, Fail>,
    frame: u64,
) -> bool {
    if m.get(&id).and_then(pending_ticket) != Some(&ticket) {
        return false;
    }
    let found_gone = matches!(res, Err(Fail::Gone)) && gone.insert(id);
    m.insert(id, res.map_or_else(Slot::Failed, |t| Slot::Ready(t, frame)));
    found_gone
}

fn failed<T>(m: &HashMap<GifId, Slot<T>>, id: GifId) -> Option<&Fail> {
    m.get(&id).and_then(Slot::failure)
}

fn settle<T>(m: &mut HashMap<GifId, Slot<T>>, id: GifId, renewed: bool) {
    if matches!(m.get(&id), Some(Slot::Pending(_))) {
        if renewed {
            m.remove(&id);
        } else {
            m.insert(id, Slot::Failed(Fail::Expired));
        }
    }
}

fn pending_ticket<T>(slot: &Slot<T>) -> Option<&Ticket> {
    if let Slot::Pending(ticket) = slot { Some(ticket) } else { None }
}

fn remove_pending<T>(m: &mut HashMap<GifId, Slot<T>>, id: GifId, ticket: Option<Ticket>) {
    if m.get(&id).and_then(pending_ticket).is_some_and(|&pending| ticket.is_none_or(|t| t == pending)) {
        m.remove(&id);
    }
}

/// Trims `m`'s loaded entries to `cap` by `weight`, dropping the least recently drawn but none
/// drawn on frame `now`. Waits until it's a quarter over so it isn't sorting every frame.
fn evict_lru<T>(m: &mut HashMap<GifId, Slot<T>>, cap: usize, now: u64, weight: impl Fn(&T) -> usize) {
    let mut ready: Vec<(u64, GifId, usize)> = m
        .iter()
        .filter_map(|(&k, v)| match v {
            Slot::Ready(t, f) => Some((*f, k, weight(t))),
            _ => None,
        })
        .collect();
    let mut total: usize = ready.iter().map(|r| r.2).sum();
    if total <= cap + cap / 4 {
        return;
    }
    ready.sort_unstable();
    for (f, k, w) in ready {
        if total <= cap || f >= now {
            break;
        }
        m.remove(&k);
        total -= w;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type L = Loads<(), ()>;
    const A: GifId = GifId(1);
    const B: GifId = GifId(2);
    const C: GifId = GifId(3);

    fn thumb(l: &mut L, id: GifId, res: Result<(), Fail>, frame: u64) -> bool {
        let ticket = l.start(id, Kind::Thumb);
        l.thumb_done(id, ticket, res, frame)
    }

    fn anim(l: &mut L, id: GifId, res: Result<(), Fail>, frame: u64) -> bool {
        let ticket = l.start(id, Kind::Anim);
        l.anim_done(id, ticket, res, frame)
    }

    #[test]
    fn old_results_and_drops_cannot_overwrite_a_retry() {
        let mut l = L::default();
        let old = l.start(A, Kind::Thumb);
        l.retry(A);
        let current = l.start(A, Kind::Thumb);
        assert_ne!(old, current);
        l.dropped(A, old, Kind::Thumb);
        assert!(l.is_pending(A, current, Kind::Thumb));
        assert!(!l.thumb_done(A, old, Err(Fail::Gone), 0));
        assert!(l.gone.is_empty() && l.is_pending(A, current, Kind::Thumb));
        l.thumb_done(A, current, Ok(()), 0);
        l.thumb_done(A, old, Err(Fail::Gone), 0);
        assert!(matches!(l.thumbs.get(&A), Some(Slot::Ready(..))));
        assert!(l.gone.is_empty());
        let old = l.start(A, Kind::Anim);
        l.shrink(0);
        let current = l.start(A, Kind::Anim);
        l.anim_done(A, old, Err(Fail::Gone), 0);
        assert!(l.is_pending(A, current, Kind::Anim) && l.gone.is_empty());
        l.forget(A);
        let current = l.start(A, Kind::Anim);
        l.anim_done(A, old, Ok(()), 0);
        assert!(l.is_pending(A, current, Kind::Anim), "undo restored the same GIF id");
    }

    fn pending(l: &mut L, id: GifId) {
        l.thumbs.insert(id, Slot::Pending(Ticket(0)));
        l.anims.insert(id, Slot::Pending(Ticket(0)));
    }

    #[test]
    fn evict_lru_drops_the_least_recently_drawn_and_keeps_pending() {
        let mut m: HashMap<GifId, Slot<()>> = (0..6).map(|i| (GifId(i), Slot::Ready((), i as u64))).collect();
        m.insert(GifId(10), Slot::Pending(Ticket(0)));
        m.insert(GifId(11), Slot::Failed(Fail::NotImage));
        evict_lru(&mut m, 4, u64::MAX, |_| 1);
        let mut left: Vec<GifId> = m.keys().copied().collect();
        left.sort_unstable();
        assert_eq!(left, [2, 3, 4, 5, 10, 11].map(GifId));
        // Within a quarter over the cap, nothing is sorted or dropped yet.
        evict_lru(&mut m, 5, u64::MAX, |_| 1);
        assert_eq!(m.len(), 6);
    }

    #[test]
    fn evict_lru_weighs_entries_and_keeps_what_is_on_screen() {
        let mut m: HashMap<GifId, Slot<usize>> =
            [(0, 0), (1, 1), (2, 5)].into_iter().map(|(k, f)| (GifId(k), Slot::Ready(60, f))).collect();
        evict_lru(&mut m, 100, 5, |&b| b);
        assert_eq!(m.keys().collect::<Vec<_>>(), [&GifId(2)]);
        // All on screen: kept, however much they take.
        m.extend([(GifId(3), Slot::Ready(60, 5)), (GifId(4), Slot::Ready(60, 5))]);
        evict_lru(&mut m, 100, 5, |&b| b);
        assert_eq!(m.len(), 3);
    }

    #[test]
    fn a_gone_file_is_marked_once_and_cleared_by_a_retry() {
        let mut l = L::default();
        assert!(thumb(&mut l, A, Err(Fail::Gone), 0));
        assert!(!anim(&mut l, A, Err(Fail::Gone), 0), "already known");
        assert!(!thumb(&mut l, B, Err(Fail::Network("timeout".into())), 0));
        assert_eq!(l.failure(A), Some(&Fail::Gone));
        assert!(l.retry(A));
        assert!(l.failure(A).is_none() && l.gone.is_empty());
        assert!(!l.thumbs.contains_key(&A) && !l.anims.contains_key(&A));
        assert!(!l.retry(B));
        assert!(l.failure(B).is_none());
    }

    #[test]
    fn hiding_frees_animations_but_remembers_still_images() {
        let mut l = L::default();
        anim(&mut l, A, Ok(()), 0);
        anim(&mut l, B, Err(Fail::Still), 0);
        anim(&mut l, C, Err(Fail::NotImage), 0);
        l.shrink(0);
        assert_eq!(l.anims.keys().collect::<Vec<_>>(), [&B]);
    }

    #[test]
    fn a_renewal_settles_only_what_waited_for_it() {
        let mut l = L::default();
        pending(&mut l, A);
        pending(&mut l, B);
        l.thumbs.insert(C, Slot::Ready((), 0));
        l.renewing.extend([A, B, C]);
        l.settle(A, true);
        l.settle(B, false);
        l.settle(C, false);
        assert!(l.renewing.is_empty());
        assert!(!l.thumbs.contains_key(&A) && !l.anims.contains_key(&A), "asked for again");
        assert_eq!(l.failure(B), Some(&Fail::Expired));
        assert!(matches!(l.thumbs.get(&C), Some(Slot::Ready(..))));
    }

    #[test]
    fn a_dropped_renewal_lets_everything_ask_again() {
        let mut l = L::default();
        pending(&mut l, A);
        l.warmed.insert(A);
        l.renewing.insert(A);
        l.anims.insert(A, Slot::Ready((), 0));
        l.renewals_dropped(A);
        assert!(l.wants_warming(A) && l.renewing.is_empty());
        assert!(matches!(l.anims.get(&A), Some(Slot::Ready(..))), "a loaded texture stays");
    }

    #[test]
    fn forgetting_a_gif_leaves_nothing_for_a_reused_id() {
        let mut l = L::default();
        thumb(&mut l, A, Err(Fail::Gone), 0);
        l.warmed.insert(A);
        l.renewing.insert(A);
        l.anims.insert(A, Slot::Pending(Ticket(0)));
        l.forget(A);
        assert!(l.wants_warming(A) && l.failure(A).is_none());
        assert!(l.gone.is_empty() && l.renewing.is_empty() && l.anims.is_empty());
    }
}
