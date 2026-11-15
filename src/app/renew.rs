//! Renewing expired Discord file links with the bot token saved in Settings. A tile in view with
//! nothing cached starts a request, which also takes the expired GIFs coming up next in the view,
//! so scrolling costs one request per `RENEW_MAX` GIFs. Like loads, renewals nearest the view
//! go first, and ones the view has left far behind are dropped unsent.

use std::{
    sync::atomic::{AtomicU64, Ordering::Relaxed},
    time::{Duration, Instant},
};

use crossbeam_channel::{Sender, TryRecvError, unbounded};
use eframe::egui;

use crate::{
    db::{GifId, Key},
    discord::{self, RenewError},
    log::{self, Area},
    media::{Fail, Focus, Kind, Ticket},
    stats, urls,
};

use super::{App, Msg};

/// How long to keep gathering links after the first, so a screenful goes in one request.
const GATHER: Duration = Duration::from_millis(250);
/// How far down the view to look for GIFs to renew along with one that's in view.
const LOOK_AHEAD: usize = 150;

/// A link to renew.
struct Ask {
    id: GifId,
    link: String,
    /// For a tile in the grid, which is renewed nearest the view first.
    grid: bool,
}

/// Tells renewers apart, so answers from one that was replaced (its token changed
/// mid-request) are ignored.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RenewerId(u64);

/// The thread that asks Discord for fresh links. Dropping it ends the thread.
pub(super) struct Renewer {
    tx: Sender<Ask>,
    id: RenewerId,
}

impl Renewer {
    pub(super) fn spawn(token: String, focus: Focus, out: Sender<Msg>, ctx: egui::Context) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = RenewerId(NEXT.fetch_add(1, Relaxed));
        let (tx, rx) = unbounded::<Ask>();
        crate::spawn("renew", move || {
            let mut waiting: Vec<Ask> = Vec::new();
            loop {
                if waiting.is_empty() {
                    let Ok(first) = rx.recv() else { return };
                    waiting.push(first);
                }
                let until = Instant::now() + GATHER;
                while waiting.len() < discord::RENEW_MAX
                    && let Ok(next) = rx.recv_deadline(until)
                {
                    waiting.push(next);
                }
                loop {
                    match rx.try_recv() {
                        Ok(next) => waiting.push(next),
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => return,
                    }
                }

                let view = focus.get();
                let (far, mut near): (Vec<Ask>, Vec<Ask>) = waiting.drain(..).partition(|a| view.too_far(a.id, a.grid));
                if !far.is_empty() {
                    let _ =
                        out.send(Msg::RenewalsDropped { renewer: id, ids: far.into_iter().map(|a| a.id).collect() });
                    ctx.request_repaint();
                }
                near.sort_by(|a, b| view.gap(a.id, a.grid).total_cmp(&view.gap(b.id, b.grid)));
                let batch: Vec<Ask> = near.drain(..near.len().min(discord::RENEW_MAX)).collect();
                waiting = near;
                if batch.is_empty() {
                    continue;
                }

                let links: Vec<&str> = batch.iter().map(|a| a.link.as_str()).collect();
                let start = Instant::now();
                let res: Result<Vec<_>, _> = discord::renew_links(&token, &links)
                    .map(|fresh| batch.iter().zip(fresh).filter_map(|(a, f)| Some((a.id, f?))).collect());
                stats::add(&stats::RENEW_REQUESTS, 1);
                match &res {
                    Ok(fresh) => stats::add(&stats::RENEWED, fresh.len()),
                    Err(_) => stats::add(&stats::RENEW_FAILED, batch.len()),
                }
                log::debug(Area::Renew, || {
                    let got = res.as_ref().map_or_else(ToString::to_string, |f| format!("{} renewed", f.len()));
                    let left = waiting.len();
                    format!("renew request: {} links, {got}, {:?}, {left} waiting", batch.len(), start.elapsed())
                });
                let _ =
                    out.send(Msg::Renewed { renewer: id, asked: batch.iter().map(|a| a.id).collect(), result: res });
                ctx.request_repaint();
            }
        });
        Self { tx, id }
    }
}

impl App {
    pub fn has_bot_token(&self) -> bool {
        self.renewer.is_some()
    }

