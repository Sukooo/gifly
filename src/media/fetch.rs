//! Downloading a GIF's file, once per file at a time, trying its cheapest source first.

use std::{
    cell::Cell,
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{Condvar, Mutex, PoisonError},
    time::Duration,
};

use crate::{
    log::{self, Area},
    stats, urls, util,
};

use super::{
    Ctx, Fail, Job, Stop, THUMB_PX,
    cache::{big_path, dead_path, media_path, needs_renewing, recent, recently_dead, touch},
    lock,
};

pub(super) const UA: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Safari/537.36";

/// Downloaded media, cached on disk. Links that are gone, or too big, get a marker so they aren't
/// refetched every session.
pub(super) fn media_bytes(cx: &Ctx, job: &Job) -> Result<Vec<u8>, Stop> {
    // A file added from the PC is read where gifly keeps it, never cached a second time.
    if let Some(name) = urls::local_name(&job.url) {
        return fs::read(cx.files.join(name)).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => Fail::Gone.into(),
            _ => Fail::Network(e.to_string()).into(),
        });
    }
    let path = media_path(&cx.dir, &job.url);
    if let Ok(b) = fs::read(&path) {
        touch(&path);
        stats::add(&stats::CACHE_HITS, 1);
        return Ok(b);
    }
    if let Some(f) = remembered(cx, &job.url) {
        return Err(f.into());
    }
    // It may have waited in the queue past its link's expiry; it'd only get a 404 now.
    if needs_renewing(&cx.dir, &job.url, job.media_link.as_deref(), true) {
        return Err(Stop::Unwanted);
    }
    let unwanted = Cell::new(false);
    let stop = || {
        let far = cx.focus.get().too_far(job.id, job.grid);
        if far {
            unwanted.set(true);
        }
        far
    };
    // One download per file: a second job for it (a tile scrolled into view while it was being
    // fetched ahead) waits for the first and reads what it saved.
    let Some(_claim) = claim(&path, &stop) else { return Err(Stop::Unwanted) };
    if let Ok(b) = fs::read(&path) {
        stats::add(&stats::CACHE_HITS, 1);
        return Ok(b);
    }
    if let Some(f) = remembered(cx, &job.url) {
        return Err(f.into());
    }
    stats::add(&stats::CACHE_MISSES, 1);
    match fetch_media(&cx.http, job, &stop) {
        Ok(b) => {
            let _ = util::write_atomic(&path, &b);
            Ok(b)
        }
        Err(_) if unwanted.get() => Err(Stop::Unwanted),
        Err(f) => {
            let marker = match f {
                Fail::Gone => Some(dead_path(&cx.dir, &job.url)),
                Fail::TooBig => Some(big_path(&cx.dir, &job.url)),
                _ => None,
            };
            if let Some(m) = marker {
                let _ = fs::write(m, []);
            }
            Err(f.into())
        }
    }
}

/// How the link failed on a recent try, if for good: then it isn't fetched again yet.
fn remembered(cx: &Ctx, url: &str) -> Option<Fail> {
    if recently_dead(&cx.dir, url) {
        Some(Fail::Gone)
    } else if recent(&big_path(&cx.dir, url)) {
        Some(Fail::TooBig)
    } else {
        None
    }
}

/// Cache files being downloaded.
static FETCHING: (Mutex<Vec<PathBuf>>, Condvar) = (Mutex::new(Vec::new()), Condvar::new());

/// Marks `path` as being downloaded until dropped.
struct Claim(PathBuf);

impl Drop for Claim {
    fn drop(&mut self) {
        lock(&FETCHING.0).retain(|p| *p != self.0);
        FETCHING.1.notify_all();
    }
}

/// Waits until no other worker is downloading `path`, then claims it. None if `stop` says the
/// job isn't wanted anymore.
fn claim(path: &Path, stop: &dyn Fn() -> bool) -> Option<Claim> {
    let mut busy = lock(&FETCHING.0);
    while busy.iter().any(|p| p == path) {
        if stop() {
            return None;
        }
        busy = FETCHING.1.wait_timeout(busy, Duration::from_millis(200)).unwrap_or_else(PoisonError::into_inner).0;
    }
    busy.push(path.to_path_buf());
    Some(Claim(path.to_path_buf()))
}

