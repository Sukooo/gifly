//! The log at %LOCALAPPDATA%\gifly\gifly.log, and the one before it in gifly.old.log. The release
//! build has no console, so this is the only trace of crashes and of failures the UI doesn't
//! show. Lines are redacted (see `redact`) and written on a thread of their own, so logging never
//! waits on the disk. Debug lines are only written with "Detailed logging" on in Settings.

use std::{
    borrow::Cow,
    fmt::Display,
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed},
    },
};

use crossbeam_channel::Sender;
use windows::{
    Win32::{
        System::SystemInformation::GetLocalTime,
        UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MB_TOPMOST, MessageBoxW},
    },
    core::HSTRING,
};

/// Past this size the log moves to gifly.old.log and starts over.
const MAX_LEN: u64 = 1 << 20;
const OLD: &str = "gifly.old.log";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
}

/// What part of gifly a line is about.
#[derive(Clone, Copy)]
pub enum Area {
    App,
    Net,
    Cache,
    Renew,
    Paste,
    Clipboard,
    Import,
    Update,
    Db,
}

impl Area {
    fn name(self) -> &'static str {
        match self {
            Self::App => "app",
            Self::Net => "net",
            Self::Cache => "cache",
            Self::Renew => "renew",
            Self::Paste => "paste",
            Self::Clipboard => "clipboard",
            Self::Import => "import",
            Self::Update => "update",
            Self::Db => "db",
        }
    }
}

static DETAILED: AtomicBool = AtomicBool::new(false);
/// Lines sent to the writer thread and not written yet (see `flush`).
static PENDING: AtomicUsize = AtomicUsize::new(0);

/// Turns debug lines on or off ("Detailed logging" in Settings).
pub fn set_detailed(on: bool) {
    DETAILED.store(on, Relaxed);
}

pub fn detailed() -> bool {
    DETAILED.load(Relaxed)
}

pub fn error(area: Area, msg: impl Display) {
    log(Level::Error, area, &msg);
}

pub fn warn(area: Area, msg: impl Display) {
    log(Level::Warn, area, &msg);
}

pub fn info(area: Area, msg: impl Display) {
    log(Level::Info, area, &msg);
}

/// A line only for tracking a problem down: built and written only with detailed logging on
/// (and printed to the console in debug builds).
pub fn debug(area: Area, msg: impl FnOnce() -> String) {
    if detailed() || cfg!(debug_assertions) {
        log(Level::Debug, area, &msg());
    }
}

fn log(level: Level, area: Area, msg: &dyn Display) {
    let label = match level {
        Level::Error => "ERROR",
        Level::Warn => "WARN ",
        Level::Info => "INFO ",
        Level::Debug => "DEBUG",
    };
    let thread = std::thread::current();
    let line = format!("{} {label} {:<9} [{}] {msg}", stamp(), area.name(), thread.name().unwrap_or("?"));
    let line = redact(&line).into_owned();
    if cfg!(debug_assertions) {
        eprintln!("{line}");
    }
    // Tests only print, so they don't fill the real log.
    if (level != Level::Debug || detailed())
        && !cfg!(test)
        && let Some(tx) = writer()
    {
        PENDING.fetch_add(1, Relaxed);
        let _ = tx.send(line);
    }
}

fn stamp() -> String {
    // SAFETY: no arguments.
    let t = unsafe { GetLocalTime() };
    format!(
        "{}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
    )
}

/// The thread that writes lines, started with the first one.
fn writer() -> Option<&'static Sender<String>> {
    static TX: OnceLock<Option<Sender<String>>> = OnceLock::new();
    TX.get_or_init(|| {
        let p = path()?;
        let (tx, rx) = crossbeam_channel::unbounded::<String>();
        let run = move || {
            while let Ok(first) = rx.recv() {
                // Everything sent meanwhile goes in the same write.
                let lines: Vec<String> = std::iter::once(first).chain(rx.try_iter()).collect();
                append(&p, &lines.iter().map(|l| format!("{l}\n")).collect::<String>());
                PENDING.fetch_sub(lines.len(), Relaxed);
            }
        };
        // Not `crate::spawn`, which logs if it fails.
        std::thread::Builder::new().name("log".into()).spawn(run).ok()?;
        Some(tx)
    })
    .as_ref()
}

