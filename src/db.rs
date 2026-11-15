//! The library: one SQLite file holding GIFs, collections, which GIF is in which, and settings.
//!
//! - It's opened in WAL mode with `synchronous = NORMAL`: writes are quick, and a power cut can
//!   lose the last changes but can't corrupt the file.
//! - The schema is in [`MIGRATIONS`], an append-only list counted by `PRAGMA user_version`.
//! - Ids are `AUTOINCREMENT` and never reused, because loads, undo steps and link renewals still
//!   in flight refer to GIFs by id.
//! - Times are Unix seconds from [`crate::clock`]. `copied_at_ms` is in milliseconds, so copies made
//!   within the same second still sort in order.
//! - [`Db::backup_to`] writes a copy with the bot token removed.

use std::path::{Path, PathBuf};

use rusqlite::types::{FromSql, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use rusqlite::{Connection, OptionalExtension, Params, Result, params};

/// Row ids, typed so a GIF's id can't be passed where a collection's is expected.
macro_rules! row_id {
    ($($(#[$doc:meta])* $name:ident;)*) => {$(
        $(#[$doc])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Default)]
        pub struct $name(pub i64);

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }

        impl ToSql for $name {
            fn to_sql(&self) -> Result<ToSqlOutput<'_>> {
                self.0.to_sql()
            }
        }

        impl FromSql for $name {
            fn column_result(v: ValueRef<'_>) -> FromSqlResult<Self> {
                i64::column_result(v).map(Self)
            }
        }
    )*};
}

row_id! {
    /// A row of `gifs`.
    GifId;
    /// A row of `collections`.
    CollectionId;
}

/// Connection settings, applied on every open.
const PRAGMAS: &str = "
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA foreign_keys = ON;
";

/// Schema changes, oldest first. `PRAGMA user_version` counts how many have run, so each runs
/// once, in its own transaction. To change the schema, append a step; never edit a shipped one.
const MIGRATIONS: &[&str] = &[
    // 1: the original schema. `IF NOT EXISTS` lets libraries from before versioning adopt it.
    "
CREATE TABLE IF NOT EXISTS gifs (
    id        INTEGER PRIMARY KEY,
    url       TEXT NOT NULL UNIQUE,
    media     TEXT,
    title     TEXT NOT NULL DEFAULT '',
    tags      TEXT NOT NULL DEFAULT '',
    imported  INTEGER NOT NULL DEFAULT 0,
    added_at  INTEGER NOT NULL,
    last_used INTEGER NOT NULL DEFAULT 0,
    use_count INTEGER NOT NULL DEFAULT 0,
    w         INTEGER NOT NULL DEFAULT 0,
    h         INTEGER NOT NULL DEFAULT 0,
    copied_at INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS collections (
    id     INTEGER PRIMARY KEY,
    name   TEXT NOT NULL UNIQUE,
    pos    INTEGER NOT NULL DEFAULT 0,
    hidden INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS gif_collections (
    gif_id        INTEGER NOT NULL REFERENCES gifs(id) ON DELETE CASCADE,
    collection_id INTEGER NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
    PRIMARY KEY (gif_id, collection_id)
);
CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
",
    // 2: ids are never reused, so nothing left over from a deleted GIF or collection (a load
    // still running, a cached file) can be taken for a new one. Rebuilds both tables as is.
    "
CREATE TABLE gifs_new (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    url       TEXT NOT NULL UNIQUE,
    media     TEXT,
    title     TEXT NOT NULL DEFAULT '',
    tags      TEXT NOT NULL DEFAULT '',
    imported  INTEGER NOT NULL DEFAULT 0,
    added_at  INTEGER NOT NULL,
    last_used INTEGER NOT NULL DEFAULT 0,
    use_count INTEGER NOT NULL DEFAULT 0,
    w         INTEGER NOT NULL DEFAULT 0,
    h         INTEGER NOT NULL DEFAULT 0,
    copied_at INTEGER NOT NULL DEFAULT 0
);
INSERT INTO gifs_new (id, url, media, title, tags, imported, added_at, last_used, use_count, w, h, copied_at)
    SELECT id, url, media, title, tags, imported, added_at, last_used, use_count, w, h, copied_at FROM gifs;
DROP TABLE gifs;
ALTER TABLE gifs_new RENAME TO gifs;
CREATE TABLE collections_new (
    id     INTEGER PRIMARY KEY AUTOINCREMENT,
    name   TEXT NOT NULL UNIQUE,
    pos    INTEGER NOT NULL DEFAULT 0,
    hidden INTEGER NOT NULL DEFAULT 0
);
INSERT INTO collections_new (id, name, pos, hidden) SELECT id, name, pos, hidden FROM collections;
DROP TABLE collections;
ALTER TABLE collections_new RENAME TO collections;
",
    // 3: remember GIFs saved by hand even after they also become Discord imports.
    "ALTER TABLE gifs ADD COLUMN saved INTEGER NOT NULL DEFAULT 0;
     UPDATE gifs SET saved = NOT imported;",
    // 4: name the only time kept in milliseconds as such.
    "ALTER TABLE gifs RENAME COLUMN copied_at TO copied_at_ms;",
];

/// Brings the schema up to date. A library from a newer gifly is left as it is.
fn migrate(c: &mut Connection) -> Result<()> {
    let done: i64 = c.pragma_query_value(None, "user_version", |r| r.get(0))?;
    // Off while a step rebuilds a table, so dropping the old copy doesn't cascade to the rows
    // that point at it. Only takes effect outside a transaction.
    c.pragma_update(None, "foreign_keys", false)?;
    for (version, step) in MIGRATIONS.iter().enumerate().skip(done.max(0) as usize) {
        let tx = c.transaction()?;
        tx.execute_batch(step)?;
        tx.pragma_update(None, "user_version", version as i64 + 1)?;
        tx.commit()?;
        crate::log::info(crate::log::Area::Db, format!("library upgraded to version {}", version + 1));
    }
    c.pragma_update(None, "foreign_keys", true)
}

/// Every setting the app stores.
#[derive(Clone, Copy)]
pub enum Key {
    AutoSend,
    Capture,
    Autostart,
    Zoom,
    /// The window size when the picker last closed: "width height".
    Size,
    HomeOrder,
    /// The Discord bot token that renews expired Discord file links. Empty when removed.
    BotToken,
    HideGone,
    /// GIFs play only while hovered or selected.
    HoverPlay,
    /// Debug lines in the log ("Detailed logging").
    DetailedLog,
}

impl Key {
    fn name(self) -> &'static str {
        match self {
            Self::AutoSend => "auto_send",
            Self::Capture => "capture",
            Self::Autostart => "autostart",
            Self::Zoom => "zoom",
            Self::Size => "size",
            Self::HomeOrder => "home_order",
            Self::BotToken => "bot_token",
            Self::HideGone => "hide_gone",
            Self::HoverPlay => "hover_play",
            Self::DetailedLog => "detailed_log",
        }
    }
}

pub struct NewGif {
    pub url: String,
    pub media_link: Option<String>,
    pub title: String,
    pub imported: bool,
    pub added_at: i64,
    pub w: u16,
    pub h: u16,
}

impl NewGif {
    /// A GIF the user brought in themselves: just its link, size unknown until it loads.
    pub fn link(url: String, title: String, added_at: i64) -> Self {
        Self { url, media_link: None, title, imported: false, added_at, w: 0, h: 0 }
    }
}

/// A GIF's size, media link and import flag, see `Db::sizes_and_media`.
#[derive(Debug, PartialEq, Eq)]
pub struct SizeAndMedia {
    pub id: GifId,
    pub w: u16,
    pub h: u16,
    pub media_link: Option<String>,
    pub imported: bool,
}

pub struct Gif {
    pub id: GifId,
    pub url: String,
    pub media_link: Option<String>,
    pub title: String,
    pub tags: String,
    /// Came from a Discord import. Stays set after the GIF is used or filed.
    pub imported: bool,
    /// Saved by hand, independently of also appearing in Discord imports.
    pub saved: bool,
    pub added_at: i64,
    pub last_used: i64,
    pub use_count: i64,
    /// Pixel size, 0 until known (from Discord's import or the first thumbnail).
    pub w: u16,
    pub h: u16,
    /// When its link was last copied anywhere on the PC (Unix ms), 0 if never.
    pub copied_at_ms: i64,
    pub cols: Vec<CollectionId>,
    /// Lowercased search text: title, tags and collection names.
    pub search_text: String,
}

impl Gif {
    pub fn from_new(id: GifId, n: NewGif) -> Self {
        Self {
            id,
            url: n.url,
            media_link: n.media_link,
            title: n.title,
            tags: String::new(),
            imported: n.imported,
            saved: !n.imported,
            added_at: n.added_at,
            last_used: 0,
            use_count: 0,
            w: n.w,
            h: n.h,
            copied_at_ms: 0,
            cols: Vec::new(),
            search_text: String::new(),
        }
    }

    /// Imports join the library once used or filed; they stay in Discord imports either way.
    pub fn in_library(&self) -> bool {
        self.saved || self.use_count > 0 || !self.cols.is_empty()
    }

    /// Height over width; 4:3 until the real size is known.
    pub fn ratio(&self) -> f32 {
        if self.w > 0 && self.h > 0 { f32::from(self.h) / f32::from(self.w) } else { 0.75 }
    }

    /// Use count decayed by a two-week half-life since last use.
    pub fn frecency(&self, now: i64) -> f32 {
        if self.use_count == 0 {
            return 0.0;
        }
        let age_days = (now - self.last_used).max(0) as f32 / 86_400.0;
        self.use_count as f32 * 0.5f32.powf(age_days / 14.0)
    }
}

#[derive(Clone)]
pub struct Collection {
    pub id: CollectionId,
    pub name: String,
    pub hidden: bool,
}

/// The SQLite library. Writes the user acts on return errors so the UI can report them; cached
/// details (sizes, copy times, settings) are best-effort and ignore failures.
pub struct Db {
    conn: Connection,
}

/// Files a GIF (`?1`) in a collection (`?2`), if that collection still exists.
const FILE_IN_EXISTING: &str =
    "INSERT OR IGNORE INTO gif_collections (gif_id, collection_id) SELECT ?1, id FROM collections WHERE id = ?2";

impl Db {
    #[cfg(test)]
    pub(crate) fn execute_test_batch(&self, sql: &str) {
        self.conn.execute_batch(sql).unwrap();
    }

    pub fn open(path: &Path) -> Result<Self> {
        let mut c = Connection::open(path)?;
        c.execute_batch(PRAGMAS)?;
        migrate(&mut c)?;
        Ok(Self { conn: c })
    }

    /// Whether a newer gifly last upgraded this library, so this one may not be able to read it.
    pub fn is_newer_than_app(&self) -> bool {
        let version: i64 = self.conn.pragma_query_value(None, "user_version", |r| r.get(0)).unwrap_or(0);
        version > MIGRATIONS.len() as i64
    }

    pub fn load(&self) -> Result<(Vec<Gif>, Vec<Collection>)> {
        let mut gifs: Vec<Gif> = self
            .conn
            .prepare("SELECT id, url, media, title, tags, imported, added_at, last_used, use_count, w, h, copied_at_ms, saved FROM gifs")?
            .query_map([], |r| {
                Ok(Gif {
                    id: r.get(0)?,
                    url: r.get(1)?,
                    media_link: r.get(2)?,
                    title: r.get(3)?,
                    tags: r.get(4)?,
                    imported: r.get(5)?,
                    saved: r.get(12)?,
                    added_at: r.get(6)?,
                    last_used: r.get(7)?,
                    use_count: r.get(8)?,
                    w: r.get(9)?,
                    h: r.get(10)?,
                    copied_at_ms: r.get(11)?,
                    cols: Vec::new(),
                    search_text: String::new(),
                })
            })?
            .collect::<Result<_>>()?;

        let cols = self
            .conn
            .prepare("SELECT id, name, hidden FROM collections ORDER BY pos, id")?
            .query_map([], |r| Ok(Collection { id: r.get(0)?, name: r.get(1)?, hidden: r.get(2)? }))?
            .collect::<Result<_>>()?;

        let idx: std::collections::HashMap<GifId, usize> = gifs.iter().enumerate().map(|(i, g)| (g.id, i)).collect();
        let mut stmt = self.conn.prepare("SELECT gif_id, collection_id FROM gif_collections")?;
        for row in stmt.query_map([], |r| Ok((r.get::<_, GifId>(0)?, r.get::<_, CollectionId>(1)?)))? {
            let (g, c) = row?;
            if let Some(&i) = idx.get(&g) {
                gifs[i].cols.push(c);
            }
        }
        Ok((gifs, cols))
    }

    /// Inserts new GIFs in one transaction and returns them. Links already saved keep everything
    /// but get their size filled in if unknown, and a media link that lasts longer.
    pub fn insert_many(&self, items: Vec<NewGif>) -> Result<Vec<Gif>> {
        let tx = self.conn.unchecked_transaction()?;
        let mut out = Vec::new();
        {
            let mut stmt = tx.prepare(
                "INSERT OR IGNORE INTO gifs (url, media, title, imported, added_at, w, h, saved)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NOT ?4)",
            )?;
            // SET expressions see the old row, so `h` checks the old `w` too.
            let mut old_media = tx.prepare("SELECT media FROM gifs WHERE url = ?1")?;
            let mut known = tx.prepare(
                "UPDATE gifs SET w = CASE WHEN w = 0 THEN ?2 ELSE w END, h = CASE WHEN w = 0 THEN ?3 ELSE h END,
                 media = COALESCE(?4, media), imported = imported OR ?5 WHERE url = ?1",
            )?;
            for n in items {
                if stmt.execute(params![n.url, n.media_link, n.title, n.imported, n.added_at, n.w, n.h])? == 1 {
                    out.push(Gif::from_new(GifId(tx.last_insert_rowid()), n));
                } else {
                    // Discord's favorites keep the link from when a GIF was favorited, often long
                    // expired, so a link renewed since is kept.
                    let old: Option<String> = old_media.query_row([&n.url], |r| r.get(0)).optional()?.flatten();
                    let media =
                        n.media_link.filter(|new| old.as_deref().is_none_or(|old| crate::urls::outlasts(new, old)));
                    known.execute(params![n.url, n.w, n.h, media, n.imported])?;
                }
            }
        }
        tx.commit()?;
        Ok(out)
    }

    pub fn mark_used(&self, id: GifId, at: i64) -> Result<()> {
        self.conn
            .execute("UPDATE gifs SET use_count = use_count + 1, last_used = ?2 WHERE id = ?1", params![id, at])?;
        Ok(())
    }

    pub fn mark_copied(&self, id: GifId, at: i64) -> Result<()> {
        self.conn.execute("UPDATE gifs SET copied_at_ms = ?2 WHERE id = ?1", params![id, at])?;
        Ok(())
    }

    pub fn set_url(&self, id: GifId, url: &str) -> Result<()> {
        self.conn.execute("UPDATE gifs SET url = ?2 WHERE id = ?1", params![id, url])?;
        Ok(())
    }

    /// Runs `sql` once for each row of parameters, all in one transaction.
    fn execute_each<P: Params>(&self, sql: &str, rows: impl IntoIterator<Item = P>) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare(sql)?;
            for row in rows {
                stmt.execute(row)?;
            }
        }
        tx.commit()
    }

    /// Saves fresh media links, `(id, link)`, in one transaction.
    pub fn set_media(&self, links: &[(GifId, String)]) -> Result<()> {
        self.execute_each("UPDATE gifs SET media = ?2 WHERE id = ?1", links.iter().map(|(id, link)| (id, link)))
    }

    pub fn set_size(&self, id: GifId, w: u16, h: u16) -> Result<()> {
        self.conn.execute("UPDATE gifs SET w = ?2, h = ?3 WHERE id = ?1", params![id, w, h])?;
        Ok(())
    }

    /// Every GIF's size, media link and import flag, as `insert_many` may have updated them.
    pub fn sizes_and_media(&self) -> Result<Vec<SizeAndMedia>> {
        self.conn
            .prepare("SELECT id, w, h, media, imported FROM gifs")?
            .query_map([], |r| {
                Ok(SizeAndMedia {
                    id: r.get(0)?,
                    w: r.get(1)?,
                    h: r.get(2)?,
                    media_link: r.get(3)?,
                    imported: r.get(4)?,
                })
            })?
            .collect()
    }

    pub fn delete_gifs(&self, ids: &[GifId]) -> Result<()> {
        self.execute_each("DELETE FROM gifs WHERE id = ?1", ids.iter().map(|id| [id]))
    }

    /// Puts deleted GIFs back as they were: same ids, stats and collections (those that still exist).
    pub fn restore_gifs(&self, gifs: &[Gif]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO gifs (id, url, media, title, tags, imported, added_at, last_used, use_count, w, h, copied_at_ms, saved)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            )?;
            let mut member = tx.prepare(FILE_IN_EXISTING)?;
            for g in gifs {
                stmt.execute(params![
                    g.id,
                    g.url,
                    g.media_link,
                    g.title,
                    g.tags,
                    g.imported,
                    g.added_at,
                    g.last_used,
                    g.use_count,
                    g.w,
                    g.h,
                    g.copied_at_ms,
                    g.saved
                ])?;
                for c in &g.cols {
                    member.execute(params![g.id, c])?;
                }
            }
        }
        tx.commit()
    }

    /// Folds a deleted GIF into `into`, another record of the same file saved since: its
    /// collections (those that still exist) and its use count and dates.
    pub fn merge_gif(&self, into: GifId, from: &Gif) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE gifs SET use_count = use_count + ?2, last_used = MAX(last_used, ?3), added_at = MIN(added_at, ?4),
             copied_at_ms = MAX(copied_at_ms, ?5), imported = imported OR ?6, saved = saved OR ?7 WHERE id = ?1",
            params![into, from.use_count, from.last_used, from.added_at, from.copied_at_ms, from.imported, from.saved],
        )?;
        {
            let mut member = tx.prepare(FILE_IN_EXISTING)?;
            for c in &from.cols {
                member.execute(params![into, c])?;
            }
        }
        tx.commit()
    }

    pub fn create_collection(&self, name: &str) -> Result<CollectionId> {
        self.conn.execute(
            "INSERT INTO collections (name, pos) VALUES (?1, (SELECT COALESCE(MAX(pos), 0) + 1 FROM collections))",
            [name],
        )?;
        Ok(CollectionId(self.conn.last_insert_rowid()))
    }

    pub fn rename_collection(&self, id: CollectionId, name: &str) -> Result<()> {
        self.conn.execute("UPDATE collections SET name = ?2 WHERE id = ?1", params![id, name])?;
        Ok(())
    }

    pub fn set_hidden(&self, id: CollectionId, hidden: bool) -> Result<()> {
        self.conn.execute("UPDATE collections SET hidden = ?2 WHERE id = ?1", params![id, hidden])?;
        Ok(())
    }

    /// Stores the collections' order as given.
    pub fn set_order(&self, ids: &[CollectionId]) -> Result<()> {
        self.execute_each("UPDATE collections SET pos = ?2 WHERE id = ?1", ids.iter().zip(0i64..))
    }

    pub fn delete_collection(&self, id: CollectionId) -> Result<()> {
        self.conn.execute("DELETE FROM collections WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Restores a deleted collection, or files its members in one recreated with the same name.
    pub fn restore_collection(&self, c: &Collection, members: &[GifId]) -> Result<CollectionId> {
        let tx = self.conn.unchecked_transaction()?;
        let id = tx.query_row(
            "INSERT INTO collections (id, name, hidden) VALUES (?1, ?2, ?3)
             ON CONFLICT(name) DO UPDATE SET name = excluded.name RETURNING id",
            params![c.id, c.name, c.hidden],
            |r| r.get::<_, CollectionId>(0),
        )?;
        {
            let mut stmt = tx.prepare(
                "INSERT OR IGNORE INTO gif_collections (gif_id, collection_id) SELECT id, ?2 FROM gifs WHERE id = ?1",
            )?;
            for &gif in members {
                stmt.execute(params![gif, id])?;
            }
        }
        tx.commit()?;
        Ok(id)
    }

    /// Adds GIFs to a collection or takes them out, in one transaction.
    pub fn set_membership(&self, gifs: &[GifId], col: CollectionId, on: bool) -> Result<()> {
        let sql = if on {
            "INSERT OR IGNORE INTO gif_collections (gif_id, collection_id) VALUES (?1, ?2)"
        } else {
            "DELETE FROM gif_collections WHERE gif_id = ?1 AND collection_id = ?2"
        };
        self.execute_each(sql, gifs.iter().map(|gif| (gif, col)))
    }

    pub fn setting(&self, key: Key) -> Option<String> {
        self.conn.query_row("SELECT value FROM settings WHERE key = ?1", [key.name()], |r| r.get(0)).ok()
    }

    /// Writes a consistent copy of the whole library into `dir`, replacing an older copy from today
    /// only once the new one is complete.
    pub fn backup_to(&self, dir: &Path) -> std::result::Result<PathBuf, String> {
        let date: String =
            self.conn.query_row("SELECT date('now', 'localtime')", [], |r| r.get(0)).map_err(|e| e.to_string())?;
        let path = dir.join(format!("gifly-{date}.db"));
        let tmp = dir.join(format!("gifly-{date}.db.partial"));
        let _ = std::fs::remove_file(&tmp);
        // Sanitize in memory before writing anything to a possibly synced backup folder.
        let done = self
            .backup_copy()
            .and_then(|copy| copy.execute("VACUUM INTO ?1", [tmp.to_string_lossy()]))
            .map_err(|e| e.to_string());
        let done = done.and_then(|_| std::fs::rename(&tmp, &path).map_err(|e| e.to_string()));
        if done.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        done.map(|_| path)
    }

    /// The snapshot to export, with secrets removed before any file is created.
    fn backup_copy(&self) -> Result<Connection> {
        let mut copy = Connection::open_in_memory()?;
        rusqlite::backup::Backup::new(&self.conn, &mut copy)?.run_to_completion(
            128,
            std::time::Duration::from_millis(10),
            None,
        )?;
        copy.pragma_update(None, "secure_delete", true)?;
        copy.execute("DELETE FROM settings WHERE key = ?1", [Key::BotToken.name()])?;
        Ok(copy)
    }

    /// Saves a setting, best-effort: for the ones that only make gifly nicer to come back to.
    pub fn set_setting(&self, key: Key, value: &str) {
        let _ = self.try_set_setting(key, value);
    }

    pub fn try_set_setting(&self, key: Key, value: &str) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2",
                [key.name(), value],
            )
            .map(|_| ())
    }

    pub fn flag(&self, key: Key, default: bool) -> bool {
        self.setting(key).map_or(default, |v| v == "1")
    }

    pub fn set_flag(&self, key: Key, on: bool) {
        self.set_setting(key, if on { "1" } else { "0" });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_source_is_already_sanitized_before_export() {
        let db = memory();
        db.set_setting(Key::BotToken, "secret");
        db.set_setting(Key::Zoom, "1.5");
        let copy = Db { conn: db.backup_copy().unwrap() };
        assert_eq!(copy.setting(Key::BotToken), None);
        assert_eq!(copy.setting(Key::Zoom).as_deref(), Some("1.5"));
        assert_eq!(db.setting(Key::BotToken).as_deref(), Some("secret"));
    }

    #[test]
    fn saved_migration_preserves_library_and_import_status() {
        let mut c = Connection::open_in_memory().unwrap();
        c.execute_batch(PRAGMAS).unwrap();
        c.execute_batch(MIGRATIONS[0]).unwrap();
        c.execute_batch("INSERT INTO gifs (url, imported, added_at) VALUES ('manual', 0, 1), ('import', 1, 1);")
            .unwrap();
        c.pragma_update(None, "user_version", 1).unwrap();
        migrate(&mut c).unwrap();
        let db = Db { conn: c };
        let (gifs, _) = db.load().unwrap();
        assert!(gifs.iter().find(|g| g.url == "manual").unwrap().in_library());
        let imported = gifs.iter().find(|g| g.url == "import").unwrap();
        assert!(imported.imported && !imported.in_library());
    }

    #[test]
    fn backup_is_a_full_copy_without_the_token_and_replaces_todays() {
        let dir = std::env::temp_dir().join(format!("gifly-backup-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("live.db")).unwrap();
        db.create_collection("cats").unwrap();
        db.set_setting(Key::Zoom, "1.3");
        let first = db.backup_to(&dir).unwrap();
        db.set_setting(Key::Zoom, "1.5");
        db.set_setting(Key::BotToken, "secret");
        let second = db.backup_to(&dir).unwrap();
        assert_eq!(first, second);
        let copy = Db::open(&second).unwrap();
        assert_eq!(copy.load().unwrap().1.len(), 1);
        assert_eq!(copy.setting(Key::Zoom).as_deref(), Some("1.5"));
        assert_eq!(copy.setting(Key::BotToken), None);
        // Not even its deleted bytes are left in the file.
        assert!(!std::fs::read(&second).unwrap().windows(6).any(|w| w == b"secret"));
        assert!(!dir.join(format!("{}.partial", second.file_name().unwrap().to_string_lossy())).exists());
        drop((db, copy));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn memory() -> Db {
        Db::open(Path::new(":memory:")).unwrap()
    }

    fn new_gif(url: &str, w: u16, h: u16) -> NewGif {
        NewGif { w, h, ..NewGif::link(url.into(), String::new(), 1) }
    }

    #[test]
    fn migrations_run_once_and_adopt_old_libraries() {
        let version = |c: &Connection| c.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0)).unwrap();
        // A library from before versioning has the original tables at version 0.
        let mut c = Connection::open_in_memory().unwrap();
        c.execute_batch(PRAGMAS).unwrap();
        c.execute_batch(MIGRATIONS[0]).unwrap();
        migrate(&mut c).unwrap();
        assert_eq!(version(&c), MIGRATIONS.len() as i64);
        // Up to date, nothing runs again.
        migrate(&mut c).unwrap();
        assert_eq!(version(&c), MIGRATIONS.len() as i64);
        assert!(!Db { conn: c }.is_newer_than_app());
    }

    #[test]
    fn a_library_from_a_newer_gifly_is_left_alone_and_recognised() {
        let db = memory();
        db.conn.pragma_update(None, "user_version", MIGRATIONS.len() as i64 + 1).unwrap();
        assert!(db.is_newer_than_app());
        let mut c = db.conn;
        migrate(&mut c).unwrap();
        assert_eq!(
            c.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0)).unwrap(),
            MIGRATIONS.len() as i64 + 1
        );
    }

    #[test]
    fn upgrading_keeps_ids_and_memberships_and_stops_reusing_ids() {
        let mut c = Connection::open_in_memory().unwrap();
        c.execute_batch(PRAGMAS).unwrap();
        c.execute_batch(MIGRATIONS[0]).unwrap();
        c.pragma_update(None, "user_version", 1).unwrap();
        c.execute_batch(
            "INSERT INTO gifs (id, url, added_at, use_count, copied_at) VALUES (1, 'a', 1, 5, 1234), (7, 'b', 1, 0, 0);
             INSERT INTO collections (id, name) VALUES (3, 'cats');
             INSERT INTO gif_collections VALUES (1, 3), (7, 3);",
        )
        .unwrap();
        migrate(&mut c).unwrap();
        let db = Db { conn: c };
        let (gifs, cols) = db.load().unwrap();
        let a = gifs.iter().find(|g| g.id == GifId(1)).unwrap();
        assert_eq!((a.url.as_str(), a.use_count, a.copied_at_ms), ("a", 5, 1234));
        assert_eq!(a.cols, [CollectionId(3)]);
        assert_eq!(cols[0].id, CollectionId(3));
        // The newest GIF's id isn't handed out again once it's deleted.
        db.delete_gifs(&[GifId(7)]).unwrap();
        assert_eq!(db.insert_many(vec![new_gif("c", 0, 0)]).unwrap()[0].id, GifId(8));
        // Deleting still cascades to memberships.
        db.delete_gifs(&[GifId(1)]).unwrap();
        let left: i64 = db.conn.query_row("SELECT COUNT(*) FROM gif_collections", [], |r| r.get(0)).unwrap();
        assert_eq!(left, 0);
    }

    #[test]
    fn membership_changes_many_gifs_at_once() {
        let db = memory();
        let ids: Vec<GifId> =
            db.insert_many(vec![new_gif("a", 0, 0), new_gif("b", 0, 0)]).unwrap().iter().map(|g| g.id).collect();
        let col = db.create_collection("cats").unwrap();
        db.set_membership(&ids, col, true).unwrap();
        // Adding again is harmless.
        db.set_membership(&ids, col, true).unwrap();
        let cols = |db: &Db| db.load().unwrap().0.into_iter().map(|g| g.cols).collect::<Vec<_>>();
        assert_eq!(cols(&db), [vec![col], vec![col]]);
        db.set_membership(&ids[..1], col, false).unwrap();
        assert_eq!(cols(&db), [vec![], vec![col]]);
    }

    #[test]
    fn undoing_deletes_restores_ids_and_memberships() {
        let db = memory();
        let a = db.insert_many(vec![new_gif("a", 4, 3)]).unwrap()[0].id;
        let (cats, dogs) = (db.create_collection("cats").unwrap(), db.create_collection("dogs").unwrap());
        db.set_membership(&[a], cats, true).unwrap();
        db.set_membership(&[a], dogs, true).unwrap();
        let mut gif = db.load().unwrap().0.remove(0);
        gif.use_count = 3;
        db.delete_gifs(&[a]).unwrap();
        // A collection deleted meanwhile is skipped, not an error.
        db.delete_collection(dogs).unwrap();
        db.restore_gifs(&[gif]).unwrap();
        let back = db.load().unwrap().0.remove(0);
        assert_eq!((back.id, back.use_count, back.w, back.cols), (a, 3, 4, vec![cats]));

        let col = Collection { id: dogs, name: "dogs".into(), hidden: true };
        db.restore_collection(&col, &[a, GifId(999)]).unwrap();
        let (gifs, cols) = db.load().unwrap();
        assert!(cols.iter().any(|c| c.id == dogs && c.hidden));
        let mut back = gifs[0].cols.clone();
        back.sort_unstable();
        assert_eq!(back, [cats, dogs]);
    }

    #[test]
    fn merging_a_deleted_gif_keeps_its_collections_and_stats() {
        let db = memory();
        let a = db.insert_many(vec![new_gif("a", 4, 3)]).unwrap()[0].id;
        let cats = db.create_collection("cats").unwrap();
        db.set_membership(&[a], cats, true).unwrap();
        db.mark_used(a, 50).unwrap();
        let old = db.load().unwrap().0.remove(0);
        db.delete_gifs(&[a]).unwrap();
        // Copied again: a new record of the same file, used since.
        let b = db.insert_many(vec![new_gif("a", 4, 3)]).unwrap()[0].id;
        db.mark_used(b, 90).unwrap();
        db.merge_gif(b, &old).unwrap();
        let g = db.load().unwrap().0.remove(0);
        assert_eq!((g.id, g.use_count, g.last_used, g.cols), (b, 2, 90, vec![cats]));
    }

    #[test]
    fn insert_many_skips_known_links_but_refreshes_size_and_media() {
        let db = memory();
        let added = db.insert_many(vec![new_gif("a", 0, 0), new_gif("b", 10, 20)]).unwrap();
        assert_eq!(added.len(), 2);
        let fresh = NewGif { media_link: Some("b?ex=new".into()), ..new_gif("b", 50, 60) };
        let again = db.insert_many(vec![new_gif("a", 30, 40), fresh, new_gif("c", 0, 0)]).unwrap();
        assert_eq!(again.iter().map(|g| g.url.as_str()).collect::<Vec<_>>(), ["c"]);
        let mut rows = db.sizes_and_media().unwrap();
        rows.sort_unstable_by_key(|r| r.id);
        let row = |id, w, h, media: Option<&str>| SizeAndMedia {
            id,
            w,
            h,
            media_link: media.map(Into::into),
            imported: false,
        };
        // "a" had no size and takes the new one; "b" keeps its size but gets the newer link.
        assert_eq!(rows[..2], [row(added[0].id, 30, 40, None), row(added[1].id, 10, 20, Some("b?ex=new"))]);

        // A re-import's older Discord link doesn't replace one renewed since; a newer one does.
        let link = |ex: &str| format!("https://cdn.discordapp.com/attachments/1/2/d.gif?ex={ex}&is=1&hm=f");
        let with = |ex: &str| NewGif { media_link: Some(link(ex)), ..new_gif("d", 1, 1) };
        let media = |db: &Db| {
            db.sizes_and_media().unwrap().into_iter().find_map(|r| r.media_link.filter(|m| m.contains("/d.gif")))
        };
        db.insert_many(vec![with("200")]).unwrap();
        db.insert_many(vec![with("100")]).unwrap();
        assert_eq!(media(&db), Some(link("200")));
        db.insert_many(vec![with("300")]).unwrap();
        assert_eq!(media(&db), Some(link("300")));
    }
}
