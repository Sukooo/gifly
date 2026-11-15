//! Changes the user makes to the library, on top of [`crate::library`]: each tells the user how it
//! went, and those a click can make by mistake show a toast that undoes them.

use std::{collections::HashSet, path::PathBuf};

use super::Msg;

use crate::{
    clock,
    db::{Collection, CollectionId, Gif, GifId, NewGif},
    log::{self, Area},
    urls,
};

use super::{App, Undo, View, files};

/// "1 GIF", "3 GIFs".
pub fn gif_count(n: usize) -> String {
    if n == 1 { "1 GIF".into() } else { format!("{n} GIFs") }
}

impl App {
    /// Logs a failed change and tells the user it didn't happen.
    fn report(&mut self, what: &str, e: &rusqlite::Error) {
        log::error(Area::Db, format!("{what}: {e}"));
        self.toast(format!("Couldn't {what}."));
    }

    /// A GIF link was copied somewhere: save it if new and put it at the top of Copied.
    pub(super) fn captured(&mut self, url: String) {
        if let Some(gi) = self.lib.record_copy(url) {
            self.retry_load(gi);
        }
        self.dirty = true;
    }

    pub(super) fn add_links(&mut self, links: Vec<String>) {
        let now = clock::now();
        let items = links
            .into_iter()
            .map(|url| {
                let title = urls::title_from_url(&url);
                NewGif::link(url, title, now)
            })
            .collect();
        self.add_new(items, 0, self.view);
    }

    /// Picture files pasted or dropped into the picker, copied into gifly's folder in the
    /// background. They're added to `view`, the screen they were dropped on.
    pub(super) fn add_files(&mut self, paths: Vec<PathBuf>) {
        let (tx, ctx, dir, view) = (self.tx.clone(), self.ctx.clone(), self.files.clone(), self.view);
        crate::spawn("files", move || {
            let (items, skipped) = files::store_all(&dir, paths);
            let _ = tx.send(Msg::FilesStored { items, skipped, view });
            ctx.request_repaint();
        });
    }

    pub(super) fn files_stored(&mut self, items: Vec<NewGif>, skipped: usize, view: View) {
        if items.is_empty() {
            return self.toast("gifly takes GIF, PNG, JPEG and WebP files.");
        }
        self.add_new(items, skipped, view);
    }

    /// Adds GIFs from a paste or drop, and puts them in `view` if it's a collection. `skipped`
    /// files weren't pictures.
    fn add_new(&mut self, items: Vec<NewGif>, skipped: usize, view: View) {
        let links: Vec<String> = items.iter().map(|n| n.url.clone()).collect();
        let added = match self.lib.add(items) {
            Ok(a) => a,
            Err(e) => return self.report("add the GIFs", &e),
        };
        self.dirty = true;
        // Every GIF that was pasted, new or not, each once (two links can be one file).
        let mut all: Vec<GifId> = links.iter().filter_map(|url| self.lib.find(url)).collect();
        let mut seen = HashSet::new();
        all.retain(|&id| seen.insert(id));
        let mut undo = Vec::new();
        if !added.is_empty() {
            undo.push(Undo::Added(added.clone()));
        }
        let mut msg = match view {
            View::Collection(col) => {
                let Some(filed) = self.apply_membership(&all, col, true) else {
                    self.toast_undo("GIFs saved, but couldn't add them to the collection.", undo);
                    return;
                };
                let name = self.view_name(view);
                if filed.is_empty() {
                    format!("Already in {name}")
                } else {
                    let n = filed.len();
                    undo.push(Undo::Filed { gifs: filed, col, on: true });
                    format!("Added {} to {name}", gif_count(n))
                }
            }
            _ if added.is_empty() => "Already in your library".into(),
            _ => format!("Added {}", gif_count(added.len())),
        };
        if skipped > 0 {
            msg += &format!(" ({skipped} skipped, not pictures)");
        }
        self.toast_undo(msg, undo);
    }

    pub(super) fn delete_gifs(&mut self, ids: &[GifId]) {
        let Some(gone) = self.remove_gifs(ids) else { return };
        if !gone.is_empty() {
            self.toast_undo(format!("Deleted {}", gif_count(gone.len())), vec![Undo::Deleted(gone)]);
        }
    }

    /// `Library::remove`, or None if it couldn't be saved. Their cached files stay (pruning gets to them), so undoing shows them right away.
    fn remove_gifs(&mut self, ids: &[GifId]) -> Option<Vec<Gif>> {
        let gone = match self.lib.remove(ids) {
            Ok(gone) => gone,
            Err(e) => {
                self.report("delete the GIFs", &e);
                return None;
            }
        };
        for g in &gone {
            self.loads.forget(g.id);
        }
        self.picked.retain(|p| !gone.iter().any(|g| g.id == *p));
        self.dirty = true;
        Some(gone)
    }

