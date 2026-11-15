//! The picker's state and everything that changes it. The UI (`crate::ui`) draws this state and
//! turns input into `Action`s, which `App::apply` carries out once the frame is drawn.

mod clipboard;
mod diagnostics;
mod edits;
mod files;
mod import;
mod loads;
mod prefs;
mod renew;
mod soak;
mod system;
mod textures;
mod views;
mod window;

use std::{path::PathBuf, sync::Arc};

use crossbeam_channel::{Receiver, Sender, unbounded};
use eframe::egui::{self, TextureHandle};
use global_hotkey::GlobalHotKeyManager;
use tray_icon::TrayIcon;

use crate::{
    db::{Collection, CollectionId, Gif, GifId, Key, NewGif},
    discord::{self, RenewError},
    library::Library,
    log::{self, Area},
    media::{Anim, Event, Media},
    platform,
};

pub use edits::gif_count;
pub use import::ImportStatus;
pub use prefs::Setting;
use renew::RenewerId;
pub use system::{HOTKEY_LABEL, Shared};
pub use views::HomeTile;

pub const MIN_ZOOM: f32 = 0.8;
pub const MAX_ZOOM: f32 = 2.0;

/// What `main` loads before the window opens.
pub struct Startup {
    pub lib: Library,
    pub cache: PathBuf,
    /// Where GIF files added from the PC are kept.
    pub files: PathBuf,
    /// Started with `--hidden` (at sign-in): stay in the tray.
    pub start_hidden: bool,
    /// The tray icon's pixels and size (square), drawn by `ui`.
    pub tray_icon: (Vec<u8>, u32),
}

