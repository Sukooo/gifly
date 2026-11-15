//! Reads favorites from the Discord app's Local Storage (a LevelDB). It holds
//! `UserSettingsProtoStore-Cache`, a JSON object whose `_state["2"].proto` is the base64
//! frecency settings.

use std::{
    fs,
    path::{Path, PathBuf},
};

use super::{Fav, proto};

/// Discord apps (under %APPDATA%) that keep Local Storage: Stable, PTB, Canary, Development, Vesktop.
const APPS: [&str; 5] = ["discord", "discordptb", "discordcanary", "discorddevelopment", r"vesktop\sessionData"];
/// The exact key (origin, then a Latin-1 marker, then the name). Nothing else is read.
const KEY: &[u8] = b"_https://discord.com\x00\x01UserSettingsProtoStore-Cache";

/// Favorites from every Discord app on this PC (may contain duplicates).
pub(super) fn favorites() -> Result<Vec<Fav>, String> {
    let appdata = PathBuf::from(std::env::var_os("APPDATA").ok_or("Couldn't find your AppData folder.")?);
    let dirs: Vec<PathBuf> = APPS
        .iter()
        .map(|app| appdata.join(app).join("Local Storage").join("leveldb"))
        .filter(|d| d.join("CURRENT").is_file())
        .collect();
    if dirs.is_empty() {
        return Err("Couldn't find the Discord app on this PC.".into());
    }
    Ok(dirs
        .iter()
        // Discord may compact its files while they're copied, so give it a few tries.
        .filter_map(|d| (0..3).find_map(|_| read_key(d, KEY)))
        .flat_map(|v| from_cache(&v).unwrap_or_default())
        .collect())
}

/// Where the LevelDB is copied to be read. Only one gifly runs at a time, so one folder is enough.
fn copy_dir() -> PathBuf {
    std::env::temp_dir().join("gifly-discord")
}

/// Deletes copies a crash left behind, since a copy holds all of Discord's Local Storage. Older
/// versions added a numeric process id to the folder name.
pub fn remove_leftovers() {
    remove_leftovers_in(&std::env::temp_dir());
}

fn remove_leftovers_in(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        let old = name
            .strip_prefix("gifly-discord-")
            .is_some_and(|pid| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()));
        if name == "gifly-discord" || old {
            let _ = fs::remove_dir_all(e.path());
        }
    }
}

/// Reads one key from a copy of the LevelDB at `dir`, since the running app keeps it locked.
fn read_key(dir: &Path, key: &[u8]) -> Option<Vec<u8>> {
    let tmp = copy_dir();
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).ok()?;
    let value = (|| {
        for e in fs::read_dir(dir).ok()?.flatten() {
            if e.file_name() != "LOCK" {
                let _ = fs::copy(e.path(), tmp.join(e.file_name()));
            }
        }
        let opts = rusty_leveldb::Options { create_if_missing: false, ..Default::default() };
        let mut db = rusty_leveldb::DB::open(&tmp, opts).ok()?;
        db.get(key).map(|v| v.to_vec())
    })();
    let _ = fs::remove_dir_all(&tmp);
    value
}

/// Chromium stores Local Storage values with a leading 0x01 (Latin-1) or 0x00 (UTF-16LE) byte.
fn from_cache(raw: &[u8]) -> Option<Vec<Fav>> {
    let text: String = match raw.split_first()? {
        (0, rest) => char::decode_utf16(rest.as_chunks::<2>().0.iter().map(|&c| u16::from_le_bytes(c)))
            .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect(),
        (_, rest) => rest.iter().map(|&b| char::from(b)).collect(),
    };
    // {"_state":{"1":{"proto":"…"},"2":{"proto":"<frecency settings>"}},"_version":0}
    let b64 = proto::json_str(&text[text.find(r#""2":{"#)?..], "proto")?;
    proto::parse_frecency(&proto::decode_base64(b64)?)
}

#[cfg(test)]
mod tests {
    use base64::Engine;

    use super::*;
    use crate::discord::proto::tests::len_field;

    #[test]
    fn cleanup_removes_only_known_copy_names() {
        let dir = std::env::temp_dir().join(format!("gifly-cleanup-test-{}", std::process::id()));
        let names = ["gifly-discord", "gifly-discord-123", "gifly-discord-notes", "gifly-discord-", "gifly-discordant"];
        for name in names {
            fs::create_dir_all(dir.join(name)).unwrap();
            fs::write(dir.join(name).join("keep.txt"), b"data").unwrap();
        }
        remove_leftovers_in(&dir);
        for name in names {
            assert_eq!(dir.join(name).exists(), !matches!(name, "gifly-discord" | "gifly-discord-123"), "{name}");
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reads_local_storage_value() {
        let entry = [len_field(1, b"https://tenor.com/view/x-gif-1"), len_field(2, &[5 << 3, 3])].concat();
        let proto = len_field(2, &len_field(1, &entry));
        let b64 = base64::engine::general_purpose::STANDARD.encode(proto);
        let json = format!(r#"{{"_state":{{"1":{{"proto":"CgA="}},"2":{{"proto":"{b64}"}}}},"_version":0}}"#);
        let latin1 = [&[1u8][..], json.as_bytes()].concat();
        let utf16 = [vec![0u8], json.encode_utf16().flat_map(u16::to_le_bytes).collect()].concat();
        for raw in [latin1, utf16] {
            let favs = from_cache(&raw).unwrap();
            assert_eq!((favs[0].url.as_str(), favs[0].order), ("https://tenor.com/view/x-gif-1", 3));
        }
    }

    /// Reads this PC's Discord app: `cargo test -- --ignored this_pc`.
    #[test]
    #[ignore]
    fn reads_this_pcs_discord() {
        let favs = favorites().unwrap();
        println!("{} favorites, first: {}", favs.len(), favs[0].url);
    }
}