    /// Saves the bot token (an empty one removes it) and retries the Discord GIFs that failed.
    pub(super) fn set_bot_token(&mut self, token: &str) {
        let token = discord::clean_bot_token(token);
        // Not best-effort: a token that wasn't saved would work now and quietly be gone next launch.
        if let Err(e) = self.lib.db().try_set_setting(Key::BotToken, token) {
            log::error(Area::Db, format!("saving the bot token: {e}"));
            self.form.renew_error = Some("Couldn't save the token. Try again.".into());
            return;
        }
        self.form.renew_error = None;
        self.renewer = (!token.is_empty())
            .then(|| Renewer::spawn(token.into(), self.media.focus.clone(), self.tx.clone(), self.ctx.clone()));
        self.loads.cancel_renewals();
        self.retry_discord();
    }

    /// Gives Discord GIFs that failed to load another go, now that a link may be renewed.
    pub(super) fn retry_discord(&mut self) {
        if self.renewer.is_none() {
            return;
        }
        // Files known to be deleted were tried with a fresh link already.
        let failed: Vec<usize> = (self.lib.gifs().iter().enumerate())
            .filter(|&(gi, g)| {
                self.load_failed(gi)
                    && !self.is_gone(g.id)
                    && urls::discord_link(&g.url, g.media_link.as_deref()).is_some()
            })
            .map(|(gi, _)| gi)
            .collect();
        for gi in failed {
            self.retry_load(gi);
        }
    }

    /// A Discord GIF's link expired before it could load: ask for a fresh one, if there's a token.
    pub(super) fn link_expired(&mut self, id: GifId, ticket: Ticket, kind: Kind, grid: bool) {
        if !self.loads.is_pending(id, ticket, kind) {
            return;
        }
        if !self.ask_for_link(id, grid) {
            if kind == Kind::Anim {
                self.anim_loaded(id, ticket, Err(Fail::Expired));
            } else {
                self.thumb_loaded(id, ticket, Err(Fail::Expired));
            }
            return;
        }
        self.ask_ahead(id);
    }

    /// Fills the request for `id` with the GIFs after it in the view that will need a fresh link
    /// too, unless a full request is already on its way.
    pub(super) fn ask_ahead(&mut self, id: GifId) {
        let Some(at) = self.list.iter().position(|&g| g == id) else { return };
        for i in at + 1..(at + 1 + LOOK_AHEAD).min(self.list.len()) {
            if self.loads.renewing.len() >= discord::RENEW_MAX {
                break;
            }
            let next = self.list[i];
            let Some(g) = self.lib.gif(next) else { continue };
            if !self.loads.renewing.contains(&next) && self.media.needs_renewing(&g.url, g.media_link.as_deref()) {
                self.ask_for_link(next, true);
            }
        }
    }

    /// Queues the GIF's Discord link for renewal; false if it has none.
    pub(super) fn ask_for_link(&mut self, id: GifId, grid: bool) -> bool {
        let (Some(r), Some(g)) = (&self.renewer, self.lib.gif(id)) else { return false };
        let Some(link) = urls::discord_link(&g.url, g.media_link.as_deref()) else { return false };
        if self.loads.renewing.insert(id) {
            let _ = r.tx.send(Ask { id, link: link.to_string(), grid });
        }
        true
    }

    /// Saves the links Discord renewed and reloads their GIFs; the rest of `asked` failed.
    pub(super) fn renewed(
        &mut self,
        renewer: RenewerId,
        asked: Vec<GifId>,
        res: Result<Vec<(GifId, String)>, RenewError>,
    ) {
        if !self.is_current(renewer) {
            return;
        }
        let mut fresh = res.unwrap_or_else(|e| {
            log::warn(Area::Renew, format!("renewing {} links: {e}", asked.len()));
            if matches!(e, RenewError::Rejected) {
                self.renewer = None;
                self.lib.db().set_setting(Key::BotToken, "");
                self.loads.cancel_renewals();
            }
            self.form.renew_error = Some(e.to_string());
            Vec::new()
        });
        // Keep a newer link that was copied or imported while the request was running.
        fresh.retain(|(id, link)| {
            let old = self.lib.gif(*id).and_then(|g| urls::discord_link(&g.url, g.media_link.as_deref()));
            old.is_some_and(|old| urls::identity(old) == urls::identity(link) && urls::outlasts(link, old))
        });
        if let Err(e) = self.lib.set_media(&fresh) {
            log::error(Area::Db, format!("saving renewed links: {e}"));
            fresh.clear();
        }
        for (id, _) in &fresh {
            // Its dead marker may be from when only the link had expired.
            if let Some(g) = self.lib.gif(*id) {
                self.media.retry(&g.url);
            }
            self.loads.warmed.remove(id);
        }
        for id in asked {
            let renewed = self.lib.gif(id).is_some_and(|g| !self.media.needs_renewing(&g.url, g.media_link.as_deref()));
            self.loads.settle(id, renewed);
        }
    }

