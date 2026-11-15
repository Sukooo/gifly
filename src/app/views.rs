//! What each view shows, the Home tiles and their order, and moving between views.

use std::{cmp::Ordering, collections::HashSet};

use crate::{
    clock,
    db::{Collection, CollectionId, Gif, GifId, Key},
};

use super::{App, View};

/// How many of the most recently sent GIFs lead Recent before frecency takes over.
const RECENT_PINNED: usize = 4;
/// Anything sent within this many seconds is in Recent regardless of how rarely it's used.
const RECENT_FRESH: i64 = 3 * 86_400;
/// Older GIFs stay only while their frecency is at least this.
const RECENT_KEEP: f32 = 2.0;
/// Recent shows at most this many GIFs (unless searching).
const RECENT_MAX: usize = 60;

/// One tile on Home: a view, with its name, size and the GIF shown on it.
pub struct HomeTile {
    pub view: View,
    pub hidden: bool,
    pub label: String,
    pub count: usize,
    pub cover: Option<GifId>,
}

/// A GIF with its frecency, worked out once per pass instead of in every comparison.
struct Ranked<'a> {
    gif: &'a Gif,
    frecency: f32,
}

/// What one pass over the library filters by: the time, the search terms, hidden collections
/// and, if they're hidden, GIFs whose file is gone.
struct Pass<'a> {
    now: i64,
    terms: Vec<String>,
    hidden: Vec<CollectionId>,
    gone: Option<&'a HashSet<GifId>>,
}

impl<'a> Pass<'a> {
    fn new(app: &'a App, query: &str) -> Self {
        Self {
            now: clock::now(),
            terms: query.to_lowercase().split_whitespace().map(str::to_string).collect(),
            // A GIF in any hidden collection is hidden everywhere until revealed.
            hidden: app.lib.collections().iter().filter(|c| c.hidden && !app.reveal).map(|c| c.id).collect(),
            gone: app.prefs.hide_gone.then_some(&app.loads.gone),
        }
    }

    /// The ids of the GIFs `view` shows, in display order.
    fn ids(&self, gifs: &[Gif], view: View) -> Vec<GifId> {
        let mut v: Vec<Ranked> = self.members(gifs, view).collect();
        v.sort_unstable_by(|a, b| order(view, self.first_term(), a, b));
        if view == View::Recent {
            // The last few sent lead, so "that one again" is always first; staples follow.
            let pinned = RECENT_PINNED.min(v.len());
            v[pinned..].sort_unstable_by(by_frecency);
            if self.terms.is_empty() {
                v.truncate(RECENT_MAX);
            }
        }
        v.iter().map(|r| r.gif.id).collect()
    }

    /// The GIFs `view` shows, unordered.
    fn members<'g>(&'g self, gifs: &'g [Gif], view: View) -> impl Iterator<Item = Ranked<'g>> {
        gifs.iter()
            .filter(move |g| self.shows(view, g))
            .filter(|g| !self.gone.is_some_and(|s| s.contains(&g.id)))
            .filter(|g| !g.cols.iter().any(|c| self.hidden.contains(c)))
            .filter(|g| self.terms.iter().all(|t| g.search_text.contains(t.as_str())))
            .map(|g| Ranked { gif: g, frecency: g.frecency(self.now) })
    }

    fn shows(&self, view: View, g: &Gif) -> bool {
        match view {
            View::Home => !self.terms.is_empty(),
            // Fresh sends stay a few days; after that only GIFs used often enough to keep
            // their frecency up remain (3 uses lasts ~8 days, 10 uses ~a month).
            View::Recent => {
                g.use_count > 0 && (self.now - g.last_used < RECENT_FRESH || g.frecency(self.now) >= RECENT_KEEP)
            }
            View::Copied => g.copied_at_ms > 0,
            View::Unsorted => g.in_library() && g.cols.is_empty(),
            // Every Discord favorite, in Discord's order, to browse as you would in Discord.
            View::Imports => g.imported,
            View::Collection(c) => g.cols.contains(&c),
            View::Settings => false,
        }
    }

    fn first_term(&self) -> &str {
        self.terms.first().map_or("", String::as_str)
    }
}

fn by_frecency(a: &Ranked, b: &Ranked) -> Ordering {
    b.frecency.total_cmp(&a.frecency).then(b.gif.added_at.cmp(&a.gif.added_at))
}

