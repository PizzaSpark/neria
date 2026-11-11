# Neria roadmap

Fast, portable, UX-first downloader / clipper / compressor. Rust + Slint, driving yt-dlp,
FFmpeg, Deno, gallery-dl (photos in posts) and libmpv, all auto-fetched into `bin/`.

**UX is the priority.** Every feature ships with a clear UI: plain-language status, sensible
defaults, no dead ends, nothing that needs a manual to understand. A feature that works but
feels confusing isn't done.

**Order:** v0.3 (finish and ship what's built) → UI foundation → feature tracks →
livestreams → bigger ideas → Android. Within each section, items are in the order we'll do
them; related ideas are merged into one item so they get built together.

## Shipped ✅

### v0.1: the basics
- **Downloader**: first-run auto-fetch of yt-dlp, FFmpeg, Deno; daily `yt-dlp -U`. Paste link
  → instant preview (title, thumbnail, channel, length). Quality chips (Best / 1080p / 720p /
  480p / Audio only), queue with live progress, cancel/retry. Advanced drawer: container,
  H.264, audio format, embeds, SponsorBlock, playlists, cookies, rate limit, concurrency,
  filename template, raw yt-dlp args
- **Clipper (local files)**: `neria-player` crate (libmpv, auto-fetched, loaded at runtime,
  rendered into a GL texture shown by Slint). Timeline with filmstrip + audio waveform, drag
  in/out handles, scrubbing, frame stepping, shortcuts (Space/K, I/O, J/L and arrows, `,` `.`,
  Home/End). Export lossless (fast, keyframe-snapped) or re-encode (frame-exact H.264); audio
  files too. Lossless mode shows keyframe ticks, highlights in amber any extra footage before
  the in point, with a one-click "Snap to keyframe". Playback stops at Out; loop toggle (R).
  Player box takes the video's own shape. Scissors button on finished downloads
- **Clip before download**: "Clip first" on a link preview, or "Paste link" on the Clip page,
  streams the link in mpv (720p preview). Filmstrip from the site's storyboards. Download clip
  sends only the selected range (`--download-sections`); Exact adds
  `--force-keyframes-at-cuts`
- **Compressor**: fit a size (20 MB Discord / 25 / 50 / 100 / custom) or pick a quality.
  Original resolution by default; Auto resolution/fps as an option; manual caps; audio bitrate
  or remove; stereo downmix. H.264 / H.265 / AV1; GPU encoders (NVENC / Quick Sync / AMF)
  detected by a real test encode, CPU fallback with two-pass. Guaranteed fit (re-encodes
  smaller on overshoot, up to 3 tries); Compress button on downloads
- **Spotify links (spotDL-style)**: metadata from Spotify's public embed pages, audio from
  YouTube Music, tagged with Spotify's metadata. Track/album/playlist/artist links, scored
  matching with "Check match" flags, "Pick another match", audio check against
  Spotify/Deezer/Apple previews, LRCLIB lyrics, numbered folders + M3U, resume, safe sync with
  Recycle Bin review. Details: `docs/SPOTIFY.md`

### v0.2.0: friends test
- **Clear answers for links Neria can't download** (`probe::explain`,
  `spotify::unsupported_reason`). Errors that won't change (unsupported site, removed, private,
  DRM, wrong country) show in red with what does work, and Download is greyed out. Errors that
  might pass (offline, rate limit, sign-in, age, members-only, not live yet) show in amber with
  what to try; pasting the link again retries. Spotify podcasts/audiobooks/profiles/short links
  get their own message; links without `https://` work. Still open: region-locked Spotify
  tracks (embed page still lists them)
- **Report a problem** (`report.rs`): a bug button on failed rows copies versions, the settings
  that matter (never the Spotify key), the link, the error and the last 40 lines of yt-dlp
  output; the user folder shows as `%USERPROFILE%`. Settings → About has a general "Copy
  report". Not yet for Clip/Compress errors
- **Versions and updates** (`update.rs`, desktop `updates_ui.rs`): Settings → About shows every
  tool's version. At start-up Neria checks GitHub releases of `PizzaSpark/neria`; a banner
  offers "Update now", swaps the exe in place (bin/ and settings.json untouched) and "Restart
  now". FFmpeg, Deno and libmpv get "Update tools" once a newer build exists and ours is over
  30 days old. Works because the repo is public
- **Auto-retry when YouTube breaks** (`probe::needs_newer_ytdlp`): a download that fails the
  way an outdated yt-dlp fails says "The site changed. Updating the downloader, then trying
  again…", runs `yt-dlp -U` (one job at a time, at most every 30 min) and retries once
