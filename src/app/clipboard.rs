//! Everything that touches the clipboard: pasting a GIF, copying its link, and watching for
//! GIF links copied elsewhere.

use std::{
    sync::{Arc, atomic::Ordering::Relaxed},
    thread,
    time::Duration,
};

use clipboard_master::{CallbackResult, ClipboardHandler, Master};
use crossbeam_channel::Sender;
use eframe::egui;

use crate::{
    clock,
    db::{self, GifId},
    log::{self, Area},
    platform, urls,
    util::retry,
};

use super::{App, Msg, Shared, files};

const CLIPBOARD_BUSY: &str = "Couldn't use the clipboard. Try again.";
/// How long the pasted link stays on the clipboard before the old contents come back. The
/// target app reads the clipboard some time after Ctrl+V arrives; restoring sooner can paste the
/// old contents.
const RESTORE_AFTER: Duration = Duration::from_millis(500);

impl App {
    /// Puts the link on the clipboard, hides, pastes into the previous window, then restores the clipboard.
    pub(super) fn send(&mut self, id: GifId, invert: bool) {
        let Some(g) = self.lib.gif(id) else { return };
        let url = clipboard_url(g).to_string();
        let owner = self.shared.hwnd.get();
        // Everything copied before (images and files too), to put back after pasting.
        let old = platform::ClipboardSnapshot::take(owner);
        // Pasting with the old clipboard still in place would send that instead, so stop here.
        if let Err(e) = self.put_on_clipboard(&url) {
            log::warn(Area::Clipboard, format!("setting the link to paste: {e}"));
            return self.toast(CLIPBOARD_BUSY);
        }
        let seq = platform::clipboard_seq();

        self.record_send(id, clock::now());

        let target = self.shared.paste_target.get();
        let enter = self.prefs.auto_send ^ invert;
        let shared = self.shared.clone();
        self.hide();
        crate::spawn("paste", move || {
            // If the paste didn't go through, the link stays on the clipboard to paste by hand.
            if !platform::paste_into(target, enter) {
                return;
            }
            let Some(old) = old else { return };
            thread::sleep(RESTORE_AFTER);
            // Leave it alone if something else was copied in the meantime.
            if platform::clipboard_seq() != seq {
                log::debug(Area::Clipboard, || "not restored: something else was copied".into());
                return;
            }
            let text = old.text();
            if let Some(t) = &text {
                shared.note_self_write(t);
            }
            if !old.restore(owner, seq)
                && let Some(t) = text
            {
                shared.take_self_write(&t);
            }
        });
    }

    /// Mirrors a send's statistics only once they have been saved.
    fn record_send(&mut self, id: GifId, now: i64) {
        if let Err(e) = self.lib.mark_used(id, now) {
            log::error(Area::Db, format!("saving a send: {e}"));
            return;
        }
        self.dirty = true;
    }

    pub(super) fn copy_link(&mut self, id: GifId) {
        let Some(g) = self.lib.gif(id) else { return };
        let url = clipboard_url(g).to_string();
        match self.put_on_clipboard(&url) {
            Ok(()) => self.toast(if urls::local_name(&url).is_some() { "File copied" } else { "Link copied" }),
            Err(e) => {
                log::warn(Area::Clipboard, format!("copying a link: {e}"));
                self.toast(CLIPBOARD_BUSY);
            }
        }
    }

    pub(super) fn copy_diagnostics(&mut self) {
        match arboard::Clipboard::new().and_then(|mut c| c.set_text(self.diagnostics())) {
            Ok(()) => self.toast("Diagnostics copied"),
            Err(e) => {
                log::warn(Area::Clipboard, format!("copying diagnostics: {e}"));
                self.toast(CLIPBOARD_BUSY);
            }
        }
    }

    /// Puts a GIF on the clipboard: its link, or the file itself for one added from the PC, which
    /// Discord pastes as an attachment.
    fn put_on_clipboard(&self, url: &str) -> Result<(), arboard::Error> {
        let mut c = arboard::Clipboard::new()?;
        match files::path(&self.files, url) {
            Some(path) => c.set().file_list(&[path]),
            None => {
                self.shared.note_self_write(url);
                c.set_text(url).inspect_err(|_| {
                    self.shared.take_self_write(url);
                })
            }
        }
    }
}

