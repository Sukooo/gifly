//! The library in memory: every GIF and collection, mirrored from [`crate::db`] with the lookups
//! the picker needs. Each change is saved to the database first and only mirrored if that worked,
//! so what's on screen never shows something that wasn't saved.

use std::collections::{HashMap, HashSet};

use rusqlite::Result;

use crate::{
    clock,
    db::{Collection, CollectionId, Db, Gif, GifId, NewGif, SizeAndMedia},
    log::{self, Area},
    urls,
};

pub struct Library {
    db: Db,
    gifs: Vec<Gif>,
    /// Each GIF's place in `gifs`.
    by_id: HashMap<GifId, usize>,
    /// Each file's GIF, by `urls::identity` of its link, so a copied link is matched in one lookup.
    by_identity: HashMap<String, GifId>,
    /// In the order menus list them, which follows the Home order.
    collections: Vec<Collection>,
}

impl Library {
    /// Loads everything and builds the lookups and every GIF's search text.
    pub fn load(db: Db) -> Result<Self> {
        let (gifs, collections) = db.load()?;
        let mut lib = Self { db, gifs, by_id: HashMap::new(), by_identity: HashMap::new(), collections };
        lib.reindex_ids();
        for g in &lib.gifs {
            // The first of several links to the same file wins, as a scan would find it.
            lib.by_identity.entry(urls::identity(&g.url)).or_insert(g.id);
        }
        for i in 0..lib.gifs.len() {
            lib.update_search_text(i);
        }
        Ok(lib)
    }

    /// The database, for settings and anything else that isn't mirrored here.
    pub fn db(&self) -> &Db {
        &self.db
    }

    pub fn gifs(&self) -> &[Gif] {
        &self.gifs
    }

    /// Where the GIF with this id is in [`Self::gifs`].
    pub fn index(&self, id: GifId) -> Option<usize> {
        self.by_id.get(&id).copied()
    }

    pub fn gif(&self, id: GifId) -> Option<&Gif> {
        self.index(id).map(|i| &self.gifs[i])
    }

    /// The GIF saved for this link's file, whichever of its links was saved.
    pub fn find(&self, url: &str) -> Option<GifId> {
        self.by_identity.get(&urls::identity(url)).copied()
    }

    pub fn collections(&self) -> &[Collection] {
        &self.collections
    }

    pub fn collection(&self, id: CollectionId) -> Option<&Collection> {
        self.collections.iter().find(|c| c.id == id)
    }

    fn collection_mut(&mut self, id: CollectionId) -> Option<&mut Collection> {
        self.collections.iter_mut().find(|c| c.id == id)
    }

    /// The collection named `name`, ignoring case, other than `except`.
    pub fn collection_named(&self, name: &str, except: Option<CollectionId>) -> Option<&Collection> {
        self.collections.iter().find(|c| Some(c.id) != except && c.name.eq_ignore_ascii_case(name))
    }

    /// Rebuilds `by_id` after GIFs moved in `gifs`.
    fn reindex_ids(&mut self) {
        self.by_id = self.gifs.iter().enumerate().map(|(i, g)| (g.id, i)).collect();
    }

    /// Rebuilds one GIF's search text: its title, tags and collection names, lowercased.
    fn update_search_text(&mut self, i: usize) {
        let g = &self.gifs[i];
        let mut text = format!("{} {}", g.title, g.tags);
        for c in self.collections.iter().filter(|c| g.cols.contains(&c.id)) {
            text.push(' ');
            text.push_str(&c.name);
        }
        self.gifs[i].search_text = text.to_lowercase();
    }

    /// Rebuilds the search text of the GIFs in collection `col`.
    fn update_search_text_in(&mut self, col: CollectionId) {
        for i in 0..self.gifs.len() {
            if self.gifs[i].cols.contains(&col) {
                self.update_search_text(i);
            }
        }
    }

    /// Mirrors a GIF just saved to the database.
    fn push(&mut self, g: Gif) {
        let i = self.gifs.len();
        self.by_id.insert(g.id, i);
        self.by_identity.entry(urls::identity(&g.url)).or_insert(g.id);
        self.gifs.push(g);
        self.update_search_text(i);
    }

