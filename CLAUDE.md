# Neria

Fast, portable, UX-first downloader / clipper / compressor. Rust + Slint, driving yt-dlp, ffmpeg,
Deno, gallery-dl (photos in posts) and libmpv, all auto-fetched into `bin/` next to the exe. Plan and status: `ROADMAP.md`.
UI design system: `docs/DESIGN.md` (Studio + Play); follow it for any new or changed screen,
including its "Adding a screen or a control" checklist. Screenshots of every page and dialog at
every width: `powershell -File scripts/ui-check.ps1` (sheets in `target/ui-check`).

## Layout
- `crates/neria-core`: everything non-UI (tool fetch, yt-dlp probe/download queue, clip export,
  compression, media info, Spotify: `spotify.rs` links/metadata, `ytmusic.rs` matching,
  `music.rs` tags/lyrics/M3U/sync, `post.rs` items of posts/playlists for the picker,
  `folders.rs` Audio/Video/Photos folders). Shared with the future Android app, so no UI code here.
- `crates/neria-player`: libmpv loaded at runtime (libloading) + OpenGL render API.
- `apps/desktop`: Slint UI in `ui/`:
  - `app.slint`: `AppWindow` only, the one API Rust sees (all properties/callbacks); wires them
    into the pages and overlays. Re-exports the structs and `Fmt` from `types.slint`, and the
    feature globals from `globals.slint` (`Accounts`, `Extras`, `Background`, `QueueActions`,
    `Presets`, `Names`): newer features keep their state there and pages import them directly,
    instead of threading every property through `app.slint` (Rust: `ui.global::<Extras>()`).
  - `pages/`: `download`, `clip`, `compress`, `settings` (drawer), `setup` (first run).
  - `dialogs/`: `match` (pick another match), `sync` (removed songs), `spotify` (heads-up),
    `drop` (drag-and-drop overlay + clip/compress choice), `tracks` (captions / audio tracks /
    chapters picker), `background` (close-to-tray question, Download later), `names`
    (filename builder).
  - `components/`: `job-row`, `quality-row` (presets + Advanced), `pick-list`, `timeline` (+
    `ChapterStrip`), `extras-row`, `sidebar` (rail + compact `TabBar`), `update-banner`.
  - `widgets.slint`: the one shared control set (buttons, chips, `ChoiceChips`, `Tick`, `Tag`,
    `Notice`, `Divider`, `ListRow`, `Thumb`, `Sticker`/`LogoMark`, `EmptyState`, `Dialog`, `Menu`,
    `Toast`…). `theme.slint`'s `Screen` global holds the width breakpoints (wide / medium /
    compact, see `docs/DESIGN.md`); pages read `Screen.compact` etc. instead of taking the width.
    Reuse these instead of restyling per page. `theme.slint`: tokens + bundled fonts in `ui/fonts/`.
  - Pages take plain properties (no `clip-`/`cmp-` prefixes) and are bound in `app.slint`.

  Rust: `main.rs` (download page), `clipper.rs` (clip page; mpv renders into a GL texture shown
    as a Slint image), `compressor.rs` (compress page), `signin.rs` (`neria --sign-in <site>`:
    the WebView2 sign-in window, its own process) + `signin_ui.rs`, `extras_ui.rs` (captions,
    audio tracks, chapters), `background_ui.rs` (tray, notifications, Download later),
    `names_ui.rs` (filename builder). Core: `cookies.rs` (per-site cookie files, only ever
    passed for links of their own site), `tracks.rs`, `names.rs`.

## Commands
```bash
cargo test -p neria-core
cargo test -p neria --test ui_rules   # UI guardrails (colours, keyboard, labels, widths, contrast)
cargo run -p neria
cargo build --release -p neria
```
Rust isn't on PATH in fresh shells here: prefix with `$env:Path += ";$env:USERPROFILE\.cargo\bin"`.

Headless checks (tools must exist in `target/debug/bin`, i.e. run the app once):
- `NERIA_HOME=target/debug cargo run -p neria-core --example smoke -- <url> [audio|video] [out] [start end [exact]]`
  (posts/playlists: lists the items and downloads them all, or `NERIA_PICK=1,3`)
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
failed download), `NERIA_PAGE` (`clip` or `compress`: open that page, empty), `NERIA_FILTER` (queue filter: `active`, `done` or `attention`), `NERIA_SETTINGS` (open the settings drawer; `download`, `spotify`, `network`, `general` (was `appearance`, still accepted) or `about` picks the section), `NERIA_UPDATE_URL` (check this "latest
release" JSON instead of GitHub; test self-update with a local server and a copy of the exe),
`NERIA_UPDATE_INSTALL` (install a found update; `restart` also restarts), `NERIA_DEMO` (sample queue
rows in every state, for design checks), `NERIA_SYNC_REVIEW` (open the removed-songs dialog), `NERIA_DIALOG` (open a dialog with sample
data: `match`, `sync`, `spotify`, `pick` (post item picker), `toast`, `update`, `update-ready`, `update-failed`,
`signin` (a problem that sign-in fixes), `extras` / `captions` / `audio` / `chapters` (a video with all three, the
picker open), `advanced` (presets with Advanced open), `close` (close-to-tray question), `later`, `names`),
`NERIA_MATCH_PICK` (open "pick another match" on the first flagged song),
`NERIA_WINDOW` (window size in logical px, e.g. `780x580`), `NERIA_FAKE_FREE_MB` (pretend the
drive has this much free, to test the free-space checks), `NERIA_DROP_HOVER` / `NERIA_DROP`
(a file path or link: show the drag-and-drop overlay for it / act as if it was dropped),
`NERIA_DROP_CLIPBOARD` (drop whatever is on the clipboard, through the real OLE drop code), `NERIA_DROP_LOG` (a file: log what each real drag offers and what Neria made of it), `NERIA_MPV_LOG` (verbose mpv log file), `NERIA_DEBUG_ARGS` (print ffmpeg args for compression),
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
