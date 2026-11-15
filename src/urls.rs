//! URL heuristics: what counts as a GIF link, where to fetch a small preview, and a title from the slug.

fn split(url: &str) -> (&str, &str) {
    let base = url.split(['?', '#']).next().unwrap_or(url);
    let rest = base.split_once("://").map_or(base, |(_, r)| r);
    let host = rest.split('/').next().unwrap_or("");
    (host, base)
}

/// A link as written to the log: without its query, which for a Discord file is a signature
/// that would let anyone the log is shared with open it.
pub fn for_log(url: &str) -> &str {
    split(url).1
}

/// `host` is `domain` or one of its subdomains (so `nottenor.com` isn't Tenor).
fn on(host: &str, domain: &str) -> bool {
    host.strip_suffix(domain).is_some_and(|rest| rest.is_empty() || rest.ends_with('.'))
}

pub fn ext(url: &str) -> String {
    let (_, base) = split(url);
    let last = base.rsplit('/').next().unwrap_or("");
    last.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default()
}

pub fn is_image(url: &str) -> bool {
    matches!(ext(url).as_str(), "gif" | "png" | "jpg" | "jpeg" | "webp")
}

/// An http(s) link, without characters a real link never holds unencoded.
fn is_link(s: &str) -> bool {
    let odd = |c: char| c.is_whitespace() || c.is_control() || matches!(c, '"' | '<' | '>' | '\\');
    (s.starts_with("https://") || s.starts_with("http://")) && !s.contains(odd)
}

pub fn is_gif_url(s: &str) -> bool {
    if !is_link(s) {
        return false;
    }
    let lower = s.to_ascii_lowercase();
    let (host, path) = split(&lower);
    (on(host, "tenor.com") && (path.contains("/view/") || is_image(path)))
        || (on(host, "giphy.com") && (path.contains("/gifs/") || path.contains("/media/")))
        || (on(host, "klipy.com") && (path.contains("/gifs/") || is_image(path)))
        // Twitter's GIFs, which are videos, as fxtwitter serves them: animated WebP.
        || (path.contains("/tweet_video/") && is_image(path))
        || matches!(ext(path).as_str(), "gif" | "gifv")
}

/// A page behind a bot check that lets only link-preview bots through: Klipy's GIF pages,
/// which answer anything else with a Cloudflare challenge.
pub fn only_for_link_bots(url: &str) -> bool {
    let (host, _) = split(url);
    host.eq_ignore_ascii_case("klipy.com") || host.eq_ignore_ascii_case("www.klipy.com")
}

/// A link to any picture, pasted into the picker on purpose. A copied link has to pass
/// `is_gif_url`, since sites serve plenty of photos as PNG, JPEG or WebP.
pub fn is_picture_link(s: &str) -> bool {
    is_gif_url(s) || (is_link(s) && is_image(s))
}

/// Tenor media URLs look like `media.tenor.com/<id><5-char format>/<name>.<ext>`.
/// Swapping the format code to `AAAAM` gives the small "tinygif" rendition.
pub fn tenor_small(url: &str) -> Option<String> {
    let (host, base) = split(url);
    if !on(host, "tenor.com") || !host.starts_with("media") {
        return None;
    }
    let (dir, file) = base.rsplit_once('/')?;
    let (prefix, id) = dir.rsplit_once('/')?;
    if id.len() < 12 || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return None;
    }
    let stem = file.rsplit_once('.').map_or(file, |(s, _)| s);
    Some(format!("{prefix}/{}AAAAM/{stem}.gif", &id[..id.len() - 5]))
}

/// Giphy: `.../media/<id>/giphy.mp4` or `giphy.com/gifs/<slug>-<id>` → the 200px-wide GIF.
pub fn giphy_small(url: &str) -> Option<String> {
    let (host, base) = split(url);
    if !on(host, "giphy.com") {
        return None;
    }
    if base.contains("/media/") {
        let (dir, _) = base.rsplit_once('/')?;
        return Some(format!("{dir}/200w.gif"));
    }
    let slug = base.trim_end_matches('/').rsplit('/').next()?;
    let id = slug.rsplit('-').next()?;
    (!id.is_empty()).then(|| format!("https://media.giphy.com/media/{id}/200w.gif"))
}

