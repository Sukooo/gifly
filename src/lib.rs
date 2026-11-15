//! gifly: a Discord-style GIF picker that lives in the Windows tray.
//!
//! `main.rs` only calls [`run`]. How the rest fits together:
//!
//! - [`app`] owns the state. The UI hands it `Action`s to apply after each frame, and
//!   background threads send it `Msg`s, which `App::pump` handles on the UI thread.
//! - [`ui`] draws that state with egui. Library changes go through `Action`s; view state, like
//!   the search text, it sets directly.
//! - [`db`] is the SQLite library: GIFs, collections, settings, and the migrations between
//!   versions of it. [`library`] mirrors its GIFs and collections in memory, saving each change
//!   first.
//! - [`media`] loads thumbnails and animations on a pool of worker threads, nearest the view
//!   first, through a disk cache.
//! - [`discord`] reads favorite GIFs from Discord's own files, or from its API, and renews
//!   expired Discord links.
//! - [`platform`] is the Win32 the window, clipboard and paste need. [`urls`], [`log`],
//!   [`stats`], [`clock`], [`update`] and [`util`] are the small pieces around them.

// The modules are private (nothing here is a library for other crates), so the docs are built
// with `--document-private-items`, where the links above work.
#![allow(rustdoc::private_intra_doc_links)]

mod app;
mod clock;
mod db;
mod discord;
mod library;
mod log;
mod media;
mod platform;
mod stats;
mod ui;
mod update;
mod urls;
mod util;

use std::path::Path;

use eframe::egui;

/// Starts gifly, or asks the copy that's already running to open its picker.
pub fn run() -> eframe::Result {
    log::catch_panics();
    update::handle_hooks();
    if platform::already_running() {
        platform::open_existing();
        return Ok(());
    }

    let Some(dirs) = directories::ProjectDirs::from("", "", "gifly") else {
        log::fatal("Couldn't find your user folder.");
    };
    // Debug builds can run against a scratch library and cache: GIFLY_DIR=<folder>.
    let (data, cache) = match std::env::var_os("GIFLY_DIR").filter(|_| cfg!(debug_assertions)) {
        Some(d) => (Path::new(&d).join("data"), Path::new(&d).join("cache")),
        None => (dirs.data_dir().to_path_buf(), dirs.cache_dir().to_path_buf()),
    };
    let _ = std::fs::create_dir_all(&data);
    let path = data.join("gifly.db");
    let fail = |e: rusqlite::Error, hint: &str| -> ! {
        log::fatal(&format!("Couldn't open your library at {}.{hint}\n\n{e}", path.display()))
    };
    let db = db::Db::open(&path).unwrap_or_else(|e| fail(e, ""));
    let newer = if db.is_newer_than_app() { " A newer gifly last used it: update gifly to open it." } else { "" };
    // Loaded here so a broken library stops gifly instead of showing up empty.
    let start_hidden = std::env::args().any(|a| a == "--hidden");
    let options = window_options(saved_size(&db), start_hidden);
    let lib = library::Library::load(db).unwrap_or_else(|e| fail(e, newer));
    let files = data.join("files");
    let tray_icon = ui::icon_rgba(32);
    let start = app::Startup { lib, cache, files, start_hidden, tray_icon };
    eframe::run_native(
        platform::TITLE,
        options,
        Box::new(move |cc| {
            ui::style(&cc.egui_ctx);
            Ok(Box::new(app::App::new(cc, start)))
        }),
    )
}

/// Starts a thread with a name, which crash reports and logs show.
fn spawn(name: &str, f: impl FnOnce() + Send + 'static) {
    if let Err(e) = std::thread::Builder::new().name(name.into()).spawn(f) {
        log::error(log::Area::App, format!("couldn't start the {name} thread: {e}"));
    }
}

/// The window size saved when the picker last closed.
fn saved_size(db: &db::Db) -> [f32; 2] {
    db.setting(db::Key::Size)
        .and_then(|s| {
            let (w, h) = s.split_once(' ')?;
            Some([w.parse().ok()?, h.parse().ok()?])
        })
        .unwrap_or([460.0, 500.0])
}

/// A borderless, always-on-top window that stays off the taskbar.
fn window_options(size: [f32; 2], start_hidden: bool) -> eframe::NativeOptions {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(platform::TITLE)
        .with_inner_size(size)
        .with_min_inner_size([320.0, 340.0])
        .with_max_inner_size([2000.0, 1600.0])
        .with_decorations(false)
        .with_resizable(true)
        .with_always_on_top()
        .with_taskbar(false)
        .with_visible(!start_hidden);
    if start_hidden {
        // eframe shows the window after its first frame no matter what, so make that showing
        // off-screen and unfocused; the app hides it again right after.
        viewport = viewport.with_position([-32000.0, -32000.0]).with_active(false);
    }
    eframe::NativeOptions { viewport, renderer: eframe::Renderer::Glow, centered: !start_hidden, ..Default::default() }
}