    /// Puts GIFs in a collection or takes them out, with an undo for the ones that changed.
    pub(super) fn set_membership(&mut self, gifs: &[GifId], col: CollectionId, on: bool) {
        let Some(changed) = self.apply_membership(gifs, col, on) else { return };
        let name = self.view_name(View::Collection(col));
        let msg = match (changed.len(), on) {
            (0, true) => format!("Already in {name}"),
            (0, false) => return,
            (1, true) => format!("Added to {name}"),
            (1, false) => format!("Removed from {name}"),
            (n, true) => format!("Added {} to {name}", gif_count(n)),
            (n, false) => format!("Removed {} from {name}", gif_count(n)),
        };
        let undo = if changed.is_empty() { Vec::new() } else { vec![Undo::Filed { gifs: changed, col, on }] };
        self.toast_undo(msg, undo);
    }

    /// `Library::set_membership`, or None if it couldn't be saved.
    fn apply_membership(&mut self, gifs: &[GifId], col: CollectionId, on: bool) -> Option<Vec<GifId>> {
        match self.lib.set_membership(gifs, col, on) {
            Ok(changed) => {
                self.dirty |= !changed.is_empty();
                Some(changed)
            }
            Err(e) => {
                self.report("update the collection", &e);
                None
            }
        }
    }

    /// Reverses the change the toast is showing.
    pub(super) fn undo(&mut self) {
        let Some(mut t) = self.toast.take() else { return };
        while let Some(u) = t.undo.pop() {
            let retry = match u {
                Undo::Deleted(gifs) => {
                    let failed = self.lib.restore(gifs);
                    self.dirty = true;
                    (!failed.is_empty()).then_some(Undo::Deleted(failed))
                }
                Undo::Added(ids) => self.remove_gifs(&ids).is_none().then_some(Undo::Added(ids)),
                Undo::Filed { gifs, col, on } => {
                    self.apply_membership(&gifs, col, !on).is_none().then_some(Undo::Filed { gifs, col, on })
                }
                Undo::Collection { col, members, at } => {
                    (!self.restore_collection(&col, &members, at)).then_some(Undo::Collection { col, members, at })
                }
            };
            if let Some(u) = retry {
                // Keep both the failed work and earlier steps; successful merges must not run twice.
                t.undo.push(u);
                self.toast_undo("Couldn't undo. Try again.", t.undo);
                return;
            }
        }
    }