- **Robust first start** (`setup_ui.rs`, `tools::explain`): the setup screen lists each tool
  (what it's for, its own meter, Ready / Stopped); failures get one plain sentence (offline,
  GitHub rate limit, cut-off download, full drive, antivirus) with "Try again" and "Copy
  details". **Writable folder check**: Neria unzipped into Program Files says "Move Neria
  somewhere else."
- **Spotify heads-up dialog** on the first Download: song info from Spotify, audio from YouTube
  Music (~128–160 kbps), each song checked and swappable; "Don't show this again"
- **Cookies hint**: Chrome, Edge, Brave, Opera and Vivaldi lock their cookies, Firefox works
  best; sign-in errors point to Settings → Network → Cookies
- **Free space check** (`disk.rs`): 200 MB spare to start, then the rest must fit (including
  the merge, re-checked every 3 s for livestreams); stops with "Not enough space on D: (381 MB
  free, needs about 1.5 GB)" and removes the partial files. Compress checks too
- **Drag and drop files from Explorer** (`drop_ui.rs`): an overlay says what will happen;
  Clip/Compress open the file, Download asks "Clip it / Compress it", audio goes to Clip
- **X GIFs saved as real GIFs** (`gif.rs`): X serves "GIFs" as short silent MP4s; Neria spots
  them, shows "GIF" instead of quality chips and turns the MP4 into a looping .gif
  (palettegen, width capped at 720). Settings → Extras keeps the MP4 instead
- **App icon**: an "N" whose left stem is a download arrow, on the yellow sticker
  (`design/icon/make_icon.py`, embedded by `build.rs`)
- **Release checks**: tag must match `Cargo.toml`; CI and release run `cargo fmt --check` and
  `cargo clippy -D warnings`; release notes from `docs/releases/<tag>.md`
- **UI foundation, first pass**: Studio + Play (`docs/DESIGN.md`): tokens and bundled fonts
  (`ui/theme.slint`), shared controls (`ui/widgets.slint`), Download page rebuilt, Clip /
  Compress / Settings restyled, Appearance (Same as Windows / Dark / Light), setup screen,
  one shared `Dialog` and `Tick` (match picker, sync review, update banner, toast)

### Since v0.2.0 (in `main`, not released yet)
- **Pick what to download from posts and playlists** (`post.rs`): X, Instagram and Facebook
  posts with several photos/videos/GIFs, and playlists, list every item under the preview
  (thumbnail, name, length) with ticks, "Select all" / "None" and "Download 3 items". Each
  picked item is its own queue row. Each video row has its own quality menu ("As above" /
  Best / 1080p / 720p / 480p / Audio only). "Number the files in order" tick
- **Photos in posts**: from gallery-dl (new bundled tool, from `gdl-org/builds`) for X and
  Facebook (Facebook `share/` links resolved through yt-dlp first), from yt-dlp itself for
  Instagram; saved directly. Photo and GIF rows only offer Show (nothing to clip/compress)
- **Every X GIF in a post** becomes a .gif (before: only the last file of the job)
- **Honest post errors**: "Neria couldn't find a video in this post" (no pointless yt-dlp
  update any more); photos-only Facebook posts say the video couldn't be reached; Instagram's
  anonymous limit gets its own message pointing to Cookies
- **Folders** (`folders.rs`): `Neria\Audio`, `Neria\Video`, `Neria\Photos`; several items
  picked together get a folder named after the post/playlist, inside the kind's folder when
  they're all one kind, else next to them. Spotify albums synced before keep their old folder.
  Settings: "Sort into Audio, Video and Photos folders"
- **Save the thumbnail as a .jpg** next to the video (Settings → Extras, off by default)
- **Clip page for links** shows the Download page's quality chips and format button (same
  settings); X GIF links say "Saved as a .gif"
- **The container setting always applies**: single files that need no merge (YouTube format
  18) are remuxed to mp4/mkv; webm picks webm streams where the site has them

## Now: v0.3
Finish and ship what's in `main`, plus the loose ends that make it feel complete.
1. **Check what's built** on real links: clip a single-GIF X link (untested: needs a post
   that's just one GIF), an Instagram carousel all the way through with cookies, a long
   YouTube playlist in the picker (scrolling, thumbnails, 100+ items), Audio only on a
   playlist (`Audio\<playlist>\01 …`)
2. **Playlist folders like Spotify ones**: one `.m3u8` per playlist folder, skip items already
   there on a re-download, and **one group card in the queue** for a playlist/post/album
   (overall progress, collapsible, "3 of 12 done")
