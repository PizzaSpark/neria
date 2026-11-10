# Neria roadmap

Fast, portable, UX-first video/audio downloader. Rust + Slint, yt-dlp + ffmpeg (+ Deno) auto-fetched into `bin/`.

**UX is the priority.** Every feature ships with a clear UI: plain-language status, sensible
defaults, no dead ends, nothing that needs a manual to understand. A feature that works but
feels confusing isn't done.

**Order:** ship v0.2 to friends → UI foundation (while their feedback comes in) → feature
tracks → livestreams → bigger ideas → Android. Within each section, items are in the order
we'll do them.

## Shipped ✅

### Downloader
- First-run auto-fetch of yt-dlp, FFmpeg, Deno; daily `yt-dlp -U`
- Paste link → instant preview (title, thumbnail, channel, length)
- Quality chips (Best / 1080p / 720p / 480p / Audio only), queue with live progress, cancel/retry
- Advanced drawer: container, H.264, audio format, embeds, SponsorBlock, playlists, cookies,
  rate limit, concurrency, filename template, raw yt-dlp args

### Clipper (local files)
- `neria-player` crate: libmpv (auto-fetched, loaded at runtime) rendered into a GL texture shown by Slint
- Timeline with filmstrip + audio waveform, drag in/out handles, scrubbing, frame stepping,
  shortcuts (Space/K, I/O, J/L and arrows, `,` `.`, Home/End)
- Export: lossless (fast, keyframe-snapped) or re-encode (frame-exact H.264); audio files too
- Lossless mode shows keyframe ticks, highlights in amber any extra footage before the in point,
  with a one-click "Snap to keyframe"
- Playback stops at Out; play from there replays from In; loop toggle (button / R)
- Player box takes the video's own shape (checked with 16:9 and 9:16)
- Scissors button on finished downloads opens them in the clipper

### Clip before download
- "Clip first" on a link preview, or "Paste link" on the Clip page, streams the link in mpv
  (yt-dlp hook, 720p preview). Filmstrip from the site's storyboards (YouTube answers direct
  ffmpeg reads with 403, so frames from a low-res stream are only a fallback for other sites)
- Download clip sends only the selected range (`--download-sections`); Exact adds
  `--force-keyframes-at-cuts`

### Compressor
- Fit a size (20 MB Discord / 25 / 50 / 100 / custom) or pick a quality (slider → CRF/CQ/QP)
- Original resolution by default; Auto resolution/fps from bits-per-pixel as an option; manual
  caps; audio bitrate or remove; stereo downmix
- H.264 / H.265 / AV1; GPU encoders (NVENC / Quick Sync / AMF) detected by a real test encode,
  CPU fallback with two-pass for x264/x265
- Guaranteed fit: re-encodes smaller on overshoot (up to 3 tries); Compress button on downloads

### Spotify links (spotDL-style)
Metadata from Spotify's public embed pages, audio from YouTube Music, tagged with Spotify's
metadata. Track/album/playlist/artist links, scored matching with "Check match" flags, "Pick
another match", audio check against Spotify/Deezer/Apple previews, LRCLIB lyrics, numbered
folders + M3U, resume, safe sync with Recycle Bin review. Full details, measurements and
decisions: `docs/SPOTIFY.md`.

## Now: v0.2 friends test
Release small and often: this list → v0.2.0 → collect feedback before the big features.
- ✅ **Clear answers for links Neria can't download** (`probe::explain`,
  `spotify::unsupported_reason`). Errors that won't change (unsupported site, removed, private,
  DRM, wrong country) show in red with what does work, and Download is greyed out. Errors that
  might pass (offline, rate limit, sign-in, age, members-only, not live yet) show in amber with
  what to try; pasting the link again retries. Spotify podcasts/audiobooks/profiles/short links
  get their own message. Text that isn't a link shows a quiet hint; links without `https://`
  work. Still open: region-locked Spotify tracks (embed page still lists them)
- ✅ **Report a problem** (`report.rs`): a bug button on failed rows copies Neria/Windows/tool
  versions, the settings that matter (never the Spotify key), the link, the error and the last
  40 lines of yt-dlp output; the user folder shows as `%USERPROFILE%`. Settings → About has a
  general "Copy report". A toast confirms. Not yet for Clip/Compress errors