/// How `view` orders its GIFs. Recent sorts again after its first few (see `Pass::ids`).
fn order(view: View, first_term: &str, a: &Ranked, b: &Ranked) -> Ordering {
    match view {
        View::Recent => b.gif.last_used.cmp(&a.gif.last_used),
        View::Collection(_) => by_frecency(a, b),
        View::Copied => b.gif.copied_at_ms.cmp(&a.gif.copied_at_ms),
        View::Unsorted | View::Imports => b.gif.added_at.cmp(&a.gif.added_at),
        // Library before imports, title-prefix hits first, then frecency. `search_text` starts with the
        // lowercased title, so the prefix check ignores case like the search does.
        View::Home => (!a.gif.in_library(), !a.gif.search_text.starts_with(first_term))
            .cmp(&(!b.gif.in_library(), !b.gif.search_text.starts_with(first_term)))
            .then_with(|| by_frecency(a, b)),
        View::Settings => Ordering::Equal,
    }
}

impl App {
    /// The GIFs in `view` that match `query`, in display order. On Home, a search covers everything.
    pub fn view_ids(&self, view: View, query: &str) -> Vec<GifId> {
        Pass::new(self, query).ids(self.lib.gifs(), view)
    }

    /// Rebuilds `list` (and `home` on Home) after the library, view or search changed.
    pub fn recompute(&mut self) {
        self.dirty = false;
        let list = self.view_ids(self.view, &self.query);
        if list != self.list {
            self.grid.selected = selection_after_change(&self.list, &list, self.grid.selected);
            self.list = list;
            // Laid out afresh, which tells the loaders where its GIFs are now.
            self.grid.tiles.clear();
        }
        if self.view == View::Home && self.query.is_empty() {
            self.home = self.home_tiles();
        }
    }

    /// Home's tiles, in order: the views shown there, with their sizes and covers.
    pub(super) fn home_tiles(&mut self) -> Vec<HomeTile> {
        self.sync_order();
        let pass = Pass::new(self, "");
        self.order
            .iter()
            .filter_map(|&view| {
                let hidden = match view {
                    View::Collection(id) => self.lib.collection(id)?.hidden,
                    _ => false,
                };
                if hidden && !self.reveal {
                    return None;
                }
                // A tile only needs its size and first GIF, so skip sorting the whole view.
                let (mut count, mut cover) = (0, None::<Ranked>);
                for r in pass.members(self.lib.gifs(), view) {
                    count += 1;
                    if cover.as_ref().is_none_or(|c| order(view, "", &r, c).is_lt()) {
                        cover = Some(r);
                    }
                }
                if count == 0 && !matches!(view, View::Collection(_)) {
                    return None;
                }
                if view == View::Recent {
                    count = count.min(RECENT_MAX);
                }
                Some(HomeTile { view, hidden, label: self.view_name(view), count, cover: cover.map(|r| r.gif.id) })
            })
            .collect()
    }

    /// Ctrl+1–9: opens the `n`th Home tile from anywhere.
    pub(super) fn jump(&mut self, n: usize) {
        if let Some(t) = self.home_tiles().get(n) {
            self.navigate(t.view);
        }
    }

    /// Picks every GIF in the grid, or unpicks them if they all are.
    pub(super) fn pick_all(&mut self) {
        let picked: HashSet<GifId> = self.picked.iter().copied().collect();
        if self.list.iter().all(|id| picked.contains(id)) {
            let list: HashSet<GifId> = self.list.iter().copied().collect();
            self.picked.retain(|p| !list.contains(p));
        } else {
            self.picked.extend(self.list.iter().filter(|id| !picked.contains(id)));
        }
    }

    /// Brings `order` up to date with the collections (see the free `sync_order`).
    fn sync_order(&mut self) {
        let ids: Vec<CollectionId> = self.lib.collections().iter().map(|c| c.id).collect();
        sync_order(&mut self.order, &ids);
    }

    /// The collections that can be shown right now: hidden ones only while revealed.
    pub fn visible_collections(&self) -> impl Iterator<Item = &Collection> {
        self.lib.collections().iter().filter(|c| self.reveal || !c.hidden)
    }