struct Fetch<'a> {
    http: &'a ureq::Agent,
    tried: Vec<String>,
    /// The last failure that might be temporary (timeout, 429, 5xx, or `stop`), so the link
    /// isn't declared dead.
    net: Option<String>,
    /// A source answered "not found", the only answer that marks a link dead.
    missing: bool,
    /// A source's file is over `MAX_BYTES`.
    too_big: bool,
    /// Whether the job is no longer wanted, which ends the fetch even mid-download.
    stop: &'a dyn Fn() -> bool,
}

impl Fetch<'_> {
    fn get(&mut self, url: &str, head: bool) -> Option<Vec<u8>> {
        if (self.stop)() {
            self.net = Some("stopped".into());
            return None;
        }
        let mut req = self.http.get(url);
        if head {
            req = req.header("Range", "bytes=0-15");
        }
        if urls::only_for_link_bots(url) {
            req = req.header("User-Agent", LINK_BOT_UA);
        }
        let res = req.call();
        stats::add(&stats::REQUESTS, 1);
        log::debug(Area::Net, || {
            let status = match &res {
                Ok(r) => r.status().as_u16().to_string(),
                Err(e) => e.to_string(),
            };
            format!("{} {} -> {status}", if head { "peek" } else { "get" }, urls::for_log(url))
        });
        match res {
            Ok(mut res) => {
                if res.body().content_length().is_some_and(|n| n > super::MAX_BYTES) {
                    self.too_big = true;
                    return None;
                }
                // No limit in ureq: `read_body` stops at `MAX_BYTES` and says so.
                let b = read_body(res.body_mut().with_config().limit(u64::MAX).reader(), self.stop);
                stats::add(&stats::DOWNLOADED, b.as_ref().map_or(0, Vec::len));
                match b {
                    Ok(b) => Some(b),
                    Err(Cut::TooBig) => {
                        self.too_big = true;
                        None
                    }
                    Err(Cut::Broke) => {
                        if (self.stop)() {
                            log::debug(Area::Net, || format!("stopped, far from view: {}", urls::for_log(url)));
                        }
                        self.net = Some("the download broke off".into());
                        None
                    }
                }
            }
            // Only "not found" counts as gone; a refusal (403) may just be this client.
            Err(ureq::Error::StatusCode(404 | 410)) => {
                self.missing = true;
                None
            }
            Err(ureq::Error::StatusCode(code)) => {
                self.net = Some(format!("HTTP {code}"));
                None
            }
            Err(e) => {
                self.net = Some(e.to_string());
                None
            }
        }
    }

    /// Fetches `u` once per job, keeping it only if it decodes as an image.
    fn image(&mut self, u: String) -> Option<Vec<u8>> {
        if self.tried.contains(&u) {
            return None;
        }
        let b = self.get(&u, false);
        self.tried.push(u);
        b.filter(|b| image::guess_format(b).is_ok())
    }

    /// Discord's media proxy returns a resized (still animated) file when asked for an exact
    /// size. It never enlarges, so asking for `THUMB_PX` wide only needs the GIF's shape: from the
    /// library, else from the file's header.
    fn discord_small(&mut self, link: &str, size: (u16, u16)) -> Option<Vec<u8>> {
        // In its own format: an animated WebP decodes about 3x slower than the GIF.
        let url = link.replace("format=webp&", "");
        let (w, h) = match size {
            (w, h) if w > 0 && h > 0 => (w, h),
            _ => {
                let Some(head) = self.get(&url, true) else {
                    // Discord answered with an error (usually 404: deleted), so the full file
                    // won't come either. `net` is set only if Discord didn't answer at all.
                    if self.net.is_none() {
                        self.tried.push(link.to_string());
                    }
                    return None;
                };
                if head.len() > 16 {
                    // Range ignored: that's the whole file.
                    self.tried.push(url);
                    return image::guess_format(&head).is_ok().then_some(head);
                }
                if head.len() < 10 || !head.starts_with(b"GIF") {
                    return None;
                }
                // The GIF header: width and height, little-endian, at bytes 6 to 9.
                let size = (u16::from_le_bytes([head[6], head[7]]), u16::from_le_bytes([head[8], head[9]]));
                if size.0 == 0 || size.1 == 0 {
                    return None; // a broken header: no shape to ask for
                }
                size
            }
        };
        let missing = std::mem::take(&mut self.missing);
        let b = self.image(sized(&url, w, h));
        if self.missing {
            // Deleted: the full file won't come either.
            self.tried.push(link.to_string());
        }
        self.missing |= missing;
        b
    }

    /// Why nothing came: gone only if a source said so and none failed for a reason that might
    /// pass (an answer that just isn't an image, like a video, doesn't count).
    fn failure(self) -> Fail {
        match self.net {
            Some(e) => Fail::Network(e),
            None if self.too_big => Fail::TooBig,
            None if self.missing => Fail::Gone,
            None => Fail::NotImage,
        }
    }
}

