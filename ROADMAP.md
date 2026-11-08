# Neria roadmap

Fast, portable, UX-first video/audio downloader. Rust + Slint, yt-dlp + ffmpeg (+ Deno) auto-fetched into `bin/`.

**UX is the priority.** Every feature ships with a clear UI: plain-language status, sensible
defaults, no dead ends, nothing that needs a manual to understand. A feature that works but
feels confusing isn't done.

## Phase 1: Downloader ✅
- First-run auto-fetch of yt-dlp, FFmpeg, Deno; daily `yt-dlp -U`
- Paste link → instant preview (title, thumbnail, channel, length)
- Quality chips (Best / 1080p / 720p / 480p / Audio only), queue with live progress, cancel/retry
- Advanced drawer: container, H.264, audio format, embeds, SponsorBlock, playlists, cookies, rate limit, concurrency, filename template, raw yt-dlp args

## Phase 2: Clipper (local files) ✅
- `neria-player` crate: libmpv (auto-fetched, loaded at runtime) rendered into a GL texture shown by Slint
- Timeline with filmstrip + audio waveform, drag in/out handles, scrubbing, frame stepping,
  shortcuts (Space/K, I/O, J/L and arrows, `,` `.`, Home/End)
- Export: lossless (fast, keyframe-snapped) or re-encode (frame-exact H.264); audio files too
- Scissors button on finished downloads opens them in the clipper
- Lossless mode shows keyframe ticks, highlights in amber any extra footage before the in point
  (where the cut really starts), with a one-click "Snap to keyframe"
- Playback stops at the Out handle; play from there replays the selection from In; loop toggle
  (button / R) repeats the selection
- Player box takes the video's own shape, centred (no black side bars in wide windows,
  vertical videos show as a tall box); checked with 16:9 and 9:16 test videos
- Audio controls (to do), two separate things so it's clear what ends up in the file:
  - **Preview volume**: a volume slider + mute for listening while you edit (mpv `volume`);
    never changes the exported clip
  - **Clip audio**, applied on export: keep as is / mute (no audio track) / adjust gain
    (± dB) / **normalize** loudness (ffmpeg `loudnorm`, EBU R128, presets like -14 LUFS for
    YouTube/TikTok/Discord and -16 for podcasts). Option to preview the effect live (mpv
    `af=loudnorm` / `volume`) before exporting
  - Fast · lossless can still be used: video stays stream-copied, only the audio is
    re-encoded when an audio change is picked (say so in the UI)
- Polish still to do: drag-and-drop files from Explorer

