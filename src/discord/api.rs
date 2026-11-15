//! Discord's HTTP API: favorites with a user token, and fresh file links with a bot token. Tokens
//! only go into a request's Authorization header, never into a log or an error message. The user
//! token is never stored.

use std::{thread, time::Duration};

use super::{Fav, proto};

const SETTINGS_URL: &str = "https://discord.com/api/v9/users/@me/settings-proto/2";
/// Discord calls renewing links "refreshing" them.
const REFRESH_URL: &str = "https://discord.com/api/v10/attachments/refresh-urls";
/// The most links Discord renews per request.
pub const RENEW_MAX: usize = 50;

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(20))).build().into()
}

pub(super) fn favorites(token: &str) -> Result<Vec<Fav>, String> {
    let token = clean_token(token);
    if token.is_empty() {
        return Err("Paste your Discord token first.".into());
    }
    let mut res = agent().get(SETTINGS_URL).header("Authorization", token).call().map_err(|e| match e {
        ureq::Error::StatusCode(401 | 403) => "Discord rejected that token.".to_string(),
        ureq::Error::StatusCode(429) => "Discord is rate limiting requests. Try again in a minute.".to_string(),
        ureq::Error::StatusCode(c) => format!("Discord answered with an error ({c})."),
        _ => "Couldn't reach Discord. Check your connection.".to_string(),
    })?;
    let body = res.body_mut().read_to_string().map_err(|_| "Couldn't read Discord's answer.".to_string())?;
    proto::json_str(&body, "settings")
        .and_then(proto::decode_base64)
        .and_then(|bytes| proto::parse_frecency(&bytes))
        .ok_or_else(|| "Unexpected answer from Discord.".to_string())
}

/// Why links weren't renewed.
pub enum RenewError {
    /// The token is no good: stop asking until there's a new one.
    Rejected,
    /// Ready to show; worth trying again later.
    Other(String),
}

impl std::fmt::Display for RenewError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rejected => f.write_str("Discord rejected the bot token. Paste a new one."),
            Self::Other(s) => f.write_str(s),
        }
    }
}

/// `s` as a JSON string, quoted and escaped.
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c < ' ' => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Fresh signed links for Discord file links (expired or unsigned ones work too), in the same
/// order: `None` for any that Discord didn't renew. At most `RENEW_MAX` at a time.
pub fn renew_links(token: &str, links: &[&str]) -> Result<Vec<Option<String>>, RenewError> {
    let other = |s: &str| RenewError::Other(s.to_string());
    let token = clean_bot_token(token);
    let quoted: Vec<String> = links.iter().map(|l| json_string(l)).collect();
    let body = format!("{{\"attachment_urls\":[{}]}}", quoted.join(","));
    let http = agent();
    // Rate limits say how long to wait; give up after a few rounds rather than queueing forever.
    for _ in 0..3 {
        let res = http
            .post(REFRESH_URL)
            .header("Authorization", format!("Bot {token}"))
            // Discord asks bots to name themselves this way.
            .header(
                "User-Agent",
                concat!("DiscordBot (https://github.com/Sukooo/gifly, ", env!("CARGO_PKG_VERSION"), ")"),
            )
            .header("Content-Type", "application/json")
            .config()
            .http_status_as_error(false)
            .build()
            .send(body.as_str());
        let mut res = res.map_err(|_| other("Couldn't reach Discord to renew links."))?;
        let status = res.status().as_u16();
        let text = res.body_mut().read_to_string().unwrap_or_default();
        match status {
            200 => return Ok(parse_refreshed(&text, links)),
            401 | 403 => return Err(RenewError::Rejected),
            429 => {
                let wait = proto::json_num(&text, "retry_after").unwrap_or(5.0).clamp(0.5, 60.0);
                thread::sleep(Duration::from_secs_f64(wait));
            }
            c => return Err(RenewError::Other(format!("Discord couldn't renew links ({c})."))),
        }
    }
    Err(other("Discord is rate limiting link renewals. Try again in a minute."))
}

/// Matches `{"refreshed_urls":[{"original":…,"refreshed":…},…]}` back to the links asked for.
fn parse_refreshed(json: &str, links: &[&str]) -> Vec<Option<String>> {
    let pairs: Vec<(&str, &str)> = json
        .split('{')
        .filter_map(|o| Some((proto::json_str(o, "original")?, proto::json_str(o, "refreshed")?)))
        .collect();
    links.iter().map(|l| pairs.iter().find(|p| p.0 == *l).map(|p| p.1.to_string())).collect()
}

/// Tokens copied from developer tools often come with whitespace or the quotes around them.
fn clean_token(token: &str) -> &str {
    token.trim().trim_matches(|c| c == '"' || c == '\'').trim()
}

/// A pasted bot token, also without the `Bot ` its header needs (added back when it's sent).
pub fn clean_bot_token(token: &str) -> &str {
    let token = clean_token(token);
    token.strip_prefix("Bot ").unwrap_or(token)
}

#[cfg(test)]
mod tests {
    use super::{clean_bot_token, clean_token, json_string, parse_refreshed};

    #[test]
    fn escapes_links_for_json() {
        assert_eq!(json_string("a\"b\\c\nd\u{1}"), r#""a\"b\\c\u000ad\u0001""#);
    }

    #[test]
    fn cleans_pasted_tokens() {
        assert_eq!(clean_token("  \"abc.def\"\n"), "abc.def");
        assert_eq!(clean_token("'abc'"), "abc");
        assert_eq!(clean_token(" \" "), "");
        assert_eq!(clean_bot_token(" 'Bot abc' "), "abc");
    }

    #[test]
    fn matches_refreshed_links_to_the_ones_asked_for() {
        let json =
            r#"{"refreshed_urls":[{"original":"b","refreshed":"b?ex=2"},{"refreshed":"a?ex=1","original":"a"}]}"#;
        assert_eq!(parse_refreshed(json, &["a", "b", "c"]), [Some("a?ex=1".into()), Some("b?ex=2".into()), None]);
    }
}