/// `url` on Discord's media proxy, asked for `THUMB_PX` wide in the shape `w` by `h`.
fn sized(url: &str, w: u16, h: u16) -> String {
    let height = ((u32::from(h) * THUMB_PX + u32::from(w) / 2) / u32::from(w)).max(1);
    let sep = if url.ends_with(['&', '?']) {
        ""
    } else if url.contains('?') {
        "&"
    } else {
        "?"
    };
    format!("{url}{sep}width={THUMB_PX}&height={height}")
}

/// Reads a response body, giving up as soon as `stop` says it's no longer wanted.
/// Why a body wasn't read to its end.
#[derive(Debug, PartialEq)]
enum Cut {
    TooBig,
    /// The connection failed, or `stop` said so.
    Broke,
}

fn read_body(mut r: impl Read, stop: &dyn Fn() -> bool) -> Result<Vec<u8>, Cut> {
    let (mut out, mut buf) = (Vec::new(), vec![0; 64 << 10]);
    loop {
        if stop() {
            return Err(Cut::Broke);
        }
        match r.read(&mut buf) {
            Ok(0) => return Ok(out),
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Err(Cut::Broke),
        }
        if out.len() as u64 > super::MAX_BYTES {
            return Err(Cut::TooBig);
        }
    }
}

/// Tries the cheapest source first: a small rendition, then the raw file, then the page's
/// og:image.
fn fetch_media(http: &ureq::Agent, job: &Job, stop: &dyn Fn() -> bool) -> Result<Vec<u8>, Fail> {
    let (url, media) = (job.url.as_str(), job.media_link.as_deref());
    let mut f = Fetch { http, tried: Vec::new(), net: None, missing: false, too_big: false, stop };
    for src in media.into_iter().chain([url]) {
        let origin = urls::unproxy(src);
        let real = origin.as_deref().unwrap_or(src);
        let small = urls::tenor_small(real).or_else(|| urls::giphy_small(real)).or_else(|| urls::imgur_gif(real));
        if let Some(b) = small.and_then(|u| f.image(u)) {
            return Ok(b);
        }
        if urls::is_discord_attachment(real)
            && let Some(b) = f.discord_small(real, job.size)
        {
            return Ok(b);
        }
        if urls::is_image(real)
            && let Some(b) = f.image(real.to_string())
        {
            return Ok(b);
        }
        // The proxy sometimes still has a copy of an origin that's gone.
        if origin.is_some()
            && let Some(b) = f.image(src.to_string())
        {
            return Ok(b);
        }
    }

    let page = urls::unproxy(url).unwrap_or_else(|| url.to_string());
    if urls::is_image(&page) || matches!(urls::ext(&page).as_str(), "mp4" | "webm" | "mov") {
        return Err(f.failure());
    }
    let og = f.get(&page, false).and_then(|html| og_image(&String::from_utf8_lossy(&html)));
    let found = og.and_then(|og| urls::tenor_small(&og).and_then(|u| f.image(u)).or_else(|| f.image(og)));
    found.ok_or_else(|| f.failure())
}

/// What Discord's link previews send, for pages that let only link-preview bots through.
const LINK_BOT_UA: &str = "Mozilla/5.0 (compatible; Discordbot/2.0; +https://discordapp.com)";

/// The page's preview image: `og:image`, else `twitter:image`. Of several, the GIF, since an
/// animated WebP decodes about 3x slower.
fn og_image(html: &str) -> Option<String> {
    let metas = html.split('<').filter_map(|t| {
        let rest = t.get(..4).filter(|m| m.eq_ignore_ascii_case("meta")).map(|_| &t[4..])?;
        rest.starts_with(|c: char| c.is_ascii_whitespace()).then(|| &rest[..rest.find('>').unwrap_or(rest.len())])
    });
    let find = |key: &str| {
        let all: Vec<&str> = metas
            .clone()
            .filter_map(|tag| {
                let name = attr(tag, "property").or_else(|| attr(tag, "name"))?;
                if name.eq_ignore_ascii_case(key) { attr(tag, "content") } else { None }
            })
            .collect();
        all.iter().find(|u| urls::ext(u) == "gif").or(all.first()).copied()
    };
    Some(find("og:image").or_else(|| find("twitter:image"))?.replace("&amp;", "&"))
}

