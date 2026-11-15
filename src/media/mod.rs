//! Background thumbnail + animation loading.
//!
//! A small worker pool (`queue`) takes the job nearest the grid's view first (in view, then the
//! rows around it), and drops jobs the view has left far behind. Each job reads its file from the
//! disk cache (`cache`) or downloads it (`fetch`), then decodes it (`decode`) into textures that
//! are uploaded on the worker thread and handed back over a channel.

mod cache;
mod decode;
mod fetch;
mod queue;

use std::{
    fs,
    path::PathBuf,
    sync::{Arc, PoisonError},
    time::Duration,
};

use crossbeam_channel::{Receiver, Sender, unbounded};
use eframe::egui::{self, TextureHandle};

use crate::{
    db::GifId,
    log::{self, Area},
    util::lock,
};

/// The biggest GIF gifly downloads or adds from a file.
pub const MAX_BYTES: u64 = 25 << 20;

pub use decode::Anim;
pub use queue::{Focus, Span};

use cache::{
    CACHE_MAX, DOWNLOADS, PREVIEWS, PRUNE_EVERY, big_path, dead_path, needs_renewing, prune, recently_dead, size,
    thumb_path,
};
use decode::{load_anim, load_thumb, thumb_image};
use fetch::UA;
use queue::{Queue, next_job, priority};

pub const THUMB_PX: u32 = 300;
const WORKERS: usize = 8;
const MAX_QUEUE: usize = 160;

/// Tells a load apart from earlier tries at the same GIF, so a stale answer can be ignored.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Ticket(pub u64);

pub struct Job {
    pub id: GifId,
    /// Matches the result to the load attempt that asked for it.
    pub ticket: Ticket,
    pub url: String,
    pub media_link: Option<String>,
    /// The GIF's size as far as the library knows (0 if not): only its shape is relied on.
    pub size: (u16, u16),
    /// How wide its tile is on screen, in pixels: animations are shrunk to fit it.
    pub px: u32,
    pub kind: Kind,
    /// For a tile in the grid, which loads nearest the view first; loads outside it go first.
    pub grid: bool,
}

impl Job {
    /// Reports that this job won't run after all, so the app asks again if it still wants it.
    fn dropped(&self) -> Event {
        Event::Dropped { id: self.id, ticket: self.ticket, kind: self.kind }
    }
}

/// What a job loads. Between jobs equally near the view, the first kind goes first.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Kind {
    Thumb,
    Anim,
    /// Only fill the disk cache, ahead of the GIF scrolling into view.
    Warm,
}

/// Why a GIF couldn't be loaded.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Fail {
    /// The server says its file is gone, and its link was still good.
    Gone,
    /// Its Discord link expired and couldn't be renewed.
    Expired,
    /// The server couldn't be reached, or answered with an error that may pass.
    Network(String),
    /// What came back isn't an image (a video, or a page without a preview).
    NotImage,
    /// It's too big to decode.
    TooBig,
    /// It's a still image, so it has no animation.
    Still,
}

impl std::fmt::Display for Fail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Gone => "Its file was deleted from where it was posted.",
            Self::Expired => "Its Discord link expired.",
            Self::Network(e) => return write!(f, "Couldn't download it: {e}."),
            Self::NotImage => "It's not an image gifly can show.",
            Self::TooBig => "It's too big to show.",
            Self::Still => "It's a still image.",
        })
    }
}

/// What the workers report back, read with `Media::next_event` on the UI thread. Each names
/// the GIF and the ticket of the `Job` it's about.
pub enum Event {
    Thumb {
        id: GifId,
        ticket: Ticket,
        result: Result<TextureHandle, Fail>,
    },
    Anim {
        id: GifId,
        ticket: Ticket,
        result: Result<Arc<Anim>, Fail>,
    },
    /// A load stopped before it finished: the view left it far behind, or its link expired.
    Dropped {
        id: GifId,
        ticket: Ticket,
        kind: Kind,
    },
    /// Its Discord link needs renewing before it can load.
    Expired {
        id: GifId,
        ticket: Ticket,
        kind: Kind,
        grid: bool,
    },
}

/// Why a job ended without a result.
enum Stop {
    /// It's not wanted anymore: the view left it far behind, or its link expired while it
    /// waited. Its GIF asks for it again when it needs it.
    Unwanted,
    Failed(Fail),
}

impl From<Fail> for Stop {
    fn from(f: Fail) -> Self {
        Self::Failed(f)
    }
}

pub struct Media {
    queue: Queue,
    tx: Sender<Event>,
    rx: Receiver<Event>,
    dir: PathBuf,
    pub focus: Focus,
}

struct Ctx {
    egui: egui::Context,
    dir: PathBuf,
    /// GIFs added from files on the PC (see `app::files`).
    files: PathBuf,
    http: ureq::Agent,
    focus: Focus,
}

