# Neria

Fast, portable, UX-first downloader / clipper / compressor. Rust + Slint, driving yt-dlp, ffmpeg,
Deno and libmpv, all auto-fetched into `bin/` next to the exe. Plan and status: `ROADMAP.md`.

## Layout
- `crates/neria-core`: everything non-UI (tool fetch, yt-dlp probe/download queue, clip export,
  media info). Shared with the future Android app, so no UI or platform UI code here.
- `crates/neria-player`: libmpv loaded at runtime (libloading) + OpenGL render API.
- `apps/desktop`: Slint UI (`ui/app.slint`), `main.rs` (download page), `clipper.rs` (clip page;
  mpv renders into a GL texture shown as a Slint image).

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

Dev env hooks for the app: `NERIA_OPEN` (file or URL into the clipper), `NERIA_CLIP_IN` /
`NERIA_CLIP_OUT`, `NERIA_MPV_LOG` (verbose mpv log file), `NERIA_HOME` (override app dir).
The window can be captured even when covered with `PrintWindow(..., PW_RENDERFULLCONTENT)`.

## Rules
- Portable: settings and tools live next to the exe; nothing system-wide.
- Spotify support = spotDL/Sunnify approach only (metadata from Spotify, audio from YouTube Music).
  Never add Zotify/streamrip-style downloading of DRM-protected streams.
- spotDL and Sunnify are references, not dependencies. `docs/UPSTREAM.md` records the upstream
  commit we last studied. When Spotify/YouTube matching breaks, check upstream changes since that
  baseline first, port the fix (spotDL is MIT; Sunnify is ideas-only), then move the baseline and
  add a review-log entry.