/// Results from background threads other than media loads (`media::Event`), handled on the UI
/// thread by `App::pump`.
pub enum Msg {
    /// A GIF link was copied somewhere on the PC.
    Captured(String),
    Import(Result<Vec<discord::Fav>, String>),
    /// Renewals that were dropped unsent, the view having moved far from their GIFs.
    RenewalsDropped {
        renewer: RenewerId,
        ids: Vec<GifId>,
    },
    /// Picture files copied into gifly's folder, to add to `view`; `skipped` weren't pictures.
    FilesStored {
        items: Vec<NewGif>,
        skipped: usize,
        view: View,
    },
    /// Renewed links, from `renewer`, for the GIFs it `asked` for.
    Renewed {
        renewer: RenewerId,
        asked: Vec<GifId>,
        result: Result<Vec<(GifId, String)>, RenewError>,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum View {
    Home,
    Recent,
    /// Every GIF link copied anywhere, newest first.
    Copied,
    Unsorted,
    Imports,
    Collection(CollectionId),
    Settings,
}

pub enum Action {
    /// Pastes a GIF; `invert` flips "Send right after pasting" for this one.
    Send {
        id: GifId,
        invert: bool,
    },
    Open(View),
    /// Mouse back/forward buttons: step through the views visited since the picker opened.
    Back,
    Forward,
    /// Puts GIFs in a collection (`on`) or takes them out.
    SetMembership {
        gifs: Vec<GifId>,
        col: CollectionId,
        on: bool,
    },
    /// Creates (or reuses) a collection by name and adds these GIFs to it.
    NewCollection(String, Vec<GifId>),
    RenameCollection(CollectionId, String),
    DeleteCollection(CollectionId),
    /// Moves the first Home tile to the second's place (live, while dragging).
    MoveTile(View, View),
    SaveOrder,
    HideCollection {
        id: CollectionId,
        hidden: bool,
    },
    /// Shows or re-hides hidden collections until the picker closes.
    Reveal,
    DeleteGifs(Vec<GifId>),
    CopyLink(GifId),
    /// Shift+click: adds a GIF to the picked ones, or takes it out. `range` picks everything up to it.
    Pick {
        id: GifId,
        range: bool,
    },
    /// Ctrl+A.
    PickAll,
    ClearPicks,
    /// Ctrl+Z, or the toast's Undo button.
    Undo,
    /// Ctrl+1–9: opens that Home tile.
    Jump(usize),
    /// GIF links pasted into the picker.
    AddLinks(Vec<String>),
    /// Files pasted or dropped into the picker.
    AddFiles(Vec<PathBuf>),
    /// Tries again to load a GIF that failed.
    Retry(GifId),
    Import(discord::Source),
    SetBotToken(String),
    Set(Setting),
    /// Saves a copy of the library to Documents.
    Backup,
    CopyDiagnostics,
    Hide,
    Quit,
}

/// A short message in the bottom-right corner, with an Undo button when `undo` isn't empty.
pub struct Toast {
    pub msg: String,
    pub until: f64,
    /// Undone last to first.
    pub undo: Vec<Undo>,
}

/// One change an Undo reverses.
pub enum Undo {
    /// Deleted GIFs, to put back.
    Deleted(Vec<Gif>),
    /// GIFs just added, to delete again.
    Added(Vec<GifId>),
    /// These GIFs were put in a collection (`on`) or taken out.
    Filed { gifs: Vec<GifId>, col: CollectionId, on: bool },
    /// A deleted collection, its GIFs and its place in the Home order.
    Collection { col: Collection, members: Vec<GifId>, at: usize },
}

pub struct App {
    ctx: egui::Context,
    pub lib: Library,
    pub shared: Arc<Shared>,
    media: Media,
    rx: Receiver<Msg>,
    tx: Sender<Msg>,
    /// Renews expired Discord links; `None` without a (working) bot token.
    renewer: Option<renew::Renewer>,
    /// Each GIF's textures and how loading it is going.
    loads: loads::Loads<TextureHandle, Arc<Anim>>,
    /// Where GIF files added from the PC are kept (see `files`).
    files: PathBuf,
    pub prefs: prefs::Prefs,
    pub view: View,
    pub query: String,
    /// The GIFs of the current view and search, in display order.
    pub list: Vec<GifId>,
    pub home: Vec<HomeTile>,
    /// `list` and `home` need recomputing.
    pub dirty: bool,
    /// Hidden collections (and their GIFs everywhere) are shown while this is on.
    pub reveal: bool,
    /// Home tile order: Recent, Copied, Discord imports, Unsorted and every collection, each once.
    order: Vec<View>,
    history: views::History,
    /// GIFs picked with Shift+click, to change their collections at once. Kept across searches.
    pub picked: Vec<GifId>,
    pub toast: Option<Toast>,
    /// Frames drawn, which age textures out.
    pub frame: u64,

    pub grid: Grid,
    pub win: WindowState,
    pub editing: HomeEdit,
    pub form: SettingsForm,
    /// The one-time "you're all set" card shown after installing.
    pub welcome: bool,
    /// False when Ctrl+Shift+G couldn't be registered (usually another app has it).
    pub hotkey_ok: bool,
    soak: Option<soak::Soak>,

    // Kept alive for the hotkey and tray icon to keep working.
    _hotkeys: Option<GlobalHotKeyManager>,
    _tray: Option<TrayIcon>,
}

/// The grid's layout and scrolling, kept between frames.
pub struct Grid {
    /// The keyboard's place in `App::list`.
    pub selected: Option<usize>,
    /// GIFs being dragged onto a collection.
    pub filing: Option<Vec<GifId>>,
    /// Masonry tile rects for `App::list`, relative to the top of the grid.
    pub tiles: Vec<egui::Rect>,
    pub scroll_off: f32,
    pub view_h: f32,
    pub scroll_to: Option<f32>,
    /// How wide a tile is on screen, in pixels, which animations are shrunk to fit.
    pub tile_px: u32,
}

impl Default for Grid {
    fn default() -> Self {
        Self {
            selected: None,
            filing: None,
            tiles: Vec::new(),
            scroll_off: 0.0,
            view_h: 0.0,
            scroll_to: None,
            tile_px: 220,
        }
    }
}

/// The window and keyboard focus.
pub struct WindowState {
    /// Put away by `hide` and not opened since: frames are skipped until it is.
    pub hidden: bool,
    pub had_focus: bool,
    pub focus_search: bool,
    /// Re-hide the window eframe shows after its first frame (see `Startup::start_hidden`).
    pub start_hidden: bool,
    /// Cloaked at launch until frames are on screen, so it doesn't flash white (see `uncloak_when_drawn`).
    pub cloaked: bool,
    /// Ctrl+V was down last frame (see `ui::paste_keys`).
    pub paste_held: bool,
}

impl WindowState {
    fn new(start_hidden: bool) -> Self {
        Self {
            hidden: false,
            had_focus: false,
            focus_search: true,
            start_hidden,
            cloaked: !start_hidden,
            paste_held: false,
        }
    }
}

/// Collections being made, renamed or moved on Home.
#[derive(Default)]
pub struct HomeEdit {
    pub creating: bool,
    pub new_name: String,
    /// Focus the new collection's name field next frame.
    pub focus_new: bool,
    pub renaming: Option<(CollectionId, String)>,
    /// The Home tile being dragged.
    pub drag: Option<View>,
}

/// What the Settings page is typing or showing.
#[derive(Default)]
pub struct SettingsForm {
    pub import: ImportStatus,
    /// The Discord token being typed. Taken (and so cleared) when the import starts; never saved.
    pub token: String,
    pub token_open: bool,
    /// The bot token being typed, until it's saved.
    pub bot_token: String,
    /// Why Discord links couldn't be renewed the last time.
    pub renew_error: Option<String>,
    /// Result of the last "Back up" click: the saved file, or what went wrong.
    pub backup: Option<Result<PathBuf, String>>,
}

impl App {
    #[cfg(test)]
    pub(crate) fn for_test(db: crate::db::Db) -> Self {
        let files = std::env::temp_dir().join(format!("gifly-app-test-{}", std::process::id()));
        Self::base(egui::Context::default(), Library::load(db).unwrap(), Media::for_test(), files, false)
    }

    /// An App with nothing running yet: `new` adds the window, background threads and hooks.
    fn base(ctx: egui::Context, lib: Library, media: Media, files: PathBuf, start_hidden: bool) -> Self {
        let (tx, rx) = unbounded();
        let prefs = prefs::Prefs::load(lib.db());
        let order = views::parse_order(&lib.db().setting(Key::HomeOrder).unwrap_or_default());
        Self {
            ctx,
            lib,
            shared: Arc::default(),
            media,
            rx,
            tx,
            renewer: None,
            loads: loads::Loads::default(),
            files,
            prefs,
            view: View::Home,
            query: String::new(),
            list: Vec::new(),
            home: Vec::new(),
            dirty: true,
            reveal: false,
            order,
            history: views::History::default(),
            picked: Vec::new(),
            toast: None,
            frame: 0,
            grid: Grid::default(),
            win: WindowState::new(start_hidden),
            editing: HomeEdit::default(),
            form: SettingsForm::default(),
            welcome: false,
            hotkey_ok: true,
            soak: None,
            _hotkeys: None,
            _tray: None,
        }
    }

    pub fn new(cc: &eframe::CreationContext<'_>, start: Startup) -> Self {
        let Startup { lib, cache, files, start_hidden, tray_icon } = start;
        log::set_detailed(lib.db().flag(Key::DetailedLog, false));
        log::info(
            Area::App,
            format!(
                "gifly v{} started on Windows {}, {} GIFs",
                env!("CARGO_PKG_VERSION"),
                platform::windows_version(),
                lib.gifs().len()
            ),
        );
        files::remove_unused(&files, lib.gifs());
        // Finish before an import can create a new copy in the same temporary folder.
        discord::remove_leftovers();
        let ctx = cc.egui_ctx.clone();
        let hwnd = platform::Hwnd::of(cc);
        platform::round_corners(hwnd);
        if !start_hidden {
            platform::cloak(hwnd, true);
        }

        let media = Media::new(ctx.clone(), cache, files.clone());
        let mut app = Self::base(ctx.clone(), lib, media, files, start_hidden);
        let (db, prefs, shared) = (app.lib.db(), &app.prefs, &app.shared);
        shared.hwnd.set(hwnd);
        // Re-written every launch so the entry follows the exe if it moves. Debug builds leave
        // it alone, so `cargo run` doesn't point sign-in at target\debug.
        if !cfg!(debug_assertions) {
            let _ = platform::set_autostart(prefs.autostart);
        }
        if start_hidden {
            platform::trim_memory_soon();
        }
        let c = ctx.clone();
        crate::update::spawn(hwnd, ctx.clone(), move || system::request_quit(&c));
        ctx.set_zoom_factor(prefs.zoom);
        shared.set_capture(prefs.capture);

        let hotkeys = system::register_hotkey(shared.clone(), ctx.clone());
        app._tray = system::build_tray(tray_icon, shared.clone(), ctx.clone());
        clipboard::spawn_watcher(shared.clone(), app.tx.clone(), ctx.clone());
        system::listen_for_relaunch(shared.clone(), ctx.clone());
        app.renewer = db
            .setting(Key::BotToken)
            .filter(|t| !t.is_empty())
            .map(|t| renew::Renewer::spawn(t, app.media.focus.clone(), app.tx.clone(), ctx));
        app.welcome = crate::update::first_run();
        app.hotkey_ok = hotkeys.is_some();
        app._hotkeys = hotkeys;
        app.soak = soak::Soak::from_env();
        if app.prefs.hide_gone {
            app.find_gone();
        }
        app
    }

    /// Handles everything background threads sent since the last frame.
    pub fn pump(&mut self) {
        if system::quit_requested() {
            self.quit();
        }
        let mut loaded = false;
        while let Some(e) = self.media.next_event() {
            loaded |= matches!(e, Event::Thumb { .. } | Event::Anim { .. });
            match e {
                Event::Thumb { id, ticket, result } => self.thumb_loaded(id, ticket, result),
                Event::Anim { id, ticket, result } => self.anim_loaded(id, ticket, result),
                Event::Dropped { id, ticket, kind } => self.loads.dropped(id, ticket, kind),
                Event::Expired { id, ticket, kind, grid } => self.link_expired(id, ticket, kind, grid),
            }
        }
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Captured(url) => self.captured(url),
                Msg::Import(res) => self.finish_import(res),
                Msg::RenewalsDropped { renewer, ids } => self.renewals_dropped(renewer, ids),
                Msg::Renewed { renewer, asked, result } => self.renewed(renewer, asked, result),
                Msg::FilesStored { items, skipped, view } => self.files_stored(items, skipped, view),
            }
        }
        // Loads that finish after hiding are trimmed too. Views are recomputed when drawn.
        if self.win.hidden && loaded {
            self.shrink_textures();
            self.paint_while_hidden();
        }
    }