fn append(p: &Path, text: &str) {
    if let Some(dir) = p.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if fs::metadata(p).is_ok_and(|m| m.len() > MAX_LEN) {
        let _ = fs::rename(p, p.with_file_name(OLD));
    }
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(p) {
        let _ = f.write_all(text.as_bytes());
    }
}

pub fn path() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("gifly").join("gifly.log"))
}

/// The last `n` lines logged, across this log and the one before it.
pub fn tail(n: usize) -> String {
    let Some(p) = path() else { return String::new() };
    let text = [p.with_file_name(OLD), p].iter().filter_map(|p| fs::read_to_string(p).ok()).collect::<String>();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// Blanks the signature of Discord file links (`ex`, `is` and `hm`), which lets anyone open the
/// file until it expires. Everything logged goes through here.
pub fn redact(s: &str) -> Cow<'_, str> {
    const KEYS: [&str; 3] = ["ex=", "is=", "hm="];
    let (mut out, mut from, mut i) = (String::new(), 0, 0);
    let b = s.as_bytes();
    while i < b.len() {
        if matches!(b[i], b'?' | b'&') && KEYS.iter().any(|k| s[i + 1..].starts_with(k)) {
            let start = i + 4;
            let len = s[start..].find(|c: char| matches!(c, '&' | '#' | '"' | '\'' | ')' | '>') || c.is_whitespace());
            out.push_str(&s[from..start]);
            out.push('…');
            from = len.map_or(s.len(), |l| start + l);
            i = from;
        } else {
            i += 1;
        }
    }
    if from == 0 {
        return Cow::Borrowed(s);
    }
    out.push_str(&s[from..]);
    Cow::Owned(out)
}

/// Waits (briefly) for the lines logged so far to be written, before the process ends.
pub fn flush() {
    let until = std::time::Instant::now() + std::time::Duration::from_millis(500);
    while PENDING.load(Relaxed) > 0 && std::time::Instant::now() < until {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// Logs panics and tells the user, since release builds abort right after.
pub fn catch_panics() {
    std::panic::set_hook(Box::new(|info| {
        flush();
        let thread = std::thread::current();
        write_now(&format!("crash in thread {}: {info}", thread.name().unwrap_or("?")));
        alert("gifly crashed", &crash_text("gifly ran into a problem and has to close."));
    }));
}

/// Logs `msg`, shows it, and exits. For problems gifly can't start without fixing.
pub fn fatal(msg: &str) -> ! {
    write_now(&format!("startup failed: {msg}"));
    alert("gifly can't start", &crash_text(msg));
    std::process::exit(1);
}

/// Writes an error line right away, for when the process is about to end.
fn write_now(msg: &str) {
    if let Some(p) = path() {
        let line = format!("{} ERROR app       v{} {msg}\n", stamp(), env!("CARGO_PKG_VERSION"));
        append(&p, &redact(&line));
    }
}

fn crash_text(msg: &str) -> String {
    match path() {
        Some(p) => format!("{msg}\n\nDetails were saved to {}", p.display()),
        None => msg.to_string(),
    }
}

fn alert(title: &str, text: &str) {
    // SAFETY: the strings outlive the call, and no owner window is passed.
    unsafe { MessageBoxW(None, &HSTRING::from(text), &HSTRING::from(title), MB_OK | MB_ICONERROR | MB_TOPMOST) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_blanks_discord_signatures_only() {
        let url = "https://cdn.discordapp.com/attachments/1/2/a.gif?ex=69a1&is=699f&hm=abc123&format=webp";
        assert_eq!(redact(url), "https://cdn.discordapp.com/attachments/1/2/a.gif?ex=…&is=…&hm=…&format=webp");
        let msg = "fetch https://x.com/a.gif?hm=ff -> 404";
        assert_eq!(redact(msg), "fetch https://x.com/a.gif?hm=… -> 404");
        assert!(matches!(redact("this=that, nothing here"), Cow::Borrowed(_)));
    }
}