    pub fn view_name(&self, view: View) -> String {
        match view {
            View::Home => "GIFs".into(),
            View::Recent => "Recent".into(),
            View::Copied => "Copied".into(),
            View::Unsorted => "Unsorted".into(),
            View::Imports => "Discord imports".into(),
            View::Settings => "Settings".into(),
            View::Collection(id) => self.lib.collection(id).map_or_else(String::new, |c| c.name.clone()),
        }
    }

    pub(super) fn move_tile(&mut self, v: View, target: View) {
        let pos = |v| self.order.iter().position(|&o| o == v);
        if let (Some(from), Some(to)) = (pos(v), pos(target)) {
            let v = self.order.remove(from);
            self.order.insert(to, v);
            self.dirty = true;
        }
    }

    /// Saves the Home order, and sorts collections (as menus list them) the same way.
    pub(super) fn save_order(&mut self) {
        self.lib.db().set_setting(Key::HomeOrder, &format_order(&self.order));
        let ids: Vec<CollectionId> =
            self.order.iter().filter_map(|&v| if let View::Collection(id) = v { Some(id) } else { None }).collect();
        self.lib.set_order(&ids);
    }

    /// Picks a GIF or unpicks it. With `range`, also picks every GIF between it and the one
    /// picked last, as listed.
    pub(super) fn pick(&mut self, id: GifId, range: bool) {
        let at = |x: GifId| self.list.iter().position(|&l| l == x);
        let span = self.picked.last().and_then(|&last| Some((at(last)?, at(id)?)));
        if let Some((a, b)) = span.filter(|_| range) {
            for &g in &self.list[a.min(b)..=a.max(b)] {
                if !self.picked.contains(&g) {
                    self.picked.push(g);
                }
            }
            // The clicked one is where the next range starts.
            self.picked.retain(|&p| p != id);
            self.picked.push(id);
        } else if let Some(p) = self.picked.iter().position(|&p| p == id) {
            self.picked.remove(p);
        } else {
            self.picked.push(id);
        }
    }

    /// Opens a view from a click, recording it in the back/forward history.
    pub(super) fn navigate(&mut self, v: View) {
        if v != self.view {
            self.history.leave(self.view);
        }
        self.reset_view(v);
    }

    /// Empties the search box and goes back to the top of the view.
    pub fn clear_search(&mut self) {
        self.query.clear();
        self.grid.selected = None;
        self.grid.scroll_to = Some(0.0);
        self.win.focus_search = true;
        self.dirty = true;
    }

    /// Shows `v` afresh: no search, picks or half-typed names carried over.
    pub(super) fn reset_view(&mut self, v: View) {
        self.view = v;
        self.query.clear();
        self.grid.selected = None;
        self.picked.clear();
        self.editing.creating = false;
        self.editing.new_name.clear();
        self.editing.renaming = None;
        self.win.focus_search = true;
        self.dirty = true;
    }

    /// Moves one step back or forward, skipping views that no longer exist or are hidden.
    pub(super) fn step_history(&mut self, back: bool) {
        let shown: Vec<CollectionId> = self.visible_collections().map(|c| c.id).collect();
        let reachable = |v| !matches!(v, View::Collection(id) if !shown.contains(&id));
        if let Some(v) = self.history.step(back, self.view, reachable) {
            self.reset_view(v);
        }
    }

    pub(super) fn toggle_reveal(&mut self) {
        self.reveal = !self.reveal;
        if let View::Collection(id) = self.view
            && !self.visible_collections().any(|c| c.id == id)
        {
            self.reset_view(View::Home);
        }
        self.grid.selected = None;
        self.dirty = true;
    }
}

/// The views to go back and forward to, since the picker opened.
#[derive(Default)]
pub(super) struct History {
    back: Vec<View>,
    fwd: Vec<View>,
}

impl History {
    /// Records leaving `from` for a newly opened view, which ends the forward history.
    fn leave(&mut self, from: View) {
        self.back.push(from);
        self.fwd.clear();
    }

    /// The view one step back (or forward) from `current`, dropping any on the way that
    /// `reachable` rejects. `current` becomes the step the other way.
    fn step(&mut self, back: bool, current: View, reachable: impl Fn(View) -> bool) -> Option<View> {
        let (from, to) = if back { (&mut self.back, &mut self.fwd) } else { (&mut self.fwd, &mut self.back) };
        while let Some(v) = from.pop() {
            if v != current && reachable(v) {
                to.push(current);
                return Some(v);
            }
        }
        None
    }
}