## Phase 3: Clip before download ✅
- "Clip first" on a link preview, or "Paste link" on the Clip page, streams the link in mpv
  (mpv's yt-dlp hook, 720p preview). Filmstrip from the site's storyboards (YouTube's
  hover-preview sprite sheets, cropped per frame); YouTube now answers direct ffmpeg reads of
  its streams with 403, so frames from a low-res stream are only a fallback for other sites
- Download clip sends only the selected range to the queue (`--download-sections`), in the
  chosen quality; Exact adds `--force-keyframes-at-cuts` for frame-exact cuts
- Polish still to do: waveform for links (needs the audio); live/very long streams untested

## Phase 4: Compressor ✅
- Fit a size (20 MB Discord / 25 / 50 / 100 / custom) or pick a quality (slider → CRF/CQ/QP)
- Keeps the original resolution by default; Auto resolution/fps from bits-per-pixel
  (e.g. 10 MB × 60 s → 720p30) as an option; manual caps; audio bitrate or remove; stereo downmix
- H.264 / H.265 / AV1; GPU encoders (NVENC / Quick Sync / AMF) detected by a real test encode,
  CPU fallback with two-pass for x264/x265
- Guaranteed fit: if the result overshoots, it re-encodes smaller (up to 3 tries)
- Compress button on finished downloads
- Polish still to do: before/after preview; batch compress; trim inside the compressor

## Phase 5: Spotify links (spotDL-style)
Spotify audio itself is DRM-protected, so Neria does what spotDL does:
read the track/album/playlist metadata from Spotify, find the matching song on
YouTube Music, download it with the bundled yt-dlp, and tag it with Spotify's metadata.

Status: ✅ core flow done (`spotify.rs`, `ytmusic.rs`, `music.rs`, desktop `spotify_ui.rs`).
Tested on Random Access Memories (13/13 matched at 1.00, whole album in 83 s) and All Out 80s.

Done:
- Paste a Spotify track / album / playlist / artist link (incl. `intl-xx` and `spotify:` URIs):
  preview with cover, song count, length; "Download N songs" queues one row per song, in order
- Metadata like Sunnify: public embed pages, no login, no API key. Limits shown to the user:
  playlists show the first 100 songs, artists their top 10, playlist tracks have no album name
  (filled from YouTube Music). Optional **own** Client ID/secret in Settings → Spotify links
  uses the Web API (full playlists, ISRC, track/disc numbers) and falls back to embed pages
- Matching like spotDL: YouTube Music "Songs" search through its web API (~0.6 s per song),
  ISRC query first when known, scored on title + artist + duration (±3 s full marks, >30 s
  rejected) with penalties for live/remix/cover/re-recorded/… versions the Spotify title
  doesn't mention. Matches below 0.8 are downloaded but flagged "Check match: …" in amber;
  below 0.6 the song fails instead of downloading the wrong thing
- Tags: title, artists, album artist, album, track n/total, disc, date, ISRC, 640px Spotify
  cover; YouTube description/URL noise removed. Lyrics from LRCLIB: embedded (USLT) + synced
  `.lrc` next to the file (toggle in Settings)
- Album tracks named `01 Artist - Title`, playlists `Artist - Title`, one folder per
  album/playlist, `<name>.m3u8` in order
- Resume like Sunnify: re-pasting a link skips songs already on disk ("Already downloaded")
- Sync like spotDL, but safe: `.neria-sync.json` manifest per folder; songs no longer on the
  Spotify list show a "Review N removed" dialog, all unticked by default, ticked ones go to
  the Recycle Bin (only for complete lists, never for truncated ones)

- "Pick another match" (swap button on Spotify rows, amber when flagged or failed): lists the
  YouTube Music candidates with album, length difference and match %, marks the current one,
  free-text search, open-in-browser to listen, and "Use this" re-downloads in place
  (`--force-overwrites`) keeping Spotify's tags, cover and lyrics. Songs with no confident match
  now fail with "Pick one yourself" instead of silently skipping

- Audio check (`fingerprint.rs`, on by default, Settings → Spotify links): after each
  download, Spotify's public 30 s preview (`audioPreview.url` in the embed data) and the file
  are fingerprinted with ffmpeg's Chromaprint muxer; the preview is slid along the song for
  the best bit agreement. Measured: same master 0.98, same recording edited 0.95, same
  recording other master/length 0.76, re-recording 0.58, different song 0.57. Verdicts:
  ≥ 0.72 "✓ sounds like Spotify", 0.66–0.72 "close, maybe another master", below that a
  different recording → automatically tries up to two other good candidates (skipping other
  copies of a version that already failed), then goes back to the closest and flags it
  "Doesn't sound like the Spotify preview (58% similar). Pick another?". ~1 s per song

- Reference preview fallbacks: Spotify's preview first, then Deezer's public API (exact ISRC,
  else search), then Apple's iTunes Search API. A fallback clip is only trusted if its title,
  artist and length (±3 s) match Spotify's, so a re-recording can't become the reference.
  Rows say which preview they matched ("✓ matches the Deezer preview"); with none anywhere
  they say "no preview to check against". Measured with Deezer as reference: original 0.88,
  re-recording 0.64 (cut-offs now 0.72 same / 0.66 unsure)
- Picker sound checks: when the dialog opens it finds the reference and checks the top 3
  matches plus same-title versions ruled out by length (often the album version of the same
  recording), in parallel; every other row has a "Check" button. YouTube refuses plain ffmpeg
  reads, so yt-dlp fetches the candidate's audio to the cache (~10 s each, deleted after).
  "✓ Same recording" rows float to the top. On Bonnie Tyler this surfaces the 6:58 album
  original (76%) above the re-recordings

Still to do:
- Faster candidate checks (yt-dlp start-up + YouTube's JS challenge dominate the ~10 s)
- No-preview-anywhere path is written but not exercised on a real track
- Own-key Web API path is written but untested (needs a real Client ID/secret)
- Spotify playlists > 100 songs without a key: no workaround yet beyond the notice
- Output note: source is YouTube Music (~128–160 kbps Opus/AAC); FLAC/WAV don't add quality
- Risks: Spotify keeps tightening its Web API (Feb 2026: Development Mode needs Premium,
  5-user cap, fewer endpoints), and YouTube Music's web API isn't official; both can break,
  see `docs/UPSTREAM.md` for where to look first

- Reference: Sunnify (sunnypatell/sunnify-spotify-downloader) does exactly this flow
  (public embed pages, no login; YouTube match on title + artist + duration). Its code is under a
  custom educational-use license, so Neria reimplements the approach rather than copying code.

Not planned: Zotify-style downloading of Spotify's own encrypted streams, or streamrip-style
downloading from Qobuz/Tidal/Deezer accounts (DRM/protection circumvention; gets accounts banned).
SoundCloud already works through yt-dlp.

## Before friends test (reliability and feedback)
Release small and often: the high-priority items below → v0.2.0 → collect feedback before
the big features (livestreams, audio tools, library).
- **Report a problem** button: copies a short report (Neria version, tool versions, the link,
  last lines of the yt-dlp/ffmpeg log) to paste in a message or issue
- **Version in the app** (Settings → About), so reports say which build
- **Auto-retry when YouTube breaks**: on known "needs a newer yt-dlp" errors, update yt-dlp
  right away and retry once instead of waiting for the daily update
- **Robust first start**: clear progress for each tool download, Retry button, plain error
  when offline or blocked
- **Writable folder check**: if Neria can't write next to itself (e.g. unzipped into
  Program Files), say "Move Neria to a folder like Documents or Desktop"
- **Cookies hint**: Chrome's newer cookie encryption usually stops yt-dlp from reading
  them; Firefox works. Say so in the cookies picker and in sign-in errors
- **Free space check** before big downloads, livestreams and compressions
- **Release checks**: the release workflow refuses to build if the tag doesn't match the
  `Cargo.toml` version; CI also runs `cargo clippy` and `cargo fmt --check`
- **Code signing** to remove the SmartScreen warning: SignPath gives free signing to
  open-source projects, if the repo goes public

## Features still wanted (before Android)
- **High priority: clear answers for links Neria can't download.** Today every failed preview
  says "You can still try downloading it." and Download stays clickable, so dead links just
  fail again in the queue.
  - Errors that won't change (unsupported site, video unavailable/deleted, private): say it
    plainly ("Neria can't download from this site"), list what does work ("Works with
    YouTube, Spotify, TikTok, Instagram, X, Twitch, SoundCloud and 1,800+ other sites") and
    grey out Download
  - Errors that might pass (network, rate limit, sign-in needed, age-restricted): keep
    Download enabled and say what to try (check connection, pick a browser under Cookies…)
  - Spotify is supported (tracks, albums, playlists, artists), so Spotify links never show
    "unsupported". Spotify links Neria can't handle get their own message instead, e.g.
    podcasts/episodes/shows, audiobooks, private or deleted playlists, region-locked tracks
  - Text that isn't a link: a quiet hint ("Paste a link from YouTube, Spotify…") instead of
    nothing
- **High priority: synced lyrics from more sources (Musixmatch first).** Users love synced
  lyrics, and Musixmatch is where Spotify's own synced lyrics come from, so it has the best
  coverage. Order: LRCLIB (free, open) → Musixmatch (synced) → plain-text fallbacks (Genius,
  AZLyrics, as spotDL does; MIT, port with credit)
  - No user login. Musixmatch's official API only gives synced lyrics on paid plans, so look
    at how spotDL / the `syncedlyrics` project reach Musixmatch without a key, check it
    still works and what the terms risk is, and record it in `docs/UPSTREAM.md`
  - Fully in our code: no keys or files fetched from third-party repos at runtime
  - Each row says the source and "synced" / "plain text only" / "no lyrics found"; Settings
    lets you turn sources on/off and pick "synced only"
  - Also used by the lyrics-only mode and "Add lyrics to songs I already have"
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
  - Ties into the filename builder (Playlist / Playlist # chips)
- **Spotify heads-up dialog**: the first time someone presses Download on a Spotify link,
  explain where the audio comes from: "Spotify doesn't allow downloading its audio. Neria
  reads the song info (title, artist, album, cover) from Spotify and downloads the matching
  song from YouTube Music, then checks it sounds the same. Quality is YouTube Music's
  (~128–160 kbps), and once in a while a match can be a different version; you can swap it."
  Buttons: **I understand** (continues the download) / **Cancel**, plus a "Don't show this
  again" tickbox saved in `settings.json` (re-enable under Settings → Spotify links). Today
  this is only a small "audio from YouTube Music" note on the preview line and a hint in
  Settings
- **Lyrics only (Spotify links)**: a "Lyrics only" choice next to "Download N songs" that
  saves just the lyrics, no audio: synced `.lrc` (and/or plain `.txt`) per song named like the
  audio would be (`01 Artist - Title.lrc`), one folder per album/playlist. Also "Add lyrics to
  songs I already have": fills in `.lrc` files and embedded lyrics for a folder Neria
  downloaded before. Source: LRCLIB (already used, free, no login, synced when available);
  rows say "synced", "plain text only" or "no lyrics found"
  - More lyrics sources: see the high-priority synced lyrics item above
  - Later, opt-in only: "Sign in to Spotify for more synced lyrics" (Spotify's own Musixmatch
    lyrics), off by default with a clear warning about account risk. Must not depend on
    third-party repos at runtime; would need a CLAUDE.md rule change first
  - Looked at akashrchandran/spotify-lyrics-api (Spotify's own Musixmatch lyrics): not used.
    It needs the user's Spotify login cookie (`sp_dc`), its README says it's probably against
    Spotify's terms (account risk), and it's GPL-3.0. Ideas-only reference, like Sunnify
- **Captions / subtitles download** (any video link):
  - On the preview, a "Captions" picker listing every language the video has, split into
    **Made by the creator** and **Auto-generated**, with the video's own language on top;
    tick one or several (search box for long lists, "All" / "None")
  - Format: SRT (plays everywhere, default), VTT, ASS, or plain text (just the words, no
    timings, e.g. for notes/transcripts)
  - Save as separate files next to the video (`Title.en.srt`), embed into the video, or both
  - **Captions only**: download just the captions, no video (`--skip-download`)
  - Remember the last picked languages; "No captions on this video" shown plainly when none
- **Livestreams**:
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
- **Bulk download**: paste several links at once (one per line, or a text file) → one queue
  row per link with the current quality; skip duplicates and links already in the queue
- **Quick download**: one-click "download with my defaults" (skip the preview step), e.g. a
  "Paste & download" button and an optional clipboard watcher that offers detected links
- **Clip render queue**: mark several In/Out ranges on the same video, list them as clips
  (rename, reorder, remove, jump to each), then export them all in one go: from files through
  a local export queue, from links as several `--download-sections` jobs. Option to also join
  them into one file
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
- **Check for updates**: Neria itself (GitHub releases: "Neria x.y is out" with changelog,
  download + swap the exe next to itself, keep `bin/` and `settings.json`, stays portable) and
  the tools: today only yt-dlp updates (daily `yt-dlp -U`); FFmpeg, Deno and libmpv are
  fetched once and never refreshed. Settings → About: versions of everything, "Check now",
  auto-check on/off

### Download, more
- **Simple format presets** on the Download page instead of codec/container talk: Best
  quality · Best compatibility (H.264 MP4, plays everywhere) · Smallest file · Audio only;
  the current chips/containers move under Advanced
- **Watch the link before downloading**: a play button on the link preview that streams it
  (we already stream links for "Clip first"; this is the same player without the trimming)
- **Chapters**: show a video's chapters on the preview; "Split by chapters" saves one file
  per chapter (`--split-chapters`), and the clipper shows chapter marks on the timeline
- **Duplicate check**: warn when a link (or the same video ID) is already in the queue or
  already downloaded, with "Open existing" / "Download again"
- **Queue survives restarts**: waiting and interrupted downloads come back after closing
  Neria, and resume where yt-dlp can
- **Tray + notifications**: closing to the tray keeps downloads running (optional);
  a Windows notification when a download/compress/export finishes or fails
- Later: follow a channel/playlist and download new uploads; schedule downloads

### Clip / edit
- **Rotate and flip** (90° steps, mirror), applied on export (forces re-encode; say so)
- **Crop and aspect ratio**: draw a crop box on the player, or pick 9:16 (Shorts/TikTok),
  1:1, 4:5, 16:9 with a draggable frame; saved with the clip
- **Make a GIF** from the selection: size, fps and loop, with an estimate of the file size
  (ffmpeg palettegen for good colors); also WebP/short MP4 "GIF-like" for Discord
- **Perfect loop GIF** (port of our own loopgif, see `docs/UPSTREAM.md`): a "Perfect loop"
  toggle in Make a GIF finds where the motion inside In/Out starts over and snaps the
  selection to exactly one clean cycle (tiny grayscale fingerprints, seam scored on both
  sides, still parts ignored, shorter cycle wins, matching end frame dropped, per-clip
  palette). Plays the loop on repeat before export; "Try another loop" = next-best cycle;
  plain "No loop found here" with "pick the frames yourself". Native Rust in `neria-core`
  (ffmpeg only, no Python/numpy)
- **Preview the result** before exporting: plays the selection with crop/rotate/audio
  changes applied
- Audio on export: fade in / fade out, mute, gain, normalize (see the audio controls in
  Phase 2); "Save audio only" (extract the selection as MP3/M4A/Opus)
- Joins with the clip render queue: export several clips, optionally merged into one file

### Compress: understandable for everyone, advanced when needed
- Start with plain goals instead of numbers: **For Discord** (20 MB) · **For email**
  (25 MB) · **For WhatsApp** · **Smaller, still sharp** · **As small as possible** · Custom
  size. Each says what it'll look like: "1080p, looks the same" / "720p, a bit softer"
- **Before/after**: original vs. estimated size up front, then actual size and "% smaller";
  a side-by-side (or slider) frame comparison to judge quality before committing
- **Plain-language options**: "Plays everywhere / Smaller / Smallest" instead of
  H.264 / H.265 / AV1; "Use my graphics card (faster)"
- Everything technical (bitrate, CRF/CQ, two-pass, fps, resolution, audio kbps) stays
  available under an **Advanced** fold, never in the way
- **Batch**: drop several files or a folder, same settings for all, one progress list

### Audio tools (own page)
- **Convert** audio: MP3, M4A/AAC, Opus, FLAC, WAV; bitrate (128–320), mono/stereo, sample
  rate under Advanced. Also "Extract audio" from videos. Batch for many files
- **Even out loudness** across songs (EBU R128 / LUFS) so a playlist plays at one volume
- **Tag editor**: title, artist, album, track #, genre, year, cover art, lyrics; edit many
  files at once (e.g. set the album/cover for 100 songs); "Find tags online" from Spotify
  metadata
- **Organize**: move songs into Artist / Album folders, make M3U playlists, find duplicates
- Split audio by timestamps / remove silence at start and end (later)

### Bigger ideas, later (after the above, keep UX simple)
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

Not planned (from outside suggestions, decided against for now):
- spotDL as a dependency: Neria already does its flow natively in Rust (see Phase 5 and
  `docs/UPSTREAM.md`); bundling Python would hurt portability and start-up
- Rewriting storage on SQLite / Tokio: JSON settings + manifests and threads are enough
  today; revisit only if the Library page needs it
- Downloading comments, subtitle translation through online services

Low priority (nice to have, after the above):
- **App icon**: a proper Neria icon for the exe (embedded `.ico`), window title bar, taskbar
  and Alt-Tab, release zip; sized for 16–256 px so it stays crisp in Explorer

## Phase 6: UI overhaul (before Android)
The features grew page by page; before porting, make the UI one coherent system that can
also scale down to a phone.
- Design system: shared tokens (colors, spacing, radii, type scale) and one component set
  (buttons, chips, rows, dialogs, toasts) instead of per-page variations; split `app.slint`
  into per-page/component files
- Layouts that adapt to window width (wide desktop → narrow), so the same screens can become
  the Android layouts
- Queue: group album/playlist songs under one collapsible card with overall progress;
  filters (active / done / needs attention); clearer flagged-match states
- Consistent dialogs (match picker, sync review, settings) and toasts for results like
  "Saved" / "Moved to Recycle Bin"
- Drag and drop files and links anywhere; keyboard navigation and focus states everywhere
- Settings reorganised into sections that match the pages (Download, Clip, Compress, Spotify)
- First-run onboarding, better empty states, light theme option, accessibility pass
  (contrast, screen-reader labels)

## Phase 7: Android (on hold)
On hold until the desktop app has every feature we want; no Android work before then.
- Same `neria-core`; Slint Android UI (built on the Phase 6 adaptive layouts); embedded Python
  for yt-dlp; ffmpeg + libmpv as `.so`
- Share-sheet target ("Share → Neria"); APK via GitHub / F-Droid (Play Store bans downloaders)