    pub fn apply(&mut self, a: Action) {
        match a {
            Action::Send { id, invert } => self.send(id, invert),
            Action::Open(v) => self.navigate(v),
            Action::Back => self.step_history(true),
            Action::Forward => self.step_history(false),
            Action::SetMembership { gifs, col, on } => self.set_membership(&gifs, col, on),
            Action::NewCollection(name, gifs) => self.new_collection(&name, &gifs),
            Action::RenameCollection(id, name) => self.rename_collection(id, &name),
            Action::DeleteCollection(id) => self.delete_collection(id),
            Action::MoveTile(v, target) => self.move_tile(v, target),
            Action::SaveOrder => self.save_order(),
            Action::HideCollection { id, hidden } => self.hide_collection(id, hidden),
            Action::Reveal => self.toggle_reveal(),
            Action::DeleteGifs(ids) => self.delete_gifs(&ids),
            Action::CopyLink(id) => self.copy_link(id),
            Action::Pick { id, range } => self.pick(id, range),
            Action::PickAll => self.pick_all(),
            Action::ClearPicks => self.picked.clear(),
            Action::Undo => self.undo(),
            Action::Jump(n) => self.jump(n),
            Action::AddLinks(links) => self.add_links(links),
            Action::AddFiles(paths) => self.add_files(paths),
            Action::Retry(id) => {
                if let Some(gi) = self.lib.index(id) {
                    self.retry_load(gi);
                }
            }
            Action::Import(source) => self.start_import(source),
            Action::SetBotToken(token) => self.set_bot_token(&token),
            Action::Set(s) => self.set(s),
            Action::Backup => self.backup(),
            Action::CopyDiagnostics => self.copy_diagnostics(),
            Action::Hide => self.hide(),
            Action::Quit => self.quit(),
        }
    }

    pub fn toast(&mut self, msg: impl Into<String>) {
        self.toast_undo(msg, Vec::new());
    }

    /// A toast whose Undo button (or Ctrl+Z) reverses `undo`. Stays longer, to leave time for it.
    pub fn toast_undo(&mut self, msg: impl Into<String>, undo: Vec<Undo>) {
        let secs = if undo.is_empty() { 2.5 } else { 5.0 };
        let until = self.ctx.input(|i| i.time) + secs;
        self.toast = Some(Toast { msg: msg.into(), until, undo });
    }
}