/// Discord's embed proxy (`images-ext-N.discordapp.net/external/<sig>/https/host/path`) often
/// expires; the original URL underneath usually still works.
pub fn unproxy(url: &str) -> Option<String> {
    let (host, _) = split(url);
    if !(host.starts_with("images-ext") && on(host, "discordapp.net")) {
        return None;
    }
    let mut rest = url.split_once("/external/")?.1.split_once('/')?.1;
    // Some carry the original's query as an extra segment first: `.../%3Fc%3D.../https/...`.
    if rest.get(..3).is_some_and(|s| s.eq_ignore_ascii_case("%3f")) {
        rest = rest.split_once('/')?.1;
    }
    let (scheme, target) = rest.split_once('/')?;
    matches!(scheme, "https" | "http").then(|| format!("{scheme}://{target}"))
}

/// Discord attachments on the media proxy, which can resize (and keeps GIFs animated).
pub fn is_discord_attachment(url: &str) -> bool {
    let (host, base) = split(url);
    host == "media.discordapp.net" && base.contains("/attachments/")
}

/// What makes two links the same GIF. Discord attachment links differ by host
/// (cdn/media) and carry expiring `?ex=&is=&hm=` params, so only their path counts.
pub fn identity(url: &str) -> String {
    let (host, base) = split(url);
    match base.split_once("/attachments/") {
        Some((_, path)) if on(host, "discordapp.net") || on(host, "discordapp.com") => {
            format!("discord/{path}")
        }
        _ => url.to_string(),
    }
}

/// A Discord attachment, on either host.
pub fn is_discord_file(url: &str) -> bool {
    identity(url).starts_with("discord/")
}

/// The Discord file link a GIF loads from, if it is a Discord file: its media link, else the GIF
/// link, or whichever lasts longer when both are links to the same file.
pub fn discord_link<'a>(url: &'a str, media: Option<&'a str>) -> Option<&'a str> {
    let url = is_discord_file(url).then_some(url);
    match (media.filter(|m| is_discord_file(m)), url) {
        (Some(m), Some(u)) if identity(m) == identity(u) && discord_expiry(u) > discord_expiry(m) => Some(u),
        (m, u) => m.or(u),
    }
}

/// Whether link `new` is good for at least as long as `old`. Discord file links carry their
/// expiry (none means it's unsigned, so already unusable); other links don't expire.
pub fn outlasts(new: &str, old: &str) -> bool {
    let until = |u: &str| discord_expiry(u).unwrap_or(if is_discord_file(u) { 0 } else { i64::MAX });
    until(new) >= until(old)
}

/// When a signed Discord link stops working, in unix seconds (its `ex` param, in hex).
pub fn discord_expiry(url: &str) -> Option<i64> {
    let query = url.split_once('?')?.1.split('#').next()?;
    let ex = query.split('&').find_map(|p| p.strip_prefix("ex="))?;
    i64::from_str_radix(ex, 16).ok()
}

/// Imgur serves `.mp4`/`.gifv` links as `.gif` too.
pub fn imgur_gif(url: &str) -> Option<String> {
    let (host, base) = split(url);
    let stem = base.strip_suffix(".mp4").or_else(|| base.strip_suffix(".gifv"))?;
    (host == "i.imgur.com").then(|| format!("{stem}.gif"))
}

/// A GIF added from a file on the PC: `local:<name>`, the file in gifly's own folder.
pub fn local_link(name: &str) -> String {
    format!("local:{name}")
}

/// The file name in a `local:` link. Only plain names count, never a path.
pub fn local_name(url: &str) -> Option<&str> {
    let name = url.strip_prefix("local:")?;
    (!name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '.') && !name.starts_with('.'))
        .then_some(name)
}

