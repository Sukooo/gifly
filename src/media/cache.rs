//! The disk cache: downloaded files, their thumbnails, and markers for links found dead, all
//! named after the link's identity. Pruned to `CACHE_MAX`, least recently used first.

use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use crate::{urls, util};

use super::THUMB_PX;

/// Links that failed for good aren't retried until this much time has passed.
pub(super) const DEAD_TTL: Duration = Duration::from_secs(7 * 24 * 3600);
/// The disk cache is kept under this, dropping the files used least recently.
pub(super) const CACHE_MAX: u64 = 1 << 30;
pub(super) const PRUNE_EVERY: Duration = Duration::from_secs(6 * 3600);
/// Cache subfolders: thumbnails (and dead-link markers), and downloaded files.
pub(super) const PREVIEWS: &str = "previews";
pub(super) const DOWNLOADS: &str = "downloads";

/// A Discord file that isn't cached and whose signed link has expired (or was never signed).
/// Fetching it would only get a 404, so the app gets it a fresh link first.
pub(super) fn needs_renewing(dir: &Path, url: &str, media: Option<&str>, anim: bool) -> bool {
    let Some(link) = urls::discord_link(url, media) else { return false };
    // A minute's margin, for the fetch itself and a clock that's slightly off.
    if urls::discord_expiry(link).is_some_and(|ex| ex > crate::clock::now() + 60) {
        return false;
    }
    // Dead markers don't count: many were left by links that had only expired.
    !(media_path(dir, url).exists() || (!anim && thumb_path(dir, url).exists()))
}

/// Cache file name for a link: an FNV-1a hash of its identity, so a renewed Discord link
/// keeps its cache.
fn cache_key(url: &str) -> String {
    format!("{:016x}", util::fnv1a(urls::identity(url).as_bytes()))
}

pub(super) fn thumb_path(dir: &Path, url: &str) -> PathBuf {
    // Size in the name, so changing THUMB_PX regenerates thumbnails instead of reusing blurry ones.
    dir.join(PREVIEWS).join(format!("{}-{THUMB_PX}.qoi", cache_key(url)))
}

pub(super) fn media_path(dir: &Path, url: &str) -> PathBuf {
    dir.join(DOWNLOADS).join(format!("{}.bin", cache_key(url)))
}

pub(super) fn dead_path(dir: &Path, url: &str) -> PathBuf {
    dir.join(PREVIEWS).join(format!("{}.dead", cache_key(url)))
}

/// Marks a link whose file is over `MAX_BYTES`, so it isn't downloaded again to find that out.
pub(super) fn big_path(dir: &Path, url: &str) -> PathBuf {
    dir.join(PREVIEWS).join(format!("{}.big", cache_key(url)))
}

/// Marks a cache file as just used, so pruning keeps it. Written at most once a day per file.
pub(super) fn touch(path: &Path) {
    let day_old = |m: fs::Metadata| m.modified().ok()?.elapsed().ok().map(|e| e > Duration::from_secs(86_400));
    if fs::metadata(path).ok().and_then(day_old).unwrap_or(false)
        && let Ok(f) = fs::File::options().write(true).open(path)
    {
        let _ = f.set_modified(SystemTime::now());
    }
}

/// Deletes the cached files used least recently until the cache is a tenth under `max` bytes,
/// so it isn't trimmed again right away. Markers (dead, too big) are kept: they're empty, and their
/// age is what `DEAD_TTL` counts. Returns the bytes freed.
pub(super) fn prune(dir: &Path, max: u64) -> u64 {
    let mut files = cached(dir);
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    if total <= max {
        return 0;
    }
    files.sort_unstable_by_key(|f| f.0);
    let goal = max - max / 10;
    let mut freed = 0;
    for (_, len, path) in files {
        if total <= goal {
            break;
        }
        if fs::remove_file(&path).is_ok() {
            total -= len;
            freed += len;
        }
    }
    freed
}

pub(super) fn size(dir: &Path) -> u64 {
    cached(dir).iter().map(|f| f.1).sum()
}

/// The cached files (not markers): when each was last used, its size and path.
fn cached(dir: &Path) -> Vec<(SystemTime, u64, PathBuf)> {
    [PREVIEWS, DOWNLOADS]
        .iter()
        .filter_map(|sub| fs::read_dir(dir.join(sub)).ok())
        .flatten()
        .filter_map(|e| {
            let (path, m) = e.ok().and_then(|e| Some((e.path(), e.metadata().ok()?)))?;
            let keep = !m.is_file() || path.extension().is_some_and(|x| x == "dead" || x == "big");
            (!keep).then(|| (m.modified().unwrap_or(SystemTime::UNIX_EPOCH), m.len(), path))
        })
        .collect()
}

/// The link was found gone within `DEAD_TTL`, so it isn't tried again yet.
pub(super) fn recently_dead(dir: &Path, url: &str) -> bool {
    recent(&dead_path(dir, url))
}

/// The marker was written within `DEAD_TTL`.
pub(super) fn recent(marker: &Path) -> bool {
    let recent = |m: fs::Metadata| m.modified().ok()?.elapsed().ok().map(|e| e < DEAD_TTL);
    fs::metadata(marker).ok().and_then(recent).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pruning_drops_the_least_recently_used_files_first() {
        let dir = std::env::temp_dir().join(format!("gifly-prune-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        for sub in [PREVIEWS, DOWNLOADS] {
            fs::create_dir_all(dir.join(sub)).unwrap();
        }
        let now = SystemTime::now();
        let files = [
            (PREVIEWS, "a.qoi", 3),
            (DOWNLOADS, "b.bin", 1),
            (DOWNLOADS, "c.bin", 2),
            (PREVIEWS, "d.dead", 9),
            (PREVIEWS, "e.big", 9),
        ];
        for (sub, name, days_old) in files {
            let path = dir.join(sub).join(name);
            let marker = name.ends_with("dead") || name.ends_with("big");
            fs::write(&path, if marker { vec![] } else { vec![0; 100] }).unwrap();
            let f = fs::File::options().write(true).open(&path).unwrap();
            f.set_modified(now - Duration::from_secs(days_old * 86_400)).unwrap();
        }
        assert_eq!(prune(&dir, 300), 0);
        // 300 bytes over a 150 cap: getting under 135 drops the two oldest files, not the marker.
        assert_eq!(prune(&dir, 150), 200);
        let left = |sub: &str, name: &str| dir.join(sub).join(name).exists();
        assert!(!left(PREVIEWS, "a.qoi") && !left(DOWNLOADS, "c.bin"));
        assert!(left(DOWNLOADS, "b.bin") && left(PREVIEWS, "d.dead") && left(PREVIEWS, "e.big"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn cache_key_follows_link_identity() {
        let a = "https://cdn.discordapp.com/attachments/1/2/cat.gif?ex=1&is=2&hm=3";
        let b = "https://media.discordapp.net/attachments/1/2/cat.gif?ex=4&is=5&hm=6";
        assert_eq!(cache_key(a), cache_key(b));
        assert_ne!(cache_key(a), cache_key("https://media.tenor.com/x/cat.gif"));
    }
}
