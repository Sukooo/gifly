//! GIFs added from files on the PC, pasted or dropped into the picker. Each file is copied into
//! gifly's own folder, named after its contents, so moving or deleting the original doesn't
//! break it and the same file added twice is one GIF. Their links are `local:<name>`.

use std::{
    collections::HashSet,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

use image::ImageFormat;

use crate::{
    clock,
    db::{Gif, NewGif},
    media, urls, util,
};

/// Copies the pictures among `paths` into `dir`: the new GIFs, and how many files weren't
/// pictures. Slow for many or big files, so it runs off the UI thread.
pub(super) fn store_all(dir: &Path, paths: Vec<PathBuf>) -> (Vec<NewGif>, usize) {
    let (now, mut items, mut skipped) = (clock::now(), Vec::new(), 0);
    for p in paths {
        let Some(url) = store(dir, &p) else {
            skipped += 1;
            continue;
        };
        let title = urls::title_from_name(&p.file_name().unwrap_or_default().to_string_lossy());
        items.push(NewGif::link(url, title, now));
    }
    (items, skipped)
}

/// Copies a picture into `dir` and returns its link. None if it isn't a GIF, PNG, JPEG or WebP
/// (or can't be read).
fn store(dir: &Path, src: &Path) -> Option<String> {
    if fs::metadata(src).ok()?.len() > media::MAX_BYTES {
        return None;
    }
    // The format from the first bytes, so a video or other file isn't read whole for nothing.
    let mut head = Vec::new();
    fs::File::open(src).ok()?.take(16).read_to_end(&mut head).ok()?;
    let ext = match image::guess_format(&head).ok()? {
        ImageFormat::Gif => "gif",
        ImageFormat::Png => "png",
        ImageFormat::Jpeg => "jpg",
        ImageFormat::WebP => "webp",
        _ => return None,
    };
    let bytes = fs::read(src).ok()?;
    let name = format!("{:016x}.{ext}", util::fnv1a(&bytes));
    let path = dir.join(&name);
    if !path.exists() {
        fs::create_dir_all(dir).ok()?;
        util::write_atomic(&path, &bytes).ok()?;
    }
    Some(urls::local_link(&name))
}

/// The file a `local:` link stands for.
pub(super) fn path(dir: &Path, url: &str) -> Option<PathBuf> {
    urls::local_name(url).map(|n| dir.join(n))
}

/// Deletes the files no GIF uses anymore. Run at startup, so a deleted GIF's file is still there
/// to undo the delete until gifly restarts.
pub(super) fn remove_unused(dir: &Path, gifs: &[Gif]) {
    let used: HashSet<&str> = gifs.iter().filter_map(|g| urls::local_name(&g.url)).collect();
    let Ok(entries) = fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        if e.file_name().to_str().is_none_or(|n| !used.contains(n)) {
            let _ = fs::remove_file(e.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_pictures_once_by_content_and_skips_other_files() {
        let tmp = std::env::temp_dir().join(format!("gifly-files-test-{}", std::process::id()));
        let (src, dir) = (tmp.join("src"), tmp.join("files"));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&src).unwrap();
        let gif = b"GIF89a\x01\x00\x01\x00\x00\x00\x00;";
        fs::write(src.join("cat dance.gif"), gif).unwrap();
        fs::write(src.join("copy.gif"), gif).unwrap();
        fs::write(src.join("notes.gif"), b"hello").unwrap();

        fs::write(src.join("clip.mp4"), b"\0\0\0\x18ftypmp42").unwrap();
        let (items, skipped) = store_all(&dir, vec![src.join("cat dance.gif"), src.join("clip.mp4")]);
        assert_eq!((items.len(), skipped), (1, 1));
        assert_eq!(items[0].title, "cat dance");

        let link = store(&dir, &src.join("cat dance.gif")).unwrap();
        assert!(link.starts_with("local:") && link.ends_with(".gif"));
        assert_eq!(store(&dir, &src.join("copy.gif")).as_ref(), Some(&link));
        assert_eq!(store(&dir, &src.join("notes.gif")), None);
        assert_eq!(store(&dir, &src.join("missing.gif")), None);
        assert!(path(&dir, &link).unwrap().exists());

        fs::write(dir.join("stale.png"), b"x").unwrap();
        let new = NewGif::link(link.clone(), String::new(), 0);
        let g = Gif::from_new(crate::db::GifId(1), new);
        remove_unused(&dir, &[g]);
        assert!(path(&dir, &link).unwrap().exists() && !dir.join("stale.png").exists());
        fs::remove_dir_all(&tmp).unwrap();
    }
}