3. **Pause / resume, and the queue survives restarts**: Pause on each downloading row and
   "Pause all"; yt-dlp stops but keeps its `.part` files, Resume picks up where it left off
   (`--continue`), the row says "Paused · 120 MB of 388 MB" and frees its slot. Waiting,
   paused and interrupted downloads come back after closing Neria (paused stay paused)
4. **Duplicate check**: warn when a link (or the same video ID) is already in the queue or
   already downloaded, with "Open existing" / "Download again"
5. **Code signing** to remove the SmartScreen warning: SignPath Foundation signs open-source
   projects for free; the repo is public, so apply and sign in the release workflow
6. Release notes (`docs/releases/v0.3.0.md`), version bump, friends test round two

## Next: UI foundation
The features grew page by page. Make the UI one coherent system, so the features below are
built once on it. Friends' feedback feeds in here.
1. Split the pages out of `app.slint` into per-page/component files; one component set
   (buttons, chips, rows, dialogs, toasts) instead of per-page variations
2. Settings reorganised into sections that match the pages (Download, Clip, Compress,
   Spotify, Network, About)
3. Layouts that adapt to window width (wide desktop → narrow), so the same screens can later
   become the Android layouts
4. Queue filters (active / done / needs attention) and clearer flagged-match states
5. Drag and drop links anywhere (files already work); keyboard navigation and focus states
   everywhere
6. First-run onboarding, better empty states, accessibility pass (contrast, screen-reader
   labels)

## Then: feature tracks
Grouped by page. Tracks can be interleaved; within a track, top first.

### Download
1. **Private and signed-in downloads**: content you can see when signed in: private and
   unlisted-to-you videos, age-restricted videos, members-only, friends-only and group posts,
   Instagram without the anonymous limit, your own Liked/Watch later lists. Today this only
   works through Settings → Network → Cookies, which fails on Chrome/Edge. Better: a "Sign in
   to this site" button on sign-in errors that opens a small sign-in window inside Neria and
   keeps that site's cookies in Neria's folder (portable, per site, "Sign out" in Settings);
   a `cookies.txt` import for power users. Cookies stay on this PC and are only sent to the
   site they belong to. (The browser extension under Later can hand over cookies too)
2. **Captions / subtitles**: a "Captions" picker on the preview listing every language,
   split into **Made by the creator** and **Auto-generated**, the video's own language on
   top; tick one or several (search for long lists, "All" / "None"). SRT (default), VTT, ASS
   or plain text. Separate files (`Title.en.srt`), embedded, or both. **Captions only**
   (`--skip-download`). Remembers the last picked languages; "No captions on this video"
3. **Audio tracks (languages)**: YouTube videos with several audio tracks (dubbed languages,
   descriptive audio, original): an "Audio" picker listing each track by language with
   "Original" marked (the default). Several ticked = separate tracks in one MKV/MP4
   (`--audio-multistreams`), named by language so players show a track menu; Audio only +
   several = one file per language (`Title (Spanish).mp3`). Shown only when there's a choice
4. **Chapters**: a "Chapters" picker on the preview and in the clipper (chapter marks on the
   timeline, click one to select it). Tick any chapters; only those parts are downloaded
   (`--download-sections` per chapter). **Separate files** (`03 Chapter title.mp4` in a
   folder named after the video) or **One file** (neighbours 2 + 3 become one range; gaps
   2 + 4 are joined with a clean cut and the UI says "skips chapter 3"). In the clipper,
   chapters set In/Out or fill the clip render queue. Audio only works the same (one song
   per chapter from a full album upload, numbered and tagged)
5. **Tray, notifications and scheduling**: closing to the tray keeps downloads running
   (optional). A Windows notification when a download/compress/export finishes or fails
   (thumbnail, title, "Open" / "Show in folder"; a group sends one "12 songs saved"). Settings:
   on/off, only when Neria isn't focused, an optional sound (Neria chime, Windows' default or
   none; separate "failed" sound). **Download later** on the Download button's menu: pick a
   time or "overnight", the row waits as "Starts at 02:00" (needs the tray)
