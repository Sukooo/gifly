//! "Copy diagnostics" in Settings: what it takes to look into a problem someone reports, minus
//! their tokens and the signatures of their Discord links.

use std::fmt::Write;

use crate::{log, platform};

use super::App;

/// How many log lines go in.
const LOG_LINES: usize = 200;

impl App {
    pub fn diagnostics(&self) -> String {
        let mut s = String::new();
        let imported = self.lib.gifs().iter().filter(|g| g.imported).count();
        let yes = |on: bool| if on { "on" } else { "off" };
        let _ = writeln!(s, "gifly v{} on Windows {}", env!("CARGO_PKG_VERSION"), platform::windows_version());
        let _ = writeln!(
            s,
            "settings: send after paste {}, save copied links {}, hide deleted {}, play on hover {}, \
             start with Windows {}, size {:.0}%, hotkey {}, bot token {}, detailed log {}",
            yes(self.prefs.auto_send),
            yes(self.prefs.capture),
            yes(self.prefs.hide_gone),
            yes(self.prefs.hover_play),
            yes(self.prefs.autostart),
            self.prefs.zoom * 100.0,
            if self.hotkey_ok { "ok" } else { "taken" },
            if self.has_bot_token() { "saved" } else { "none" },
            yes(log::detailed()),
        );
        let _ = writeln!(
            s,
            "library: {} GIFs, {imported} from Discord, {} collections",
            self.lib.gifs().len() - imported,
            self.lib.collections().len()
        );
        let l = &self.loads;
        let (thumbs, anims) = l.loaded();
        let _ = writeln!(
            s,
            "loaded: {thumbs} thumbnails, {anims} animations, {} gone, {} waiting on a link",
            l.gone.len(),
            l.renewing.len(),
        );
        let _ = writeln!(s, "failed: {:?}", l.failures());
        let _ = writeln!(s, "cache: {} MB", self.media.cache_size() >> 20);
        let _ = writeln!(s, "{}", crate::stats::summary());
        if let Some(e) = &self.form.renew_error {
            let _ = writeln!(s, "renewal error: {e}");
        }
        let _ = write!(s, "\nlast {LOG_LINES} log lines:\n{}", log::tail(LOG_LINES));
        log::redact(&s).into_owned()
    }
}
