# gifly

A small Discord-style GIF picker for the Windows tray. Save GIF links, sort them into collections, and paste one into any app with a hotkey.

## Why..?

Because I hate Discord's GIF picker (and maybe also because I hit the limit on how many GIFs you can save :P).
I always wanted to sort them into categories and get to them faster, so I made my own, and made it fast and light while I was at it.

## Install

Download `gifly-win-Setup.exe` from [Releases](https://github.com/Sukooo/gifly/releases/latest). The installer isn't code-signed, so SmartScreen may warn you: click **More info**, then **Run anyway**. Windows 10 and 11.

## Usage

- **Ctrl+Shift+G** opens the picker at your cursor. Type to search, then **Enter** or click a GIF to paste it into the app you were in.
- Copied GIF links (Tenor, Giphy, Klipy, Imgur, direct links) are saved automatically. You can also paste any picture link (GIF, PNG, JPEG, WebP) or drop image files into the picker.
- Right-click or drag GIFs to file them into collections. **Ctrl+1–9** opens Home's tiles, **Ctrl+H** shows hidden collections, **Ctrl+Z** undoes.
- **Shift+click** or **Ctrl+A** selects several GIFs. **Esc** steps back, **Backspace** goes Home.

### Discord favorites

**Settings → Import from Discord** reads the favorites the Discord app keeps on your PC, with no token and no network request ([`src/discord/local.rs`](src/discord/local.rs)).

Discord file links expire after about a day. To keep them loading, paste a bot token (from the [Developer Portal](https://discord.com/developers/applications)) under **Keep Discord files loading**.

> [!WARNING]
> **Use a token instead** imports with your account token. Discord's Terms of Service don't allow this and accounts have been suspended for it. gifly sends it only to `discord.com` and never saves it.

## Privacy

No telemetry or accounts. gifly only connects to GIF hosts (including Discord's CDN) for previews, GitHub for updates, and Discord's API if you give it a token, which goes nowhere else. Your library is at `%APPDATA%\gifly`, the preview cache and log at `%LOCALAPPDATA%\gifly`.

## Building

```
cargo build --release
```

Needs Rust 1.95+. A debug build can use a scratch library: `$env:GIFLY_DIR = "$env:TEMP\gifly-dev"; cargo run`.

## License

MIT. Third-party licenses are in [THIRD-PARTY-NOTICES.txt](THIRD-PARTY-NOTICES.txt).