    /// Saves new GIFs and returns their ids. Each file is saved once: another link to a saved file
    /// (a Discord attachment on the other host, or with fresh `ex`/`hm` params) is treated as the
    /// saved link, whose size and media link it may fill in (see [`Db::insert_many`]).
    pub fn add(&mut self, mut items: Vec<NewGif>) -> Result<Vec<GifId>> {
        let mut seen = HashSet::new();
        items.retain(|n| seen.insert(urls::identity(&n.url)));
        for n in &mut items {
            if let Some(g) = self.find(&n.url).and_then(|id| self.gif(id)) {
                n.url.clone_from(&g.url);
            }
        }
        let added = self.db.insert_many(items)?;
        let ids = added.iter().map(|g| g.id).collect();
        for g in added {
            self.push(g);
        }
        Ok(ids)
    }

    /// A link was copied: saves it if it's new and marks it copied now. Returns where its GIF is
    /// if its link changed, as a fresh copy of an expiring Discord link does.
    pub fn record_copy(&mut self, url: String) -> Option<usize> {
        if self.find(&url).is_none() {
            let title = urls::title_from_url(&url);
            if let Err(e) = self.add(vec![NewGif::link(url.clone(), title, clock::now())]) {
                log::error(Area::Db, format!("saving a copied link: {e}"));
            }
        }
        let gi = self.find(&url).and_then(|id| self.index(id))?;
        let g = &mut self.gifs[gi];
        let now = clock::now_ms();
        match self.db.mark_copied(g.id, now) {
            Ok(()) => g.copied_at_ms = now,
            Err(e) => log::error(Area::Db, format!("saving when a link was copied: {e}")),
        }
        if g.url == url || self.db.set_url(g.id, &url).is_err() {
            return None;
        }
        g.url = url;
        Some(gi)
    }

    /// Counts a send.
    pub fn mark_used(&mut self, id: GifId, now: i64) -> Result<()> {
        let Some(i) = self.index(id) else { return Ok(()) };
        self.db.mark_used(id, now)?;
        let g = &mut self.gifs[i];
        g.use_count += 1;
        g.last_used = now;
        Ok(())
    }

    /// Notes a GIF's size, learned from its first thumbnail. Kept even if saving it fails: the
    /// grid needs it to lay the tile out, and the next launch learns it again.
    pub fn set_size(&mut self, id: GifId, w: u16, h: u16) {
        let Some(i) = self.index(id) else { return };
        if let Err(e) = self.db.set_size(id, w, h) {
            log::warn(Area::Db, format!("saving a GIF's size: {e}"));
        }
        (self.gifs[i].w, self.gifs[i].h) = (w, h);
    }

    /// Saves renewed media links, `(id, link)`.
    pub fn set_media(&mut self, links: &[(GifId, String)]) -> Result<()> {
        self.db.set_media(links)?;
        for (id, link) in links {
            if let Some(i) = self.index(*id) {
                self.gifs[i].media_link = Some(link.clone());
            }
        }
        Ok(())
    }

    /// Picks up the sizes, media links and import flags an import saved to GIFs that were
    /// already in the library. Returns the GIFs whose media link changed.
    pub fn reload_imported(&mut self) -> Result<Vec<usize>> {
        let mut renewed = Vec::new();
        for SizeAndMedia { id, w, h, media_link, imported } in self.db.sizes_and_media()? {
            let Some(i) = self.index(id) else { continue };
            let g = &mut self.gifs[i];
            (g.w, g.h, g.imported) = (w, h, imported);
            if g.media_link != media_link {
                g.media_link = media_link;
                renewed.push(i);
            }
        }
        Ok(renewed)
    }

    /// Deletes GIFs and returns them as they were, for an undo.
    pub fn remove(&mut self, ids: &[GifId]) -> Result<Vec<Gif>> {
        let ids: HashSet<GifId> = ids.iter().copied().filter(|id| self.by_id.contains_key(id)).collect();
        self.db.delete_gifs(&ids.iter().copied().collect::<Vec<_>>())?;
        let (gone, kept): (Vec<Gif>, Vec<Gif>) =
            std::mem::take(&mut self.gifs).into_iter().partition(|g| ids.contains(&g.id));
        self.gifs = kept;
        self.reindex_ids();
        let mut lost = HashSet::new();
        for gif in &gone {
            let key = urls::identity(&gif.url);
            if self.by_identity.get(&key) == Some(&gif.id) {
                self.by_identity.remove(&key);
                lost.insert(key);
            }
        }
        // Another link to the same file takes over, if there is one.
        if !lost.is_empty() {
            for g in &self.gifs {
                let key = urls::identity(&g.url);
                if lost.contains(&key) {
                    self.by_identity.entry(key).or_insert(g.id);
                }
            }
        }
        Ok(gone)
    }

