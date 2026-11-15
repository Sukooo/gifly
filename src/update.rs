//! Self-updates through Velopack: checks GitHub releases in the background, downloads quietly,
//! and restarts into the new version once the picker has stayed closed for a bit.

use std::{
    sync::atomic::{AtomicBool, AtomicI64, AtomicU8, Ordering::Relaxed},
    thread,
    time::Duration,
};

use eframe::egui;
use velopack::{UpdateCheck, UpdateManager, VelopackApp, sources::GithubSource};

use crate::{
    clock,
    log::{self, Area},
    platform,
};

const REPO: &str = "https://github.com/Sukooo/gifly";
/// How often to look for a new release.
const CHECK_EVERY: Duration = Duration::from_secs(4 * 3600);
/// How often a downloaded update checks whether the picker has gone quiet.
const IDLE_POLL: Duration = Duration::from_secs(15);
/// How long the picker must have stayed closed (since its last show or hide) before restarting.
const QUIET_SECS: i64 = 60;

/// Where the updater is, for the version line in Settings.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Status {
    Idle,
    Downloading,
    /// Downloaded; installs once the picker has been closed for a while.
    Ready,
}

static STATUS: AtomicU8 = AtomicU8::new(Status::Idle as u8);
static LAST_ACTIVE: AtomicI64 = AtomicI64::new(0);
/// Set on the launch right after a fresh install, to greet the user once.
static FIRST_RUN: AtomicBool = AtomicBool::new(false);

pub fn status() -> Status {
    match STATUS.load(Relaxed) {
        s if s == Status::Downloading as u8 => Status::Downloading,
        s if s == Status::Ready as u8 => Status::Ready,
        _ => Status::Idle,
    }
}

/// True on the first launch after installing.
pub fn first_run() -> bool {
    FIRST_RUN.load(Relaxed)
}

/// Marks the picker as in use (called on show and hide, so a send's paste counts too).
pub fn touch() {
    LAST_ACTIVE.store(clock::now(), Relaxed);
}

/// Velopack's install/update/uninstall hooks. Must run first in `main`: for those events
/// Velopack starts the exe with special arguments and expects it to exit right away.
/// Startup auto-apply is off so `spawn` is the only place updates get applied: a second
/// launch (which only wakes the running copy) must not swap files under it.
pub fn handle_hooks() {
    VelopackApp::build()
        .set_auto_apply_on_startup(false)
        .on_before_uninstall_fast_callback(|_| {
            platform::set_autostart(false);
        })
        .on_first_run(|_| FIRST_RUN.store(true, Relaxed))
        .run();
}

/// Starts the background updater, which calls `quit` once an update is ready to install
/// (Velopack installs it after gifly exits). Does nothing when gifly wasn't installed by
/// Velopack (e.g. `cargo run`).
pub fn spawn(hwnd: platform::Hwnd, ctx: egui::Context, quit: impl FnOnce() + Send + 'static) {
    let Ok(um) = UpdateManager::new(GithubSource::new(REPO, None, false), None, None) else { return };
    let set = move |s: Status| {
        STATUS.store(s as u8, Relaxed);
        ctx.request_repaint();
    };
    crate::spawn("update", move || {
        loop {
            match um.check_for_updates() {
                Ok(UpdateCheck::UpdateAvailable(info)) => {
                    set(Status::Downloading);
                    match um.download_updates(&info, None) {
                        Ok(()) => {
                            set(Status::Ready);
                            // Never restart while in use or right after a send (its paste is still running).
                            while platform::is_visible(hwnd) || clock::now() - LAST_ACTIVE.load(Relaxed) < QUIET_SECS {
                                thread::sleep(IDLE_POLL);
                            }
                            match um.wait_exit_then_apply_updates(&info.TargetFullRelease, true, true, ["--hidden"]) {
                                Ok(()) => return quit(),
                                Err(e) => log::warn(Area::Update, format!("apply: {e}")),
                            }
                        }
                        Err(e) => log::warn(Area::Update, format!("download: {e}")),
                    }
                    set(Status::Idle);
                }
                Ok(_) => {}
                Err(e) => log::warn(Area::Update, format!("check: {e}")),
            }
            thread::sleep(CHECK_EVERY);
        }
    });
}