6. **More control in the picker**: drag rows into your own order before downloading (numbers
   follow your order); "Reverse" for newest-first playlists; remember the order on
   re-download; a container/audio format per item (today Settings' format applies to all)
7. **Simple format presets** instead of codec/container talk: Best quality · Best
   compatibility (H.264 MP4, plays everywhere) · Smallest file · Audio only; the current
   chips/containers move under Advanced
8. **Filename builder** (drag and drop, no yt-dlp template syntax): field chips (Title,
   Channel, Upload date, Video ID, Playlist, Playlist #, Resolution, Duration, Site…) dragged
   into a line with plain text between them; a `/` makes a folder. Live example using the
   current link; warns about characters Windows doesn't allow and names that would collide.
   Presets ("Title", "Channel - Title", "Date - Title", "Title [ID]", "Playlist / 01 Title"),
   save your own; "Advanced" still shows the raw `-o` template
9. **Bulk and quick download**: paste several links at once (one per line, or a text file) →
   one row per link, skipping duplicates. "Paste & download" with your defaults (skips the
   preview) and an optional clipboard watcher that offers detected links
10. **Queue order**: drag waiting rows to reorder; "Move to top" / "Move to bottom" (Ctrl+↑ /
    Ctrl+↓); running jobs stay put, the next free slot takes the top waiting row; groups move
    as one (and can be reordered inside)
11. **Watch the link before downloading**: a play button on the preview that streams it (same
    player as "Clip first", without the trimming)
12. Later: follow a channel/playlist and download new uploads on a schedule

### Music (Spotify and lyrics)
1. **Synced lyrics from more sources (Musixmatch first).** Musixmatch is where Spotify's own
   synced lyrics come from. Order: LRCLIB (free, open) → Musixmatch (synced) → plain-text
   fallbacks (Genius, AZLyrics, as spotDL does; MIT, port with credit)
   - No user login. Musixmatch's official API only gives synced lyrics on paid plans, so look
     at how spotDL / the `syncedlyrics` project reach it without a key, check it still works
     and what the terms risk is, and record it in `docs/UPSTREAM.md`
   - Fully in our code: no keys or files fetched from third-party repos at runtime
   - Each row says the source and "synced" / "plain text only" / "no lyrics found"; Settings
     lets you turn sources on/off and pick "synced only"
2. **Lyrics only (Spotify links)**: a "Lyrics only" choice next to "Download N songs" that
   saves synced `.lrc` (and/or `.txt`) per song named like the audio would be. Also "Add
   lyrics to songs I already have" for a folder Neria downloaded before
   - Later, opt-in only: "Sign in to Spotify for more synced lyrics", off by default with a
     clear warning about account risk; would need a CLAUDE.md rule change first
3. Optional "Number Spotify playlist songs" (`01 Artist - Title`) so the folder sorts in
   playlist order without the M3U (albums are numbered already)
4. Loose ends from the Spotify work:
   - Faster candidate checks (yt-dlp start-up + YouTube's JS challenge dominate the ~10 s)
   - No-preview-anywhere path is written but not exercised on a real track
   - Own-key Web API path is written but untested (needs a real Client ID/secret)
   - Spotify playlists > 100 songs without a key: no workaround yet beyond the notice

### Clip / edit
1. **Audio controls**, two separate things so it's clear what ends up in the file:
   - **Preview volume**: a volume slider + mute for listening (mpv `volume`); never changes
     the exported clip
   - **Clip audio**, applied on export: keep / mute / gain (± dB) / **normalize** (ffmpeg
     `loudnorm`, presets like -14 LUFS for YouTube/TikTok/Discord, -16 for podcasts) / fade
     in / fade out, with a live preview. Fast · lossless still works: only the audio is
     re-encoded (say so)
   - "Save audio only": extract the selection as MP3/M4A/Opus
2. **GIFs**:
   - **Open and trim a `.gif`** in the clipper (today only video and audio files open), save
     as GIF again or as MP4/WebP, which are much smaller
   - **Make a GIF** from any selection: size, fps and loop, with a file-size estimate
     (palettegen); also WebP/short MP4 "GIF-like" for Discord
   - **Perfect loop** (port of our own loopgif, see `docs/UPSTREAM.md`): finds where the
     motion inside In/Out starts over and snaps to one clean cycle; plays it on repeat before
     export; "Try another loop"; plain "No loop found here". Native Rust, ffmpeg only
3. **Clip render queue**: several In/Out ranges on one video, listed as clips (rename,
   reorder, remove, jump to each), exported in one go (files through a local export queue,
   links as several `--download-sections` jobs); option to join them into one file. Chapters
   (Download track) feed into this
4. **Crop and aspect ratio**: draw a crop box, or pick 9:16 (Shorts/TikTok), 1:1, 4:5, 16:9
   with a draggable frame
5. **Rotate and flip** (90° steps, mirror), applied on export (forces re-encode; say so)
6. **Preview the result** before exporting, with crop/rotate/audio changes applied
7. Links: waveform for streamed links (needs the audio); test live/very long streams

### Compress: understandable for everyone, advanced when needed
1. Plain goals instead of numbers: **For Discord** (20 MB) · **For email** (25 MB) · **For
   WhatsApp** · **Smaller, still sharp** · **As small as possible** · Custom size. Each says
   what it'll look like: "1080p, looks the same" / "720p, a bit softer"
2. **Plain-language options**: "Plays everywhere / Smaller / Smallest" instead of H.264 /
   H.265 / AV1; "Use my graphics card (faster)". Everything technical stays under an
   **Advanced** fold
3. **Before/after**: original vs. estimated size up front, then actual size and "% smaller";
   a side-by-side (or slider) frame comparison before committing
4. **Batch**: drop several files or a folder, same settings for all, one progress list
5. **Trim inside the compressor** (simple In/Out, without going to the Clip page)

## Livestreams (own milestone)
- Two ways to record, picked automatically and said plainly on the preview:
  - **From the start**: when the stream allows rewinding (YouTube with DVR on), download
    everything since it went live (`--live-from-start`) and keep following it live
  - **From now**: when rewinding is off (or unsupported), record from the moment you press
    Download; the preview says "This stream can't be rewound, recording starts now"
- Scheduled streams and premieres: "Waiting for the stream to start" with a countdown
  (`--wait-for-video`)
- Stop button that finishes the file cleanly; recording also ends when the stream ends
- **Live timeline bar** on the queue row: stream start to "now", recorded parts solid, the
  part before you joined hatched, missed segments as visible holes with their time ("missed
  0:42 at 1:13:05"), a pulsing live edge
- **Stats**: recorded length, size, speed, stream bitrate/resolution, how far behind live,
  segments received / retried / missed, connection status
- Clipping a live stream: pick a range from the rewindable window

## Later: bigger ideas (keep UX simple)
- **Browser extension (Chrome, Edge, Firefox)**: a Neria button on video pages and posts, and
  "Download with Neria" when right-clicking a link, image or video. It hands the link to the
  desktop app (a `neria://` link handler, or a localhost port only Neria listens on), which
  comes to the front with the preview ready; "Download now" queues it with your defaults. Can
  also hand over the page's cookies for that one site ("Use my sign-in for this download"),
  which answers Chrome's locked cookies. Portable: registering the handler is a button in
  Settings, not an installer
- **Audio tools page**: **Convert** (MP3, M4A/AAC, Opus, FLAC, WAV; bitrate, mono/stereo,
  sample rate under Advanced; "Extract audio" from videos; batch). **Even out loudness**
  across songs. **Tag editor** (many files at once; "Find tags online" from Spotify).
  **Organize** (Artist / Album folders, M3U playlists, duplicates). Split by timestamps /
  remove silence
- **Library page**: everything Neria downloaded/made, with thumbnails/covers, search,
  audio/video/photo filters, sort by date/size/length; open, show in folder, clip/compress
- **Command palette** (Ctrl+K): type "compress", "extract audio", "clip"… to jump to any
  action; also lists recent files
- **Saved workflows**: simple "after download" actions first (extract audio, compress for
  Discord, move to folder); a builder only if people want chains like download → clip → 9:16
  → compress. Watched folder: auto-compress/convert files dropped into a folder
- **External tools** (instead of a plugin system): Settings → Tools lets power users add
  their own command-line tools as actions with placeholders like `{file}`, `{in}`, `{out}`,
  `{dir}`; jobs run in Neria's queue. No extension code loaded into Neria, nothing bundled

## Android (on hold)
On hold until the desktop app has every feature we want; no Android work before then.
- Same `neria-core`; Slint Android UI (built on the adaptive layouts from the UI foundation);
  embedded Python for yt-dlp; ffmpeg + libmpv as `.so`
- **"Download with Neria" in the share sheet**: in YouTube, Instagram, X, TikTok, Spotify or
  the browser, Share → Neria opens Neria with the link pasted and the preview loading (the
  same picker for posts and playlists). "Download now" skips the app and queues it with your
  defaults, progress in a notification
- APK via GitHub / F-Droid (Play Store bans downloaders)

## Not planned
- Zotify-style downloading of Spotify's own encrypted streams, or streamrip-style downloading
  from Qobuz/Tidal/Deezer accounts (DRM/protection circumvention; gets accounts banned)
- spotDL as a dependency: Neria already does its flow natively in Rust (see
  `docs/SPOTIFY.md` and `docs/UPSTREAM.md`); bundling Python would hurt portability and
  start-up
- Rewriting storage on SQLite / Tokio: JSON settings + manifests and threads are enough
  today; revisit only if the Library page needs it
- Downloading comments, subtitle translation through online services