/// The value of attribute `name` in the inside of an HTML tag (which starts with whitespace).
fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let lower = tag.to_ascii_lowercase(); // same byte offsets as `tag`
    let mut from = 0;
    while let Some(p) = lower[from..].find(name) {
        let at = from + p;
        from = at + name.len();
        let rest = tag[from..].trim_start();
        // A whole attribute name (not `og:content`), followed by `=`.
        if !lower[..at].ends_with(|c: char| c.is_ascii_whitespace()) || !rest.starts_with('=') {
            continue;
        }
        let value = rest[1..].trim_start();
        return Some(match value.chars().next() {
            Some(q @ ('"' | '\'')) => value[1..].split(q).next()?,
            _ => value.split(|c: char| c.is_ascii_whitespace()).next()?,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn a_body_read_stops_once_unwanted() {
        let body = vec![7u8; 200 << 10];
        assert_eq!(read_body(Cursor::new(&body), &|| false).as_deref(), Ok(&body[..]));
        let reads = Cell::new(0);
        let stop = || {
            reads.set(reads.get() + 1);
            reads.get() > 2
        };
        assert_eq!(read_body(Cursor::new(&body), &stop), Err(Cut::Broke));
    }

    #[test]
    fn discord_files_are_asked_for_thumbnail_wide_in_their_shape() {
        let u = "https://media.discordapp.net/attachments/1/2/a.gif";
        assert_eq!(sized(&format!("{u}?ex=1&"), 600, 338), format!("{u}?ex=1&width=300&height=169"));
        assert_eq!(sized(u, 240, 300), format!("{u}?width=300&height=375"), "never enlarged by Discord");
        assert_eq!(sized(&format!("{u}?ex=1"), 4000, 2), format!("{u}?ex=1&width=300&height=1"));
    }

    #[test]
    fn only_a_clean_not_found_means_gone() {
        let http: ureq::Agent = ureq::Agent::config_builder().build().into();
        let f = |net: Option<&str>, missing, too_big| {
            let stop = &|| false;
            Fetch { http: &http, tried: Vec::new(), net: net.map(Into::into), missing, too_big, stop }.failure()
        };
        assert_eq!(f(None, true, false), Fail::Gone);
        assert_eq!(f(Some("HTTP 503"), true, false), Fail::Network("HTTP 503".into()));
        assert_eq!(f(None, false, false), Fail::NotImage);
        assert_eq!(f(None, true, true), Fail::TooBig, "its small rendition is gone, its file too big");
    }

    #[test]
    fn a_body_over_the_limit_is_cut_as_too_big() {
        let max = super::super::MAX_BYTES as usize;
        assert_eq!(read_body(&vec![0u8; max][..], &|| false).map(|b| b.len()), Ok(max));
        assert_eq!(read_body(&vec![0u8; max + 1][..], &|| false), Err(Cut::TooBig));
        assert_eq!(read_body(&[0u8; 9][..], &|| true), Err(Cut::Broke));
    }

    #[test]
    fn og_image_reads_either_quote_style_and_attribute_order() {
        let double = r#"<head><meta property="og:image" content="https://a.com/x.gif?a=1&amp;b=2"></head>"#;
        assert_eq!(og_image(double).as_deref(), Some("https://a.com/x.gif?a=1&b=2"));
        let single = "<META content='https://a.com/y.gif' Property='og:image' />";
        assert_eq!(og_image(single).as_deref(), Some("https://a.com/y.gif"));
    }

    #[test]
    fn og_image_prefers_og_over_twitter() {
        let html = r#"<meta name="twitter:image" content="t.gif"><meta property="og:image" content="o.gif">"#;
        assert_eq!(og_image(html).as_deref(), Some("o.gif"));
        assert_eq!(og_image(r#"<meta name="twitter:image" content="t.gif">"#).as_deref(), Some("t.gif"));
        assert_eq!(og_image(r#"<meta property="og:title" content="x"><metadata>"#), None);
    }

    #[test]
    fn og_image_prefers_the_gif_of_several() {
        let html = r#"<meta property="og:image" content="https://s.com/a.webp"/>
            <meta property="og:image" content="https://s.com/a.gif"/>"#;
        assert_eq!(og_image(html).as_deref(), Some("https://s.com/a.gif"));
        let webp = r#"<meta property="og:image" content="https://s.com/a.webp"/>"#;
        assert_eq!(og_image(webp).as_deref(), Some("https://s.com/a.webp"));
    }
}