    /// Puts deleted GIFs back and returns any that couldn't be saved, for another try.
    pub fn restore(&mut self, mut gifs: Vec<Gif>) -> Vec<Gif> {
        // Collections deleted since don't come back with them.
        for g in &mut gifs {
            g.cols.retain(|&c| self.collection(c).is_some());
        }
        // A file copied again since is already back under a new id: that one gets their
        // collections and history instead.
        let (back, gifs): (Vec<Gif>, Vec<Gif>) = gifs.into_iter().partition(|g| self.find(&g.url).is_some());
        let mut failed = Vec::new();
        for old in back {
            let Some(gi) = self.find(&old.url).and_then(|id| self.index(id)) else {
                failed.push(old);
                continue;
            };
            if let Err(e) = self.db.merge_gif(self.gifs[gi].id, &old) {
                log::error(Area::Db, format!("undoing a delete: {e}"));
                failed.push(old);
                continue;
            }
            let g = &mut self.gifs[gi];
            g.use_count += old.use_count;
            g.last_used = g.last_used.max(old.last_used);
            g.added_at = g.added_at.min(old.added_at);
            g.copied_at_ms = g.copied_at_ms.max(old.copied_at_ms);
            g.imported |= old.imported;
            g.saved |= old.saved;
            for c in old.cols {
                if !g.cols.contains(&c) {
                    g.cols.push(c);
                }
            }
            self.update_search_text(gi);
        }
        if let Err(e) = self.db.restore_gifs(&gifs) {
            log::error(Area::Db, format!("undoing a delete: {e}"));
            failed.extend(gifs);
            return failed;
        }
        for g in gifs {
            self.push(g);
        }
        failed
    }

    /// Puts the GIFs that aren't in `col` in it (or takes those that are out) and returns them.
    pub fn set_membership(&mut self, gifs: &[GifId], col: CollectionId, on: bool) -> Result<Vec<GifId>> {
        let changed: Vec<GifId> =
            gifs.iter().copied().filter(|&id| self.gif(id).is_some_and(|g| g.cols.contains(&col) != on)).collect();
        if changed.is_empty() {
            return Ok(changed);
        }
        self.db.set_membership(&changed, col, on)?;
        for &gif in &changed {
            let Some(i) = self.index(gif) else { continue };
            let g = &mut self.gifs[i];
            g.cols.retain(|&c| c != col);
            if on {
                g.cols.push(col);
            }
            self.update_search_text(i);
        }
        Ok(changed)
    }

    /// The collection called `name` (ignoring case), created if there isn't one.
    pub fn create_collection(&mut self, name: &str) -> Result<CollectionId> {
        if let Some(c) = self.collection_named(name, None) {
            return Ok(c.id);
        }
        let id = self.db.create_collection(name)?;
        self.collections.push(Collection { id, name: name.to_string(), hidden: false });
        Ok(id)
    }

    /// Renames a collection. The caller checks no other one has the name.
    pub fn rename_collection(&mut self, id: CollectionId, name: &str) -> Result<()> {
        self.db.rename_collection(id, name)?;
        if let Some(c) = self.collection_mut(id) {
            c.name = name.to_string();
        }
        self.update_search_text_in(id);
        Ok(())
    }

    pub fn set_hidden(&mut self, id: CollectionId, hidden: bool) -> Result<()> {
        self.db.set_hidden(id, hidden)?;
        if let Some(c) = self.collection_mut(id) {
            c.hidden = hidden;
        }
        Ok(())
    }

    /// Deletes a collection and returns it with its GIFs, for an undo. Its GIFs stay.
    pub fn delete_collection(&mut self, id: CollectionId) -> Result<Option<(Collection, Vec<GifId>)>> {
        let Some(col) = self.collection(id).cloned() else { return Ok(None) };
        let members = self.gifs.iter().filter(|g| g.cols.contains(&id)).map(|g| g.id).collect();
        self.db.delete_collection(id)?;
        self.collections.retain(|c| c.id != id);
        for i in 0..self.gifs.len() {
            if self.gifs[i].cols.contains(&id) {
                self.gifs[i].cols.retain(|&c| c != id);
                self.update_search_text(i);
            }
        }
        Ok(Some((col, members)))
    }