/// A renewed Discord attachment link, if it lasts longer; other GIFs keep their original link.
fn clipboard_url(g: &db::Gif) -> &str {
    urls::discord_link(&g.url, g.media_link.as_deref())
        .filter(|link| urls::identity(link) == urls::identity(&g.url))
        .unwrap_or(&g.url)
}

struct Watcher {
    shared: Arc<Shared>,
    tx: Sender<Msg>,
    ctx: egui::Context,
}

impl ClipboardHandler for Watcher {
    fn on_clipboard_change(&mut self) -> CallbackResult {
        if !self.shared.capture.load(Relaxed) {
            return CallbackResult::Next;
        }
        // The writer may still hold the clipboard open for a moment.
        let text = retry(3, Duration::from_millis(25), || arboard::Clipboard::new().ok()?.get_text().ok());
        if let Some(text) = text {
            let t = text.trim();
            if !self.shared.take_self_write(t) && urls::is_gif_url(t) {
                log::debug(Area::Clipboard, || format!("captured {t}"));
                let _ = self.tx.send(Msg::Captured(t.to_string()));
                self.ctx.request_repaint();
            }
        }
        CallbackResult::Next
    }
}

/// Watches the clipboard on its own thread and reports copied GIF links as `Msg::Captured`.
pub(super) fn spawn_watcher(shared: Arc<Shared>, tx: Sender<Msg>, ctx: egui::Context) {
    crate::spawn("clipboard", move || {
        if let Ok(mut m) = Master::new(Watcher { shared, tx, ctx }) {
            let _ = m.run();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_send_write_keeps_memory_and_database_in_agreement() {
        let db = db::Db::open(std::path::Path::new(":memory:")).unwrap();
        let id = db.insert_many(vec![db::NewGif::link("a".into(), String::new(), 1)]).unwrap()[0].id;
        let mut app = App::for_test(db);
        app.lib.db().execute_test_batch(
            "CREATE TRIGGER fail_use BEFORE UPDATE OF use_count ON gifs
            BEGIN SELECT RAISE(ABORT, 'test failure'); END;",
        );
        app.record_send(id, 10);
        assert_eq!((app.lib.gifs()[0].use_count, app.lib.gifs()[0].last_used), (0, 0));
        assert_eq!(app.lib.db().load().unwrap().0[0].use_count, 0);
        app.lib.db().execute_test_batch("DROP TRIGGER fail_use;");
        app.record_send(id, 20);
        assert_eq!((app.lib.gifs()[0].use_count, app.lib.gifs()[0].last_used), (1, 20));
        assert_eq!(app.lib.db().load().unwrap().0[0].use_count, 1);
    }

    #[test]
    fn clipboard_uses_the_freshest_link_to_the_same_discord_attachment() {
        let original = "https://cdn.discordapp.com/attachments/1/2/cat.gif?ex=100&is=1&hm=a";
        let renewed = "https://media.discordapp.net/attachments/1/2/cat.gif?ex=200&is=2&hm=b";
        let mut g = db::Gif::from_new(GifId(1), db::NewGif::link(original.into(), String::new(), 0));
        g.media_link = Some(renewed.into());
        assert_eq!(clipboard_url(&g), renewed);
        g.media_link = Some(original.replace("ex=100", "ex=50"));
        assert_eq!(clipboard_url(&g), original);
        g.media_link = Some(renewed.replace("/1/2/", "/1/3/"));
        assert_eq!(clipboard_url(&g), original, "another attachment isn't the GIF's link");
        g.url = "https://tenor.com/view/cat-gif-1".into();
        assert_eq!(clipboard_url(&g), g.url, "a preview doesn't replace a canonical page link");
        g.url = "local:1234.gif".into();
        assert_eq!(clipboard_url(&g), g.url);
    }
}
