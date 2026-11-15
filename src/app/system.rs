//! The pieces that run outside the window: the global hotkey and the tray icon, plus the state
//! they share with the UI thread.

use std::sync::{
    Arc, Mutex, MutexGuard,
    atomic::{AtomicBool, Ordering::Relaxed},
};

use eframe::egui;
use global_hotkey::{
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
    hotkey::{Code, HotKey, Modifiers},
};
use tray_icon::{
    MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuEvent, MenuItem},
};

use crate::{
    log::{self, Area},
    platform, update, util,
};

pub const HOTKEY_LABEL: &str = "Ctrl + Shift + G";
/// How many of our own clipboard writes are remembered (see `Shared::note_self_write`).
const SELF_WRITES: usize = 8;

/// State touched from the hotkey, tray and clipboard threads.
#[derive(Default)]
pub struct Shared {
    pub(super) hwnd: platform::AtomicHwnd,
    /// The window that was in front before the picker opened: where a GIF gets pasted.
    pub(super) paste_target: platform::AtomicHwnd,
    pub(super) just_shown: AtomicBool,
    /// Set by the hotkey so the UI thread hides the picker through `App::hide`.
    pub(super) hide_requested: AtomicBool,
    /// Mirrors the "Save GIF links I copy" setting for the clipboard watcher.
    pub(super) capture: AtomicBool,
    self_writes: Mutex<Vec<String>>,
}

impl Shared {
    fn show(&self, ctx: &egui::Context) {
        let hwnd = self.hwnd.get();
        let fg = platform::foreground();
        if fg != hwnd {
            self.paste_target.set(fg);
        }
        self.just_shown.store(true, Relaxed);
        update::touch();
        platform::show_at_cursor(hwnd);
        ctx.request_repaint();
    }

    fn toggle(&self, ctx: &egui::Context) {
        let hwnd = self.hwnd.get();
        if platform::is_visible(hwnd) && platform::foreground() == hwnd {
            self.hide_requested.store(true, Relaxed);
            ctx.request_repaint();
        } else {
            self.show(ctx);
        }
    }

    /// True once after each hotkey or tray open.
    pub fn take_just_shown(&self) -> bool {
        self.just_shown.swap(false, Relaxed)
    }

    /// True once after the hotkey asks to close the picker.
    pub fn take_hide_requested(&self) -> bool {
        self.hide_requested.swap(false, Relaxed)
    }

    /// Remembers clipboard text we wrote ourselves so the watcher doesn't capture it.
    pub(super) fn note_self_write(&self, s: &str) {
        let mut v = self.self_writes();
        if !self.capture.load(Relaxed) {
            return;
        }
        v.push(s.trim().to_string());
        if v.len() > SELF_WRITES {
            v.remove(0);
        }
    }

    pub(super) fn take_self_write(&self, s: &str) -> bool {
        let mut v = self.self_writes();
        v.iter().position(|w| w == s).map(|i| v.remove(i)).is_some()
    }

    pub(super) fn set_capture(&self, on: bool) {
        let mut v = self.self_writes();
        self.capture.store(on, Relaxed);
        v.clear();
    }

    fn self_writes(&self) -> MutexGuard<'_, Vec<String>> {
        // A list of strings can't be left half-updated, so a panic elsewhere doesn't matter.
        util::lock(&self.self_writes)
    }
}

/// Opens the picker when gifly is started again while it's running (from the Start menu, say),
/// the same way the tray does, so the window to paste into is the one in front at that moment.
pub(super) fn listen_for_relaunch(shared: Arc<Shared>, ctx: egui::Context) {
    platform::on_relaunch(move || shared.show(&ctx));
}

/// Registers Ctrl+Shift+G to toggle the picker. `None` if another app already has it.
pub(super) fn register_hotkey(shared: Arc<Shared>, ctx: egui::Context) -> Option<GlobalHotKeyManager> {
    let registered = GlobalHotKeyManager::new().map_err(|e| e.to_string()).and_then(|m| {
        m.register(HotKey::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyG))
            .map(|()| m)
            .map_err(|e| e.to_string())
    });
    GlobalHotKeyEvent::set_event_handler(Some(move |e: GlobalHotKeyEvent| {
        if e.state == HotKeyState::Pressed {
            shared.toggle(&ctx);
        }
    }));
    registered.inspect_err(|e| log::warn(Area::App, format!("hotkey: {e}"))).ok()
}

static QUIT: AtomicBool = AtomicBool::new(false);

/// Asks the UI thread to quit (see `App::quit`), from any thread.
pub(super) fn request_quit(ctx: &egui::Context) {
    QUIT.store(true, Relaxed);
    // Drawn even while hidden, so it's seen right away.
    ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaintWhileHidden);
    ctx.request_repaint();
}

pub(super) fn quit_requested() -> bool {
    QUIT.load(Relaxed)
}

/// The tray icon: left-click opens the picker; the menu has Open and Quit.
pub(super) fn build_tray((rgba, size): (Vec<u8>, u32), shared: Arc<Shared>, ctx: egui::Context) -> Option<TrayIcon> {
    let menu = Menu::new();
    let open = MenuItem::with_id("open", format!("Open  ({HOTKEY_LABEL})"), true, None);
    let quit = MenuItem::with_id("quit", "Quit gifly", true, None);
    menu.append_items(&[&open, &quit]).ok()?;
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_tooltip("gifly")
        .with_icon(tray_icon::Icon::from_rgba(rgba, size, size).ok()?)
        .build()
        .ok()?;
    {
        let (shared, ctx) = (shared.clone(), ctx.clone());
        TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                shared.show(&ctx);
            }
        }));
    }
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| match e.id.0.as_str() {
        "quit" => request_quit(&ctx),
        "open" => shared.show(&ctx),
        _ => {}
    }));
    Some(tray)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_changes_discard_old_clipboard_markers() {
        let shared = Shared::default();
        shared.note_self_write("off");
        shared.set_capture(true);
        assert!(!shared.take_self_write("off"));
        shared.note_self_write("on");
        assert!(shared.take_self_write("on"));
        shared.note_self_write("old");
        shared.set_capture(false);
        shared.note_self_write("off again");
        shared.set_capture(true);
        assert!(!shared.take_self_write("old") && !shared.take_self_write("off again"));
    }
}
