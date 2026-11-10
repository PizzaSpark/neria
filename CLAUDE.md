# Neria

Fast, portable, UX-first downloader / clipper / compressor. Rust + Slint, driving yt-dlp, ffmpeg,
Deno and libmpv, all auto-fetched into `bin/` next to the exe. Plan and status: `ROADMAP.md`.
UI design system: `docs/DESIGN.md` (Studio + Play); follow it for any new or changed screen.

## Layout
- `crates/neria-core`: everything non-UI (tool fetch, yt-dlp probe/download queue, clip export,
  compression, media info, Spotify: `spotify.rs` links/metadata, `ytmusic.rs` matching,
  `music.rs` tags/lyrics/M3U/sync). Shared with the future Android app, so no UI code here.
- `crates/neria-player`: libmpv loaded at runtime (libloading) + OpenGL render API.
- `apps/desktop`: Slint UI (`ui/app.slint` screens, `ui/widgets.slint` shared controls,
  `ui/theme.slint` tokens + bundled fonts in `ui/fonts/`), `main.rs` (download page), `clipper.rs` (clip page;
  mpv renders into a GL texture shown as a Slint image), `compressor.rs` (compress page).

## Commands
```bash
cargo test -p neria-core
cargo run -p neria
cargo build --release -p neria
```
Rust isn't on PATH in fresh shells here: prefix with `$env:Path += ";$env:USERPROFILE\.cargo\bin"`.

Headless checks (tools must exist in `target/debug/bin`, i.e. run the app once):
- `NERIA_HOME=target/debug cargo run -p neria-core --example smoke -- <url> [audio|video] [out] [start end [exact]]`
- `NERIA_HOME=target/debug cargo run -p neria-core --example clip -- <file> <start> <end> [fast|exact]`
- `NERIA_HOME=target/debug cargo run -p neria-core --example compress -- <file> <mb|q0.0-1.0> [h264|hevc|av1] [cpu|gpu]`
- `NERIA_HOME=target/debug cargo run -p neria-core --example spotify -- <spotify link> <out_dir> [max_tracks] [--match-only] [--rematch]`
- `NERIA_HOME=target/debug cargo run -p neria-core --example verify -- <spotify track id> [--no-spotify] <file|youtube url>...`
  (audio check scores; `--no-spotify` forces the Deezer/Apple fallback reference)
- `NERIA_HOME=target/debug cargo run -p neria-core --example updates -- [--any-age] [--update <tool>]`
  (new Neria release / newer tool builds; `--update` re-downloads into NERIA_HOME/bin, use a scratch copy)

Dev env hooks for the app: `NERIA_OPEN` (file or URL into the clipper), `NERIA_CLIP_IN` /
`NERIA_CLIP_OUT`, `NERIA_CLIP_PLAY` (start playing from In), `NERIA_CLIP_LOOP` (loop on), `NERIA_COMPRESS` (file onto the Compress page), `NERIA_COMPRESS_START`
(auto-start it), `NERIA_URL` (paste a link on the Download page), `NERIA_URL_START` (press
Download once a preview is ready, unless it's greyed out), `NERIA_REPORT` (copy the report of the first
failed download), `NERIA_SETTINGS` (open the settings drawer), `NERIA_UPDATE_URL` (check this "latest
release" JSON instead of GitHub; test self-update with a local server and a copy of the exe),
`NERIA_UPDATE_INSTALL` (install a found update; `restart` also restarts), `NERIA_DEMO` (sample queue
rows in every state, for design checks), `NERIA_SYNC_REVIEW` (open the removed-songs dialog),
`NERIA_MATCH_PICK` (open "pick another match" on the first flagged song),
`NERIA_MPV_LOG` (verbose mpv log file), `NERIA_DEBUG_ARGS` (print ffmpeg args for compression),
`NERIA_HOME` (override app dir; for UI tests point it at a scratch folder with its own
`settings.json` and a junction `bin` → `target/debug/bin`, so downloads don't land in the real
Videos folder).
The window can be captured even when covered with `PrintWindow(..., PW_RENDERFULLCONTENT)`.

## Rules
- Portable: settings and tools live next to the exe; nothing system-wide.
- Spotify support = spotDL/Sunnify approach only (metadata from Spotify, audio from YouTube Music).
  Never add Zotify/streamrip-style downloading of DRM-protected streams.
- spotDL and Sunnify are references, not dependencies. `docs/UPSTREAM.md` records the upstream
  commit we last studied. When Spotify/YouTube matching breaks, check upstream changes since that
  baseline first, port the fix (spotDL is MIT; Sunnify is ideas-only), then move the baseline and
  add a review-log entry.
