//! Importing Discord favorites into "Discord imports". The fetch runs on its own thread and
//! comes back as `Msg::Import`.

use crate::{
    clock,
    db::NewGif,
    discord::{self, Fav},
    log::{self, Area},
    urls,
};

use super::{App, Msg};

/// What the Settings card shows about the last import.
#[derive(Default)]
pub enum ImportStatus {
    #[default]
    Idle,
    Running,
    Done(String),
    Failed(String),
}

impl App {
    pub(super) fn start_import(&mut self, source: discord::Source) {
        if matches!(self.form.import, ImportStatus::Running) {
            return;
        }
        self.form.import = ImportStatus::Running;
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        crate::spawn("import", move || {
            let _ = tx.send(Msg::Import(discord::favorites(source)));
            ctx.request_repaint();
        });
    }

    pub(super) fn finish_import(&mut self, res: Result<Vec<Fav>, String>) {
        self.form.import = match res.and_then(|favs| self.save_favorites(favs)) {
            Ok(msg) => {
                log::info(Area::Import, &msg);
                ImportStatus::Done(msg)
            }
            Err(e) => {
                log::warn(Area::Import, &e);
                ImportStatus::Failed(e)
            }
        };
    }

    /// Adds the favorites that are new and returns the summary to show.
    fn save_favorites(&mut self, favs: Vec<Fav>) -> Result<String, String> {
        let total = favs.len();
        // Imports sort below everything added by hand, in Discord's own order.
        let base = clock::now() - 10_000_000;
        let items: Vec<NewGif> = favs
            .into_iter()
            .map(|f| NewGif {
                title: urls::title_from_url(&f.url),
                media_link: (!f.src.is_empty()).then_some(f.src),
                url: f.url,
                imported: true,
                added_at: base.saturating_add(i64::try_from(f.order).unwrap_or(i64::MAX)),
                w: f.width,
                h: f.height,
            })
            .collect();
        let added = self.lib.add(items).map_err(|e| format!("Couldn't save the imported GIFs: {e}"))?.len();
        self.dirty = true;
        // GIFs that were already saved may have got their size or a fresh link from the import.
        let renewed = self.lib.reload_imported().map_err(|e| format!("Couldn't read the imported GIFs: {e}"))?;
        for gi in renewed {
            self.retry_load(gi);
        }
        Ok(format!("Imported {} ({} already saved).", super::gif_count(added), total - added))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    #[test]
    fn extreme_favorite_orders_cannot_overflow_the_import_timestamp() {
        let db = Db::open(std::path::Path::new(":memory:")).unwrap();
        let mut app = App::for_test(db);
        let favs = [i64::MAX as u64, u64::MAX]
            .into_iter()
            .enumerate()
            .map(|(i, order)| Fav { url: format!("https://x/{i}.gif"), src: String::new(), order, width: 1, height: 1 })
            .collect();
        app.save_favorites(favs).unwrap();
        assert!(app.lib.gifs().iter().all(|g| g.added_at == i64::MAX));
        assert_eq!(app.lib.db().load().unwrap().0.len(), 2);
    }

    #[test]
    fn importing_a_saved_link_updates_memory_and_preserves_library_membership() {
        let db = Db::open(std::path::Path::new(":memory:")).unwrap();
        db.insert_many(vec![NewGif::link("https://x/cat.gif".into(), "cat".into(), 1)]).unwrap();
        let mut app = App::for_test(db);
        app.dirty = false;
        app.save_favorites(vec![Fav {
            url: "https://x/cat.gif".into(),
            src: String::new(),
            order: 0,
            width: 1,
            height: 1,
        }])
        .unwrap();
        let g = &app.lib.gifs()[0];
        assert!(g.imported && g.in_library() && app.dirty);
        let (gifs, _) = app.lib.db().load().unwrap();
        assert!(gifs[0].imported && gifs[0].in_library());
    }
}