impl Media {
    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        let (tx, rx) = unbounded();
        Self {
            queue: Arc::default(),
            tx,
            rx,
            dir: std::env::temp_dir().join(format!("gifly-media-test-{}", std::process::id())),
            focus: Focus::default(),
        }
    }

    pub fn new(egui: egui::Context, dir: PathBuf, files: PathBuf) -> Self {
        let (tx, rx) = unbounded();
        for sub in [PREVIEWS, DOWNLOADS] {
            let _ = fs::create_dir_all(dir.join(sub));
        }
        let http: ureq::Agent =
            ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(15))).user_agent(UA).build().into();
        let queue: Queue = Arc::default();
        let focus = Focus::default();
        for i in 0..WORKERS {
            let (queue, tx) = (queue.clone(), tx.clone());
            let cx = Ctx {
                egui: egui.clone(),
                dir: dir.clone(),
                files: files.clone(),
                http: http.clone(),
                focus: focus.clone(),
            };
            crate::spawn(&format!("media-{i}"), move || worker(&queue, &tx, &cx));
        }
        let cache = dir.clone();
        crate::spawn("cache-prune", move || {
            loop {
                let freed = prune(&cache, CACHE_MAX);
                if freed > 0 {
                    log::info(Area::Cache, format!("pruned {} MB", freed >> 20));
                }
                std::thread::sleep(PRUNE_EVERY);
            }
        });
        Self { queue, tx, rx, dir, focus }
    }

    pub fn next_event(&self) -> Option<Event> {
        self.rx.try_recv().ok()
    }

    pub fn request(&self, job: Job) {
        // Checked here rather than on a worker, so a screenful of expired links is reported in
        // the same frame and renewed in one request.
        if needs_renewing(&self.dir, &job.url, job.media_link.as_deref(), job.kind == Kind::Anim) {
            log::debug(Area::Renew, || format!("gif {} needs a fresh link", job.id));
            let _ = self.tx.send(Event::Expired { id: job.id, ticket: job.ticket, kind: job.kind, grid: job.grid });
            return;
        }
        let (jobs, cv) = &*self.queue;
        let mut jobs = lock(jobs);
        jobs.push_front(job);
        // Over the cap, the job furthest from the view goes.
        let focus = self.focus.get();
        while jobs.len() > MAX_QUEUE {
            let far = jobs.iter().enumerate().max_by_key(|(_, j)| priority(&focus, j)).map(|(i, _)| i);
            if let Some(j) = far.and_then(|i| jobs.remove(i)) {
                let _ = self.tx.send(j.dropped());
            }
        }
        cv.notify_one();
    }

    /// Downloads a GIF and saves its thumbnail without loading it. False if the queue is full.
    pub fn warm(&self, job: Job) -> bool {
        let (jobs, cv) = &*self.queue;
        let mut jobs = lock(jobs);
        if jobs.len() >= MAX_QUEUE {
            return false;
        }
        jobs.push_front(job);
        cv.notify_one();
        true
    }

    /// The queue is full, so `warm` would turn a job away.
    pub fn busy(&self) -> bool {
        lock(&self.queue.0).len() >= MAX_QUEUE
    }

    /// The GIF's file is known to be gone: it was missing on its last try, with a link that
    /// was still good. Matches when loading it would fail with `Fail::Gone`. Usually a single file
    /// lookup: the link is only checked when there's a dead marker.
    pub fn is_gone(&self, url: &str, media: Option<&str>) -> bool {
        recently_dead(&self.dir, url) && !needs_renewing(&self.dir, url, media, false)
    }

    /// The GIF's still thumbnail would need a fresh Discord link to load (see `needs_renewing`).
    pub fn needs_renewing(&self, url: &str, media: Option<&str>) -> bool {
        needs_renewing(&self.dir, url, media, false)
    }

    /// Lets a link that failed before be fetched again right away.
    pub fn retry(&self, url: &str) {
        let _ = fs::remove_file(dead_path(&self.dir, url));
        let _ = fs::remove_file(big_path(&self.dir, url));
    }

    pub fn cache_size(&self) -> u64 {
        size(&self.dir)
    }
}

fn worker(queue: &Queue, tx: &Sender<Event>, cx: &Ctx) {
    loop {
        let job = {
            let (jobs, cv) = &**queue;
            let mut jobs = lock(jobs);
            loop {
                if let Some(j) = next_job(&mut jobs, &cx.focus.get(), tx) {
                    break j;
                }
                jobs = cv.wait(jobs).unwrap_or_else(PoisonError::into_inner);
            }
        };
        let msg = match job.kind {
            Kind::Warm if thumb_path(&cx.dir, &job.url).exists() => continue,
            // Warming only fills the cache: there's nothing to report unless it should be asked
            // for again.
            Kind::Warm => match thumb_image(cx, &job) {
                Err(Stop::Unwanted) => job.dropped(),
                _ => continue,
            },
            Kind::Anim => outcome(&job, load_anim(cx, &job).map(Arc::new), |id, ticket, result| Event::Anim {
                id,
                ticket,
                result,
            }),
            Kind::Thumb => {
                outcome(&job, load_thumb(cx, &job), |id, ticket, result| Event::Thumb { id, ticket, result })
            }
        };
        let _ = tx.send(msg);
        cx.egui.request_repaint();
    }
}

/// The event reporting how `job` went.
fn outcome<T>(job: &Job, res: Result<T, Stop>, msg: fn(GifId, Ticket, Result<T, Fail>) -> Event) -> Event {
    match res {
        Ok(v) => msg(job.id, job.ticket, Ok(v)),
        Err(Stop::Failed(f)) => {
            let line = || {
                format!(
                    "gif {} {:?}: {f:?}, {}",
                    job.id,
                    job.kind,
                    crate::urls::for_log(job.media_link.as_deref().unwrap_or(&job.url))
                )
            };
            // A deleted file is worth knowing about; the rest are usually passing or expected.
            if f == Fail::Gone {
                log::info(Area::Net, line());
            } else {
                log::debug(Area::Net, line);
            }
            msg(job.id, job.ticket, Err(f))
        }
        Err(Stop::Unwanted) => job.dropped(),
    }
}