    /// Answers from a renewer that was since replaced are ignored.
    fn is_current(&self, renewer: RenewerId) -> bool {
        self.renewer.as_ref().is_some_and(|r| r.id == renewer)
    }

    /// Renewals dropped unsent: their GIFs ask again if they come back into view.
    pub(super) fn renewals_dropped(&mut self, renewer: RenewerId, ids: Vec<GifId>) {
        if !self.is_current(renewer) {
            return;
        }
        for id in ids {
            self.loads.renewals_dropped(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::loads::Slot,
        db::{Db, NewGif},
    };

    fn app() -> App {
        let db = Db::open(std::path::Path::new(":memory:")).unwrap();
        db.insert_many(vec![NewGif::link(
            "https://cdn.discordapp.com/attachments/1/2/cat.gif?ex=1".into(),
            String::new(),
            1,
        )])
        .unwrap();
        let mut app = App::for_test(db);
        let (tx, _) = unbounded();
        app.renewer = Some(Renewer { tx, id: RenewerId(7) });
        let id = app.lib.gifs()[0].id;
        app.loads.thumbs.insert(id, Slot::Pending(Ticket(0)));
        app.loads.renewing.insert(id);
        app
    }

    #[test]
    fn replaced_renewer_cannot_settle_or_change_current_requests() {
        let mut app = app();
        let id = app.lib.gifs()[0].id;
        app.renewals_dropped(RenewerId(6), vec![id]);
        app.renewed(RenewerId(6), vec![id], Err(RenewError::Rejected));
        app.renewed(RenewerId(6), vec![id], Ok(vec![(id, app.lib.gifs()[0].url.replace("ex=1", "ex=7fffffff"))]));
        assert!(app.loads.renewing.contains(&id));
        assert!(matches!(app.loads.thumbs.get(&id), Some(Slot::Pending(Ticket(0)))));
        assert!(app.form.renew_error.is_none() && app.renewer.is_some());
        assert!(
            app.lib.gifs()[0].media_link.is_none() && app.lib.db().sizes_and_media().unwrap()[0].media_link.is_none()
        );
    }

    #[test]
    fn removing_a_token_releases_every_waiting_load() {
        let mut app = app();
        let id = app.lib.gifs()[0].id;
        app.loads.warmed.insert(id);
        app.set_bot_token("");
        assert!(app.loads.renewing.is_empty());
        assert!(app.loads.wants_warming(id));
        assert!(!app.loads.thumbs.contains_key(&id));
    }

    #[test]
    fn failed_link_save_does_not_change_memory() {
        let mut app = app();
        let id = app.lib.gifs()[0].id;
        app.lib.db().execute_test_batch(
            "CREATE TRIGGER fail_media BEFORE UPDATE OF media ON gifs
            BEGIN SELECT RAISE(ABORT, 'test failure'); END;",
        );
        app.renewed(RenewerId(7), vec![id], Ok(vec![(id, app.lib.gifs()[0].url.replace("ex=1", "ex=7fffffff"))]));
        assert!(
            app.lib.gifs()[0].media_link.is_none() && app.lib.db().sizes_and_media().unwrap()[0].media_link.is_none()
        );
        assert_eq!(app.loads.failure(id), Some(&Fail::Expired));
    }

    #[test]
    fn an_older_renewal_keeps_a_newer_link_and_retries_the_waiting_load() {
        let mut app = app();
        let id = app.lib.gifs()[0].id;
        let fresh = app.lib.gifs()[0].url.replace("ex=1", "ex=7fffffff");
        app.lib.set_media(&[(id, fresh.clone())]).unwrap();
        app.renewed(RenewerId(7), vec![id], Ok(vec![(id, fresh.replace("7fffffff", "2"))]));
        assert_eq!(app.lib.gifs()[0].media_link.as_deref(), Some(fresh.as_str()));
        assert!(!app.loads.thumbs.contains_key(&id));
        assert!(app.loads.renewing.is_empty());
    }
}