    /// Undoes a collection delete. Its members go into a collection recreated with the same name
    /// since, if there is one. Returns the collection's id if it came back as itself.
    pub fn restore_collection(&mut self, c: &Collection, members: &[GifId]) -> Result<Option<CollectionId>> {
        // Names are unique ignoring case, which the database's own check doesn't know: a
        // collection made since under the same name in other case takes the GIFs back instead.
        let existing = self.collection_named(&c.name, None).map(|e| Collection { name: e.name.clone(), ..c.clone() });
        let id = self.db.restore_collection(existing.as_ref().unwrap_or(c), members)?;
        let new = self.collection(id).is_none();
        if new {
            self.collections.push(c.clone());
        }
        for &gif in members {
            if let Some(i) = self.index(gif)
                && !self.gifs[i].cols.contains(&id)
            {
                self.gifs[i].cols.push(id);
            }
        }
        self.update_search_text_in(id);
        Ok(new.then_some(id))
    }

    /// Puts the collections in this order and saves it. Any not listed go first.
    pub fn set_order(&mut self, ids: &[CollectionId]) {
        let mut sorted = self.collections.clone();
        sorted.sort_by_key(|c| ids.iter().position(|&id| id == c.id));
        let ids: Vec<CollectionId> = sorted.iter().map(|c| c.id).collect();
        match self.db.set_order(&ids) {
            Ok(()) => self.collections = sorted,
            Err(e) => log::error(Area::Db, format!("saving collection order: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lib() -> Library {
        let db = Db::open(std::path::Path::new(":memory:")).unwrap();
        db.insert_many(vec![NewGif::link("a".into(), "cat".into(), 1), NewGif::link("b".into(), "dog".into(), 1)])
            .unwrap();
        Library::load(db).unwrap()
    }

    #[test]
    fn another_link_to_a_deleted_file_takes_over_its_lookup() {
        // Libraries saved before `add` matched files by identity can hold two links to one file.
        let cdn = "https://cdn.discordapp.com/attachments/1/2/x.gif?ex=1";
        let media = "https://media.discordapp.net/attachments/1/2/x.gif?ex=2";
        let link = |url: &str| NewGif::link(url.into(), "x".into(), 1);
        let db = Db::open(std::path::Path::new(":memory:")).unwrap();
        let ids: Vec<GifId> = db.insert_many(vec![link(cdn), link(media)]).unwrap().iter().map(|g| g.id).collect();
        let mut lib = Library::load(db).unwrap();
        assert_eq!(lib.find(media), Some(ids[0]));
        lib.remove(&ids[..1]).unwrap();
        assert_eq!(lib.find(cdn), Some(ids[1]));
        assert!(lib.index(ids[0]).is_none());
    }

    #[test]
    fn another_link_to_a_saved_file_is_not_saved_again() {
        let mut lib = lib();
        let cdn = "https://cdn.discordapp.com/attachments/1/2/x.gif?ex=1";
        let media = "https://media.discordapp.net/attachments/1/2/x.gif?ex=2&hm=3";
        let saved = lib.add(vec![NewGif::link(cdn.into(), "x".into(), 1)]).unwrap()[0];
        let imported = || NewGif { imported: true, w: 40, h: 30, ..NewGif::link(media.into(), "x".into(), 1) };
        assert!(lib.add(vec![imported(), imported()]).unwrap().is_empty());
        lib.reload_imported().unwrap();
        assert_eq!(lib.gifs().len(), 3);
        let g = lib.gif(saved).unwrap();
        assert!(g.imported && g.w == 40, "the saved GIF takes what the import knows");
    }

    #[test]
    fn undoing_a_delete_merges_into_a_collection_named_alike() {
        let mut lib = lib();
        let gif = lib.gifs()[0].id;
        let old = lib.create_collection("Cats").unwrap();
        lib.set_membership(&[gif], old, true).unwrap();
        let (col, members) = lib.delete_collection(old).unwrap().unwrap();
        let new = lib.create_collection("cats").unwrap();
        assert_eq!(lib.restore_collection(&col, &members).unwrap(), None);
        assert_eq!(lib.collections().len(), 1);
        assert_eq!(lib.gif(gif).unwrap().cols, [new]);
        assert_eq!(lib.db().load().unwrap().1.len(), 1);
    }

    #[test]
    fn a_rename_reaches_the_search_text_of_its_gifs() {
        let mut lib = lib();
        let gif = lib.gifs()[0].id;
        let col = lib.create_collection("cats").unwrap();
        lib.set_membership(&[gif], col, true).unwrap();
        lib.rename_collection(col, "Kittens").unwrap();
        assert!(lib.gif(gif).unwrap().search_text.contains("kittens"));
        assert!(!lib.gif(gif).unwrap().search_text.contains("cats"));
    }
}