pub fn title_from_url(url: &str) -> String {
    let (_, base) = split(url);
    title_from_name(base.trim_end_matches('/').rsplit('/').next().unwrap_or(""))
}

/// A title from a file name or a link's last part: its words, without the extension and noise
/// like ids and "gif".
pub fn title_from_name(name: &str) -> String {
    let stem = name.rsplit_once('.').map_or(name, |(s, _)| s);
    stem.split(['-', '_', '+', ' '])
        .filter(|w| !w.is_empty() && !is_noise(w))
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_noise(w: &str) -> bool {
    let digits = w.chars().filter(char::is_ascii_digit).count();
    matches!(w.to_ascii_lowercase().as_str(), "gif" | "gifs" | "giphy" | "tenor" | "200w" | "tinygif")
        || digits == w.len()
        || (digits > 0 && w.len() >= 6)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_gif_links() {
        assert!(is_gif_url("https://tenor.com/view/cat-dance-gif-12345"));
        assert!(is_gif_url("https://giphy.com/gifs/cat-dance-3o7TKSjRrfIPjeiVyM"));
        assert!(is_gif_url("https://cdn.discordapp.com/attachments/1/2/funny.gif?ex=1"));
        assert!(is_gif_url("https://klipy.com/gifs/who-is-this-178"));
        assert!(is_gif_url("https://static2.klipy.com/ii/4493325008d34b7bf8cd6813cd5c1619/4e/d3/Wtoz2JGsmOjd.webp"));
        assert!(!is_gif_url("https://example.com/page"));
        assert!(!is_gif_url("https://nottenor.com/view/x"));
        assert!(!is_gif_url("https://example.com/a\".gif"));
        assert!(!is_gif_url("https://example.com/<a>.gif"));
        assert!(is_gif_url("https://media.tenor.com/abc/x.gif"));
        assert!(!is_gif_url("hello https://tenor.com/view/x"));
    }

    #[test]
    fn gif_link_edge_cases() {
        assert!(is_gif_url("https://Media.Tenor.com/abc/X.GIF"));
        assert!(is_gif_url("http://i.imgur.com/abc.gifv"));
        assert!(is_gif_url("https://example.com/cat.gif#frag"));
        assert!(!is_gif_url("https://example.com/cat.gif.html"));
        assert!(!is_gif_url("https://example.com/gif"));
        assert!(!is_gif_url("https://tenor.com.evil.com/view/x"));
        assert!(!is_gif_url("ftp://example.com/a.gif"));
        assert!(!is_gif_url("https://example.com/a b.gif"));
    }

    #[test]
    fn only_klipy_pages_are_asked_for_as_a_link_bot() {
        assert!(only_for_link_bots("https://klipy.com/gifs/cat-kitty-575"));
        assert!(only_for_link_bots("https://www.Klipy.com/gifs/x"));
        assert!(!only_for_link_bots("https://static2.klipy.com/ii/a/b/c.gif"));
        assert!(!only_for_link_bots("https://tenor.com/view/x"));
    }

    #[test]
    fn twitter_gifs_are_saved_when_copied_other_pictures_only_when_pasted() {
        let fx = "https://gif.fxtwitter.com/tweet_video/HWmBvzmaEAAOsqC.webp";
        assert!(is_gif_url(fx) && is_picture_link(fx));
        let photo = "https://cdn.discordapp.com/attachments/1/2/cat.webp?ex=1";
        assert!(!is_gif_url(photo) && is_picture_link(photo));
        assert!(is_picture_link("https://example.com/a.PNG"));
        assert!(!is_picture_link("https://example.com/page"));
        assert!(!is_picture_link("https://example.com/a b.webp"));
        assert!(!is_picture_link("C:\\cat.webp"));
    }

    #[test]
    fn discord_links_match_across_hosts_and_params() {
        let a = identity("https://cdn.discordapp.com/attachments/1/2/cat.gif?ex=1&is=2&hm=3");
        assert_eq!(a, identity("https://media.discordapp.net/attachments/1/2/cat.gif"));
        assert_ne!(a, identity("https://media.discordapp.net/attachments/1/3/cat.gif"));
        assert_eq!(identity("https://tenor.com/view/x-1"), "https://tenor.com/view/x-1");
    }

    #[test]
    fn discord_file_links_and_expiry() {
        let cdn = "https://cdn.discordapp.com/attachments/1/2/cat.gif";
        let signed = "https://media.discordapp.net/attachments/1/2/cat.gif?ex=6af2f686&is=6af1a506&hm=ab&";
        assert_eq!(discord_link(cdn, Some(signed)), Some(signed));
        // A newer copy of the same link wins over an older media link.
        let newer = signed.replace("ex=", "ex=f");
        assert_eq!(discord_link(&newer, Some(signed)), Some(newer.as_str()));
        assert_eq!(discord_link(cdn, Some("https://media.tenor.com/x/a.mp4")), Some(cdn));
        assert_eq!(discord_link("https://tenor.com/view/x-1", None), None);
        assert_eq!(discord_expiry(signed), Some(0x6af2_f686));
        assert_eq!(discord_expiry(cdn), None);
        assert_eq!(discord_expiry("https://cdn.discordapp.com/attachments/1/2/a.gif?is=1&ex=zz"), None);
    }

    #[test]
    fn titles() {
        assert_eq!(title_from_url("https://tenor.com/view/cat-dance-gif-12345"), "cat dance");
        assert_eq!(title_from_url("https://giphy.com/gifs/happy-dog-3o7TKSjRrfIPjeiVyM"), "happy dog");
        assert_eq!(title_from_url("https://klipy.com/gifs/who-is-this-178"), "who is this");
        assert_eq!(title_from_name("Cat Dance_2.GIF"), "cat dance");
    }

    #[test]
    fn local_links_name_only_a_file_in_gifly_s_folder() {
        assert_eq!(local_name(&local_link("0123abcd.gif")), Some("0123abcd.gif"));
        assert_eq!(local_name(r"local:..\x.gif"), None);
        assert_eq!(local_name("local:a/b.gif"), None);
        assert_eq!(local_name("local:"), None);
        assert_eq!(local_name("https://x/local:a.gif"), None);
    }

    #[test]
    fn small_renditions() {
        assert_eq!(
            tenor_small("https://media.tenor.com/x4ZDE1ijHWwAAAPo/cat-dance.mp4").as_deref(),
            Some("https://media.tenor.com/x4ZDE1ijHWwAAAAM/cat-dance.gif")
        );
        assert_eq!(
            tenor_small("https://media.tenor.com/-jAoZkvjK5UAAAPo/ja.mp4").as_deref(),
            Some("https://media.tenor.com/-jAoZkvjK5UAAAAM/ja.gif")
        );
        assert_eq!(
            unproxy("https://images-ext-1.discordapp.net/external/3Ol2_pPP6X/https/media.tenor.com/abc/x.mp4")
                .as_deref(),
            Some("https://media.tenor.com/abc/x.mp4")
        );
        // With the original's query as an extra segment.
        assert_eq!(
            unproxy(
                "https://images-ext-2.discordapp.net/external/m9qa/%3Fc%3DVjFfZGlzY29yZA/https/media.tenor.com/k-7S4OjrJ0cAAAPo/daredevil-daredevil-dog.mp4"
            )
            .as_deref(),
            Some("https://media.tenor.com/k-7S4OjrJ0cAAAPo/daredevil-daredevil-dog.mp4")
        );
        assert_eq!(imgur_gif("https://i.imgur.com/Tj9Lc1G.mp4").as_deref(), Some("https://i.imgur.com/Tj9Lc1G.gif"));
        assert_eq!(
            giphy_small("https://giphy.com/gifs/happy-dog-abc123").as_deref(),
            Some("https://media.giphy.com/media/abc123/200w.gif")
        );
    }
}