/// Keeps keyboard selection on the same GIF when a capture, deletion or filter changes the list.
fn selection_after_change<T: PartialEq>(old: &[T], new: &[T], selected: Option<usize>) -> Option<usize> {
    let id = old.get(selected?)?;
    new.iter().position(|g| g == id)
}

/// Keeps `order` listing each Home tile once: drops deleted collections and puts new ones after
/// the last collection, or after Copied when there's none yet (so by default: Recent, Copied,
/// collections, Discord imports, Unsorted).
fn sync_order(order: &mut Vec<View>, cols: &[CollectionId]) {
    order.retain(|v| !matches!(v, View::Collection(id) if !cols.contains(id)));
    let after = |order: &[View], v: View| order.iter().position(|&o| o == v).map(|i| i + 1);
    if !order.contains(&View::Recent) {
        order.insert(0, View::Recent);
    }
    if !order.contains(&View::Copied) {
        let at = after(order, View::Recent).unwrap_or(0);
        order.insert(at, View::Copied);
    }
    for v in [View::Imports, View::Unsorted] {
        if !order.contains(&v) {
            order.push(v);
        }
    }
    for &id in cols {
        let v = View::Collection(id);
        if !order.contains(&v) {
            let last = order.iter().rposition(|v| matches!(v, View::Collection(_))).map(|i| i + 1);
            let at = last.or_else(|| after(order, View::Copied)).unwrap_or(0);
            order.insert(at, v);
        }
    }
}

/// Parses the saved Home order ("recent 3 imports 5 unsorted"); unknown words are skipped.
pub(super) fn parse_order(s: &str) -> Vec<View> {
    let mut order = Vec::new();
    for w in s.split_whitespace() {
        let v = match w {
            "recent" => View::Recent,
            "copied" => View::Copied,
            "imports" => View::Imports,
            "unsorted" => View::Unsorted,
            _ => match w.parse() {
                Ok(id) => View::Collection(CollectionId(id)),
                Err(_) => continue,
            },
        };
        if !order.contains(&v) {
            order.push(v);
        }
    }
    order
}