    /// Creates a collection, or reuses one with the same name, and adds `gifs` to it.
    pub(super) fn new_collection(&mut self, name: &str, gifs: &[GifId]) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        let id = match self.lib.create_collection(name) {
            Ok(id) => id,
            Err(e) => return self.report("create the collection", &e),
        };
        self.editing.creating = false;
        self.editing.new_name.clear();
        if !gifs.is_empty() {
            self.set_membership(gifs, id, true);
        }
        self.dirty = true;
    }

    /// Renames a collection. A name another collection has is refused with a toast.
    pub(super) fn rename_collection(&mut self, id: CollectionId, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        if self.lib.collection_named(name, Some(id)).is_some() {
            return self.toast(format!("There's already a collection called \"{name}\"."));
        }
        if let Err(e) = self.lib.rename_collection(id, name) {
            return self.report("rename the collection", &e);
        }
        self.editing.renaming = None;
        self.dirty = true;
    }

    /// Deletes a collection, with an undo. Its GIFs stay in the library.
    pub(super) fn delete_collection(&mut self, id: CollectionId) {
        let at = self.order.iter().position(|&v| v == View::Collection(id)).unwrap_or(self.order.len());
        let (col, members) = match self.lib.delete_collection(id) {
            Ok(Some(deleted)) => deleted,
            Ok(None) => return,
            Err(e) => return self.report("delete the collection", &e),
        };
        if self.view == View::Collection(id) {
            self.reset_view(View::Home);
        }
        self.toast_undo(format!("Deleted {}", col.name), vec![Undo::Collection { col, members, at }]);
        self.dirty = true;
    }

    /// Undoes a collection delete, putting it back at its place on Home.
    fn restore_collection(&mut self, c: &Collection, members: &[GifId], at: usize) -> bool {
        let restored = match self.lib.restore_collection(c, members) {
            Ok(restored) => restored,
            Err(e) => {
                self.report("undo", &e);
                return false;
            }
        };
        if let Some(id) = restored {
            let v = View::Collection(id);
            self.order.retain(|&o| o != v);
            self.order.insert(at.min(self.order.len()), v);
        }
        self.save_order();
        self.dirty = true;
        true
    }

    pub(super) fn hide_collection(&mut self, id: CollectionId, hidden: bool) {
        if let Err(e) = self.lib.set_hidden(id, hidden) {
            return self.report(if hidden { "hide the collection" } else { "unhide the collection" }, &e);
        }
        if hidden && !self.reveal {
            self.toast("Collection hidden. Ctrl+H shows hidden ones.");
        }
        self.dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    fn app() -> App {
        let db = Db::open(std::path::Path::new(":memory:")).unwrap();
        db.insert_many(vec![NewGif::link("a".into(), "cat".into(), 1), NewGif::link("b".into(), "dog".into(), 1)])
            .unwrap();
        App::for_test(db)
    }

    #[test]
    fn failed_undo_keeps_only_unfinished_restores_and_does_not_merge_twice() {
        let mut app = app();
        let ids: Vec<GifId> = app.lib.gifs().iter().map(|g| g.id).collect();
        app.lib.mark_used(ids[0], 10).unwrap();
        app.delete_gifs(&ids);
        // A capture brings the first GIF back while the delete's Undo is still showing.
        app.captured("a".into());
        app.lib.db().execute_test_batch(
            "CREATE TRIGGER fail_restore BEFORE INSERT ON gifs
            WHEN NEW.url = 'b' BEGIN SELECT RAISE(ABORT, 'test failure'); END;",
        );
        app.undo();
        assert_eq!(app.lib.gifs().len(), 1);
        assert_eq!(app.lib.gifs()[0].use_count, 1);
        assert!(matches!(&app.toast.as_ref().unwrap().undo[..], [Undo::Deleted(g)] if g.len() == 1 && g[0].url == "b"));
        app.lib.db().execute_test_batch("DROP TRIGGER fail_restore;");
        app.undo();
        assert_eq!(app.lib.gifs().len(), 2);
        assert_eq!(app.lib.gifs()[0].use_count, 1, "the successful merge must not be repeated");
        assert_eq!(app.lib.db().load().unwrap().0.len(), 2);
    }

    #[test]
    fn pasting_files_twice_and_saved_ones_files_each_once() {
        let mut app = app();
        app.new_collection("cats", &[]);
        let col = app.lib.collections()[0].id;
        app.view = View::Collection(col);
        let cdn = "https://cdn.discordapp.com/attachments/1/2/c.gif?ex=1&is=2&hm=3";
        let media = "https://media.discordapp.net/attachments/1/2/c.gif";
        app.add_links(vec!["a".into(), cdn.into(), media.into()]);
        assert_eq!(app.lib.gifs().len(), 3, "the two Discord links are one file");
        assert!(app.lib.gifs().iter().filter(|g| g.url != "b").all(|g| g.cols == [col]));
        let toast = app.toast.as_ref().unwrap();
        assert_eq!(toast.msg, "Added 2 GIFs to cats");
        assert!(
            matches!(&toast.undo[..], [Undo::Added(a), Undo::Filed { gifs, .. }] if a.len() == 1 && gifs.len() == 2)
        );
    }

    #[test]
    fn failed_filing_undo_does_not_delete_the_added_gifs() {
        let mut app = app();
        app.new_collection("cats", &[]);
        let col = app.lib.collections()[0].id;
        app.view = View::Collection(col);
        app.add_links(vec!["c".into()]);
        let id = app.lib.gifs().last().unwrap().id;
        app.lib.db().execute_test_batch(
            "CREATE TRIGGER fail_unfile BEFORE DELETE ON gif_collections
            BEGIN SELECT RAISE(ABORT, 'test failure'); END;",
        );
        app.undo();
        assert!(app.lib.gif(id).is_some());
        assert_eq!(app.toast.as_ref().unwrap().undo.len(), 2);
        app.lib.db().execute_test_batch("DROP TRIGGER fail_unfile;");
        app.undo();
        assert!(app.lib.gif(id).is_none());
        assert_eq!(app.lib.db().load().unwrap().0.len(), 2);
    }

    #[test]
    fn undo_merges_members_into_a_collection_recreated_with_the_same_name() {
        let mut app = app();
        let gif = app.lib.gifs()[0].id;
        app.new_collection("cats", &[gif]);
        let old = app.lib.collections()[0].id;
        app.delete_collection(old);
        app.new_collection("cats", &[]);
        let new = app.lib.collections()[0].id;
        assert_ne!(old, new);
        app.undo();
        assert_eq!(app.lib.collections().len(), 1);
        assert_eq!(app.lib.gifs()[0].cols, [new]);
        assert_eq!(app.lib.db().load().unwrap().0[0].cols, [new]);
        assert!(app.lib.gifs()[0].search_text.contains("cats"));
    }

    #[test]
    fn failed_filing_reports_the_partial_add_and_keeps_its_undo() {
        let mut app = app();
        app.new_collection("cats", &[]);
        app.view = View::Collection(app.lib.collections()[0].id);
        app.lib.db().execute_test_batch(
            "CREATE TRIGGER fail_file BEFORE INSERT ON gif_collections
            BEGIN SELECT RAISE(ABORT, 'test failure'); END;",
        );
        app.add_links(vec!["c".into()]);
        let toast = app.toast.as_ref().unwrap();
        assert!(toast.msg.contains("couldn't add"));
        assert!(matches!(&toast.undo[..], [Undo::Added(ids)] if ids.len() == 1));
        app.undo();
        assert_eq!(app.lib.gifs().len(), 2);
    }
}