- ✅ **Version in the app**: Settings → About shows Neria, yt-dlp, FFmpeg, Deno, libmpv
- ✅ **Check for updates** (`update.rs`, desktop `updates_ui.rs`): at start-up (Settings → About
  toggle, "Check now") Neria checks GitHub releases of `PizzaSpark/neria`; a banner offers
  "Update now", downloads the zip, swaps the exe in place (bin/ and settings.json untouched) and
  "Restart now" (warns about running downloads). FFmpeg, Deno and libmpv get "Update tools" once
  a newer build exists and ours is over 30 days old (old files moved aside, restored on failure).
  **Needs the repo to be public** (private repos answer 404 to unauthenticated checks)
- **Auto-retry when YouTube breaks**: on known "needs a newer yt-dlp" errors, update yt-dlp
  right away and retry once instead of waiting for the daily update
- ✅ **Robust first start** (desktop `setup_ui.rs`, `tools::explain`): the setup screen lists
  each tool (what it's for, Waiting / its own meter with sizes / Ready / Stopped), and failures
  get one plain sentence (offline or firewall, GitHub rate limit or hiccup, cut-off download,
  full drive, antivirus/permission) with "Try again" and "Copy details". Retry resumes with
  what's still missing
- ✅ **Writable folder check** (`paths::check_writable`): if Neria can't write next to itself
  (e.g. unzipped into Program Files) the setup screen says "Move Neria somewhere else.", shows
  the folder, and offers "Show Neria's folder" / "Check again"; nothing gets downloaded
- **Spotify heads-up dialog**: the first time someone presses Download on a Spotify link,
  explain where the audio comes from: "Spotify doesn't allow downloading its audio. Neria
  reads the song info (title, artist, album, cover) from Spotify and downloads the matching
  song from YouTube Music, then checks it sounds the same. Quality is YouTube Music's
  (~128–160 kbps), and once in a while a match can be a different version; you can swap it."
  Buttons: **I understand** (continues the download) / **Cancel**, plus a "Don't show this
  again" tickbox saved in `settings.json` (re-enable under Settings → Spotify links). Today
  this is only a small "audio from YouTube Music" note on the preview line and a hint in
  Settings
- **Cookies hint**: Chrome's newer cookie encryption usually stops yt-dlp from reading
  them; Firefox works. Say so in the cookies picker and in sign-in errors
- **Free space check** before big downloads, livestreams and compressions
- **Drag and drop files from Explorer** onto the Clip and Compress pages
- **App icon**: a proper Neria icon for the exe (embedded `.ico`), window title bar, taskbar
  and Alt-Tab, release zip; sized for 16–256 px so it stays crisp in Explorer
- **Release checks**: the release workflow refuses to build if the tag doesn't match the
  `Cargo.toml` version; CI also runs `cargo clippy` and `cargo fmt --check`
- Optional: **code signing** to remove the SmartScreen warning (SignPath gives free signing to
  open-source projects, if the repo goes public)

## Next: UI foundation
The features grew page by page. Make the UI one coherent system now, while it's small, so the
features below are built once on it instead of rebuilt later. Friends' feedback feeds in here.
Direction chosen: **Studio + Play** (`docs/DESIGN.md`, mockups in `design/download-directions.html`):
calm where you read and watch, playful where you touch; sticker yellow, Space Grotesk / Inter
Tight / JetBrains Mono, dark and light themes.
- ✅ First pass: tokens and bundled fonts (`ui/theme.slint`), shared controls
  (`ui/widgets.slint`), Download page rebuilt (link box with Download inside, big preview, queue
  as calm rows with labelled actions instead of unlabelled icons, friendlier empty state),
  Clip/Compress/Settings restyled, Settings → Appearance (Same as Windows / Dark / Light)
- ✅ First-run setup screen in the new style (logo sticker, headline, calm tool rows)
- Next: split the pages out of `app.slint`, restyle the match picker and sync review in
  detail, then the items below
- Design system: shared tokens (colors, spacing, radii, type scale) and one component set
  (buttons, chips, rows, dialogs, toasts) instead of per-page variations; split `app.slint`
  into per-page/component files
- Settings reorganised into sections that match the pages (Download, Clip, Compress, Spotify)
- Consistent dialogs (match picker, sync review, settings) and toasts for results like
  "Saved" / "Moved to Recycle Bin"
- Layouts that adapt to window width (wide desktop → narrow), so the same screens can later
  become the Android layouts
- Queue: group album/playlist songs under one collapsible card with overall progress;
  filters (active / done / needs attention); clearer flagged-match states
- Drag and drop links anywhere (files already done in v0.2); keyboard navigation and focus
  states everywhere
- First-run onboarding, better empty states, light theme (follows Windows), accessibility pass
  (contrast, screen-reader labels)

## Then: feature tracks
Grouped by page. Tracks can be interleaved; within a track, top first.

### Download
- **Queue order**: drag rows to reorder waiting downloads; "Move to top" / "Move to bottom"
  in each row's menu (and Ctrl+↑ / Ctrl+↓); jobs already downloading stay put, the next free
  slot takes the top waiting row. Albums/playlists move as one group (and can be reordered
  inside). Optional pause/resume per row, "Pause all"
- **Folders and order** (today: everything goes to `Videos\Neria`; Spotify albums/playlists
  get their own folder + numbered names/M3U, but YouTube playlists land loose in the main
  folder, unnumbered, so their order is lost)
  - Separate places for audio and video: audio (Audio only, Spotify) to `Music\Neria`, video
    to `Videos\Neria`, each changeable in Settings
  - YouTube/other playlists like Spotify ones: a folder per playlist, files numbered in
    playlist order (`01 Title`), a `.m3u8`, and resume/skip what's already there
  - Optional "Number Spotify playlist songs" (`01 Artist - Title`) so the folder sorts in
    playlist order without the M3U
- **Filename builder** (drag and drop, no yt-dlp template syntax):
  - A palette of field chips: Title, Channel, Upload date, Video ID, Playlist, Playlist #,
    Resolution, Duration, Site…
  - Drag chips into a builder line, reorder them by dragging, remove with × or by dragging
    out; type anything between them as plain text (` -- `, ` [`, `] `, `_`…). A `/` makes a
    folder (e.g. Channel / Title)
  - Live example under the builder using the current link (or a sample video), e.g.
    `Rick Astley -- Never Gonna Give You Up [dQw4w9WgXcQ].mp4`; warns about characters
    Windows doesn't allow and names that would collide
  - Presets ("Title", "Channel - Title", "Date - Title", "Title [ID]", "Playlist / 01 Title")
    fill the builder as a starting point; save your own as named presets
  - Behind the scenes it becomes the yt-dlp `-o` template; "Advanced" still shows the raw
    template for power users
- **Simple format presets** instead of codec/container talk: Best quality · Best
  compatibility (H.264 MP4, plays everywhere) · Smallest file · Audio only; the current
  chips/containers move under Advanced
- **Duplicate check**: warn when a link (or the same video ID) is already in the queue or
  already downloaded, with "Open existing" / "Download again"
- **Queue survives restarts**: waiting and interrupted downloads come back after closing
  Neria, and resume where yt-dlp can
- **Bulk download**: paste several links at once (one per line, or a text file) → one queue
  row per link with the current quality; skip duplicates and links already in the queue
- **Quick download**: one-click "download with my defaults" (skip the preview step), e.g. a
  "Paste & download" button and an optional clipboard watcher that offers detected links
- **Captions / subtitles download** (any video link):
  - On the preview, a "Captions" picker listing every language the video has, split into
    **Made by the creator** and **Auto-generated**, with the video's own language on top;
    tick one or several (search box for long lists, "All" / "None")
  - Format: SRT (plays everywhere, default), VTT, ASS, or plain text (just the words, no
    timings, e.g. for notes/transcripts)
  - Save as separate files next to the video (`Title.en.srt`), embed into the video, or both
  - **Captions only**: download just the captions, no video (`--skip-download`)
  - Remember the last picked languages; "No captions on this video" shown plainly when none
- **Chapters**: show a video's chapters on the preview; "Split by chapters" saves one file
  per chapter (`--split-chapters`), and the clipper shows chapter marks on the timeline
- **Watch the link before downloading**: a play button on the link preview that streams it
  (same player as "Clip first", without the trimming)
- **Tray + notifications**: closing to the tray keeps downloads running (optional);
  a Windows notification when a download/compress/export finishes or fails
- Later: follow a channel/playlist and download new uploads; schedule downloads

### Music (Spotify and lyrics)
- **Synced lyrics from more sources (Musixmatch first).** Musixmatch is where Spotify's own
  synced lyrics come from, so it has the best coverage. Order: LRCLIB (free, open) →
  Musixmatch (synced) → plain-text fallbacks (Genius, AZLyrics, as spotDL does; MIT, port
  with credit)
  - No user login. Musixmatch's official API only gives synced lyrics on paid plans, so look
    at how spotDL / the `syncedlyrics` project reach Musixmatch without a key, check it
    still works and what the terms risk is, and record it in `docs/UPSTREAM.md`
  - Fully in our code: no keys or files fetched from third-party repos at runtime
  - Each row says the source and "synced" / "plain text only" / "no lyrics found"; Settings
    lets you turn sources on/off and pick "synced only"
- **Lyrics only (Spotify links)**: a "Lyrics only" choice next to "Download N songs" that
  saves just the lyrics, no audio: synced `.lrc` (and/or plain `.txt`) per song named like the
  audio would be (`01 Artist - Title.lrc`), one folder per album/playlist. Also "Add lyrics to
  songs I already have": fills in `.lrc` files and embedded lyrics for a folder Neria
  downloaded before. Uses the same lyrics sources as above
  - Later, opt-in only: "Sign in to Spotify for more synced lyrics" (Spotify's own Musixmatch
    lyrics), off by default with a clear warning about account risk. Must not depend on
    third-party repos at runtime; would need a CLAUDE.md rule change first
- Loose ends from the Spotify work:
  - Faster candidate checks (yt-dlp start-up + YouTube's JS challenge dominate the ~10 s)
  - No-preview-anywhere path is written but not exercised on a real track
  - Own-key Web API path is written but untested (needs a real Client ID/secret)
  - Spotify playlists > 100 songs without a key: no workaround yet beyond the notice

### Clip / edit
- **Audio controls**, two separate things so it's clear what ends up in the file:
  - **Preview volume**: a volume slider + mute for listening while you edit (mpv `volume`);
    never changes the exported clip
  - **Clip audio**, applied on export: keep as is / mute (no audio track) / adjust gain
    (± dB) / **normalize** loudness (ffmpeg `loudnorm`, EBU R128, presets like -14 LUFS for
    YouTube/TikTok/Discord and -16 for podcasts) / fade in / fade out. Option to preview the
    effect live (mpv `af=loudnorm` / `volume`) before exporting
  - Fast · lossless can still be used: video stays stream-copied, only the audio is
    re-encoded when an audio change is picked (say so in the UI)
  - "Save audio only": extract the selection as MP3/M4A/Opus
- **Clip render queue**: mark several In/Out ranges on the same video, list them as clips
  (rename, reorder, remove, jump to each), then export them all in one go: from files through
  a local export queue, from links as several `--download-sections` jobs. Option to also join
  them into one file
- **Make a GIF** from the selection: size, fps and loop, with an estimate of the file size
  (ffmpeg palettegen for good colors); also WebP/short MP4 "GIF-like" for Discord
- **Perfect loop GIF** (port of our own loopgif, see `docs/UPSTREAM.md`): a "Perfect loop"
  toggle in Make a GIF finds where the motion inside In/Out starts over and snaps the
  selection to exactly one clean cycle (tiny grayscale fingerprints, seam scored on both
  sides, still parts ignored, shorter cycle wins, matching end frame dropped, per-clip
  palette). Plays the loop on repeat before export; "Try another loop" = next-best cycle;
  plain "No loop found here" with "pick the frames yourself". Native Rust in `neria-core`
  (ffmpeg only, no Python/numpy)
- **Crop and aspect ratio**: draw a crop box on the player, or pick 9:16 (Shorts/TikTok),
  1:1, 4:5, 16:9 with a draggable frame; saved with the clip
- **Rotate and flip** (90° steps, mirror), applied on export (forces re-encode; say so)
- **Preview the result** before exporting: plays the selection with crop/rotate/audio
  changes applied
- Links: waveform for streamed links (needs the audio); test live/very long streams

### Compress: understandable for everyone, advanced when needed
- Start with plain goals instead of numbers: **For Discord** (20 MB) · **For email**
  (25 MB) · **For WhatsApp** · **Smaller, still sharp** · **As small as possible** · Custom
  size. Each says what it'll look like: "1080p, looks the same" / "720p, a bit softer"
- **Plain-language options**: "Plays everywhere / Smaller / Smallest" instead of
  H.264 / H.265 / AV1; "Use my graphics card (faster)"
- Everything technical (bitrate, CRF/CQ, two-pass, fps, resolution, audio kbps) stays
  available under an **Advanced** fold, never in the way
- **Before/after**: original vs. estimated size up front, then actual size and "% smaller";
  a side-by-side (or slider) frame comparison to judge quality before committing
- **Batch**: drop several files or a folder, same settings for all, one progress list
- **Trim inside the compressor** (simple In/Out, without going to the Clip page)

## Livestreams (own milestone)
- Two ways to record, picked automatically and said plainly on the preview:
  - **From the start**: when the stream allows rewinding (YouTube with DVR on), download
    everything since it went live (`--live-from-start`) and keep following it live
  - **From now**: when the creator turned rewinding off (or the site doesn't support it),
    record from the moment you press Download. The preview says "This stream can't be
    rewound, recording starts now"
- Scheduled streams: "Waiting for the stream to start" with a countdown (`--wait-for-video`)
- Stop button that finishes the file cleanly (playable even if stopped mid-way); recording
  also ends on its own when the stream ends
- **Live timeline bar** on the queue row: a strip from stream start to "now" that fills as
  segments arrive. Recorded parts solid, the part before you joined (From now) hatched,
  gaps where segments were missed or the connection dropped shown as visible holes with
  their time ("missed 0:42 at 1:13:05"), and a pulsing live edge
- **Stats** under the bar: recorded length, file size, current download speed, stream
  bitrate/resolution, how far behind live you are, segments received / retried / missed,
  connection status (OK / reconnecting / lost)
- Clipping a live stream: pick a range from the rewindable window

## Later: bigger ideas (keep UX simple)
- **Audio tools page**:
  - **Convert** audio: MP3, M4A/AAC, Opus, FLAC, WAV; bitrate (128–320), mono/stereo, sample
    rate under Advanced. Also "Extract audio" from videos. Batch for many files
  - **Even out loudness** across songs (EBU R128 / LUFS) so a playlist plays at one volume
  - **Tag editor**: title, artist, album, track #, genre, year, cover art, lyrics; edit many
    files at once (e.g. set the album/cover for 100 songs); "Find tags online" from Spotify
    metadata
  - **Organize**: move songs into Artist / Album folders, make M3U playlists, find duplicates
  - Split audio by timestamps / remove silence at start and end
- **Library page**: everything Neria downloaded/made, with thumbnails/covers, search,
  audio/video filters, sort by date/size/length; open, show in folder, clip/compress again
- **Command palette** (Ctrl+K): type "compress", "extract audio", "clip"… to jump to any
  action; also lists recent files
- **Saved workflows**: start with simple "after download" actions (extract audio,
  compress for Discord, move to folder) before any full workflow builder; a builder only if
  people actually want chains like download → clip → 9:16 → compress
- Watched folder: auto-compress/convert files dropped into a folder
- **External tools** (instead of a plugin system): Settings → Tools lets power users add
  their own command-line tools as actions ("Run on this file" / "Run on selection") with
  placeholders like `{file}`, `{in}`, `{out}`, `{dir}`; jobs run in Neria's queue with
  output/errors shown. If many people use it, that's the signal a real extension system is
  worth it. Not a plugin API: no extension code loaded into Neria, nothing bundled (users
  bring their own tools, e.g. Python scripts)

## Android (on hold)
On hold until the desktop app has every feature we want; no Android work before then.
- Same `neria-core`; Slint Android UI (built on the adaptive layouts from the UI foundation);
  embedded Python for yt-dlp; ffmpeg + libmpv as `.so`
- Share-sheet target ("Share → Neria"); APK via GitHub / F-Droid (Play Store bans downloaders)

## Not planned
- Zotify-style downloading of Spotify's own encrypted streams, or streamrip-style downloading
  from Qobuz/Tidal/Deezer accounts (DRM/protection circumvention; gets accounts banned)
- spotDL as a dependency: Neria already does its flow natively in Rust (see
  `docs/SPOTIFY.md` and `docs/UPSTREAM.md`); bundling Python would hurt portability and
  start-up
- Rewriting storage on SQLite / Tokio: JSON settings + manifests and threads are enough
  today; revisit only if the Library page needs it
- Downloading comments, subtitle translation through online services
