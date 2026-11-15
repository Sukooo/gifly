//! Decodes Discord's "frecency" settings protobuf. Only the favorites are read: field 2 =
//! `FavoriteGIFs { map<string, FavoriteGIF> gifs = 1 }`,
//! `FavoriteGIF { format = 1, src = 2, width = 3, height = 4, order = 5 }`.

use base64::Engine;

use super::Fav;

pub(super) fn parse_frecency(buf: &[u8]) -> Option<Vec<Fav>> {
    let mut out = Vec::new();
    let mut top = Pb(buf);
    while let Some((num, val)) = top.field() {
        let (2, Val::Bytes(favs)) = (num, val) else { continue };
        let mut favs = Pb(favs);
        while let Some((num, val)) = favs.field() {
            let (1, Val::Bytes(entry)) = (num, val) else { continue };
            out.extend(parse_entry(entry));
        }
        if !favs.0.is_empty() {
            return None; // stopped on bytes that aren't a field: not the message we expected
        }
    }
    top.0.is_empty().then_some(out)
}

/// One map entry: the GIF's link (the key) and its `FavoriteGIF` (the value).
fn parse_entry(buf: &[u8]) -> Option<Fav> {
    let mut url = None;
    let mut fav = Fav { url: String::new(), src: String::new(), order: 0, width: 0, height: 0 };
    let mut pb = Pb(buf);
    while let Some((num, val)) = pb.field() {
        match (num, val) {
            (1, Val::Bytes(k)) => url = Some(String::from_utf8_lossy(k).into_owned()),
            (2, Val::Bytes(v)) => {
                let mut inner = Pb(v);
                while let Some((n, v)) = inner.field() {
                    match (n, v) {
                        (2, Val::Bytes(s)) => fav.src = String::from_utf8_lossy(s).into_owned(),
                        (3, Val::Int(w)) => fav.width = u16::try_from(w).unwrap_or(u16::MAX),
                        (4, Val::Int(h)) => fav.height = u16::try_from(h).unwrap_or(u16::MAX),
                        (5, Val::Int(o)) => fav.order = o,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    fav.url = url.filter(|u| !u.is_empty())?;
    Some(fav)
}

/// Base64 with or without padding.
pub(super) fn decode_base64(s: &str) -> Option<Vec<u8>> {
    let b64 = base64::engine::general_purpose::STANDARD;
    b64.decode(s).or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(s)).ok()
}

/// The string value of `"key"` in a JSON text. Enough for the base64 values and links read here,
/// which never contain quotes or escapes.
pub(super) fn json_str<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let start = json.find(&format!("\"{key}\""))?;
    let rest = &json[start + key.len() + 2..];
    let rest = &rest[rest.find('"')? + 1..];
    Some(&rest[..rest.find('"')?])
}

/// The number value of `"key"` in a JSON text.
pub(super) fn json_num(json: &str, key: &str) -> Option<f64> {
    let start = json.find(&format!("\"{key}\""))?;
    let rest = json[start + key.len() + 2..].trim_start().strip_prefix(':')?.trim_start();
    let end = rest.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(rest.len());
    rest[..end].parse().ok()
}

enum Val<'a> {
    Int(u64),
    Bytes(&'a [u8]),
}

/// A minimal protobuf reader: one field at a time, nested messages left as bytes.
struct Pb<'a>(&'a [u8]);

impl<'a> Pb<'a> {
    fn varint(&mut self) -> Option<u64> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let (&b, rest) = self.0.split_first()?;
            self.0 = rest;
            v |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Some(v);
            }
        }
        None
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if n > self.0.len() {
            return None;
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Some(a)
    }

    /// The next field. One that doesn't parse is left unread, so callers can tell the message
    /// ended early from it running out.
    fn field(&mut self) -> Option<(u64, Val<'a>)> {
        let mut p = Pb(self.0);
        let f = p.read_field()?;
        self.0 = p.0;
        Some(f)
    }

    fn read_field(&mut self) -> Option<(u64, Val<'a>)> {
        if self.0.is_empty() {
            return None;
        }
        let key = self.varint()?;
        let val = match key & 7 {
            0 => Val::Int(self.varint()?),
            1 => Val::Bytes(self.take(8)?),
            2 => {
                let n = usize::try_from(self.varint()?).ok()?;
                Val::Bytes(self.take(n)?)
            }
            5 => Val::Bytes(self.take(4)?),
            _ => return None,
        };
        Some((key >> 3, val))
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    /// A length-delimited field, for building test messages.
    pub(in crate::discord) fn len_field(num: u8, data: &[u8]) -> Vec<u8> {
        let mut v = vec![(num << 3) | 2, data.len() as u8];
        v.extend_from_slice(data);
        v
    }

    #[test]
    fn reads_json_numbers() {
        assert_eq!(json_num(r#"{"message": "slow", "retry_after": 1.25, "global": false}"#, "retry_after"), Some(1.25));
        assert_eq!(json_num(r#"{"retry_after":3}"#, "retry_after"), Some(3.0));
        assert_eq!(json_num(r#"{"other": 1}"#, "retry_after"), None);
    }

    #[test]
    fn parses_favorites() {
        let mut gif = len_field(2, b"https://media.tenor.com/abcdefghijklAAAPo/x.mp4");
        gif.extend([3 << 3, 0xa0, 0x03]); // width = 416
        gif.extend([4 << 3, 120]); // height = 120
        gif.extend([5 << 3, 7]); // order = 7
        let mut entry = len_field(1, b"https://tenor.com/view/x-gif-1");
        entry.extend(len_field(2, &gif));
        let favs_msg = len_field(1, &entry);
        let mut root = vec![1 << 3, 1]; // unrelated varint field
        root.extend(len_field(2, &favs_msg));

        let favs = parse_frecency(&root).unwrap();
        assert_eq!(favs.len(), 1);
        assert_eq!(favs[0].url, "https://tenor.com/view/x-gif-1");
        assert!(favs[0].src.ends_with("x.mp4"));
        assert_eq!(favs[0].order, 7);
        assert_eq!((favs[0].width, favs[0].height), (416, 120));
    }

    #[test]
    fn rejects_garbled_messages() {
        assert!(parse_frecency(&[]).unwrap().is_empty());
        assert!(parse_frecency(&[(2 << 3) | 2, 9, 1]).is_none()); // says 9 bytes, has 1
        assert!(parse_frecency(&[(2 << 3) | 7]).is_none()); // no such wire type
    }

    #[test]
    fn reads_json_string() {
        assert_eq!(json_str(r#"{"settings": "QUJD"}"#, "settings"), Some("QUJD"));
    }
}