/// The inverse of `parse_order`.
fn format_order(order: &[View]) -> String {
    let words: Vec<String> = order
        .iter()
        .filter_map(|v| match v {
            View::Recent => Some("recent".into()),
            View::Copied => Some("copied".into()),
            View::Imports => Some("imports".into()),
            View::Unsorted => Some("unsorted".into()),
            View::Collection(id) => Some(id.to_string()),
            View::Home | View::Settings => None,
        })
        .collect();
    words.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_follows_the_gif_when_the_list_changes() {
        let old = [1, 2, 3];
        assert_eq!(selection_after_change(&old, &[4, 1, 2, 3], Some(1)), Some(2), "a new capture comes first");
        assert_eq!(selection_after_change(&old, &[2, 3], Some(1)), Some(0), "a GIF before the selection was deleted");
        assert_eq!(selection_after_change(&old, &[3, 1, 2], Some(1)), Some(2), "the list was reordered");
        assert_eq!(selection_after_change(&old, &[1, 3], Some(1)), None, "the selected GIF was deleted or hidden");
        assert_eq!(selection_after_change(&old, &[], Some(1)), None);
        assert_eq!(selection_after_change(&old, &[1, 2, 3], None), None);
        assert_eq!(selection_after_change(&old, &[1, 2, 3], Some(9)), None);
    }

    #[test]
    fn history_steps_back_and_forward_past_views_that_are_gone() {
        let gone = View::Collection(CollectionId(7));
        let reachable = |v| v != gone;
        let mut h = History::default();
        // Home -> Recent -> (deleted collection) -> Copied.
        h.leave(View::Home);
        h.leave(View::Recent);
        h.leave(gone);
        assert_eq!(h.step(true, View::Copied, reachable), Some(View::Recent), "skips the deleted collection");
        assert_eq!(h.step(true, View::Recent, reachable), Some(View::Home));
        assert_eq!(h.step(true, View::Home, reachable), None);
        assert_eq!(h.step(false, View::Home, reachable), Some(View::Recent));
        assert_eq!(h.step(false, View::Recent, reachable), Some(View::Copied));
        // Opening a view from Recent ends the forward history.
        h.step(true, View::Copied, reachable);
        h.leave(View::Recent);
        assert_eq!(h.step(false, View::Unsorted, reachable), None);
    }

    #[test]
    fn home_order_round_trips() {
        let order = parse_order("recent 3 copied imports 5 unsorted bogus 3");
        assert_eq!(
            order,
            [
                View::Recent,
                View::Collection(CollectionId(3)),
                View::Copied,
                View::Imports,
                View::Collection(CollectionId(5)),
                View::Unsorted
            ]
        );
        assert_eq!(format_order(&order), "recent 3 copied imports 5 unsorted");
    }

    #[test]
    fn new_collections_go_after_the_last_one() {
        use View::{Copied, Imports, Recent, Unsorted};
        let c = |id| View::Collection(CollectionId(id));
        let mut order = parse_order("imports copied recent");
        sync_order(&mut order, &[1, 2].map(CollectionId));
        assert_eq!(order, [Imports, Copied, c(1), c(2), Recent, Unsorted]);
        let mut order = vec![c(9), Recent];
        sync_order(&mut order, &[CollectionId(1)]);
        assert_eq!(order, [Recent, Copied, c(1), Imports, Unsorted]);
    }

    const NOW: i64 = 1_000 * 86_400;
    const HOUR: i64 = 3_600;

    fn gif(id: i64, title: &str, edit: impl FnOnce(&mut Gif)) -> Gif {
        let mut g = Gif::from_new(GifId(id), crate::db::NewGif::link(format!("https://x/{id}.gif"), title.into(), id));
        g.search_text = title.to_lowercase();
        edit(&mut g);
        g
    }

    fn used(count: i64, ago: i64) -> impl FnOnce(&mut Gif) {
        move |g| (g.use_count, g.last_used) = (count, NOW - ago)
    }

    fn pass(query: &str, hidden: &[i64]) -> Pass<'static> {
        Pass {
            now: NOW,
            terms: query.split_whitespace().map(str::to_lowercase).collect(),
            hidden: hidden.iter().copied().map(CollectionId).collect(),
            gone: None,
        }
    }

    #[test]
    fn gone_gifs_are_left_out_only_when_hidden() {
        let gifs = [gif(1, "cat", |_| {}), gif(2, "cat", |_| {}), gif(3, "cat", |_| {})];
        let gone = HashSet::from([GifId(2)]);
        let mut p = pass("cat", &[]);
        assert_eq!(p.ids(&gifs, View::Home).len(), 3);
        p.gone = Some(&gone);
        let mut ids = p.ids(&gifs, View::Home);
        ids.sort_unstable();
        assert_eq!(ids, [1, 3].map(GifId));
    }

    #[test]
    fn recent_pins_the_last_sends_then_ranks_by_frecency() {
        let gifs = [
            gif(1, "", used(1, HOUR)),
            gif(2, "", used(1, 2 * HOUR)),
            gif(3, "", used(1, 3 * HOUR)),
            gif(4, "", used(1, 4 * HOUR)),
            // Fresh but rarely used: stays, below the staple.
            gif(5, "", used(1, 5 * HOUR)),
            // Ten days old, used often enough to stay.
            gif(6, "", used(10, 10 * 86_400)),
            // Ten days old and used once: gone.
            gif(7, "", used(1, 10 * 86_400)),
            // Never sent.
            gif(8, "", |_| {}),
            // In a hidden collection.
            gif(9, "", |g| {
                used(50, HOUR / 2)(g);
                g.cols = vec![CollectionId(42)];
            }),
        ];
        assert_eq!(pass("", &[42]).ids(&gifs, View::Recent), [1, 2, 3, 4, 6, 5].map(GifId));
        assert_eq!(pass("", &[]).ids(&gifs, View::Recent)[0], GifId(9));
    }

    #[test]
    fn search_puts_library_and_title_prefix_hits_first() {
        let gifs = [
            gif(1, "Dancing cat", used(20, HOUR)),
            gif(2, "Cat jump", used(1, HOUR)),
            gif(3, "cat imported", |g| {
                g.imported = true;
                g.saved = false;
            }),
            gif(4, "dog", used(99, HOUR)),
        ];
        assert_eq!(pass("CAT", &[]).ids(&gifs, View::Home), [2, 1, 3].map(GifId));
        assert!(pass("", &[]).ids(&gifs, View::Home).is_empty());
    }
}
